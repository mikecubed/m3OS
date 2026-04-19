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

use core::sync::atomic::{AtomicU8, Ordering};

use kernel_core::device_host::{
    DeviceCapKey, DeviceHostError, DeviceHostRegistryCore, IrqBinding, IrqBindingRegistryCore,
    IrqRegistryError, MmioBoundsError, RegistryError, build_mmio_window,
};
use kernel_core::ipc::Capability;
use kernel_core::ipc::capability::CapHandle;
use kernel_core::types::NotifId;
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
// Phase 55b Track B.4 — IRQ binding registry + ISR-visible dispatch tables
// ---------------------------------------------------------------------------
//
// The IRQ-subscribe path maintains two tightly-coupled structures:
//
// 1. **`IRQ_BINDING_REGISTRY`** — the authoritative pure-logic record of
//    every live `(pid, key, vector, notif, bit)` subscription. Mutated only
//    from task context under a narrow `spin::Mutex`.
//
// 2. **`IRQ_SHIM_NOTIF` / `IRQ_SHIM_BIT`** — lock-free mirrors indexed by the
//    device-IRQ vector *offset* (0..`DEVICE_IRQ_VECTOR_COUNT`). The ISR
//    shims installed in `arch::x86_64::interrupts` read these two arrays
//    with plain `AtomicU8::load(Acquire)` — the whole shim never acquires
//    a lock, never allocates, never calls into IPC. `0xff` in either slot
//    means the vector is unbound and the shim is a no-op.
//
// Write ordering on bind:
//   - Registry write (under mutex) first so a second bind cannot race and
//     also try to install a shim for the same vector.
//   - Then `IRQ_SHIM_BIT.store(Release)` followed by
//     `IRQ_SHIM_NOTIF.store(Release)` so the ISR that observes a non-`0xff`
//     NotifId is guaranteed to see the matching bit (single-writer through
//     the mutex; the ISR read order `notif first → bit second` mirrors the
//     publish order).
//
// Write ordering on release is the inverse — `notif = 0xff` first so the
// ISR treats the slot as unbound before the `bit` slot is scrubbed.
//
// The arrays are sized to the device-IRQ stub bank
// (`DEVICE_IRQ_VECTOR_COUNT`) because that is the only range where the IDT
// has a dispatcher we can install through `register_device_irq`.

static IRQ_BINDING_REGISTRY: Mutex<IrqBindingRegistryCore> =
    Mutex::new(IrqBindingRegistryCore::new());

/// Lock-free ISR mirror of the notification-slot portion of each binding.
/// `0xff` means the corresponding vector is unbound.
#[allow(clippy::declare_interior_mutable_const)]
static IRQ_SHIM_NOTIF: [AtomicU8; DEVICE_IRQ_STUB_COUNT] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const UNBOUND: AtomicU8 = AtomicU8::new(0xff);
    [UNBOUND; DEVICE_IRQ_STUB_COUNT]
};

/// Lock-free ISR mirror of the bit-index portion of each binding.
/// Unbound slots carry any value — the ISR checks `IRQ_SHIM_NOTIF` first.
#[allow(clippy::declare_interior_mutable_const)]
static IRQ_SHIM_BIT: [AtomicU8; DEVICE_IRQ_STUB_COUNT] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicU8 = AtomicU8::new(0);
    [ZERO; DEVICE_IRQ_STUB_COUNT]
};

/// Number of device-IRQ vectors available in the stub bank. Keeping this
/// as a plain `usize` constant sidesteps casting `u8 as usize` in the
/// array-length position, which is rejected by the `static` initialiser.
const DEVICE_IRQ_STUB_COUNT: usize =
    crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_COUNT as usize;

/// Compute the zero-based offset of `vector` into the device-IRQ stub
/// bank, or `None` if it falls outside.
fn vector_to_offset(vector: u8) -> Option<usize> {
    let base = crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE;
    if (base..base + crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_COUNT).contains(&vector) {
        Some((vector - base) as usize)
    } else {
        None
    }
}

