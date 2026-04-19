//! Phase 55b Tracks B.1 + B.2 — device-host syscall wrappers.
//!
//! Bridges the arch-level syscall dispatcher (`arch::x86_64::syscall::mod.rs`)
//! to the pure-logic `DeviceHostRegistryCore` (in `kernel_core`) and the
//! PCI / paging subsystems. The wrapper is deliberately thin: it owns the
//! claim-slot state (PCI handles keyed by `(PID, BDF)`), validates the
//! caller, and hands the resulting `Capability::Device` (B.1) or
//! `Capability::Mmio` (B.2) back through the scheduler's per-task
//! capability table.
//!
//! ## Locking contract
//!
//! Two narrow [`spin::Mutex`] locks are introduced in this module:
//!
//! * `DEVICE_HOST_REGISTRY` — protects:
//!     1. the `DeviceHostRegistryCore` (BDF → owning PID mapping), and
//!     2. the backing store that keeps [`crate::pci::PciDeviceHandle`]
//!        values alive for the life of the claim.
//! * `MMIO_REGISTRY` (B.2) — protects the per-device list of installed
//!   MMIO mappings. Each entry records `(pid, key, bar_index, user_va,
//!   len, cap_handle)` so the cleanup cascade can unmap every derived
//!   `Capability::Mmio` when the owning `Capability::Device` is released.
//!
//! Lock ordering (top → bottom; outer locks acquired before inner):
//!
//! 1. `crate::task::scheduler::SCHEDULER` — per-process capability tables
//! 2. `crate::process::PROCESS_TABLE` — `AddressSpace` snapshots
//! 3. `DEVICE_HOST_REGISTRY` — claim slots (this module)
//! 4. `MMIO_REGISTRY` — derived MMIO capabilities (this module)
//! 5. `crate::pci::PCI_DEVICE_REGISTRY` — PCI claim slots
//! 6. `crate::iommu::registry::*` — IOMMU unit registry
//!
//! `sys_device_claim` and `sys_device_mmio_map` acquire these in order.
//! `release_for_pid` (process teardown) takes the registry locks only: the
//! scheduler lock is not held during teardown because the dying task's
//! capabilities have already been cleared by `cleanup_task_ipc`.
//!
//! No lock is held across IPC or page-table operations — page-table
//! mutation in `sys_device_mmio_map` uses the target `AddressSpace`'s
//! own lock, which sits below the registry locks in the ordering. No
//! lock is held across `log::*!` calls either — every structured event
//! is emitted after the relevant registry guard is dropped.

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;

use kernel_core::device_host::{
    DeviceCapKey, DeviceHostError, DeviceHostRegistryCore, MmioBoundsError, RegistryError,
    build_mmio_window,
};
use kernel_core::ipc::Capability;
use kernel_core::ipc::capability::CapHandle;
use spin::Mutex;

use crate::mm::AddressSpace;
use crate::pci::bar::{UserMapError, map_mmio_region_to_user, unmap_mmio_region_from_user};
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
const NEG_EBADF: isize = -9;
/// Negative errno `-ENOMEM` (12) for capability-table exhaustion.
const NEG_ENOMEM: isize = -12;
/// Negative errno `-ESRCH` (3) when the calling PID cannot be resolved.
const NEG_ESRCH: isize = -3;
/// Negative errno `-EINVAL` (22) — bad argument (B.2 bar_index validation).
const NEG_EINVAL: isize = -22;
/// Negative errno `-EPERM` (1) — capability not owned by the caller.
const NEG_EPERM: isize = -1;
/// Negative errno `-EFAULT` (14) — unexpected internal fault, used as a
/// catch-all when the kernel detects an invariant violation it cannot
/// map onto a more specific errno.
const NEG_EFAULT: isize = -14;

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
/// key without re-walking `DeviceHostRegistryCore`. `handle` is the live
/// `PciDeviceHandle` whose `Drop` tears down the IOMMU domain and returns
/// the PCI registry slot when this entry is removed.
struct ClaimSlot {
    pid: Pid,
    key: DeviceCapKey,
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

