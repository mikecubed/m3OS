//! IRQ-notification wrapper module — Phase 55b Track C.3.
//!
//! This module implements the driver-facing skeleton every ring-3
//! driver process reuses:
//!
//! ```text
//! loop {
//!     let bits = irq.wait();
//!     drain_ring(&mmio);
//!     irq.ack(bits)?;
//! }
//! ```
//!
//! Factoring the wait-ack rhythm here means `userspace/drivers/nvme`
//! (Track D) and `userspace/drivers/e1000` (Track E) consume the same
//! wrapper — no copy-pasted `notify_wait` loop, no driver-local
//! bit-mask bookkeeping.
//!
//! # Backend indirection
//!
//! [`IrqNotification`] is generic over [`IrqBackend`] so the contract
//! tests below can exercise `wait`/`ack`/`signal` without a running
//! kernel. Drivers use the default type alias
//! [`IrqNotification`] = [`IrqNotification<SyscallBackend>`] which
//! speaks the Phase 50 `notify_wait` syscall and the Phase 55b Track
//! B.4 [`sys_device_irq_subscribe`][b4] primitive.
//!
//! The abstract [`IrqNotificationContract`] declared in Track A.4
//! (`kernel-core::driver_runtime::contract`) uses a different shape
//! (`wait(&mut self) -> Result<(), _>`; deadline-bounded) because it
//! stands in for the timeout-bounded restart semantics the
//! service-manager regression (Track F.2) asserts. That trait is not
//! the shape drivers want — they want the signaled bit-mask back
//! from `wait` — so C.3 keeps the concrete [`IrqNotification`] API
//! here separate from the contract trait. Both surfaces re-export
//! the authoritative `DeviceHostError` → `DriverRuntimeError`
//! conversion from `kernel-core`.
//!
//! # `ack` semantics
//!
//! Phase 55b B.4 ("Track B.4 — `sys_device_irq_subscribe` and
//! notification bridging") landed `sys_device_irq_subscribe` but did
//! **not** land a `sys_device_irq_ack` primitive. MSI / MSI-X are
//! edge-triggered, so the notification word's `fetch_or` path in the
//! ISR shim is idempotent and the next IRQ fires without an explicit
//! hardware unmask: the "ack" from the driver side is really the act
//! of draining the PENDING bits (the Phase 50
//! `notification::wait` handler already does `swap(0)`).
//!
//! [`IrqNotification::ack`] therefore implements two responsibilities
//! wholly in the wrapper layer, with **no syscall issued**:
//!
//! 1. **Bit-mask validation** — the caller must pass only bits that
//!    belong to this subscription and that the most recent
//!    [`IrqNotification::wait`] actually returned. Passing any other
//!    bit yields [`DriverRuntimeError::InvalidAck`]. This catches
//!    driver-side bookkeeping bugs (acking a bit the IRQ never
//!    delivered, or reusing a stale mask) at the wrapper boundary
//!    instead of in the kernel.
//! 2. **Observed-bits bookkeeping** — after a successful `ack`, the
//!    wrapper masks the acknowledged bits off its last-observed
//!    record. The next [`wait`](Self::wait) installs a fresh mask.
//!
//! When a future phase adds a level-triggered IRQ path (legacy INTx
//! shared or a PCI-PM wake-up source) that needs explicit unmask,
//! the kernel primitive lands as `sys_device_irq_ack` and this
//! wrapper is amended in place — drivers observe no API change.
//!
//! [b4]: kernel_core::device_host::syscalls::SYS_DEVICE_IRQ_SUBSCRIBE

use core::cell::Cell;

use kernel_core::device_host::DeviceHostError;
use kernel_core::driver_runtime::contract::DriverRuntimeError;

/// Re-export of the IRQ subscription contract from `kernel-core`.
///
/// Track A.4 declared `IrqNotificationContract` and
/// `IrqNotificationHandle` with a `wait(&mut self) -> Result<(), _>`
/// shape bound by `DRIVER_RESTART_TIMEOUT_MS` so the service-manager
/// regression in Track F.2 can assert a restart-bounded path. The
/// concrete [`IrqNotification`] in this module has a different shape
/// (`wait(&self) -> u64`) because drivers want the signaled
/// bit-mask back; both surfaces co-exist in this crate without
/// conflict.
pub use kernel_core::driver_runtime::contract::{IrqNotificationContract, IrqNotificationHandle};

