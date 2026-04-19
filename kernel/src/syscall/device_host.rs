//! Phase 55b Track B.1 — `sys_device_claim` kernel-side wrapper.
//!
//! Bridges the arch-level syscall dispatcher (`arch::x86_64::syscall::mod.rs`)
//! to the pure-logic `DeviceHostRegistryCore` (in `kernel_core`) and the
//! PCI subsystem's `claim_pci_device_by_bdf`. The wrapper is deliberately
//! thin: it owns the claim-slot state (PCI handles keyed by `(PID, BDF)`),
//! validates the caller, and hands the resulting `Capability::Device` back
//! through the scheduler's per-task capability table.
//!
//! ## Locking contract
//!
//! `DEVICE_HOST_REGISTRY` is the only new lock introduced in this track. It
//! is a narrow [`spin::Mutex`] that protects:
//!
//! 1. the `DeviceHostRegistryCore` (BDF → owning PID mapping), and
//! 2. the backing store that keeps [`crate::pci::PciDeviceHandle`] values
//!    alive for the life of the claim.
//!
//! Lock ordering (top → bottom; outer locks acquired before inner):
//!
//! 1. `crate::task::scheduler::SCHEDULER` — per-process capability tables
//! 2. `DEVICE_HOST_REGISTRY` — this module
//! 3. `crate::pci::PCI_DEVICE_REGISTRY` — PCI claim slots
//! 4. `crate::iommu::registry::*` — IOMMU unit registry
//!
//! `sys_device_claim` acquires these in order. `release_for_pid` (process
//! teardown) only takes the registry lock — the scheduler lock is not held
//! during teardown because the dying task's capabilities have already
//! been cleared by `cleanup_task_ipc`.
//!
//! No lock is held across IPC or page-table operations. No lock is held
//! across `log::*!` calls either — the registry is released before the
//! structured `device_host.claim` event is logged.

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::device_host::{
    DeviceCapKey, DeviceHostError, DeviceHostRegistryCore, RegistryError,
};
use kernel_core::ipc::Capability;
use spin::Mutex;

use crate::pci::{ClaimError, PciDeviceHandle, claim_pci_device_by_bdf};
use crate::process::Pid;
use crate::task::scheduler;

// ---------------------------------------------------------------------------
// Errno constants (duplicated locally so we don't have to reach into the arch
// module). Values match the x86_64 Linux ABI.
// ---------------------------------------------------------------------------

/// Negative errno `-EACCES` (13) encoded as a sign-extended `isize`.
const NEG_EACCES: isize = -13;
/// Negative errno `-EBUSY` (16).
const NEG_EBUSY: isize = -16;
/// Negative errno `-ENODEV` (19).
const NEG_ENODEV: isize = -19;
/// Negative errno `-EBADF` (9).
///
/// Reserved for release paths (double-drop) — B.1 dispatch does not emit
/// this on the claim path, but the process-teardown helper uses it when
/// the caller asks to release an already-released handle.
#[allow(dead_code)]
const NEG_EBADF: isize = -9;
/// Negative errno `-ENOMEM` (12) for capability-table exhaustion.
const NEG_ENOMEM: isize = -12;
/// Negative errno `-ESRCH` (3) when the calling PID cannot be resolved.
const NEG_ESRCH: isize = -3;

/// Driver name recorded in the PCI registry for ring-3 claims.
///
/// Per-driver names would require looking up the calling process's exec
/// path — deferred until the Phase 51 supervisor records the driver name
/// on its `.conf` side. For B.1 the tag is shared by every ring-3 driver.
const RING3_DRIVER_TAG: &str = "ring3-driver";

// ---------------------------------------------------------------------------
// Registry state
// ---------------------------------------------------------------------------

/// One entry in [`DeviceHostRegistry`] — the `PciDeviceHandle` kept alive
/// for the life of the claim, paired with the owning PID.
///
/// Storing the handle here (rather than dropping it after claim) is how we
/// guarantee the IOMMU domain and PCI claim slot survive across the
/// syscall return; the driver's `Capability::Device` is a lightweight
/// alias into this table, not the handle itself.
///
/// `key` is stored alongside the handle so B.2 (`sys_device_mmio_map`) and
/// B.3 (`sys_device_dma_alloc`) can look up a slot by `Capability::Device`
/// key without re-walking `DeviceHostRegistryCore`. The fields are
/// `#[allow(dead_code)]` for B.1 because the lookup API lands in B.2.
struct ClaimSlot {
    pid: Pid,
    #[allow(dead_code)]
    key: DeviceCapKey,
    /// The `PciDeviceHandle` whose `Drop` tears down the IOMMU domain and
    /// returns the PCI registry slot when this entry is removed. The field
    /// is not read by B.1, but it must not be dropped before the claim is
    /// released — keeping it in the slot is the Drop-ordering guarantee.
    #[allow(dead_code)]
    handle: PciDeviceHandle,
}

/// Kernel-side registry that tracks every `Capability::Device` issued to
/// ring-3 driver processes.
///
/// The pure-logic `DeviceHostRegistryCore` keeps the ownership invariant;
/// this struct carries the side-state (live `PciDeviceHandle` values) that
/// cannot live in `kernel-core`. The two fields are always updated under the
/// same lock — see module docs.
struct DeviceHostRegistry {
    core: DeviceHostRegistryCore,
    slots: Vec<ClaimSlot>,
}

