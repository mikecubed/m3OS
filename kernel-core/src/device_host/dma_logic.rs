// Pure-logic DMA allocation bookkeeping — Phase 55b Track B.3.
//
// The kernel-side wrapper (`kernel/src/syscall/device_host.rs`) plugs physical
// frames, IOMMU mappings, and user-AS page mappings around this type. Keeping
// the allocation math, cross-device isolation, and handle-lookup invariants in
// `kernel-core` lets every B.3 invariant (rollback, cross-device negation,
// identity-fallback iova == phys, per-pid cleanup) run as host tests without
// booting a kernel.
//
// Stored data: for each live `Capability::Dma`, the owning `(pid, device_key)`,
// the IOVA the device sees (or phys, under identity fallback), the user VA the
// driver reads/writes, and the length. The kernel side keeps the physical
// frame / IOMMU / user-AS state alongside each entry, keyed by the same
// `DmaAllocId` this core hands back.

extern crate alloc;

use alloc::vec::Vec;

use super::registry_logic::RegistryPid;
use super::types::{DeviceCapKey, DmaHandle};

/// Opaque identifier for a single live DMA allocation.
///
/// Returned by [`DmaAllocationRegistryCore::insert`] and passed back through
/// [`DmaAllocationRegistryCore::remove_owned`] / [`DmaAllocationRegistryCore::get_owned`].
/// The numeric value is not stable across allocations — treat as opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DmaAllocId(pub u64);

/// Errors returned by the pure-logic DMA allocation bookkeeping.
///
/// Data (not strings) so both the kernel-side syscall dispatcher and the host
/// tests can pattern-match on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DmaRegistryError {
    /// Allocation size was zero.
    ZeroLen,
    /// Requested alignment is not a power of two.
    AlignmentNotPowerOfTwo,
    /// Requested alignment is larger than one page (4 KiB) — the kernel-side
    /// allocator cannot satisfy alignment greater than the page the buddy
    /// allocator hands out.
    AlignmentTooLarge,
    /// Allocation size request overflowed `usize` when rounded up to the
    /// minimum granularity.
    SizeOverflow,
    /// `DmaAllocId` does not refer to a live allocation.
    NotFound,
    /// Entry exists but names a different `(pid)` owner.
    WrongOwner,
}

/// Single live DMA allocation record.
///
/// `pid` owns the handle through the process's capability table; `device`
/// names the PCI device whose IOMMU domain the IOVA lives in. The kernel-side
/// wrapper pairs each entry with a `DmaBuffer` (phys frames, IOMMU mapping,
/// user-AS mapping) keyed by the same `DmaAllocId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaAllocEntry {
    pub id: DmaAllocId,
    pub pid: RegistryPid,
    pub device: DeviceCapKey,
    pub user_va: usize,
    pub iova: u64,
    pub len: usize,
}

impl DmaAllocEntry {
    /// Decompose into a runtime [`DmaHandle`] the `sys_device_dma_handle_info`
    /// syscall copies into the caller-provided buffer.
    pub const fn as_handle(&self) -> DmaHandle {
        DmaHandle {
            user_va: self.user_va,
            iova: self.iova,
            len: self.len,
        }
    }
}

/// Minimum DMA allocation granularity (4 KiB pages).
pub const DMA_MIN_ALIGN: usize = 4096;

/// Validate (and round up) a `(size, align)` request.
///
/// Returns the rounded-up byte count suitable for passing to the buddy
/// allocator, or a typed error. Alignment must be a power of two and at most
/// one page — the kernel-side buddy allocator produces whole pages and cannot
/// satisfy stricter alignment.
pub fn validate_size_align(size: usize, align: usize) -> Result<usize, DmaRegistryError> {
    if size == 0 {
        return Err(DmaRegistryError::ZeroLen);
    }
    let align = if align == 0 { DMA_MIN_ALIGN } else { align };
    if !align.is_power_of_two() {
        return Err(DmaRegistryError::AlignmentNotPowerOfTwo);
    }
    if align > DMA_MIN_ALIGN {
        return Err(DmaRegistryError::AlignmentTooLarge);
    }
    let pages = size
        .checked_add(DMA_MIN_ALIGN - 1)
        .ok_or(DmaRegistryError::SizeOverflow)?
        / DMA_MIN_ALIGN;
    pages
        .checked_mul(DMA_MIN_ALIGN)
        .ok_or(DmaRegistryError::SizeOverflow)
}

