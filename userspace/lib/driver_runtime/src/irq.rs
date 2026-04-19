//! IRQ-notification wrapper module — Phase 55b Track C.3 (red commit).
//!
//! This is the failing-test commit for Track C.3. The public surface
//! (`IrqBackend`, `IrqNotification`, `IrqNotification::subscribe` /
//! `wait` / `ack`, `irq_loop`, `DeviceCapHandle`) is declared here so
//! the test module below compiles, but every implementation is a
//! stub that intentionally returns a wrong value. The green commit
//! replaces the stubs with the real wrapper against the Phase 50
//! `notify_wait` syscall and the Phase 55b Track B.4
//! `sys_device_irq_subscribe` primitive.
//!
//! See `docs/roadmap/tasks/55b-ring-3-driver-host-tasks.md` section
//! C.3 for the authoritative acceptance criteria.

use core::cell::Cell;

use kernel_core::device_host::DeviceHostError;
use kernel_core::driver_runtime::contract::DriverRuntimeError;

/// Re-export of the IRQ subscription contract from `kernel-core`.
///
/// Track A.4 declared `IrqNotificationContract` and
/// `IrqNotificationHandle` with a `wait(&mut self) -> Result<(), _>`
/// shape bound by `DRIVER_RESTART_TIMEOUT_MS` so the service-manager
/// regression in Track F.2 can assert a restart-bounded path. The
/// concrete `IrqNotification` wrapper in this module has a different
/// shape (`wait(&self) -> u64`) because drivers want the signaled
/// bit-mask back; both surfaces co-exist in this crate without
/// conflict.
pub use kernel_core::driver_runtime::contract::{IrqNotificationContract, IrqNotificationHandle};

// ---------------------------------------------------------------------------
// DeviceCapHandle — minimal view of a claimed device
// ---------------------------------------------------------------------------

/// Minimal behavioral bound `IrqNotification::subscribe` requires of a
/// device handle. Track C.2 lands the concrete `DeviceHandle` wrapper
/// around `Capability::Device` and will impl this trait.
pub trait DeviceCapHandle {
    /// Return the process-local capability-table handle that names a
    /// `Capability::Device` slot.
    fn cap_handle(&self) -> u32;
}

// ---------------------------------------------------------------------------
// IrqBackend — indirection for real syscalls vs. mock
// ---------------------------------------------------------------------------

/// Backend-level operations `IrqNotification` needs.
pub trait IrqBackend {
    /// Subscribe to a device IRQ; return the cap handle on success.
    fn subscribe(
        &self,
        dev_cap: u32,
        vector_hint: Option<u8>,
        notification_index: u32,
    ) -> Result<u32, DriverRuntimeError>;

    /// Block until the notification word has any bit set; return the
    /// cleared bits.
    fn wait(&self, notif_cap: u32) -> u64;

    /// Release the subscription (best-effort; used by `Drop`).
    fn release(&self, notif_cap: u32);
}

// ---------------------------------------------------------------------------
// SyscallBackend — stub for the red commit
// ---------------------------------------------------------------------------

/// Zero-sized production backend. Stubbed in the red commit — the
/// green commit wires this to `syscall_lib::notify_wait` and
/// `SYS_DEVICE_IRQ_SUBSCRIBE`.
#[derive(Default, Clone, Copy, Debug)]
pub struct SyscallBackend;

impl IrqBackend for SyscallBackend {
    fn subscribe(
        &self,
        _dev_cap: u32,
        _vector_hint: Option<u8>,
        _notification_index: u32,
    ) -> Result<u32, DriverRuntimeError> {
        // Red-commit stub — force the red tests to fail.
        Err(DriverRuntimeError::from(DeviceHostError::Internal))
    }

    fn wait(&self, _notif_cap: u32) -> u64 {
        // Red-commit stub — no bits delivered.
        0
    }

    fn release(&self, _notif_cap: u32) {}
}

// ---------------------------------------------------------------------------
// IrqNotification — stub body for the red commit
// ---------------------------------------------------------------------------

/// Ring-3 driver's view of a device IRQ subscription. Stubbed in the
/// red commit; the green commit lands the real wait/ack state
/// machine.
#[derive(Debug)]
pub struct IrqNotification<B: IrqBackend = SyscallBackend> {
    cap_handle: u32,
    bit_mask: u64,
    last_observed: Cell<u64>,
    backend: B,
}

impl<B: IrqBackend> IrqNotification<B> {
    /// Construct from parts (stub). Green commit retains this shape.
    pub fn from_parts(cap_handle: u32, bit_mask: u64, backend: B) -> Self {
        Self {
            cap_handle,
            bit_mask,
            last_observed: Cell::new(0),
            backend,
        }
    }

    /// Cap handle (stub getter).
    pub fn cap_handle(&self) -> u32 {
        self.cap_handle
    }