impl DeviceHostRegistry {
    const fn new() -> Self {
        Self {
            core: DeviceHostRegistryCore::new(),
            slots: Vec::new(),
        }
    }

    /// Record a claim. `handle` is moved into the registry so its Drop
    /// runs only when the claim is released.
    fn insert_claim(
        &mut self,
        pid: Pid,
        key: DeviceCapKey,
        handle: PciDeviceHandle,
    ) -> Result<(), RegistryError> {
        self.core.try_claim(pid, key)?;
        self.slots.push(ClaimSlot { pid, key, handle });
        Ok(())
    }

    /// Release every claim held by `pid`. Returns the number of freed
    /// slots. Dropping each removed `ClaimSlot` runs `PciDeviceHandle::drop`,
    /// tearing down the IOMMU domain and freeing the PCI registry slot.
    fn release_for_pid(&mut self, pid: Pid) -> usize {
        let freed_keys = self.core.release_for_pid(pid);
        if freed_keys.is_empty() {
            return 0;
        }
        let before = self.slots.len();
        self.slots.retain(|s| s.pid != pid);
        before - self.slots.len()
    }
}

/// Global registry. Narrow `spin::Mutex` — no lock is held across IPC or
/// page-table operations; see module docs for the ordering.
static DEVICE_HOST_REGISTRY: Mutex<DeviceHostRegistry> = Mutex::new(DeviceHostRegistry::new());

// ---------------------------------------------------------------------------
// sys_device_claim
// ---------------------------------------------------------------------------

/// Syscall entry: `sys_device_claim(segment, bus, dev, func) -> isize`.
///
/// Returns a non-negative `CapHandle` on success or a negative errno on
/// failure. See B.1 acceptance in
/// `docs/roadmap/tasks/55b-ring-3-driver-host-tasks.md` for the exact
/// failure surface.
pub fn sys_device_claim(segment: u16, bus: u8, dev: u8, func: u8) -> isize {
    // Resolve caller — we need both its PID (for the registry) and its
    // task id (to drop the capability into its per-task cap table).
    let pid = crate::process::current_pid();
    if pid == 0 {
        // Kernel tasks cannot claim devices through the ring-3 syscall path.
        // A real kernel-context claim would use `claim_pci_device_by_bdf`
        // directly; funneling it through the syscall is a misuse.
        return NEG_ESRCH;
    }
    let task_id = match scheduler::current_task_id() {
        Some(id) => id,
        None => return NEG_ESRCH,
    };

    // FIXME(phase-48): enforce the Phase 48 credential gate — only drivers
    // spawned under the `driver` policy should be permitted to claim
    // devices. Phase 48 credentials are not yet exposed through a policy
    // decision point (see `kernel_core::cred`), so for B.1 the gate is
    // syntactically present (this block) but always permissive. The
    // acceptance contract is: when Phase 48 lands, flip `false` to the
    // real cred check and EACCES will start firing on unauthorized
    // callers without any other change in this file.
    #[allow(clippy::overly_complex_bool_expr)]
    if false {
        return NEG_EACCES;
    }

    // 1) Lock the registry and try to claim the BDF. This is the full
    //    critical section — it covers the PCI claim and the registry
    //    insert so a race between two processes is resolved atomically.
    let key = DeviceCapKey::new(segment, bus, dev, func);
    let claim_result = {
        let mut reg = DEVICE_HOST_REGISTRY.lock();
        // Fast-reject duplicate claims before touching PCI so we do not
        // spuriously acquire-and-release a domain on contention.
        if reg.core.owner_of(key).is_some() {
            Err(DeviceHostError::AlreadyClaimed)
        } else {
            match claim_pci_device_by_bdf(segment, bus, dev, func, RING3_DRIVER_TAG) {
                Ok(handle) => match reg.insert_claim(pid, key, handle) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(DeviceHostError::from(e)),
                },
                Err(ClaimError::NotFound) => Err(DeviceHostError::NotClaimed),
                Err(ClaimError::AlreadyClaimed) => Err(DeviceHostError::AlreadyClaimed),
            }
        }
    };

    if let Err(e) = claim_result {
        return match e {
            DeviceHostError::AlreadyClaimed => NEG_EBUSY,
            // `claim_pci_device_by_bdf` returns `NotFound` for an absent
            // BDF; `NotClaimed` is the corresponding DeviceHostError
            // surface. Map it to ENODEV per acceptance.
            DeviceHostError::NotClaimed => NEG_ENODEV,
            // Any other surface at this site is an internal bug — log and
            // surface as ENODEV so the caller retries / bails rather
            // than interpreting a random errno.
            other => {
                log::warn!(
                    "[device-host] sys_device_claim({segment:#x},{bus:#04x},{dev:#04x},{func}) \
                     unexpected registry error: {other:?}"
                );
                NEG_ENODEV
            }
        };
    }

    // 2) Registry now owns the PciDeviceHandle. Install the capability in
    //    the caller's table.
    let cap = Capability::Device { key };
    let handle = match scheduler::insert_cap(task_id, cap) {
        Ok(h) => h,
        Err(_) => {
            // Unwind: the caller could not receive the capability — drop
            // the registry entry so the device is not left orphaned. A
            // subsequent claim attempt from the same or another process
            // can succeed.
            let mut reg = DEVICE_HOST_REGISTRY.lock();
            let _freed = reg.release_for_pid(pid);
            // Note: release_for_pid frees *every* claim for pid, which is
            // correct here because any prior successful claim this pid
            // held would also have an installed capability; unwinding the
            // current one while leaving others is not observable because
            // the cap-table insertion failed and therefore `pid` cannot
            // make progress. In the common case pid has exactly one
            // claim (the one we just inserted).
            return NEG_ENOMEM;
        }
    };

    // 3) Log the structured claim event outside the registry lock.
    log::info!(
        "device_host.claim pid={} bdf={:04x}:{:02x}:{:02x}.{} cap_handle={}",
        pid,
        segment,
        bus,
        dev,
        func,
        handle
    );

    isize::try_from(handle).unwrap_or(isize::MAX)
}