    /// Find the `ClaimSlot` owned by `pid` for `key`, if any.
    ///
    /// Used by B.2 to cross-validate a `Capability::Device` against the
    /// registry: a cap whose `(pid, key)` pair is not recorded returns
    /// `None` so the syscall boundary can emit `-EPERM` (rather than the
    /// capability validation's `-EBADF`, which is reserved for a missing
    /// or wrong-type cap). This is the registry-level analogue of the
    /// "cap not owned by caller's PID" acceptance clause.
    fn slot_for(&self, pid: Pid, key: DeviceCapKey) -> Option<&ClaimSlot> {
        self.slots.iter().find(|s| s.pid == pid && s.key == key)
    }
}

/// Global registry. Narrow `spin::Mutex` — no lock is held across IPC or
/// page-table operations; see module docs for the ordering.
static DEVICE_HOST_REGISTRY: Mutex<DeviceHostRegistry> = Mutex::new(DeviceHostRegistry::new());

// ---------------------------------------------------------------------------
// MMIO registry (Phase 55b Track B.2)
// ---------------------------------------------------------------------------

/// Per-device MMIO-capability slot cap — task doc B.2 "Resource bounds".
///
/// 32 is the initial cap named in the task list; raising it requires an
/// audited review of per-driver memory pressure.
pub const MAX_MMIO_PER_DEVICE: usize = 32;

/// One installed MMIO mapping under a `Capability::Device`.
///
/// Recorded by `sys_device_mmio_map` after the page-table install succeeds
/// and cleared by `release_claims_for_pid` as part of the cleanup cascade
/// (dropping a `Capability::Device` implicitly drops every derived
/// `Capability::Mmio`). The `cap_handle` field is kept so a future Track D
/// revoke path can flip the slot to `None` without consulting the
/// scheduler lock.
///
/// `Debug` is deliberately not derived because `AddressSpace` is not
/// `Debug`; callers that need to log an entry should format the fields
/// they care about directly.
struct MmioEntry {
    pid: Pid,
    key: DeviceCapKey,
    bar_index: u8,
    user_va: u64,
    len: usize,
    /// Cap-handle in the owning task's capability table. `None` only in
    /// tests that bypass cap-table insertion; production entries always
    /// carry the handle they installed.
    cap_handle: Option<CapHandle>,
    /// Cached address-space handle so the unmap path can drop page-table
    /// entries even after the owning process has torn down its cap table.
    /// Stored as an `Arc` so the cleanup cascade holds its own reference
    /// to the AS alongside the task's own reference.
    addr_space: Arc<AddressSpace>,
}

/// Kernel-side registry of `Capability::Mmio` mappings.
///
/// See module-level "Locking contract" for the ordering relative to
/// `DEVICE_HOST_REGISTRY` — this lock sits *below* it because the cleanup
/// cascade calls `drain_mmio_for` while it already holds the device-host
/// guard. Within any single syscall the two are acquired in strict order,
/// never interleaved.
struct MmioRegistry {
    entries: Vec<MmioEntry>,
}