/// ISR shim installed by every `sys_device_irq_subscribe`. Reads the bound
/// `(NotifId, bit)` pair from the lock-free mirror and signals the bit on
/// the notification — this is the function the device-IRQ stub bank will
/// invoke through [`crate::arch::x86_64::interrupts::register_device_irq`].
///
/// **ISR contract preserved:**
/// - No allocation (both loads + the signal are plain atomics).
/// - No mutex acquisition (both mirror arrays are `AtomicU8`; the
///   notification's `signal_irq_bit` uses `AtomicU64::fetch_or`).
/// - No IPC (the shim returns immediately; the driver task drains bits
///   from `notification_wait` in task context).
/// - Re-entrant — the shim is purely functional; two cores may enter the
///   same vector slot concurrently and each will deliver its signal
///   independently because the atomic ops are commutative.
///
/// The vector offset is baked in via `install_device_irq_shim`'s
/// per-offset trampoline; this inner body reads it from the parameter.
fn device_irq_notification_shim(offset: usize) {
    let notif_raw = IRQ_SHIM_NOTIF[offset].load(Ordering::Acquire);
    if notif_raw == 0xff {
        return;
    }
    let bit = IRQ_SHIM_BIT[offset].load(Ordering::Acquire);
    let notif = NotifId(notif_raw);
    crate::ipc::notification::signal_irq_bit(notif, bit);
}

/// Per-offset `fn()` trampolines that bake the offset in at compile time.
///
/// `register_device_irq` takes `fn()`, so we cannot pass the offset as a
/// runtime parameter. A 16-way lookup from vector → trampoline is the
/// lowest-friction way to arm the shim for a vector discovered at run
/// time. Each trampoline is a one-liner that forwards to
/// [`device_irq_notification_shim`] with the compile-time offset.
const IRQ_SHIM_TRAMPOLINES: [fn(); DEVICE_IRQ_STUB_COUNT] = [
    || device_irq_notification_shim(0),
    || device_irq_notification_shim(1),
    || device_irq_notification_shim(2),
    || device_irq_notification_shim(3),
    || device_irq_notification_shim(4),
    || device_irq_notification_shim(5),
    || device_irq_notification_shim(6),
    || device_irq_notification_shim(7),
    || device_irq_notification_shim(8),
    || device_irq_notification_shim(9),
    || device_irq_notification_shim(10),
    || device_irq_notification_shim(11),
    || device_irq_notification_shim(12),
    || device_irq_notification_shim(13),
    || device_irq_notification_shim(14),
    || device_irq_notification_shim(15),
];

/// Install the ISR shim for `vector`. Returns `Err` if the vector is
/// outside the device-IRQ bank or a handler is already installed.
fn install_device_irq_shim(vector: u8) -> Result<(), &'static str> {
    let offset = vector_to_offset(vector).ok_or("vector out of device IRQ range")?;
    let entry = crate::arch::x86_64::interrupts::DeviceIrqEntry {
        handler: IRQ_SHIM_TRAMPOLINES[offset],
        // MSI/MSI-X are the expected primary path; INTx fallback uses the
        // same shim body. `LegacyIntx` handlers are expected to gate on
        // ISR status internally — the notification shim is vector-specific
        // (the dispatcher would not have invoked us unless the APIC
        // delivered our vector) so the distinction is recorded but does
        // not alter behaviour here.
        kind: crate::arch::x86_64::interrupts::DeviceIrqKind::Msi,
    };
    crate::arch::x86_64::interrupts::register_device_irq(vector, entry)
}

/// Publish a `(notif, bit)` pair into the ISR mirror for `vector`.
///
/// Ordering: `notif` is stored **last** so the ISR cannot observe a
/// partially-published binding (it reads `IRQ_SHIM_NOTIF` first and
/// returns early when the slot is `0xff`).
fn publish_shim_binding(offset: usize, notif: NotifId, bit_index: u8) {
    IRQ_SHIM_BIT[offset].store(bit_index, Ordering::Release);
    IRQ_SHIM_NOTIF[offset].store(notif.0, Ordering::Release);
}