// ---------------------------------------------------------------------------
// Phase 55b Track B.2 / B.3 / B.4 — stub dispatch targets
// ---------------------------------------------------------------------------
//
// Stubs that return `-ENOSYS` until the corresponding track lands. Each
// track replaces only its own function body; the dispatch arms in
// `arch/x86_64/syscall/mod.rs` already route to these names so a track
// never has to touch the arch dispatcher.

const NEG_ENOSYS: isize = -38;
/// Negative errno `-EINVAL` (22) — invalid argument.
const NEG_EINVAL: isize = -22;
/// Negative errno `-EFAULT` (14) — bad user-pointer argument.
const NEG_EFAULT: isize = -14;
/// Negative errno `-EIO` (5) — IOMMU map/unmap hardware fault.
const NEG_EIO: isize = -5;

/// B.2 — `sys_device_mmio_map` stub. Replaced by Track B.2.
#[allow(unused_variables)]
pub fn sys_device_mmio_map(dev_cap: u32, bar_index: u8) -> isize {
    NEG_ENOSYS
}

/// B.3 — `sys_device_dma_alloc(dev_cap, size, align) -> isize`.
///
/// Strict allocation order per acceptance:
///   1. Validate the `Capability::Device` handle and resolve the target BDF.
///   2. Allocate a `DmaBuffer` (buddy alloc + IOMMU `map`) against the
///      claimed device's domain. `DmaBuffer::allocate` already enforces
///      rollback at this layer: on IOMMU failure it frees the frames.
///   3. Install the user-side page-table mapping (or kernel-virt view in
///      the test / no-AS path).
///   4. Record the allocation in `DMA_REGISTRY` so `handle_info` and
///      process-exit cleanup find it.
///   5. Insert `Capability::Dma` into the caller's cap table.
///
/// Any failure rolls back every earlier step without leaking frames,
/// IOMMU entries, or user mappings.
pub fn sys_device_dma_alloc(dev_cap: u32, size: usize, align: usize) -> isize {
    let pid = crate::process::current_pid();
    if pid == 0 {
        return NEG_ESRCH;
    }
    let task_id = match scheduler::current_task_id() {
        Some(id) => id,
        None => return NEG_ESRCH,
    };

    // Capability validation. A non-Device handle returns -EBADF per B.3.
    let key = match scheduler::task_cap(task_id, dev_cap) {
        Ok(Capability::Device { key }) => key,
        Ok(_) => return NEG_EBADF,
        Err(_) => return NEG_EBADF,
    };

    match alloc_dma_for_pid_impl(pid, key, size, align) {
        Ok(entry) => {
            let cap = Capability::Dma {
                device: key,
                iova: entry.iova,
                len: entry.len,
            };
            match scheduler::insert_cap(task_id, cap) {
                Ok(cap_handle) => {
                    log::info!(
                        "device_host.dma_alloc pid={} bdf={:04x}:{:02x}:{:02x}.{} \
                         size={} iova={:#x} user_va={:#x} cap_handle={}",
                        pid,
                        key.segment,
                        key.bus,
                        key.dev,
                        key.func,
                        entry.len,
                        entry.iova,
                        entry.user_va,
                        cap_handle,
                    );
                    isize::try_from(cap_handle).unwrap_or(isize::MAX)
                }
                Err(_) => {
                    // Roll back the allocation — the caller never
                    // received the capability so the backing storage
                    // would be unreferenced.
                    let _ = remove_dma_entry_by_id(pid, entry.id);
                    NEG_ENOMEM
                }
            }
        }
        Err(e) => map_alloc_error(e),
    }
}