impl MmioRegistry {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Insert a new MMIO mapping record.
    ///
    /// Returns `Err(CapacityExceeded)` if the per-device slot count
    /// already reached [`MAX_MMIO_PER_DEVICE`] under this `(pid, key)` pair.
    /// Duplicate `(pid, key, bar_index, user_va)` tuples are rejected with
    /// `Duplicate` — a caller should never hit this in production; it
    /// surfaces as an internal invariant violation in the syscall logs.
    #[allow(clippy::too_many_arguments)]
    fn insert(
        &mut self,
        pid: Pid,
        key: DeviceCapKey,
        bar_index: u8,
        user_va: u64,
        len: usize,
        cap_handle: Option<CapHandle>,
        addr_space: Arc<AddressSpace>,
    ) -> Result<(), MmioRegistryError> {
        let per_dev = self
            .entries
            .iter()
            .filter(|e| e.pid == pid && e.key == key)
            .count();
        if per_dev >= MAX_MMIO_PER_DEVICE {
            return Err(MmioRegistryError::CapacityExceeded);
        }
        if self.entries.iter().any(|e| {
            e.pid == pid && e.key == key && e.bar_index == bar_index && e.user_va == user_va
        }) {
            return Err(MmioRegistryError::Duplicate);
        }
        self.entries.push(MmioEntry {
            pid,
            key,
            bar_index,
            user_va,
            len,
            cap_handle,
            addr_space,
        });
        Ok(())
    }

    /// Remove every entry whose `(pid, key)` pair is in `keys` and return
    /// them so the caller can run the page-table unmap outside the lock.
    ///
    /// Used by the cleanup cascade: when a `Capability::Device` is released,
    /// the caller passes the freed keys in here to pull the matching MMIO
    /// records for the same PID. Keys owned by other PIDs are untouched.
    fn drain_for_keys(&mut self, pid: Pid, keys: &[DeviceCapKey]) -> Vec<MmioEntry> {
        let mut drained = Vec::new();
        self.entries.retain(|e| {
            if e.pid == pid && keys.contains(&e.key) {
                // Can't move out of a `&mut` in retain without using
                // swap-style extraction, so clone the fields and push a
                // new `MmioEntry` with cloned Arc + scalar data. Arc clone
                // is cheap (atomic inc).
                drained.push(MmioEntry {
                    pid: e.pid,
                    key: e.key,
                    bar_index: e.bar_index,
                    user_va: e.user_va,
                    len: e.len,
                    cap_handle: e.cap_handle,
                    addr_space: Arc::clone(&e.addr_space),
                });
                false
            } else {
                true
            }
        });
        drained
    }

    /// Remove every entry owned by `pid` regardless of device key. Used by
    /// the final sweep in `release_claims_for_pid` to catch any MMIO record
    /// whose matching claim was already drained.
    fn drain_for_pid(&mut self, pid: Pid) -> Vec<MmioEntry> {
        let mut drained = Vec::new();
        self.entries.retain(|e| {
            if e.pid == pid {
                drained.push(MmioEntry {
                    pid: e.pid,
                    key: e.key,
                    bar_index: e.bar_index,
                    user_va: e.user_va,
                    len: e.len,
                    cap_handle: e.cap_handle,
                    addr_space: Arc::clone(&e.addr_space),
                });
                false
            } else {
                true
            }
        });
        drained
    }

    /// Count entries for a PID — used by the test harness. Not wired
    /// into production paths; marked `#[allow(dead_code)]` so non-test
    /// builds do not lint it.
    #[allow(dead_code)]
    fn count_for_pid(&self, pid: Pid) -> usize {
        self.entries.iter().filter(|e| e.pid == pid).count()
    }
}

/// Errors surfaced by [`MmioRegistry::insert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MmioRegistryError {
    CapacityExceeded,
    Duplicate,
}

/// Global MMIO registry. Declared with the same narrow-mutex convention as
/// [`DEVICE_HOST_REGISTRY`]; see the module-level locking contract.
static MMIO_REGISTRY: Mutex<MmioRegistry> = Mutex::new(MmioRegistry::new());

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

// ---------------------------------------------------------------------------
// sys_device_mmio_map (Phase 55b Track B.2)
// ---------------------------------------------------------------------------