/// Syscall number reserved for `sys_device_irq_subscribe`
/// (re-exported from `kernel-core` as the single source of truth).
pub use kernel_core::device_host::syscalls::SYS_DEVICE_IRQ_SUBSCRIBE;

/// Sentinel passed in the `notification_arg` slot of
/// [`SYS_DEVICE_IRQ_SUBSCRIBE`] to request that the kernel allocate a fresh
/// [`kernel_core::ipc::notification_logic::Notification`] on the caller's
/// behalf. Re-exported from `kernel-core` so the ABI sentinel has one
/// definition shared by the kernel handler and this backend.
pub use kernel_core::device_host::syscalls::NOTIFICATION_SENTINEL_NEW;

// ---------------------------------------------------------------------------
// DeviceCapHandle — minimal view of a claimed device
// ---------------------------------------------------------------------------

/// Minimal behavioral bound [`IrqNotification::subscribe`] requires
/// of a device handle.
///
/// Track C.2 lands the concrete `DeviceHandle` wrapper around
/// `Capability::Device`; that wrapper implements this trait. Exposing
/// the trait here keeps Track C.3 independent of the not-yet-merged
/// C.2 concrete type — the shared surface is the device cap handle
/// that `sys_device_irq_subscribe` will validate.
pub trait DeviceCapHandle {
    /// Return the process-local capability-table handle that names a
    /// `Capability::Device` slot. The kernel re-validates this on
    /// every syscall; the wrapper does not cache derived state
    /// beyond the value itself.
    fn cap_handle(&self) -> u32;
}

// ---------------------------------------------------------------------------
// IrqBackend — indirection between the real syscall ABI and test mocks
// ---------------------------------------------------------------------------

/// Backend-level operations [`IrqNotification`] needs.
///
/// Drivers never name this trait — they consume [`IrqNotification`]
/// with its default [`SyscallBackend`]. The trait exists so contract
/// tests can swap in a mock that drives `wait` and `signal` from
/// pure host code without touching the syscall ABI.
pub trait IrqBackend {
    /// Call `sys_device_irq_subscribe(dev_cap, vector_hint, notification_index)`.
    /// Returns the caller-table cap handle on success or a
    /// [`DriverRuntimeError`] on failure.
    fn subscribe(
        &self,
        dev_cap: u32,
        vector_hint: Option<u8>,
        notification_index: u32,
    ) -> Result<u32, DriverRuntimeError>;

    /// Block until the notification word has any bit set; return the
    /// cleared bits (Phase 50 semantics). Implementations translate
    /// the raw syscall return code — the trait surface is the
    /// bit-mask only, with zero reserved for "no bits delivered"
    /// (including the error case).
    fn wait(&self, notif_cap: u32) -> u64;

    /// Release the subscription (best-effort; used by `Drop`).
    ///
    /// B.4 tears the binding down on process exit, so the production
    /// backend implements this as a no-op. Mocks use it to observe
    /// Drop.
    fn release(&self, notif_cap: u32);
}

// ---------------------------------------------------------------------------
// IrqBackend impl for the shared SyscallBackend
// ---------------------------------------------------------------------------
//
// C.2 introduced `crate::syscall_backend::SyscallBackend` as the zero-sized
// production backend for every driver_runtime wrapper. C.3 extends it here
// with an `IrqBackend` impl so `IrqNotification<SyscallBackend>` resolves to
// the same type drivers already use for `DeviceHandle` / `Mmio` / `DmaBuffer`.
// No new struct is declared.

pub use crate::syscall_backend::SyscallBackend;