/// B.3 — `sys_device_dma_handle_info(dma_cap, out_user_ptr) -> isize`.
///
/// Reads the `(user_va, iova, len)` triple for the given DMA capability
/// into a caller-provided buffer. Non-`Capability::Dma` handles surface as
/// `-EBADF`. The registry's `(pid, device, iova, len)` is cross-validated
/// against the capability so a racing teardown between cap lookup and
/// record lookup returns `-EBADF` rather than a stale triple.
pub fn sys_device_dma_handle_info(dma_cap: u32, out_user_ptr: usize) -> isize {
    let pid = crate::process::current_pid();
    if pid == 0 {
        return NEG_ESRCH;
    }
    let task_id = match scheduler::current_task_id() {
        Some(id) => id,
        None => return NEG_ESRCH,
    };

    let (cap_device, cap_iova, cap_len) = match scheduler::task_cap(task_id, dma_cap) {
        Ok(Capability::Dma { device, iova, len }) => (device, iova, len),
        Ok(_) => return NEG_EBADF,
        Err(_) => return NEG_EBADF,
    };

    let handle = {
        let reg = DMA_REGISTRY.lock();
        let entries = reg.core.entries_for_pid(pid);
        entries
            .iter()
            .find(|e| e.device == cap_device && e.iova == cap_iova && e.len == cap_len)
            .map(|e| e.as_handle())
    };
    let handle = match handle {
        Some(h) => h,
        None => return NEG_EBADF,
    };

    let bytes = dma_handle_to_bytes(&handle);
    // Try to copy into the caller's buffer. For the ring-3 path this uses
    // the user-AS copy-out primitive; for the test / no-AS path the
    // out_user_ptr may be a kernel-virt address (tests do not call this
    // syscall entry directly — they use `test_dma_handle_info`).
    match copy_dma_handle_out(out_user_ptr, &bytes) {
        Ok(()) => 0,
        Err(_) => NEG_EFAULT,
    }
}

/// B.4 — `sys_device_irq_subscribe` stub. Replaced by Track B.4.
#[allow(unused_variables)]
pub fn sys_device_irq_subscribe(dev_cap: u32, vector_hint: u32, notification_index: u32) -> isize {
    NEG_ENOSYS
}

// ---------------------------------------------------------------------------
// Phase 55b Track B.3 — DMA allocation machinery
// ---------------------------------------------------------------------------

/// Error surface from the internal allocation path. Mapped to a negative
/// errno at the syscall boundary and to [`TestDmaError`] at the test
/// boundary. Each variant names a distinct, observable condition — callers
/// pattern-match rather than parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum DmaAllocError {
    /// No claim recorded under `(pid, key)` — the caller's
    /// `Capability::Device` was never issued or was released.
    NoDevice,
    /// Validation (zero size, bad align, oversized request) rejected the
    /// request.
    InvalidArg,
    /// Buddy allocator out of contiguous memory at the requested order.
    OutOfMemory,
    /// IOMMU `map` call failed (domain out of IOVA, hardware fault).
    IommuFault,
    /// Per-driver DMA slot cap would be exceeded. Reserved for a future
    /// rate-limit; unused in B.3.
    CapExhausted,
    /// Invariant violation — a genuine bug. Mapped to `-EIO` at the
    /// syscall boundary so the driver sees a generic failure rather than
    /// an unexpected errno.
    Internal,
}

fn map_alloc_error(e: DmaAllocError) -> isize {
    match e {
        DmaAllocError::NoDevice => NEG_EBADF,
        DmaAllocError::InvalidArg => NEG_EINVAL,
        DmaAllocError::OutOfMemory => NEG_ENOMEM,
        DmaAllocError::IommuFault => NEG_EIO,
        DmaAllocError::CapExhausted => NEG_ENOMEM,
        DmaAllocError::Internal => NEG_EIO,
    }
}

/// One live DMA allocation slot. Owns:
///   - the `DmaBuffer<[u8]>` (physical frames + IOMMU mapping); `Drop`
///     returns both to their allocators.
///   - a `UserUnmapCtx` when the user-AS mapping was installed in a real
///     process (the test / kernel-context path stores `None` because it
///     aliases the kernel-virt phys_offset window).
///
/// Drop order (tighter than field order): user-AS unmap first so the
/// driver cannot observe a translation to a freed frame, then the
/// `DmaBuffer` drop unmaps IOVA and returns frames.
#[allow(dead_code)]
struct DmaSlot {
    id: kernel_core::device_host::DmaAllocId,
    buffer: Option<crate::mm::dma::DmaBuffer<[u8]>>,
    user_unmap: Option<UserUnmapCtx>,
}

/// Context the Drop path needs to tear down a user-side mapping.
struct UserUnmapCtx {
    cr3_phys: u64,
    user_va: u64,
    pages: usize,
}

impl Drop for DmaSlot {
    fn drop(&mut self) {
        // 1. User-AS unmap (only when a real process AS was mapped).
        if let Some(ctx) = self.user_unmap.take() {
            unmap_user_pages(ctx.cr3_phys, ctx.user_va, ctx.pages);
        }
        // 2. DmaBuffer drop: unmaps IOVA (flushes IOMMU TLB) + frees frames.
        drop(self.buffer.take());
    }
}

/// Kernel-side DMA registry. Pairs the pure-logic registry with live
/// `DmaSlot` storage keyed by the same `DmaAllocId`.
struct DmaRegistry {
    core: kernel_core::device_host::DmaAllocationRegistryCore,
    slots: alloc::collections::BTreeMap<kernel_core::device_host::DmaAllocId, DmaSlot>,
}

impl DmaRegistry {
    const fn new() -> Self {
        Self {
            core: kernel_core::device_host::DmaAllocationRegistryCore::new(),
            slots: alloc::collections::BTreeMap::new(),
        }
    }
}

