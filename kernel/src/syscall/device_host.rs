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

use core::sync::atomic::{AtomicU8, Ordering};

use kernel_core::device_host::{
    DeviceCapKey, DeviceHostError, DeviceHostRegistryCore, IrqBinding, IrqBindingRegistryCore,
    IrqRegistryError, RegistryError,
};
use kernel_core::ipc::Capability;
use kernel_core::types::NotifId;
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
/// Negative errno `-EINVAL` (22) — bad argument (e.g. bit_index out of
/// range, vector outside the stub bank, or the B.4 acceptance's
/// "IrqUnavailable" case mapped to EINVAL because no MSI/INTx path
/// succeeded).
const NEG_EINVAL: isize = -22;
/// Negative errno `-EPERM` (1).
const NEG_EPERM: isize = -1;

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
// Process-exit hook
// ---------------------------------------------------------------------------

/// Release every claim held by `pid`.
///
/// Called from `do_full_process_exit` (arch/x86_64/syscall) so a driver
/// crash or kill automatically frees the devices it owned for the
/// supervisor restart to re-claim. Safe to call for a PID that holds no
/// claims — returns zero and performs no I/O.
///
/// Order of teardown matters: IRQ bindings are removed **before** the
/// claim itself so the ISR shim is a no-op before the notification (and,
/// transitively, the PCI handle) goes away. This avoids a race where an
/// in-flight interrupt arrives after the notification has been released
/// but before the shim mirror has been scrubbed.
pub fn release_claims_for_pid(pid: Pid) {
    let irqs = release_irq_bindings_for_pid(pid);
    let freed = {
        let mut reg = DEVICE_HOST_REGISTRY.lock();
        reg.release_for_pid(pid)
    };
    if freed > 0 || irqs > 0 {
        log::info!(
            "device_host.release pid={} freed_claims={} freed_irqs={}",
            pid,
            freed,
            irqs,
        );
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