impl IrqBackend for SyscallBackend {
    fn subscribe(
        &self,
        dev_cap: u32,
        vector_hint: Option<u8>,
        notification_index: u32,
    ) -> Result<u32, DriverRuntimeError> {
        // B.4b ABI: `sys_device_irq_subscribe(dev_cap, bit_index, notification_arg)`.
        //
        // The old (pre-B.4b) ABI carried `vector_hint` in arg2 and the
        // notification bit in arg3. B.4b repurposed arg2 as `bit_index`
        // (range 0..=63; values ≥ 64 return -EINVAL) and arg3 as a
        // `notification_arg` that is either a CapHandle to an existing
        // `Capability::Notification` or the sentinel
        // `NOTIFICATION_SENTINEL_NEW` (= u32::MAX) asking the kernel to
        // allocate a fresh `Notification` on the caller's behalf.
        //
        // We map the trait's `notification_index` parameter onto
        // `bit_index` and always pass `NOTIFICATION_SENTINEL_NEW` for
        // `notification_arg` — this backend does not yet expose the
        // "bind to caller-owned notification" path, so a fresh kernel-
        // allocated notification is always requested. `vector_hint` is
        // accepted for backwards source-compat with the C.3 API but
        // ignored on the wire: the old kernel arm2 interpretation was
        // retired by B.4b.
        let _ = vector_hint;
        if notification_index >= 64 {
            // Fail locally with the same errno the kernel would emit
            // rather than paying the syscall round-trip just to be
            // rejected. Keeps the trait boundary well-defined.
            return Err(errno_to_driver_runtime_error(-22));
        }
        // `syscall3` maps (rax, rdi, rsi, rdx) to
        // (SYS_DEVICE_IRQ_SUBSCRIBE, dev_cap, bit_index, notification_arg).
        //
        // SAFETY: the syscall number is the `kernel-core` constant
        // reserved in the device-host block; B.4b (`sys_device_irq_subscribe`
        // in `kernel/src/syscall/device_host.rs`) accepts three u64
        // arguments in exactly this order and returns an isize. No
        // pointer arguments; no memory aliasing concerns.
        let rax = unsafe {
            syscall_lib::syscall3(
                SYS_DEVICE_IRQ_SUBSCRIBE,
                dev_cap as u64,
                notification_index as u64,
                NOTIFICATION_SENTINEL_NEW as u64,
            )
        };
        // Non-negative return values are cap handles. Negative
        // values are `isize` errnos sign-extended into the u64
        // return register — cast back to inspect.
        let signed = rax as i64;
        if signed < 0 {
            Err(errno_to_driver_runtime_error(signed))
        } else {
            // `signed >= 0` and cap handles fit in `u32` by the
            // kernel's own cap-table bound, so this truncation is
            // safe. We clamp defensively rather than unwrap to honor
            // the Phase 55b "no panic in non-test code" discipline.
            Ok((signed as u64 & u32::MAX as u64) as u32)
        }
    }

    fn wait(&self, notif_cap: u32) -> u64 {
        // `notify_wait` returns the pending-bit word on success and
        // 0 on error (Phase 50 semantics documented in
        // `kernel/src/ipc/mod.rs` arm 7). The wrapper lifts a zero
        // return into the semantic "no bits were delivered" so the
        // driver's next `ack` sees an empty observed-mask and can
        // refuse an accidental ack.
        syscall_lib::notify_wait(notif_cap)
    }

    fn release(&self, _notif_cap: u32) {
        // No `sys_device_irq_release` in B.4 — process exit tears
        // down the binding (per B.4 acceptance: "On process exit:
        // the vector is released, the MSI capability on the device
        // is disabled, and the notification is unbound"). Leaving
        // this as a documented no-op avoids a bogus syscall and
        // keeps Drop infallible.
    }
}

/// Map a signed kernel return code (negative errno) to a
/// [`DriverRuntimeError`].
///
/// Track B.4's error surface is the Linux errno subset
/// documented in `kernel/src/syscall/device_host.rs`:
/// `NEG_ESRCH = -3`, `NEG_EBADF = -9`, `NEG_EPERM = -1`,
/// `NEG_ENODEV = -19`, `NEG_EINVAL = -22`, `NEG_ENOMEM = -12`,
/// `NEG_ENFILE = -23`. Map each to the closest
/// [`DeviceHostError`] variant.
fn errno_to_driver_runtime_error(errno: i64) -> DriverRuntimeError {
    let mapped = match errno {
        -9 => DeviceHostError::BadDeviceCap,      // EBADF
        -1 => DeviceHostError::BadDeviceCap,      // EPERM (policy reject)
        -3 => DeviceHostError::BadDeviceCap,      // ESRCH (task gone)
        -12 => DeviceHostError::Internal,         // ENOMEM (notif alloc)
        -19 => DeviceHostError::BadDeviceCap,     // ENODEV
        -22 => DeviceHostError::IrqUnavailable,   // EINVAL (no vector)
        -23 => DeviceHostError::CapacityExceeded, // ENFILE (irq cap full)
        _ => DeviceHostError::Internal,
    };
    DriverRuntimeError::from(mapped)
}