/// Lock ordering for the DMA registry, relative to the B.1 chain:
///
/// 1. `crate::task::scheduler::SCHEDULER` — per-process capability tables
/// 2. `DEVICE_HOST_REGISTRY` — device claims (B.1)
/// 3. `DMA_REGISTRY` — live DMA allocations (this, B.3)
/// 4. `crate::pci::PCI_DEVICE_REGISTRY`
/// 5. `crate::iommu::registry::*`
/// 6. Buddy allocator
///
/// The B.3 allocation path holds `DEVICE_HOST_REGISTRY` across the
/// `DmaBuffer::allocate` call (which walks 5 + 6) so a concurrent
/// `release_claims_for_pid` cannot race the handle reference. No lock is
/// held across `log::*!` writes.
static DMA_REGISTRY: Mutex<DmaRegistry> = Mutex::new(DmaRegistry::new());

/// Records the domains for which the `device_host.dma_alloc.identity`
/// event has already been emitted. Once per device, per boot, not per
/// allocation.
static IDENTITY_FALLBACK_LOGGED: Mutex<Vec<DeviceCapKey>> = Mutex::new(Vec::new());

/// Internal allocation path shared between the syscall entry and the test
/// helpers. Runs the four-step allocation order; rolls back cleanly on
/// every failure arm.
fn alloc_dma_for_pid_impl(
    pid: Pid,
    key: DeviceCapKey,
    size: usize,
    align: usize,
) -> Result<kernel_core::device_host::DmaAllocEntry, DmaAllocError> {
    // Step 0: validate size / alignment BEFORE taking any lock or
    // allocating any resource. A rejection here does not leak anything.
    let rounded = kernel_core::device_host::validate_size_align(size, align).map_err(|e| {
        use kernel_core::device_host::DmaRegistryError as E;
        match e {
            E::ZeroLen | E::AlignmentNotPowerOfTwo | E::AlignmentTooLarge | E::SizeOverflow => {
                DmaAllocError::InvalidArg
            }
            _ => DmaAllocError::Internal,
        }
    })?;

    // Steps 1-3 (IOVA reserve + phys frames + IOMMU map) under the
    // device-host lock so the PciDeviceHandle reference stays valid. The
    // kernel-side `DmaBuffer::allocate` already rolls back frames if
    // IOMMU install fails, per Phase 55a E.2 — we only need to roll back
    // the reservation bookkeeping on subsequent failures below.
    let (phys, iova, buffer) = {
        let reg = DEVICE_HOST_REGISTRY.lock();
        let slot_idx = reg
            .slots
            .iter()
            .position(|s| s.pid == pid && s.key == key)
            .ok_or(DmaAllocError::NoDevice)?;
        let handle = &reg.slots[slot_idx].handle;
        let buf = crate::mm::dma::DmaBuffer::<[u8]>::allocate(handle, rounded)
            .map_err(map_dma_error_to_alloc_error)?;
        let phys = buf.physical_address().as_u64();
        let iova = buf.bus_address();
        (phys, iova, buf)
    };
    let ident_fallback = iova == phys;

    // Step 4: user-AS mapping. On failure the `buffer` drop unwinds the
    // IOMMU install and frees the frames.
    let (user_va, user_unmap) = match install_user_mapping(pid, phys, rounded) {
        Ok(pair) => pair,
        Err(()) => {
            // Roll back IOMMU + frames via DmaBuffer drop.
            drop(buffer);
            return Err(DmaAllocError::Internal);
        }
    };

    // Step 5: commit the record. Using the DMA registry lock (held
    // separately from the device-host lock) preserves the documented
    // lock ordering (2 → 3).
    let id = {
        let mut reg = DMA_REGISTRY.lock();
        let id = reg.core.insert(pid, key, user_va, iova, rounded);
        reg.slots.insert(
            id,
            DmaSlot {
                id,
                buffer: Some(buffer),
                user_unmap,
            },
        );
        id
    };

    // Identity-fallback structured event — once per device domain.
    if ident_fallback {
        let mut seen = IDENTITY_FALLBACK_LOGGED.lock();
        if !seen.contains(&key) {
            seen.push(key);
            drop(seen);
            log::info!(
                "device_host.dma_alloc.identity bdf={:04x}:{:02x}:{:02x}.{} iova={:#x} len={}",
                key.segment,
                key.bus,
                key.dev,
                key.func,
                iova,
                rounded,
            );
        }
    }

    Ok(kernel_core::device_host::DmaAllocEntry {
        id,
        pid,
        device: key,
        user_va,
        iova,
        len: rounded,
    })
}

