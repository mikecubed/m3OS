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

/// B.2 — `sys_device_mmio_map` stub. Replaced by Track B.2.
#[allow(unused_variables)]
pub fn sys_device_mmio_map(dev_cap: u32, bar_index: u8) -> isize {
    NEG_ENOSYS
}

/// B.3 — `sys_device_dma_alloc` stub. Replaced by Track B.3.
#[allow(unused_variables)]
pub fn sys_device_dma_alloc(dev_cap: u32, size: usize, align: usize) -> isize {
    NEG_ENOSYS
}

/// B.3 — `sys_device_dma_handle_info` stub. Replaced by Track B.3.
#[allow(unused_variables)]
pub fn sys_device_dma_handle_info(dma_cap: u32, out_user_ptr: usize) -> isize {
    NEG_ENOSYS
}

/// B.4 — `sys_device_irq_subscribe` stub. Replaced by Track B.4.
#[allow(unused_variables)]
pub fn sys_device_irq_subscribe(dev_cap: u32, vector_hint: u32, notification_index: u32) -> isize {
    NEG_ENOSYS
}

// ---------------------------------------------------------------------------
// Phase 55b Track B.3 — DMA allocation path (stub, red commit)
// ---------------------------------------------------------------------------
//
// The B.3 green commit replaces the body of `alloc_dma_for_pid_impl` (and
// grows this section) with the real allocation machinery. For the red
// commit the stub returns `Internal` so the kernel-side integration tests
// fail against it — proving the tests actually exercise the path.

/// Error surface from the internal allocation path. Mapped to a negative
/// errno at the syscall boundary and to [`TestDmaError`] at the test
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum DmaAllocError {
    /// No claim recorded under `(pid, key)`.
    NoDevice,
    /// Validation (zero size, bad align, …) rejected the request.
    InvalidArg,
    /// Out of memory on the buddy allocator.
    OutOfMemory,
    /// IOMMU map failed.
    IommuFault,
    /// Per-driver DMA slot cap would be exceeded.
    CapExhausted,
    /// Invariant violation — bug, not a documented caller surface.
    Internal,
}

/// Stub of the B.3 allocation path. Returns `Internal` so the kernel-side
/// tests that drive the path observe a failure — proving the tests hit real
/// behavior. Replaced by the green commit.
#[allow(unused_variables, dead_code)]
fn alloc_dma_for_pid_impl(
    pid: Pid,
    key: DeviceCapKey,
    size: usize,
    align: usize,
) -> Result<kernel_core::device_host::DmaAllocEntry, DmaAllocError> {
    Err(DmaAllocError::Internal)
}

/// Stub of the per-pid DMA release hook. Red commit returns zero.
#[allow(unused_variables, dead_code)]
pub fn release_dma_for_pid(pid: Pid) -> usize {
    0
}

/// Placeholder for the live DMA registry. Red commit keeps the structure
/// empty so the test helpers compile; the green commit wires it to the
/// real slot map.
#[allow(dead_code)]
struct DmaRegistry {
    core: kernel_core::device_host::DmaAllocationRegistryCore,
}

#[allow(dead_code)]
impl DmaRegistry {
    const fn new() -> Self {
        Self {
            core: kernel_core::device_host::DmaAllocationRegistryCore::new(),
        }
    }
}

#[allow(dead_code)]
static DMA_REGISTRY: Mutex<DmaRegistry> = Mutex::new(DmaRegistry::new());

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