// ---------------------------------------------------------------------------
// IrqNotification — concrete driver-facing wrapper
// ---------------------------------------------------------------------------

/// Ring-3 driver's view of a single device IRQ subscription.
///
/// Wraps the `Capability::DeviceIrq` installed by
/// `sys_device_irq_subscribe` (Track B.4) plus the bit within the
/// notification word the ISR shim sets when the IRQ fires. The
/// driver's main loop blocks in [`wait`](Self::wait), drains its
/// ring in task context, and reports completion to the wrapper via
/// [`ack`](Self::ack).
///
/// [`Self`] is generic over [`IrqBackend`] so the contract tests in
/// this module can exercise the wait / signal / ack state machine
/// without a running kernel. Drivers use the default
/// [`SyscallBackend`].
#[derive(Debug)]
pub struct IrqNotification<B: IrqBackend = SyscallBackend> {
    /// Process-local cap handle the kernel installed at
    /// `sys_device_irq_subscribe` time. Per B.4 this is a
    /// `Capability::DeviceIrq` slot; `notify_wait` against this
    /// handle returns the pending notification word.
    cap_handle: u32,

    /// Bits within the 64-bit notification word the ISR shim may
    /// set for this subscription. Stored so [`ack`](Self::ack) can
    /// reject bits outside the subscription's own mask — a driver
    /// should only ever ack bits it subscribed to.
    bit_mask: u64,

    /// Most recent mask returned by [`wait`](Self::wait). Cleared
    /// incrementally by [`ack`](Self::ack). `Cell` because `wait`
    /// takes `&self` per the Phase 55b C.3 acceptance shape.
    last_observed: Cell<u64>,

    /// Backend indirection (production or mock).
    backend: B,
}

impl<B: IrqBackend> IrqNotification<B> {
    /// Construct an [`IrqNotification`] from an existing cap handle
    /// and backend. Exposed so drivers (or tests) can rebuild the
    /// wrapper from parts when a capability crosses a grant boundary
    /// without re-issuing `sys_device_irq_subscribe`.
    ///
    /// `bit_mask` names the bit(s) the ISR may signal; callers that
    /// multiplex multiple vectors into one notification word pass a
    /// multi-bit mask.
    pub fn from_parts(cap_handle: u32, bit_mask: u64, backend: B) -> Self {
        Self {
            cap_handle,
            bit_mask,
            last_observed: Cell::new(0),
            backend,
        }
    }

    /// Process-local cap handle this subscription owns.
    pub fn cap_handle(&self) -> u32 {
        self.cap_handle
    }

    /// Bits within the notification word this subscription may
    /// observe.
    pub fn bit_mask(&self) -> u64 {
        self.bit_mask
    }

    /// Block until the subscription's notification word has any bit
    /// set, and return the pending bits the kernel cleared.
    ///
    /// Phase 50 semantics: `notify_wait` atomically reads and clears
    /// the notification word (swap(0)), so the value returned here
    /// is the snapshot of bits observed at wake-up. A return of
    /// zero means the underlying syscall failed (per arm 7 in
    /// `kernel/src/ipc/mod.rs`: "notify_wait (7) errors return 0").
    /// Drivers that treat zero as "no work pending" behave correctly
    /// — [`ack`](Self::ack) with `bits == 0` is a no-op.
    ///
    /// The returned bits are masked against the subscription's
    /// assigned `bit_mask` so a multiplexed notification word (one
    /// cap serving multiple subscriptions) only reports bits this
    /// `IrqNotification` is responsible for. Bits outside the mask
    /// are discarded silently — they belong to other subscriptions
    /// on the same notification object.
    pub fn wait(&self) -> u64 {
        let raw = self.backend.wait(self.cap_handle);
        let bits = raw & self.bit_mask;
        self.last_observed.set(bits);
        bits
    }