/// Install a user-side read/write mapping for the given physical run into
/// the caller's current address space.
///
/// Returns `(user_va, Some(ctx))` when the mapping landed in a real
/// process AS. Returns `(kernel_virt, None)` when the caller has no
/// process AS (kernel test runner task) — the kernel-virt view through
/// `phys_offset` is readable/writable and the B.3 same-byte invariant
/// holds because the kernel-virt view and the IOVA resolve to the same
/// physical frame.
///
/// Rolls back on any per-page mapping failure: already-mapped pages are
/// unmapped in reverse order, the VA reservation is returned to
/// `mmap_next`.
fn install_user_mapping(
    pid: Pid,
    phys: u64,
    len: usize,
) -> Result<(usize, Option<UserUnmapCtx>), ()> {
    let pages = len.div_ceil(4096);
    let Some((cr3_phys, base)) = reserve_user_va_for_pid(pid, pages) else {
        // Kernel-virt fallback — the phys-offset window is always mapped
        // and gives us a readable/writable view on the same frames.
        let kvirt = (crate::mm::phys_offset() + phys) as usize;
        return Ok((kvirt, None));
    };

    use x86_64::VirtAddr;
    use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size4KiB};

    let cr3_frame = match PhysFrame::<Size4KiB>::from_start_address(x86_64::PhysAddr::new(cr3_phys))
    {
        Ok(f) => f,
        Err(_) => {
            release_user_va_reservation(pid, base, pages);
            return Err(());
        }
    };

    let pt_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    // SAFETY: cr3_frame names the caller's PML4. No other OffsetPageTable
    // over the same frame is alive on this core.
    let mut mapper = unsafe { crate::mm::mapper_for_frame(cr3_frame) };
    let mut alloc = crate::mm::paging::GlobalFrameAlloc;
    let mut mapped: Vec<u64> = Vec::new();
    for i in 0..pages {
        let p = phys + (i as u64) * 4096;
        let frame = match PhysFrame::<Size4KiB>::from_start_address(x86_64::PhysAddr::new(p)) {
            Ok(f) => f,
            Err(_) => {
                // Roll back already-mapped pages.
                for va in mapped.iter().rev() {
                    let pg: Page<Size4KiB> = Page::containing_address(VirtAddr::new(*va));
                    if let Ok((_f, flush)) = mapper.unmap(pg) {
                        flush.flush();
                    }
                }
                release_user_va_reservation(pid, base, pages);
                return Err(());
            }
        };
        let page: Page<Size4KiB> =
            Page::containing_address(VirtAddr::new(base + (i as u64) * 4096));
        match unsafe { mapper.map_to(page, frame, pt_flags, &mut alloc) } {
            Ok(flush) => {
                flush.flush();
                mapped.push(page.start_address().as_u64());
            }
            Err(_) => {
                for va in mapped.iter().rev() {
                    let pg: Page<Size4KiB> = Page::containing_address(VirtAddr::new(*va));
                    if let Ok((_f, flush)) = mapper.unmap(pg) {
                        flush.flush();
                    }
                }
                release_user_va_reservation(pid, base, pages);
                return Err(());
            }
        }
    }

    Ok((
        base as usize,
        Some(UserUnmapCtx {
            cr3_phys,
            user_va: base,
            pages,
        }),
    ))
}

/// Attempt to reserve `pages` contiguous pages of user VA from the
/// process's `mmap_next` bump pointer. Returns `None` when the process
/// has no address space (e.g. the kernel test runner).
fn reserve_user_va_for_pid(pid: Pid, pages: usize) -> Option<(u64, u64)> {
    const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;
    const ANON_MMAP_BASE: u64 = 0x0000_0000_2000_0000;
    let bytes = (pages as u64).checked_mul(4096)?;
    let cr3: u64 = {
        let table = crate::process::PROCESS_TABLE.lock();
        table
            .find(pid)
            .and_then(|p| p.addr_space.as_ref().map(|a| a.pml4_phys().as_u64()))?
    };
    let base = crate::process::with_shared_mm_mut(pid, |_brk, mmap_next, _vmas| {
        let current = if *mmap_next == 0 {
            ANON_MMAP_BASE
        } else {
            *mmap_next
        };
        let end = current
            .checked_add(bytes)
            .filter(|v| *v <= USER_SPACE_END)?;
        *mmap_next = end;
        Some(current)
    })??;
    Some((cr3, base))
}

/// Roll back a user VA reservation. Only returns the VA to `mmap_next`
/// when the reservation is still the tail — subsequent allocations may
/// have bumped past it. That is acceptable: the VA window is 128 TiB and
/// drivers do not churn allocations.
fn release_user_va_reservation(pid: Pid, base: u64, pages: usize) {
    let bytes = (pages as u64) * 4096;
    let _ = crate::process::with_shared_mm_mut(pid, |_brk, mmap_next, _vmas| {
        if *mmap_next == base + bytes {
            *mmap_next = base;
        }
    });
}

/// Tear down a user-side mapping installed by [`install_user_mapping`].
fn unmap_user_pages(cr3_phys: u64, base: u64, pages: usize) {
    use x86_64::VirtAddr;
    use x86_64::structures::paging::{Mapper, Page, PhysFrame, Size4KiB};
    let cr3_frame = match PhysFrame::<Size4KiB>::from_start_address(x86_64::PhysAddr::new(cr3_phys))
    {
        Ok(f) => f,
        Err(_) => {
            log::warn!(
                "[device-host] dma unmap skipped: cr3 not aligned ({:#x})",
                cr3_phys
            );
            return;
        }
    };
    let mut mapper = unsafe { crate::mm::mapper_for_frame(cr3_frame) };
    for i in 0..pages {
        let page: Page<Size4KiB> =
            Page::containing_address(VirtAddr::new(base + (i as u64) * 4096));
        if let Ok((_f, flush)) = mapper.unmap(page) {
            flush.flush();
        }
    }
}