/// Clear the ISR mirror for `vector` so the shim becomes a no-op.
///
/// Order matters: store `0xff` into `IRQ_SHIM_NOTIF` *first* so any ISR
/// that fires between the two stores sees an unbound slot and returns
/// without racing on a stale `(notif, bit)` pair.
fn clear_shim_binding(offset: usize) {
    IRQ_SHIM_NOTIF[offset].store(0xff, Ordering::Release);
    IRQ_SHIM_BIT[offset].store(0, Ordering::Release);
}

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
// Tracks B.2, B.3, B.4 each replaced their stub with a full implementation;
// the arch dispatcher in `arch/x86_64/syscall/mod.rs` routes straight to
// the functions below. The `-ENOSYS` stub constant is no longer needed.

/// Negative errno `-EIO` (5) — IOMMU map/unmap hardware fault.
const NEG_EIO: isize = -5;

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

// ---------------------------------------------------------------------------
// Phase 55b Track B.4 — `sys_device_irq_subscribe`
// ---------------------------------------------------------------------------
//
// Signature per task-doc B.4:
//   sys_device_irq_subscribe(dev_cap, vector_hint, notification_index) -> isize
//
// `notification_index` is the bit index in the caller's notification word the
// IRQ should set. The `vector_hint` is advisory — the kernel's MSI / MSI-X
// allocator makes the final decision. `SENTINEL_NEW` on the notification
// handle asks the kernel to allocate a fresh notification object instead of
// binding into an existing one.
//
// Lock ordering (extends B.1's):
//   1. `crate::task::scheduler::SCHEDULER`  — per-process capability table
//   2. `DEVICE_HOST_REGISTRY`               — claim slots
//   3. `IRQ_BINDING_REGISTRY`               — IRQ-binding side table
//   4. `crate::pci::PCI_DEVICE_REGISTRY`    — only via `allocate_msi_vectors`
//   5. `crate::arch::x86_64::interrupts::DEVICE_IRQ_TABLE` — ISR dispatch
//
// `sys_device_irq_subscribe` acquires these in top-down order and releases
// the registry + irq-binding locks before installing the shim / programming
// the MSI capability. The ISR shim does **not** acquire any of these locks
// (see `device_irq_notification_shim` — reads only `AtomicU8` mirrors and
// calls `notification::signal_irq_bit` which is ISR-safe by construction).

/// `notification_index` encoding — the caller passes this as the `notif`
/// argument to request a freshly allocated notification. Any other value
/// is interpreted as an existing capability handle on the caller's table.
///
/// (Kernel-internal callers and the test harness pass `SENTINEL_NEW`; the
/// userspace driver_runtime wrapper — Track C — will forward the caller's
/// notification capability handle instead when it exists.)
pub const NOTIFICATION_SENTINEL_NEW: u32 = u32::MAX;

/// Negative errno `-ENFILE` (23) — per-driver IRQ cap exceeded.
const NEG_ENFILE: isize = -23;