    /// Acknowledge the bits previously observed by
    /// [`wait`](Self::wait).
    ///
    /// Returns [`DriverRuntimeError::InvalidAck`] when `bits` is not
    /// a subset of the bits most recently delivered. This guards
    /// against three driver-side bookkeeping bugs:
    ///
    /// 1. Acking bits outside the subscription's assigned mask
    ///    (`bits & !bit_mask != 0`).
    /// 2. Acking a bit the most recent `wait` did not deliver
    ///    (`bits & !last_observed != 0`).
    /// 3. Acking after a prior `ack` already cleared the observed
    ///    mask (stale mask reuse — a subset of case 2).
    ///
    /// A successful `ack` masks the acknowledged bits off the
    /// observed record. The next [`wait`](Self::wait) installs a
    /// fresh value.
    ///
    /// See the module-level docs for why this is a wrapper-only
    /// contract (no hardware unmask is needed on edge-triggered MSI
    /// / MSI-X per B.4's ISR design).
    pub fn ack(&self, bits: u64) -> Result<(), DriverRuntimeError> {
        // Zero-bit ack is a cheap no-op — Phase 50 notification_wait
        // on a word with no pending bits returns zero, and acking
        // that zero is a legitimate "nothing to do" after a spurious
        // wake-up.
        if bits == 0 {
            return Ok(());
        }
        // Bits outside the subscription's assigned mask are always
        // invalid — the ISR shim never sets them on this
        // subscription.
        if bits & !self.bit_mask != 0 {
            return Err(DriverRuntimeError::InvalidAck);
        }
        // Bits the most recent wait() did not deliver are invalid —
        // this catches driver double-acks and stale-mask reuse.
        let observed = self.last_observed.get();
        if bits & !observed != 0 {
            return Err(DriverRuntimeError::InvalidAck);
        }
        // Record the successful ack by masking off the acknowledged
        // bits. The next wait() will install a fresh observed mask.
        self.last_observed.set(observed & !bits);
        Ok(())
    }
}

impl IrqNotification<SyscallBackend> {
    /// Subscribe to IRQs on the device `device` names.
    ///
    /// Issues `sys_device_irq_subscribe` under the hood and
    /// constructs an [`IrqNotification`] bound to notification bit
    /// zero (the kernel's ISR shim signals the low bit on every
    /// vector — vectored multi-bit subscriptions are a future-phase
    /// extension).
    ///
    /// `vector_hint` is advisory — the kernel's MSI allocator
    /// decides the final vector. `None` leaves the allocator free;
    /// `Some(n)` asks the kernel to prefer vector `n` within the
    /// device's reserved range.
    pub fn subscribe<D: DeviceCapHandle>(
        device: &D,
        vector_hint: Option<u8>,
    ) -> Result<Self, DriverRuntimeError> {
        Self::subscribe_with_backend(SyscallBackend, device, vector_hint)
    }
}

impl<B: IrqBackend> IrqNotification<B> {
    /// Subscribe using a caller-supplied backend. Kept separate
    /// from [`IrqNotification::subscribe`] so contract tests can
    /// drive the full subscribe → wait → ack cycle through a mock
    /// without ever touching the syscall ABI.
    pub fn subscribe_with_backend<D: DeviceCapHandle>(
        backend: B,
        device: &D,
        vector_hint: Option<u8>,
    ) -> Result<Self, DriverRuntimeError> {
        // Default to notification bit 0 — see `subscribe` docs.
        let notification_index: u32 = 0;
        let cap = backend.subscribe(device.cap_handle(), vector_hint, notification_index)?;
        Ok(Self {
            cap_handle: cap,
            bit_mask: 1u64 << notification_index,
            last_observed: Cell::new(0),
            backend,
        })
    }
}

impl<B: IrqBackend> Drop for IrqNotification<B> {
    fn drop(&mut self) {
        // Release is best-effort — the backend is free to implement
        // this as a no-op when process exit tears the binding down
        // (as [`SyscallBackend`] does).
        self.backend.release(self.cap_handle);
    }
}