fn map_dma_error_to_alloc_error(e: crate::mm::dma::DmaError) -> DmaAllocError {
    use crate::mm::dma::DmaError;
    match e {
        DmaError::ZeroSize
        | DmaError::SizeOverflow
        | DmaError::UnsupportedAlignment
        | DmaError::InvalidSize => DmaAllocError::InvalidArg,
        DmaError::OutOfMemory => DmaAllocError::OutOfMemory,
        DmaError::IovaExhausted | DmaError::DomainHardwareFault => DmaAllocError::IommuFault,
        DmaError::NoDomainAttached => DmaAllocError::NoDevice,
    }
}

/// Remove a single DMA slot by id, owned by `pid`. Used on the
/// cap-table-install rollback path and by the test helpers.
fn remove_dma_entry_by_id(pid: Pid, id: kernel_core::device_host::DmaAllocId) -> bool {
    let slot = {
        let mut reg = DMA_REGISTRY.lock();
        if reg.core.remove_owned(id, pid).is_err() {
            return false;
        }
        reg.slots.remove(&id)
    };
    drop(slot);
    true
}

/// Release every DMA allocation owned by `pid`.
///
/// Called from `do_full_process_exit` so a driver crash or kill
/// automatically frees its DMA state. Safe for a PID that holds no
/// allocations.
pub fn release_dma_for_pid(pid: Pid) -> usize {
    let drained_slots = {
        let mut reg = DMA_REGISTRY.lock();
        let drained = reg.core.drain_pid(pid);
        let mut slots: Vec<DmaSlot> = Vec::with_capacity(drained.len());
        for entry in &drained {
            if let Some(slot) = reg.slots.remove(&entry.id) {
                slots.push(slot);
            }
        }
        slots
    };
    let count = drained_slots.len();
    drop(drained_slots);
    if count > 0 {
        log::info!("device_host.dma_release pid={} freed={}", pid, count);
    }
    count
}

fn dma_handle_to_bytes(h: &kernel_core::device_host::DmaHandle) -> [u8; 24] {
    let mut out = [0u8; 24];
    out[0..8].copy_from_slice(&(h.user_va as u64).to_le_bytes());
    out[8..16].copy_from_slice(&h.iova.to_le_bytes());
    out[16..24].copy_from_slice(&(h.len as u64).to_le_bytes());
    out
}

/// Copy the 24-byte DmaHandle representation into the caller-provided
/// buffer. Uses the user-AS copy-out path when the caller has an address
/// space; falls through to a direct kernel-virt write for the no-AS test
/// path.
fn copy_dma_handle_out(dst: usize, bytes: &[u8; 24]) -> Result<(), ()> {
    let dst_u64 = dst as u64;
    // Validate that the target range lies in canonical user space. If it
    // does not, treat the pointer as a kernel-virt write (tests use this
    // path; real syscalls would reject this with EFAULT through the
    // upstream validator).
    if dst_u64 < 0x0000_8000_0000_0000 {
        // User-space address. Walk the caller's page tables to copy.
        // `copy_from_kernel` validates the range and copies through the
        // phys-offset window.
        let out = crate::mm::user_mem::UserSliceWo::new(dst_u64, bytes.len()).map_err(|_| ())?;
        out.copy_from_kernel(bytes)?;
        Ok(())
    } else {
        // Kernel-virt address (test path).
        // SAFETY: dst is a kernel-virt address inside the phys-offset
        // window; caller guarantees the 24 bytes are writable.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Process-exit hook
// ---------------------------------------------------------------------------

/// Release every claim held by `pid`.
///
/// Called from `do_full_process_exit` (arch/x86_64/syscall) so a driver
/// crash or kill automatically frees the devices it owned for the
/// supervisor restart to re-claim. Safe to call for a PID that holds no
/// claims — returns zero and performs no I/O.
pub fn release_claims_for_pid(pid: Pid) {
    let freed = {
        let mut reg = DEVICE_HOST_REGISTRY.lock();
        reg.release_for_pid(pid)
    };
    if freed > 0 {
        log::info!("device_host.release pid={} freed_claims={}", pid, freed);
    }
}

// ---------------------------------------------------------------------------
// Test-only helpers (Phase 55b Track B.1)
// ---------------------------------------------------------------------------
//
// Expose a minimal surface for the kernel-side `#[test_case]` harness
// without leaking the registry state to the rest of the kernel. These
// helpers bypass the `current_pid()` lookup (which returns 0 inside the
// test runner task) so the invariants can be exercised without booting a
// real ring-3 driver. The userspace-side integration test lands with
// Track D.1 once the stub NVMe driver exists.

/// Error returned by [`test_try_claim_for_pid`] mirroring the public
/// syscall boundary — but typed rather than negative errno so tests can
/// pattern-match directly.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestClaimError {
    Busy,
    NoDev,
    #[allow(dead_code)]
    Internal,
}

/// Register a claim for `pid` on `key` WITHOUT the capability-table
/// insertion. Used in `#[test_case]` paths that simulate two driver
/// processes racing on the same BDF.
///
/// The test must arrange for a BDF that `pci_device()` returns; tests that
/// run before PCI enumeration can drive the `DeviceHostRegistryCore`
/// directly instead (see the kernel-core unit tests).
#[cfg(test)]
pub(crate) fn test_try_claim_for_pid(pid: Pid, key: DeviceCapKey) -> Result<(), TestClaimError> {
    let mut reg = DEVICE_HOST_REGISTRY.lock();
    if reg.core.owner_of(key).is_some() {
        return Err(TestClaimError::Busy);
    }
    match claim_pci_device_by_bdf(
        u16::from(key.segment),
        key.bus,
        key.dev,
        key.func,
        RING3_DRIVER_TAG,
    ) {
        Ok(handle) => match reg.insert_claim(pid, key, handle) {
            Ok(()) => Ok(()),
            Err(RegistryError::AlreadyClaimed) => Err(TestClaimError::Busy),
            Err(_) => Err(TestClaimError::Internal),
        },
        Err(ClaimError::NotFound) => Err(TestClaimError::NoDev),
        Err(ClaimError::AlreadyClaimed) => Err(TestClaimError::Busy),
    }
}

/// Drop every claim registered to `pid`, without going through the
/// process-exit path.
#[cfg(test)]
pub(crate) fn test_release_for_pid(pid: Pid) -> usize {
    let mut reg = DEVICE_HOST_REGISTRY.lock();
    reg.release_for_pid(pid)
}

/// Query the current owner of a BDF (for test assertions).
#[cfg(test)]
pub(crate) fn test_owner_of(key: DeviceCapKey) -> Option<Pid> {
    let reg = DEVICE_HOST_REGISTRY.lock();
    reg.core.owner_of(key)
}

// ---------------------------------------------------------------------------
// Phase 55b Track B.3 — test-only helpers for the DMA-alloc path
// ---------------------------------------------------------------------------
//
// These mirror the `test_try_claim_for_pid` / `test_release_for_pid` surface
// introduced by B.1. They drive `sys_device_dma_alloc` / `sys_device_dma_handle_info`
// without going through the capability table, because the kernel test runner
// task does not have a user address space or a Capability::Device installed.
// The real ring-3 path is exercised by Track D.1's NVMe integration test.

/// Error surface exposed to kernel tests. Not `#[non_exhaustive]` because
/// tests want exhaustive matches.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestDmaError {
    /// `pid` does not own a claim on `key`.
    NoDevice,
    /// Size / alignment validation rejected the request.
    InvalidArg,
    /// Buddy allocator out of memory.
    OutOfMemory,
    /// IOMMU map failed.
    IommuFault,
    /// Any other invariant violation (a bug, not a caller-visible condition).
    Internal,
}