/// Syscall entry: `sys_device_mmio_map(dev_cap, bar_index) -> isize`.
///
/// Returns the user VA of the installed mapping (non-negative) on success,
/// or a negative errno on failure. See the task-doc B.2 acceptance surface
/// for the exact error mapping.
///
/// The syscall runs in the caller's address space — `pid` is resolved via
/// `current_pid()` and the capability is looked up in the current task's
/// per-task table. The page-table mutation targets the same address space
/// (held via an `Arc<AddressSpace>` captured under `PROCESS_TABLE`) so
/// concurrent claims on different BDFs do not contend.
pub fn sys_device_mmio_map(dev_cap: u32, bar_index: u8) -> isize {
    // 1) Resolve caller identity.
    let pid = crate::process::current_pid();
    if pid == 0 {
        return NEG_ESRCH;
    }
    let task_id = match scheduler::current_task_id() {
        Some(id) => id,
        None => return NEG_ESRCH,
    };

    // 2) Resolve the capability. A non-`Device` cap or a missing handle
    //    returns `-EBADF` per the B.2 acceptance.
    let key = match scheduler::task_cap(task_id, dev_cap as CapHandle) {
        Ok(Capability::Device { key }) => key,
        Ok(_) => return NEG_EBADF,
        Err(_) => return NEG_EBADF,
    };

    // 3) Reject BAR indices outside the 0..6 range up front. This mirrors
    //    the kernel-core `validate_mmio_bar_size` check but uses the raw
    //    index before the destructive PCI sizing dance runs.
    if bar_index >= 6 {
        return NEG_EINVAL;
    }

    // 4) Resolve phys_base + size via the PCI BAR-sizing dance. `map_bar`
    //    takes the live `PciDeviceHandle` and is the only caller that
    //    touches config space for MMIO-type BARs. We hold the registry
    //    lock long enough to own the handle reference safely while the
    //    dance runs — the dance writes 0xFFFFFFFF then restores the
    //    original, so no persistent side-effect. A cap resolved to a
    //    device the caller does not own (e.g. forged) returns `-EPERM`.
    let mapping_info = {
        let reg = DEVICE_HOST_REGISTRY.lock();
        let slot = match reg.slot_for(pid, key) {
            Some(slot) => slot,
            None => return NEG_EPERM,
        };
        resolve_mmio_bar_info(&slot.handle, bar_index)
    };
    let (phys_base, bar_size, prefetchable) = match mapping_info {
        Ok(tuple) => tuple,
        Err(e) => return mmio_bounds_error_to_errno(e),
    };

    // 6) Build the descriptor (pure logic — bounds + cache-mode).
    let descriptor = match build_mmio_window(bar_index, phys_base, bar_size, prefetchable) {
        Ok(d) => d,
        Err(e) => return mmio_bounds_error_to_errno(e),
    };

    // 7) Capture the caller's AddressSpace Arc under the process-table
    //    lock so the page-table mutation below can proceed without
    //    serialising against unrelated processes.
    let addr_space = match snapshot_address_space(pid) {
        Some(a) => a,
        None => return NEG_ESRCH,
    };

    // 8) Pre-check the MMIO slot cap so a caller that is already at the
    //    limit does not pay for a wasted page-table install.
    {
        let mmio = MMIO_REGISTRY.lock();
        let per_dev = mmio
            .entries
            .iter()
            .filter(|e| e.pid == pid && e.key == key)
            .count();
        if per_dev >= MAX_MMIO_PER_DEVICE {
            return capacity_exceeded_errno();
        }
    }

    // 9) Install the mapping. No registry lock is held across this call —
    //    page-table work happens under the `AddressSpace`'s own lock.
    let user_va = match map_mmio_region_to_user(
        pid,
        &addr_space,
        descriptor.phys_base,
        descriptor.len as u64,
        descriptor.prefetchable,
    ) {
        Ok(va) => va,
        Err(e) => return user_map_error_to_errno(e),
    };

    // 10) Install the Mmio capability in the caller's cap table. If that
    //     fails, unwind the mapping so the AS is left unchanged.
    let cap = Capability::Mmio {
        device: key,
        bar_index,
        len: descriptor.len,
    };
    let mmio_handle = match scheduler::insert_cap(task_id, cap) {
        Ok(h) => h,
        Err(_) => {
            unmap_mmio_region_from_user(&addr_space, user_va, descriptor.len);
            return NEG_ENOMEM;
        }
    };

    // 11) Record the mapping. Between step (8) and step (11) a concurrent
    //     claim on this cap could have filled the slot — recheck under
    //     the MMIO lock. If the insert now fails, unwind both the cap
    //     and the mapping so the driver sees a clean failure.
    let insert_result = {
        let mut mmio = MMIO_REGISTRY.lock();
        mmio.insert(
            pid,
            key,
            bar_index,
            user_va,
            descriptor.len,
            Some(mmio_handle),
            Arc::clone(&addr_space),
        )
    };
    if let Err(e) = insert_result {
        // Rollback cap table + mapping.
        let _ = scheduler::remove_task_cap(task_id, mmio_handle);
        unmap_mmio_region_from_user(&addr_space, user_va, descriptor.len);
        return match e {
            MmioRegistryError::CapacityExceeded => capacity_exceeded_errno(),
            MmioRegistryError::Duplicate => NEG_EFAULT,
        };
    }

    // 12) Log the structured event outside the registry locks.
    log::info!(
        "device_host.mmio_map pid={} bdf={:04x}:{:02x}:{:02x}.{} bar={} user_va={:#x} len={:#x}",
        pid,
        key.segment,
        key.bus,
        key.dev,
        key.func,
        bar_index,
        user_va,
        descriptor.len,
    );

    // The user VA is guaranteed to fit in `isize` because the user-VA
    // allocator caps it below `0x0000_8000_0000_0000`.
    user_va as isize
}