    /// Bit mask (stub getter).
    pub fn bit_mask(&self) -> u64 {
        self.bit_mask
    }

    /// Wait stub — always returns zero so the observation tests fail.
    pub fn wait(&self) -> u64 {
        // Red-commit stub: intentionally does not drive the backend.
        let _ = self.last_observed.get();
        0
    }

    /// Ack stub — always returns Ok so the negative tests fail.
    pub fn ack(&self, _bits: u64) -> Result<(), DriverRuntimeError> {
        // Red-commit stub: never rejects a bit.
        let _ = self.bit_mask;
        Ok(())
    }
}

impl IrqNotification<SyscallBackend> {
    /// Subscribe stub that always errors — red-commit driver would
    /// never get a handle back.
    pub fn subscribe<D: DeviceCapHandle>(
        device: &D,
        vector_hint: Option<u8>,
    ) -> Result<Self, DriverRuntimeError> {
        Self::subscribe_with_backend(SyscallBackend, device, vector_hint)
    }
}

impl<B: IrqBackend> IrqNotification<B> {
    /// Subscribe-with-backend stub — forwards to `backend.subscribe`
    /// which (in the red SyscallBackend) returns Err, and in the mock
    /// returns Ok. The red mock tests will observe the subscribe
    /// succeeding but wait/ack producing wrong results.
    pub fn subscribe_with_backend<D: DeviceCapHandle>(
        backend: B,
        device: &D,
        vector_hint: Option<u8>,
    ) -> Result<Self, DriverRuntimeError> {
        let cap = backend.subscribe(device.cap_handle(), vector_hint, 0)?;
        Ok(Self {
            cap_handle: cap,
            bit_mask: 1u64 << 0,
            last_observed: Cell::new(0),
            backend,
        })
    }
}

impl<B: IrqBackend> Drop for IrqNotification<B> {
    fn drop(&mut self) {
        // Red-commit stub: read the backend without invoking
        // `release`, so the drop test (which asserts the backend
        // recorded a release) still fails. The unused-field lint
        // needs the read to see `backend` as live.
        let _ = &self.backend;
    }
}

// ---------------------------------------------------------------------------
// irq_loop — stub for the red commit
// ---------------------------------------------------------------------------

/// Stub loop helper that exits immediately with Ok(()) so the red
/// loop test fails. Green commit lands the real wait-ack loop.
pub fn irq_loop<B: IrqBackend>(
    _notif: &IrqNotification<B>,
    _f: impl FnMut(),
) -> Result<(), DriverRuntimeError> {
    // Red-commit stub — surfaces a distinct error so the test
    // assertion on InvalidAck fails.
    Err(DriverRuntimeError::from(DeviceHostError::Internal))
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

    struct MockDevice {
        cap_handle: u32,
    }
    impl DeviceCapHandle for MockDevice {
        fn cap_handle(&self) -> u32 {
            self.cap_handle
        }
    }

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
        assert_eq!(notif.ack(bits), Err(DriverRuntimeError::InvalidAck));
    }

    #[test]
    fn ack_zero_is_noop_even_without_prior_wait() {
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend, &device, None)
            .expect("subscribe");
        assert!(notif.ack(0).is_ok());
    }

    // -- Negative tests ----------------------------------------------------

    #[test]
    fn ack_with_bits_outside_subscription_mask_returns_invalid_ack() {
        // The authoritative C.3 acceptance negative test: `ack` with
        // bits the caller did not observe returns
        // DriverRuntimeError::InvalidAck, not a panic.
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, None)
            .expect("subscribe");
        let result = notif.ack(0b10);
        assert_eq!(result, Err(DriverRuntimeError::InvalidAck));
    }

    #[test]
    fn ack_with_unobserved_bits_returns_invalid_ack() {
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend, &device, None)
            .expect("subscribe");
        assert_eq!(notif.ack(0b1), Err(DriverRuntimeError::InvalidAck));
    }

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

    #[test]
    fn irq_loop_invokes_callback_on_delivery_and_acks() {
        let backend = MockBackend::new();
        let device = MockDevice { cap_handle: 3 };
        let notif = IrqNotification::subscribe_with_backend(backend.clone(), &device, None)
            .expect("subscribe");
        backend.signal(notif.cap_handle(), 0b1);
        backend.signal(notif.cap_handle(), 0b10); // outside mask — ack will error

        let counter = Rc::new(RefCell::new(0_u32));
        let counter_for_closure = counter.clone();
        let err = irq_loop(&notif, move || {
            *counter_for_closure.borrow_mut() += 1;
        })
        .expect_err("irq_loop should surface the ack error");
        assert_eq!(err, DriverRuntimeError::InvalidAck);
        assert_eq!(*counter.borrow(), 2);
    }

    #[test]
    fn from_parts_constructs_a_usable_notification_without_subscribe() {
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