/// Snapshot of a live DMA allocation. Mirrors `DmaHandle` with the id so the
/// test can look the entry up again later.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TestDmaSnapshot {
    pub id: u64,
    pub user_va: usize,
    pub iova: u64,
    pub len: usize,
}

/// Drive the B.3 allocation path for `pid`, assuming the caller already
/// claimed `key` via `test_try_claim_for_pid`. Returns the snapshot on
/// success, the typed error on failure.
#[cfg(test)]
pub(crate) fn test_dma_alloc_for_pid(
    pid: Pid,
    key: DeviceCapKey,
    size: usize,
    align: usize,
) -> Result<TestDmaSnapshot, TestDmaError> {
    alloc_dma_for_pid_impl(pid, key, size, align)
        .map(|entry| TestDmaSnapshot {
            id: entry.id.0,
            user_va: entry.user_va,
            iova: entry.iova,
            len: entry.len,
        })
        .map_err(|e| match e {
            DmaAllocError::NoDevice => TestDmaError::NoDevice,
            DmaAllocError::InvalidArg => TestDmaError::InvalidArg,
            DmaAllocError::OutOfMemory => TestDmaError::OutOfMemory,
            DmaAllocError::IommuFault => TestDmaError::IommuFault,
            DmaAllocError::Internal => TestDmaError::Internal,
            DmaAllocError::CapExhausted => TestDmaError::Internal,
        })
}

/// Look up a live allocation by `(pid, id)` — the test-harness equivalent of
/// `sys_device_dma_handle_info`.
#[cfg(test)]
pub(crate) fn test_dma_handle_info(pid: Pid, id: u64) -> Option<TestDmaSnapshot> {
    let reg = DMA_REGISTRY.lock();
    let entry = reg
        .core
        .get_owned(kernel_core::device_host::DmaAllocId(id), pid)
        .ok()?;
    Some(TestDmaSnapshot {
        id: entry.id.0,
        user_va: entry.user_va,
        iova: entry.iova,
        len: entry.len,
    })
}

/// Drop every live DMA allocation for `pid`. Returns the number of slots
/// freed. Mirrors what `release_dma_for_pid` does in the process-exit path.
#[cfg(test)]
pub(crate) fn test_dma_release_for_pid(pid: Pid) -> usize {
    release_dma_for_pid(pid)
}

/// Count live DMA allocations (diagnostic).
#[cfg(test)]
pub(crate) fn test_dma_count() -> usize {
    DMA_REGISTRY.lock().core.len()
}