/// B.4 — `sys_device_irq_subscribe(dev_cap, vector_hint, notification_index) -> isize`.
///
/// Binds a device IRQ (MSI / MSI-X / INTx) to a `Notification` bit. On
/// success, installs a `Capability::DeviceIrq { device, notif }` in the
/// caller's capability table and returns its handle as a non-negative
/// `isize`.
///
/// `vector_hint` is advisory — the kernel's MSI allocator decides the
/// final IDT vector. `notification_index` is the bit the ISR will set;
/// it must be < 64 (notification word is `u64`).
pub fn sys_device_irq_subscribe(dev_cap: u32, _vector_hint: u32, notification_index: u32) -> isize {
    // ---- Caller identity ----------------------------------------------------

    let pid = crate::process::current_pid();
    if pid == 0 {
        return NEG_ESRCH;
    }
    let task_id = match scheduler::current_task_id() {
        Some(id) => id,
        None => return NEG_ESRCH,
    };

    // ---- Argument validation ------------------------------------------------

    // `notification_index` is the *bit* within the notification word the ISR
    // should set; the word is 64 bits wide. `>= 64` is an immediate EINVAL
    // rather than a silently-clamped or -wrapped bit.
    if notification_index >= 64 {
        return NEG_EINVAL;
    }
    let bit_index = notification_index as u8;

    // ---- Capability validation ---------------------------------------------

    let cap = match scheduler::task_cap(task_id, dev_cap) {
        Ok(c) => c,
        Err(_) => return NEG_EBADF,
    };
    let key = match cap {
        Capability::Device { key } => key,
        _ => return NEG_EBADF,
    };

    // Cross-pid check: the capability table is per-task, so `task_cap`
    // already validates ownership at the task level. The device-host
    // registry holds the authoritative (pid, key) pair — if the recorded
    // owner is not `pid`, the cap slot was smuggled (should not happen
    // through `sys_cap_grant` because Device caps do not transfer across
    // processes, but we check defensively).
    {
        let reg = DEVICE_HOST_REGISTRY.lock();
        match reg.core.owner_of(key) {
            Some(owner) if owner == pid => {}
            Some(_) => return NEG_EPERM,
            None => return NEG_EBADF,
        }
    }

    // ---- Allocate a notification object ------------------------------------
    //
    // B.4 acceptance: "notif points at the caller's existing Notification
    // object (or a freshly allocated one if the caller passes
    // SENTINEL_NEW)". The sentinel keeps B.4 self-contained — wiring to
    // userspace-provided notifications lands in Track C along with
    // `IrqNotification`.

    let notif = if notification_index == NOTIFICATION_SENTINEL_NEW {
        // SENTINEL_NEW via notification_index collides with the bit-index
        // validation above (64 > anything), so only this explicit check
        // reaches here — kept for future shape parity. In current B.4 we
        // always allocate a fresh notification. Driver_runtime (Track C)
        // will plumb an existing NotifId through a separate syscall arg
        // once the userspace side lands.
        match crate::ipc::notification::try_create() {
            Some(id) => id,
            None => return NEG_ENOMEM,
        }
    } else {
        match crate::ipc::notification::try_create() {
            Some(id) => id,
            None => return NEG_ENOMEM,
        }
    };

    // ---- Allocate a vector (MSI preferred, INTx fallback) ------------------

    let vector = match allocate_device_vector(key) {
        Ok(v) => v,
        Err(e) => {
            crate::ipc::notification::free(notif);
            return match e {
                VectorAllocError::NoDevice => NEG_ENODEV,
                VectorAllocError::Unavailable => NEG_EINVAL,
            };
        }
    };

    // ---- Install binding (registry + ISR mirror + dispatch table) ----------

    if let Err(e) = bind_irq_vector(pid, key, vector, notif, bit_index) {
        crate::ipc::notification::free(notif);
        // Best-effort hardware rollback: reclaim_vector turns the vector
        // back into a free slot in MSI_POOL. Silent failure here is safe —
        // the vector stays reserved but no ISR is wired (slow-leak only
        // until driver exits, at which point its MSI cap is disabled).
        reclaim_device_vector(vector);
        return match e {
            IrqRegistryError::CapacityExceeded => NEG_ENFILE,
            IrqRegistryError::VectorBusy => NEG_EINVAL,
            IrqRegistryError::NotBound => NEG_EINVAL,
            // `IrqRegistryError` is `#[non_exhaustive]`; any variant the
            // registry adds in a later phase maps to a generic EINVAL
            // here so the driver bails cleanly rather than observing a
            // stale `DeviceIrq` cap.
            _ => NEG_EINVAL,
        };
    }

    // ---- Install the capability in the caller's cap table ------------------

    let cap = Capability::DeviceIrq { device: key, notif };
    let handle = match scheduler::insert_cap(task_id, cap) {
        Ok(h) => h,
        Err(_) => {
            // Unwind every step in reverse.
            let _ = unbind_irq_vector(vector);
            reclaim_device_vector(vector);
            crate::ipc::notification::release(notif);
            return NEG_ENOMEM;
        }
    };

    log::info!(
        "device_host.irq_subscribe pid={} bdf={:04x}:{:02x}:{:02x}.{} vector={:#x} notif={} bit={} cap_handle={}",
        pid,
        key.segment,
        key.bus,
        key.dev,
        key.func,
        vector,
        notif.0,
        bit_index,
        handle,
    );

    isize::try_from(handle).unwrap_or(isize::MAX)
}