/// Convert an [`MmioBoundsError`] to a negative errno value.
fn mmio_bounds_error_to_errno(e: MmioBoundsError) -> isize {
    match e {
        MmioBoundsError::BarIndexOutOfRange => NEG_EINVAL,
        MmioBoundsError::BarTooLarge => NEG_EINVAL,
        MmioBoundsError::UnalignedPhysBase => NEG_EINVAL,
        MmioBoundsError::ZeroSizedBar => NEG_ENODEV,
        MmioBoundsError::ZeroPhysBase => NEG_ENODEV,
    }
}

/// Convert a [`UserMapError`] to a negative errno value.
fn user_map_error_to_errno(e: UserMapError) -> isize {
    match e {
        UserMapError::NotMmio => NEG_EINVAL,
        UserMapError::NoFreeUserVa => NEG_ENOMEM,
        UserMapError::PageTableInsertFailed => NEG_ENOMEM,
        UserMapError::InvalidBarGeometry => NEG_EINVAL,
        UserMapError::NoProcess => NEG_ESRCH,
    }
}

/// The capacity-exceeded errno — `-ENOMEM` is the closest match on Linux's
/// surface; a future phase may introduce a dedicated `-EMFILE`-style code.
fn capacity_exceeded_errno() -> isize {
    NEG_ENOMEM
}

/// Read a claimed device's BAR metadata through the PCI sizing-dance
/// (destructive write-0xFFFFFFFF / restore) and return
/// `(phys_base, size, prefetchable)`.
///
/// The caller has already confirmed `handle` belongs to the requested
/// `(pid, key)` pair. Holds `handle` by reference rather than by value —
/// the `DEVICE_HOST_REGISTRY` lock must remain held across this call so
/// the handle is not freed mid-sizing-dance.
fn resolve_mmio_bar_info(
    handle: &PciDeviceHandle,
    bar_index: u8,
) -> Result<(u64, u64, bool), MmioBoundsError> {
    use crate::pci::bar::{BarError, BarMapping, map_bar};

    match map_bar(handle, bar_index) {
        Ok(BarMapping::Mmio { region, bar_type }) => {
            let prefetchable = bar_type.is_prefetchable();
            Ok((region.phys_base(), region.size(), prefetchable))
        }
        Ok(BarMapping::Pio { .. }) => {
            // I/O port BAR — cannot be mapped into user AS.
            Err(MmioBoundsError::UnalignedPhysBase)
        }
        Err(BarError::IndexOutOfRange) => Err(MmioBoundsError::BarIndexOutOfRange),
        Err(BarError::Unimplemented) => Err(MmioBoundsError::ZeroSizedBar),
        Err(BarError::Reserved) => Err(MmioBoundsError::ZeroSizedBar),
        Err(BarError::InvalidPair) => Err(MmioBoundsError::BarIndexOutOfRange),
        Err(BarError::InvalidSize) => Err(MmioBoundsError::ZeroSizedBar),
    }
}