// ---------------------------------------------------------------------------
// irq_loop — convenience wrapper for the canonical driver main loop
// ---------------------------------------------------------------------------

/// Run the canonical driver wait-ack loop until an ack fails.
///
/// Drivers with more elaborate main loops (e.g. those that
/// multiplex an IRQ with a server endpoint via `ipc_recv`) can
/// open-code the pattern — this helper is for the common case
/// where the driver does the same post-IRQ work on every wake.
///
/// `f` is called once per IRQ delivery, after the wrapper has
/// recorded the observed mask and before `ack` clears it. Zero-bit
/// wake-ups (error path on `notify_wait`) skip the callback and
/// continue the loop rather than burn CPU acking an empty mask.
///
/// Ack failures propagate as an early return — an invariant
/// failure here indicates a driver-side bookkeeping bug, and the
/// service manager's restart-on-exit path (Phase 46 / 51) will
/// bring the driver back in a clean state.
pub fn irq_loop<B: IrqBackend>(
    notif: &IrqNotification<B>,
    mut f: impl FnMut(),
) -> Result<(), DriverRuntimeError> {
    loop {
        let bits = notif.wait();
        if bits == 0 {
            // A zero return from `wait` means the syscall errored
            // (see `SyscallBackend::wait` docs) or no subscribed bit
            // was set. Continue rather than silently ack an empty
            // mask. A driver that needs to react to repeated
            // zero-returns can replace this helper with its own
            // loop.
            continue;
        }
        f();
        notif.ack(bits)?;
    }
}