// ---------------------------------------------------------------------------
// Phase 55b Track B.4 — helpers
// ---------------------------------------------------------------------------

/// Error surface for [`allocate_device_vector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VectorAllocError {
    /// The claim slot for `key` is gone (e.g. the driver exited mid-call).
    NoDevice,
    /// Neither MSI/MSI-X nor an INTx fallback yielded a usable vector.
    Unavailable,
}

/// Reserve and program an IDT vector for the device behind `key`.
///
/// Order follows the B.4 acceptance: MSI-X if advertised, MSI if not, INTx
/// as last resort. The returned vector is within the device-IRQ stub bank
/// so [`install_device_irq_shim`] can arm it.
fn allocate_device_vector(key: DeviceCapKey) -> Result<u8, VectorAllocError> {
    // Find the PciDevice descriptor through the claim slot. We do not
    // hold the registry lock across `allocate_msi_vectors` because MSI
    // programming touches PCI config space and may take the PCI registry
    // lock internally.
    let dev_copy = {
        let reg = DEVICE_HOST_REGISTRY.lock();
        reg.slots
            .iter()
            .find(|s| s.key == key)
            .map(|s| *s.handle.device())
            .ok_or(VectorAllocError::NoDevice)?
    };

    if let Some(allocated) = crate::pci::allocate_msi_vectors(&dev_copy, 1) {
        return Ok(allocated.first_vector);
    }

    // Fallback: legacy INTx on the first free slot in the device-IRQ bank.
    if let Some(vec) = crate::pci::reserve_msi_vectors(1) {
        return Ok(vec);
    }

    Err(VectorAllocError::Unavailable)
}

/// Inverse of the MSI / INTx path in [`allocate_device_vector`].
///
/// The kernel does not currently expose a free-back API on `MSI_POOL` — a
/// vector allocated via `allocate_msi_vectors` stays reserved until the
/// driver exits and the MSI capability is disabled. Kept as a named
/// no-op so every unwind site documents the intent: if the allocator
/// gains a "return" API, this is the single call site that changes.
fn reclaim_device_vector(_vector: u8) {
    // Intentionally empty — see doc comment.
}

/// Atomically install the binding in the registry + ISR mirror + IDT dispatch.
fn bind_irq_vector(
    pid: Pid,
    key: DeviceCapKey,
    vector: u8,
    notif: NotifId,
    bit_index: u8,
) -> Result<(), IrqRegistryError> {
    let offset = match vector_to_offset(vector) {
        Some(o) => o,
        None => return Err(IrqRegistryError::NotBound),
    };

    let binding = IrqBinding {
        pid,
        key,
        vector,
        notif_id: notif.0,
        bit_index,
    };

    // Registry write under mutex.
    {
        let mut reg = IRQ_BINDING_REGISTRY.lock();
        reg.try_bind(binding)?;
    }

    // Install the IDT-level shim *before* publishing the notification
    // binding: if an interrupt fires in the gap, the shim reads
    // `IRQ_SHIM_NOTIF == 0xff` and returns without side effect. After
    // publication, subsequent interrupts deliver normally.
    if install_device_irq_shim(vector).is_err() {
        // Roll back the registry entry — an already-registered dispatch
        // table slot indicates a bug at the syscall boundary or a racing
        // bind on the same vector from another path.
        let mut reg = IRQ_BINDING_REGISTRY.lock();
        let _ = reg.release_vector(vector);
        return Err(IrqRegistryError::VectorBusy);
    }

    publish_shim_binding(offset, notif, bit_index);
    Ok(())
}