/// Pure-logic backing store for per-pid DMA allocations.
#[derive(Default)]
pub struct DmaAllocationRegistryCore {
    entries: Vec<DmaAllocEntry>,
    next_id: u64,
}

impl DmaAllocationRegistryCore {
    /// Construct an empty registry.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
        }
    }

    /// Record a new live DMA allocation. Returns the assigned `DmaAllocId`.
    pub fn insert(
        &mut self,
        pid: RegistryPid,
        device: DeviceCapKey,
        user_va: usize,
        iova: u64,
        len: usize,
    ) -> DmaAllocId {
        let id = DmaAllocId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.entries.push(DmaAllocEntry {
            id,
            pid,
            device,
            user_va,
            iova,
            len,
        });
        id
    }

    /// Look up an entry by id, confirming `pid` ownership.
    pub fn get_owned(
        &self,
        id: DmaAllocId,
        pid: RegistryPid,
    ) -> Result<DmaAllocEntry, DmaRegistryError> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.id == id)
            .copied()
            .ok_or(DmaRegistryError::NotFound)?;
        if entry.pid != pid {
            return Err(DmaRegistryError::WrongOwner);
        }
        Ok(entry)
    }

    /// Remove and return the entry by id, confirming `pid` ownership.
    pub fn remove_owned(
        &mut self,
        id: DmaAllocId,
        pid: RegistryPid,
    ) -> Result<DmaAllocEntry, DmaRegistryError> {
        let pos = self
            .entries
            .iter()
            .position(|e| e.id == id)
            .ok_or(DmaRegistryError::NotFound)?;
        if self.entries[pos].pid != pid {
            return Err(DmaRegistryError::WrongOwner);
        }
        Ok(self.entries.swap_remove(pos))
    }

    /// Remove every entry owned by `pid`; returns the drained list so the
    /// kernel-side wrapper can tear down matching `DmaBuffer` state in one
    /// pass.
    pub fn drain_pid(&mut self, pid: RegistryPid) -> Vec<DmaAllocEntry> {
        let mut drained: Vec<DmaAllocEntry> = Vec::new();
        self.entries.retain(|e| {
            if e.pid == pid {
                drained.push(*e);
                false
            } else {
                true
            }
        });
        drained
    }

    /// Diagnostic: number of live allocations.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the registry has no live allocations.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Diagnostic: count live allocations matching `key`.
    pub fn count_for_device(&self, key: DeviceCapKey) -> usize {
        self.entries.iter().filter(|e| e.device == key).count()
    }

    /// Snapshot of every live entry owned by `pid`.
    pub fn entries_for_pid(&self, pid: RegistryPid) -> Vec<DmaAllocEntry> {
        self.entries
            .iter()
            .filter(|e| e.pid == pid)
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BDF_A: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x03, 0);
    const BDF_B: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x04, 0);

    const PID_A: RegistryPid = 100;
    const PID_B: RegistryPid = 200;

    // ---- validate_size_align ------------------------------------------

    #[test]
    fn validate_rejects_zero_size() {
        assert_eq!(validate_size_align(0, 4096), Err(DmaRegistryError::ZeroLen));
    }

    #[test]
    fn validate_accepts_one_byte_and_rounds_to_page() {
        assert_eq!(validate_size_align(1, 4096), Ok(4096));
        assert_eq!(validate_size_align(1, 0), Ok(4096));
    }

    #[test]
    fn validate_rounds_up_partial_pages() {
        assert_eq!(validate_size_align(4097, 4096), Ok(8192));
        assert_eq!(validate_size_align(5 * 1024, 4096), Ok(8192));
    }

    #[test]
    fn validate_rejects_non_power_of_two_alignment() {
        assert_eq!(
            validate_size_align(4096, 3),
            Err(DmaRegistryError::AlignmentNotPowerOfTwo)
        );
        assert_eq!(
            validate_size_align(4096, 12),
            Err(DmaRegistryError::AlignmentNotPowerOfTwo)
        );
    }

    #[test]
    fn validate_rejects_oversize_alignment() {
        assert_eq!(
            validate_size_align(4096, 8192),
            Err(DmaRegistryError::AlignmentTooLarge)
        );
        assert_eq!(
            validate_size_align(4096, 65_536),
            Err(DmaRegistryError::AlignmentTooLarge)
        );
    }

    #[test]
    fn validate_accepts_sub_page_alignments() {
        for align in [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096] {
            assert!(validate_size_align(4096, align).is_ok());
        }
    }

    #[test]
    fn validate_reports_size_overflow() {
        let err = validate_size_align(usize::MAX, 4096).unwrap_err();
        assert_eq!(err, DmaRegistryError::SizeOverflow);
    }

    // ---- insert / get_owned / remove_owned ---------------------------

    #[test]
    fn insert_returns_unique_ids() {
        let mut reg = DmaAllocationRegistryCore::new();
        let a = reg.insert(PID_A, BDF_A, 0x1_0000, 0x2_0000, 0x1000);
        let b = reg.insert(PID_A, BDF_A, 0x3_0000, 0x4_0000, 0x1000);
        assert_ne!(a, b);
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn get_owned_returns_full_entry() {
        let mut reg = DmaAllocationRegistryCore::new();
        let id = reg.insert(PID_A, BDF_A, 0x1_0000, 0x2_0000, 0x1000);
        let entry = reg.get_owned(id, PID_A).expect("owner lookup");
        assert_eq!(entry.pid, PID_A);
        assert_eq!(entry.device, BDF_A);
        assert_eq!(entry.user_va, 0x1_0000);
        assert_eq!(entry.iova, 0x2_0000);
        assert_eq!(entry.len, 0x1000);
        let handle = entry.as_handle();
        assert_eq!(handle.user_va, 0x1_0000);
        assert_eq!(handle.iova, 0x2_0000);
        assert_eq!(handle.len, 0x1000);
    }

    #[test]
    fn get_owned_by_wrong_pid_returns_wrong_owner() {
        let mut reg = DmaAllocationRegistryCore::new();
        let id = reg.insert(PID_A, BDF_A, 0x1_0000, 0x2_0000, 0x1000);
        assert_eq!(reg.get_owned(id, PID_B), Err(DmaRegistryError::WrongOwner));
    }

    #[test]
    fn get_owned_unknown_id_returns_not_found() {
        let reg = DmaAllocationRegistryCore::new();
        assert_eq!(
            reg.get_owned(DmaAllocId(42), PID_A),
            Err(DmaRegistryError::NotFound)
        );
    }

    #[test]
    fn remove_owned_frees_slot() {
        let mut reg = DmaAllocationRegistryCore::new();
        let id = reg.insert(PID_A, BDF_A, 0x1_0000, 0x2_0000, 0x1000);
        assert_eq!(reg.len(), 1);
        let removed = reg.remove_owned(id, PID_A).expect("owner remove");
        assert_eq!(removed.id, id);
        assert_eq!(reg.len(), 0);
        assert_eq!(reg.remove_owned(id, PID_A), Err(DmaRegistryError::NotFound));
    }

    #[test]
    fn remove_owned_by_wrong_pid_does_not_free_slot() {
        let mut reg = DmaAllocationRegistryCore::new();
        let id = reg.insert(PID_A, BDF_A, 0x1_0000, 0x2_0000, 0x1000);
        assert_eq!(
            reg.remove_owned(id, PID_B),
            Err(DmaRegistryError::WrongOwner)
        );
        assert!(reg.get_owned(id, PID_A).is_ok());
        assert_eq!(reg.len(), 1);
    }

    // ---- cross-device isolation --------------------------------------

    #[test]
    fn cross_device_entries_do_not_alias_by_iova() {
        // B.3 cross-device invariant: even if two devices happen to live
        // at the same IOVA value (identity fallback with overlapping
        // physical addresses is not realistic but domain translation can
        // hand out the same IOVA in two different domains), the registry
        // MUST distinguish the entries by `(id, device)` so a cap forged
        // for BDF B cannot be used to address BDF A's allocation.
        let mut reg = DmaAllocationRegistryCore::new();
        let a = reg.insert(PID_A, BDF_A, 0x0, 0x4000_0000, 0x1000);
        let b = reg.insert(PID_A, BDF_B, 0x0, 0x4000_0000, 0x1000);
        assert_ne!(a, b);

        let ea = reg.get_owned(a, PID_A).unwrap();
        let eb = reg.get_owned(b, PID_A).unwrap();
        assert_eq!(ea.device, BDF_A);
        assert_eq!(eb.device, BDF_B);
        assert_eq!(ea.iova, eb.iova);
    }

    #[test]
    fn count_for_device_segregates_per_bdf() {
        let mut reg = DmaAllocationRegistryCore::new();
        reg.insert(PID_A, BDF_A, 0x0, 0x1_0000, 0x1000);
        reg.insert(PID_A, BDF_A, 0x1_0000, 0x2_0000, 0x1000);
        reg.insert(PID_A, BDF_B, 0x2_0000, 0x3_0000, 0x1000);
        assert_eq!(reg.count_for_device(BDF_A), 2);
        assert_eq!(reg.count_for_device(BDF_B), 1);
    }

    // ---- drain_pid (process-exit path) -------------------------------

    #[test]
    fn drain_pid_returns_only_owned_entries() {
        let mut reg = DmaAllocationRegistryCore::new();
        let a = reg.insert(PID_A, BDF_A, 0x0, 0x1_0000, 0x1000);
        let b = reg.insert(PID_A, BDF_B, 0x1_0000, 0x2_0000, 0x1000);
        let c = reg.insert(PID_B, BDF_A, 0x2_0000, 0x3_0000, 0x1000);

        let drained = reg.drain_pid(PID_A);
        assert_eq!(drained.len(), 2);
        let ids: Vec<DmaAllocId> = drained.iter().map(|e| e.id).collect();
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));

        assert!(reg.get_owned(c, PID_B).is_ok());
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn drain_pid_with_no_entries_returns_empty() {
        let mut reg = DmaAllocationRegistryCore::new();
        reg.insert(PID_A, BDF_A, 0x0, 0x1_0000, 0x1000);
        let drained = reg.drain_pid(PID_B);
        assert!(drained.is_empty());
        assert_eq!(reg.len(), 1);
    }

    // ---- entries_for_pid (info lookup path) --------------------------

    #[test]
    fn entries_for_pid_enumerates_only_owned() {
        let mut reg = DmaAllocationRegistryCore::new();
        reg.insert(PID_A, BDF_A, 0x0, 0x1_0000, 0x1000);
        reg.insert(PID_B, BDF_A, 0x1_0000, 0x2_0000, 0x1000);
        reg.insert(PID_A, BDF_B, 0x2_0000, 0x3_0000, 0x1000);

        let a_list = reg.entries_for_pid(PID_A);
        assert_eq!(a_list.len(), 2);
        assert!(a_list.iter().all(|e| e.pid == PID_A));

        let b_list = reg.entries_for_pid(PID_B);
        assert_eq!(b_list.len(), 1);
        assert_eq!(b_list[0].pid, PID_B);
    }

    // ---- identity-fallback invariant ---------------------------------

    #[test]
    fn identity_fallback_records_iova_equal_to_phys() {
        // B.3 acceptance: in identity fallback, IOVA equals the physical
        // address. The registry records whatever the kernel supplies;
        // this pins that the handle-info path exposes it verbatim.
        let mut reg = DmaAllocationRegistryCore::new();
        let phys: u64 = 0x4000_0000;
        let id = reg.insert(PID_A, BDF_A, 0x1234_0000, phys, 0x2000);
        let entry = reg.get_owned(id, PID_A).unwrap();
        assert_eq!(
            entry.iova, phys,
            "identity fallback: iova must equal phys in the registry record"
        );
        assert_eq!(entry.as_handle().iova, phys);
    }

    // ---- id stability across insertions ------------------------------

    #[test]
    fn id_is_not_recycled_after_remove() {
        // After remove, a new insertion must not reuse the old id, so a
        // stale capability that races with cleanup cannot accidentally
        // address a fresh allocation.
        let mut reg = DmaAllocationRegistryCore::new();
        let a = reg.insert(PID_A, BDF_A, 0x0, 0x1_0000, 0x1000);
        reg.remove_owned(a, PID_A).unwrap();
        let b = reg.insert(PID_A, BDF_A, 0x0, 0x1_0000, 0x1000);
        assert_ne!(a, b);
    }
}