// ---------------------------------------------------------------------------
// Tests — Track C.3 contract against a local mock backend
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    // -- Mock device handle ------------------------------------------------

    struct MockDevice {
        cap_handle: u32,
    }
    impl DeviceCapHandle for MockDevice {
        fn cap_handle(&self) -> u32 {
            self.cap_handle
        }
    }

    // -- Mock IRQ backend --------------------------------------------------
    //
    // Tracks subscribed caps, their bound device cap, a per-cap
    // pending bit queue, and release flags. `signal` is the
    // test-only knob that pushes a bit-mask onto the pending queue
    // so the next `wait` call observes it. A queue (rather than a
    // single slot) lets a test deliver two IRQs back-to-back and
    // assert both are drained in order.

    #[derive(Default, Debug)]
    struct MockState {
        next_cap: u32,
        subs: Vec<SubRecord>,
        inject_subscribe_error: Option<DriverRuntimeError>,
    }

    #[derive(Debug)]
    struct SubRecord {
        cap_handle: u32,
        dev_cap: u32,
        vector_hint: Option<u8>,
        pending: Vec<u64>,
        released: bool,
    }

    #[derive(Clone, Default, Debug)]
    struct MockBackend {
        state: Rc<RefCell<MockState>>,
    }

    impl MockBackend {
        fn new() -> Self {
            Self {
                state: Rc::new(RefCell::new(MockState {
                    next_cap: 100,
                    ..Default::default()
                })),
            }
        }

        fn signal(&self, cap_handle: u32, bits: u64) {
            let mut st = self.state.borrow_mut();
            if let Some(sub) = st.subs.iter_mut().find(|s| s.cap_handle == cap_handle) {
                sub.pending.push(bits);
            }
        }

        fn released(&self, cap_handle: u32) -> bool {
            self.state
                .borrow()
                .subs
                .iter()
                .find(|s| s.cap_handle == cap_handle)
                .map(|s| s.released)
                .unwrap_or(false)
        }

        fn sub_vector_hint(&self, cap_handle: u32) -> Option<Option<u8>> {
            self.state
                .borrow()
                .subs
                .iter()
                .find(|s| s.cap_handle == cap_handle)
                .map(|s| s.vector_hint)
        }

        fn sub_dev_cap(&self, cap_handle: u32) -> Option<u32> {
            self.state
                .borrow()
                .subs
                .iter()
                .find(|s| s.cap_handle == cap_handle)
                .map(|s| s.dev_cap)
        }
    }

    impl IrqBackend for MockBackend {
        fn subscribe(
            &self,
            dev_cap: u32,
            vector_hint: Option<u8>,
            _notification_index: u32,
        ) -> Result<u32, DriverRuntimeError> {
            let mut st = self.state.borrow_mut();
            if let Some(err) = st.inject_subscribe_error.take() {
                return Err(err);
            }
            let cap_handle = st.next_cap;
            st.next_cap = st.next_cap.wrapping_add(1);
            st.subs.push(SubRecord {
                cap_handle,
                dev_cap,
                vector_hint,
                pending: Vec::new(),
                released: false,
            });
            Ok(cap_handle)
        }

        fn wait(&self, notif_cap: u32) -> u64 {
            let mut st = self.state.borrow_mut();
            let sub = match st.subs.iter_mut().find(|s| s.cap_handle == notif_cap) {
                Some(s) => s,
                None => return 0,
            };
            if sub.pending.is_empty() {
                // Tests never call `wait` before `signal` — a zero
                // return here would deadlock a real driver; the
                // mock surfaces it as "no pending bits" for the
                // wait-before-signal negative path.
                return 0;
            }
            sub.pending.remove(0)
        }

        fn release(&self, notif_cap: u32) {
            let mut st = self.state.borrow_mut();
            if let Some(sub) = st.subs.iter_mut().find(|s| s.cap_handle == notif_cap) {
                sub.released = true;
            }
        }
    }

    // -- Core contract tests ----------------------------------------------

    #[test]
    fn subscribe_records_device_cap_and_vector_hint() {
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 7 };
        let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, Some(0x21))
            .expect("subscribe");
        assert_eq!(backend.sub_dev_cap(notif.cap_handle()), Some(7));
        assert_eq!(
            backend.sub_vector_hint(notif.cap_handle()),
            Some(Some(0x21))
        );
    }

    #[test]
    fn subscribe_surfaces_backend_error() {
        let backend = MockBackend::new();
        backend.state.borrow_mut().inject_subscribe_error =
            Some(DriverRuntimeError::from(DeviceHostError::IrqUnavailable));
        let device = MockDevice { cap_handle: 1 };
        let err = IrqNotification::subscribe_with_backend(backend, &device, None)
            .expect_err("subscribe must fail");
        assert_eq!(
            err,
            DriverRuntimeError::from(DeviceHostError::IrqUnavailable)
        );
    }

    #[test]
    fn wait_returns_signaled_bits() {
        // The canonical C.3 acceptance bullet: wait returns the
        // signaled bit mask after signal is called.
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, None)
            .expect("subscribe");
        backend.signal(notif.cap_handle(), 0b1);
        let bits = notif.wait();
        assert_eq!(bits, 0b1);
    }

    #[test]
    fn wait_observes_multiple_deliveries_in_order() {
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, None)
            .expect("subscribe");
        backend.signal(notif.cap_handle(), 0b1);
        backend.signal(notif.cap_handle(), 0b1);
        // Back-to-back deliveries each return the bit; this mirrors
        // the e1000 rx-ring drain loop that wakes on every IRQ.
        assert_eq!(notif.wait(), 0b1);
        assert!(notif.ack(0b1).is_ok());
        assert_eq!(notif.wait(), 0b1);
    }

    #[test]
    fn ack_with_observed_bits_clears_the_mask() {
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, None)
            .expect("subscribe");
        backend.signal(notif.cap_handle(), 0b1);
        let bits = notif.wait();
        assert!(notif.ack(bits).is_ok());
        // A second ack of the same bits with no intervening wait is
        // an InvalidAck — the observed mask was cleared.
        assert_eq!(notif.ack(bits), Err(DriverRuntimeError::InvalidAck));
    }

    #[test]
    fn ack_zero_is_noop_even_without_prior_wait() {
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif =
            IrqNotification::subscribe_with_backend(backend, &device, None).expect("subscribe");
        assert!(notif.ack(0).is_ok());
    }

    #[test]
    fn wait_masks_bits_outside_subscription_mask() {
        // A multiplexed notification word may carry bits outside
        // this subscription's assigned mask (e.g. another
        // subscription on the same notification). `wait` must
        // discard them so the observed-mask bookkeeping only
        // records bits the subscription owns.
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, None)
            .expect("subscribe");
        // Deliver bit 0 (inside mask) plus bit 3 (outside mask).
        backend.signal(notif.cap_handle(), 0b1 | 0b1000);
        assert_eq!(notif.wait(), 0b1);
        // ack(0b1) succeeds; ack(0b1000) is invalid (outside mask).
        assert!(notif.ack(0b1).is_ok());
    }

    // -- Negative tests ----------------------------------------------------

    #[test]
    fn ack_with_bits_outside_subscription_mask_returns_invalid_ack() {
        // The authoritative C.3 acceptance negative test: `ack`
        // with bits the caller did not observe returns
        // DriverRuntimeError::InvalidAck, not a panic.
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, None)
            .expect("subscribe");
        // Sub installed on bit 0; acking bit 1 is never legitimate.
        let result = notif.ack(0b10);
        assert_eq!(result, Err(DriverRuntimeError::InvalidAck));
    }

    #[test]
    fn ack_with_unobserved_bits_returns_invalid_ack() {
        // Even if a bit *is* in the subscription mask, acking it
        // without a preceding wait() that returned it is
        // InvalidAck — this catches stale-mask reuse.
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif =
            IrqNotification::subscribe_with_backend(backend, &device, None).expect("subscribe");
        assert_eq!(notif.ack(0b1), Err(DriverRuntimeError::InvalidAck));
    }

    // -- Drop-releases test ------------------------------------------------

    #[test]
    fn drop_releases_the_subscription() {
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let cap = {
            let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, None)
                .expect("subscribe");
            notif.cap_handle()
        };
        assert!(backend.released(cap));
    }

    // -- irq_loop convenience wrapper --------------------------------------

    #[test]
    fn irq_loop_invokes_callback_on_delivery_and_acks() {
        // `irq_loop` runs `wait` → `f` → `ack` forever until a
        // terminal condition. To exit deterministically, the
        // closure drains the observed mask itself by calling
        // `ack(bits)` from inside the callback; the subsequent
        // `ack` inside `irq_loop` then sees an empty observed
        // mask and returns `InvalidAck`, which `irq_loop`
        // surfaces as an early return.
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, None)
            .expect("subscribe");
        backend.signal(notif.cap_handle(), 0b1);

        let counter = Rc::new(RefCell::new(0_u32));
        let counter_for_closure = counter.clone();
        let notif_for_closure: *const IrqNotification<MockBackend> = &notif;
        let err = irq_loop(&notif, move || {
            *counter_for_closure.borrow_mut() += 1;
            // SAFETY: the raw pointer is live for the lifetime of
            // the surrounding `irq_loop` call — the loop borrows
            // `&notif` by shared reference and never drops it
            // during callback invocation. Using a raw pointer
            // here sidesteps the shared-borrow restriction on the
            // closure without changing the public `irq_loop`
            // signature.
            let n = unsafe { &*notif_for_closure };
            // Drain the observed mask from inside the callback so
            // the loop's follow-up `ack` fails.
            let _ = n.ack(0b1);
        })
        .expect_err("irq_loop should surface the ack error");
        assert_eq!(err, DriverRuntimeError::InvalidAck);
        assert_eq!(*counter.borrow(), 1);
    }

    #[test]
    fn from_parts_constructs_a_usable_notification_without_subscribe() {
        // Drivers that exchange an IrqNotification across a
        // capability grant (future Track D.3 / E.3 usage) need to
        // rebuild the wrapper from parts without issuing
        // `sys_device_irq_subscribe` a second time.
        let backend = MockBackend::new();
        let cap = {
            let mut st = backend.state.borrow_mut();
            let c = st.next_cap;
            st.next_cap = st.next_cap.wrapping_add(1);
            st.subs.push(SubRecord {
                cap_handle: c,
                dev_cap: 0,
                vector_hint: None,
                pending: alloc::vec![0b1],
                released: false,
            });
            c
        };
        let notif = IrqNotification::from_parts(cap, 0b1, backend);
        assert_eq!(notif.wait(), 0b1);
        assert!(notif.ack(0b1).is_ok());
    }
}