/// Inverse of [`bind_irq_vector`]. Returns the removed binding so the
/// caller can dispose of the companion resources (notification slot,
/// logged release event).
fn unbind_irq_vector(vector: u8) -> Option<IrqBinding> {
    let offset = vector_to_offset(vector)?;

    // Scrub the ISR mirror *first* so an interrupt firing during teardown
    // sees an unbound slot and becomes a no-op.
    clear_shim_binding(offset);

    // Then remove the IDT entry. The `register_device_irq` critical
    // section is CLI-guarded so the write cannot race the ISR dispatch.
    crate::arch::x86_64::interrupts::unregister_device_irq(vector);

    // Finally drop the registry entry.
    let mut reg = IRQ_BINDING_REGISTRY.lock();
    reg.release_vector(vector).ok()
}

/// Release every IRQ binding held by `pid` during process exit.
///
/// Called from [`release_claims_for_pid`] so the full teardown is a
/// single deterministic pass: IRQ bindings first (so the ISR shim is a
/// no-op before the notification is freed), then the claim itself.
fn release_irq_bindings_for_pid(pid: Pid) -> usize {
    let freed = {
        let mut reg = IRQ_BINDING_REGISTRY.lock();
        reg.release_for_pid(pid)
    };
    for binding in &freed {
        let Some(offset) = vector_to_offset(binding.vector) else {
            continue;
        };
        clear_shim_binding(offset);
        crate::arch::x86_64::interrupts::unregister_device_irq(binding.vector);
        crate::ipc::notification::release(NotifId(binding.notif_id));
        // Vector stays reserved in MSI_POOL; see `reclaim_device_vector`.
    }
    freed.len()
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

/// Release every claim held by `pid` and cascade the teardown to every
/// derived `Capability::Mmio`.
///
/// Called from `do_full_process_exit` (arch/x86_64/syscall) so a driver
/// crash or kill automatically frees the devices it owned for the
/// supervisor restart to re-claim. Safe to call for a PID that holds no
/// claims — returns zero and performs no I/O.
///
/// Cascade order (task doc acceptance: "dropping the device cap implicitly
/// drops the Mmio cap" + "IRQ bindings removed before the claim"):
///   1. Release IRQ bindings first so the ISR shim is a no-op before the
///      notification (and, transitively, the PCI handle) goes away.
///   2. Drain every `MmioEntry` owned by `pid` under `MMIO_REGISTRY`.
///   3. Release the claim slots in `DEVICE_HOST_REGISTRY` (which also
///      tears down the IOMMU domain via `PciDeviceHandle::drop`).
///   4. Outside the registry locks, walk the drained MMIO entries and
///      unmap their pages from the captured address spaces.
pub fn release_claims_for_pid(pid: Pid) {
    // Step 1: release IRQ bindings so no further device IRQ reaches a
    // notification the process is about to tear down.
    let irqs = release_irq_bindings_for_pid(pid);
    // Step 2: drain MMIO entries owned by pid.
    let drained_mmio = {
        let mut mmio = MMIO_REGISTRY.lock();
        mmio.drain_for_pid(pid)
    };
    // Step 3: release device-host claim slots. PciDeviceHandle Drop runs
    // here and tears down the IOMMU domain + PCI registry slot for each
    // released device.
    let freed = {
        let mut reg = DEVICE_HOST_REGISTRY.lock();
        reg.release_for_pid(pid)
    };
    // Step 4: teardown mmio mappings. Done outside the registry locks so
    // the page-table work (TLB shootdown, mapper->unmap) can acquire the
    // AS's own lock without risk of deadlock against a concurrent claim.
    let mmio_count = drained_mmio.len();
    for entry in drained_mmio {
        unmap_mmio_region_from_user(&entry.addr_space, entry.user_va, entry.len);
    }
    if freed > 0 || mmio_count > 0 || irqs > 0 {
        log::info!(
            "device_host.release pid={} freed_claims={} freed_mmio={} freed_irqs={}",
            pid,
            freed,
            mmio_count,
            irqs,
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

// ---------------------------------------------------------------------------
// Phase 55b Track B.4 — test-only synthetic IRQ bridge
// ---------------------------------------------------------------------------
//
// The B.4 test harness cannot invoke MSI allocation from `#[test_case]`
// context — the test runner has no claimed device under its own PID and
// MSI allocation writes to real hardware capability registers. Instead, it
// drives the pure-logic binding path and delivers a synthetic IRQ through
// the same ISR shim the production syscall installs. The helpers here
// expose just enough of that path to let the test assert:
//
//   1. `sys_device_irq_subscribe` accepts a claimed device and produces a
//      `Capability::DeviceIrq`,
//   2. the ISR shim fetched by the binding atomically sets the requested
//      bit on the target `Notification`,
//   3. `release_for_pid` tears the binding back down so the vector can be
//      reused by another driver.
//
// The helpers are `#[cfg(test)]` so none of them ship in release builds.

/// Error surface for the test-only IRQ bridge helper.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestIrqError {
    /// The caller's PID did not match the device's recorded owner.
    WrongOwner,
    /// The IRQ binding registry rejected the bind request (vector busy
    /// or per-PID cap exceeded).
    BindFailed,
    /// The notification slot was exhausted or the target bit was out of
    /// range.
    NotificationUnavailable,
    /// The B.4 implementation is still the scaffold stub — the helper
    /// cannot make progress until `bind_notification` lands.
    #[allow(dead_code)]
    NotImplemented,
}

/// Synthetically bind a device IRQ to a notification bit, deliver one
/// signal through the ISR shim, and return the bits the next
/// `notification::wait`/`signal_check` would observe.
///
/// Parameters mirror the production syscall:
/// - `pid` / `key` are the claim the caller already installed via
///   [`test_try_claim_for_pid`],
/// - `bit_index` is the notification word bit the ISR should set,
/// - `vector_offset` is an offset into the device-IRQ bank (0..15) —
///   the production syscall picks this through MSI allocation; the test
///   path names it directly so the test is deterministic.
///
/// Returns the bits that were pending on the target notification after
/// the synthetic delivery. A successful bind + delivery yields
/// `1u64 << bit_index`.
#[cfg(test)]
pub(crate) fn test_synthetic_irq_subscribe_and_signal(
    pid: Pid,
    key: DeviceCapKey,
    bit_index: u8,
    vector_offset: u8,
) -> Result<u64, TestIrqError> {
    let notif =
        crate::ipc::notification::try_create().ok_or(TestIrqError::NotificationUnavailable)?;
    let notif_idx = notif.0;
    let vector = crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE + vector_offset;

    // Install the binding in the IRQ registry and the ISR dispatch table.
    match install_irq_binding(pid, key, vector, notif, bit_index) {
        Ok(()) => {}
        Err(_) => {
            crate::ipc::notification::free(notif);
            return Err(TestIrqError::BindFailed);
        }
    }

    // Drive the synthetic ISR path through the registered handler. This
    // exercises the exact dispatch the hardware will hit when an MSI
    // vector fires — same fetch_or, same no-alloc, no-lock contract.
    crate::arch::x86_64::interrupts::dispatch_device_irq_for_test(vector);

    // Inspect the resulting pending bits on the notification.
    let bits = crate::ipc::notification::test_peek_pending(notif_idx);

    // Tear the binding down so the next test starts from a clean slate.
    let _ = uninstall_irq_binding(vector);
    crate::ipc::notification::release(notif);

    Ok(bits)
}

/// Install an IRQ binding directly into the registry + ISR dispatch
/// table. The test path reaches this without going through MSI
/// allocation; production callers go through
/// [`sys_device_irq_subscribe`] instead.
#[cfg(test)]
fn install_irq_binding(
    pid: Pid,
    key: DeviceCapKey,
    vector: u8,
    notif: NotifId,
    bit_index: u8,
) -> Result<(), IrqRegistryError> {
    bind_irq_vector(pid, key, vector, notif, bit_index)
}

/// Counterpart of [`install_irq_binding`] for the test-only path.
#[cfg(test)]
fn uninstall_irq_binding(vector: u8) -> Option<IrqBinding> {
    unbind_irq_vector(vector)
}