/// Snapshot the `Arc<AddressSpace>` for `pid` by cloning it out from under
/// `PROCESS_TABLE`. Returns `None` if the PID has no process entry or no
/// dedicated address space (e.g. kernel tasks).
fn snapshot_address_space(pid: Pid) -> Option<Arc<AddressSpace>> {
    let table = crate::process::PROCESS_TABLE.lock();
    table.find(pid).and_then(|p| p.addr_space.as_ref().cloned())
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
// Process-exit hook
// ---------------------------------------------------------------------------

/// Release every claim held by `pid` and cascade the teardown to every
/// derived `Capability::Mmio`.
///
/// Called from `do_full_process_exit` (arch/x86_64/syscall) so a driver
/// crash or kill automatically frees the devices it owned for the
/// supervisor restart to re-claim. Safe to call for a PID that holds no
/// claims — returns zero and performs no I/O.
///
/// Cascade order (task doc acceptance: "dropping the device cap implicitly
/// drops the Mmio cap"):
///   1. Drain every `MmioEntry` owned by `pid` under the `MMIO_REGISTRY`
///      lock (keyed by PID, not by device — a process can only hold caps
///      for the devices it claimed, so this sweep is sufficient).
///   2. Release the claim slots in `DEVICE_HOST_REGISTRY` (which also
///      tears down the IOMMU domain via `PciDeviceHandle::drop`).
///   3. Outside both registry locks, walk the drained MMIO entries and
///      unmap their pages from the captured address spaces.
pub fn release_claims_for_pid(pid: Pid) {
    // Step 1: drain MMIO entries owned by pid.
    let drained_mmio = {
        let mut mmio = MMIO_REGISTRY.lock();
        mmio.drain_for_pid(pid)
    };
    // Step 2: release device-host claim slots. PciDeviceHandle Drop runs
    // here and tears down the IOMMU domain + PCI registry slot for each
    // released device.
    let freed = {
        let mut reg = DEVICE_HOST_REGISTRY.lock();
        reg.release_for_pid(pid)
    };
    // Step 3: teardown mmio mappings. Done outside the registry locks so
    // the page-table work (TLB shootdown, mapper->unmap) can acquire the
    // AS's own lock without risk of deadlock against a concurrent claim.
    let mmio_count = drained_mmio.len();
    for entry in drained_mmio {
        unmap_mmio_region_from_user(&entry.addr_space, entry.user_va, entry.len);
    }
    if freed > 0 || mmio_count > 0 {
        log::info!(
            "device_host.release pid={} freed_claims={} freed_mmio={}",
            pid,
            freed,
            mmio_count,
        );
    }
}

/// Release derived MMIO mappings for a specific set of `(pid, key)` pairs.
///
/// Exposed for future use when a driver explicitly drops a
/// `Capability::Device` via a cap-table revoke without exiting. B.2 itself
/// does not surface such a syscall (the only current path is process exit,
/// handled by [`release_claims_for_pid`]) — this helper is provided so the
/// cleanup cascade primitive exists in one place.
#[allow(dead_code)]
pub(crate) fn release_mmio_for_keys(pid: Pid, keys: &[DeviceCapKey]) -> usize {
    let drained = {
        let mut mmio = MMIO_REGISTRY.lock();
        mmio.drain_for_keys(pid, keys)
    };
    let count = drained.len();
    for entry in drained {
        unmap_mmio_region_from_user(&entry.addr_space, entry.user_va, entry.len);
    }
    count
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
/// process-exit path. Mirrors the production cascade order but skips the
/// `unmap_mmio_region_from_user` call because test entries carry a
/// sentinel `AddressSpace` with no real page table.
#[cfg(test)]
pub(crate) fn test_release_for_pid(pid: Pid) -> usize {
    // Step 1: drain MMIO entries (cascade).
    {
        let mut mmio = MMIO_REGISTRY.lock();
        let _ = mmio.drain_for_pid(pid);
    }
    // Step 2: release device-host claim slots.
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
// Track B.2 test-only helpers (GREEN — backed by MMIO_REGISTRY)
// ---------------------------------------------------------------------------
//
// These drive the same `MmioRegistry` the syscall path uses, but without
// requiring a real task / cap-table insertion. They keep the production
// state-machine under test while avoiding the dependency on a running
// driver process (that integration test lands with D.1 once the stub NVMe
// driver exists).

/// Test-only error surface mirroring [`MmioRegistryError`], plus the
/// `NotClaimed` variant that the syscall path checks via `slot_for` before
/// ever touching the MMIO registry.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestMmioError {
    /// No `Capability::Device` entry recorded for this `(pid, key)` pair.
    NotClaimed,
    /// Adding this entry would exceed [`MAX_MMIO_PER_DEVICE`].
    CapacityExceeded,
    /// A duplicate entry already exists.
    #[allow(dead_code)]
    Duplicate,
}

/// Record an MMIO entry under `(pid, key)`. Mirrors the production path
/// minus the page-table install and cap-table insert. Returns `NotClaimed`
/// if no claim slot exists for the pair; otherwise consults the MMIO
/// registry's own `insert` path.
///
/// Uses a sentinel `Arc<AddressSpace>` so the `drain_for_pid` / `drain_for_keys`
/// paths can be exercised without touching paging — the test explicitly
/// skips the `unmap_mmio_region_from_user` call via `test_release_for_pid`
/// (which only drains the registry state; the production release path
/// runs unmap).
#[cfg(test)]
pub(crate) fn test_record_mmio(
    pid: Pid,
    key: DeviceCapKey,
    bar_index: u8,
    len: usize,
    user_va: u64,
) -> Result<(), TestMmioError> {
    // Cross-check that the caller has a matching claim — the syscall path
    // enforces this via `slot_for` before ever reaching the MMIO registry.
    {
        let reg = DEVICE_HOST_REGISTRY.lock();
        if reg.slot_for(pid, key).is_none() {
            return Err(TestMmioError::NotClaimed);
        }
    }
    // Fabricate a sentinel AddressSpace — the test path never walks the
    // page table, so a fresh-zero PML4 is sufficient. `PhysAddr::new(0)`
    // is acceptable here because the only consumer of `addr_space` on the
    // release path is the production `unmap_mmio_region_from_user`, which
    // test code does not call; `test_release_for_pid` drains the registry
    // without running the unmap.
    let phantom_addr_space = Arc::new(AddressSpace::new(x86_64::PhysAddr::new(0)));
    let mut mmio = MMIO_REGISTRY.lock();
    match mmio.insert(pid, key, bar_index, user_va, len, None, phantom_addr_space) {
        Ok(()) => Ok(()),
        Err(MmioRegistryError::CapacityExceeded) => Err(TestMmioError::CapacityExceeded),
        Err(MmioRegistryError::Duplicate) => Err(TestMmioError::Duplicate),
    }
}

/// Return the number of MMIO entries recorded under `pid`.
#[cfg(test)]
pub(crate) fn test_mmio_count_for_pid(pid: Pid) -> usize {
    let mmio = MMIO_REGISTRY.lock();
    mmio.count_for_pid(pid)
}
