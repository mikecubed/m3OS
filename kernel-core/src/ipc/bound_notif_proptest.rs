//! Property tests for bound-notification wake-race atomicity (A.3).
//!
//! These tests operate on a sequential model of the IPC recv path and verify
//! that no sequence of `bind`, `unbind`, `signal` / `send` / `recv` can lose
//! a wake or accidentally merge a notification signal with a message label.
//!
//! # Model
//!
//! The model maintains two independent pending pools plus a binding flag:
//!
//! - `message_queue`: a FIFO of peer-sent labels.
//! - `signal_bits`: a bitset of OR-accumulated notification signals.
//! - `bound`: whether the receiver is currently bound to a notification object.
//!
//! `signal()` always OR-accumulates into `signal_bits` regardless of binding
//! state (the notification pending word is always updated, mirroring real
//! hardware). `recv()` dispatches at most one wake per call:
//! 1. If `message_queue` is non-empty → dispatch the front message.
//! 2. Else if `bound && signal_bits != 0` → drain and dispatch the bits.
//! 3. Otherwise → no pending wake (returns `None`).
//!
//! Signals accumulated while unbound are preserved in `signal_bits` and become
//! observable after the next `bind()`.
//!
//! # Invariants checked
//!
//! - **No-loss**: every dispatched wake was produced by a prior `send` or
//!   `signal` operation; nothing appears from thin air.
//! - **No-merge**: the label returned on a message wake was never modified by
//!   any `signal` call, and the bits returned on a notification wake carry no
//!   label information.
//! - **Round-trip**: every dispatched wake encodes and decodes without loss
//!   through the [`WakeKind`] ABI.
//! - **Observability**: if `recv()` returns `None`, nothing dispatchable is
//!   pending — i.e. either the message queue is empty and the model is unbound,
//!   or the signal bitset is zero.
//! - **Binding-dependent observability**: `recv()` dispatches notification bits
//!   only while `bound == true`; unbound signals are never dispatched.
//! - **No-lost-wake across next recv**: if `recv()` dispatches a message while
//!   a notification was also pending, the notification must survive and be
//!   returned by the immediately following `recv()` without contamination.
//!
//! Proptest runs at least 1024 cases (configured via `ProptestConfig`).

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use crate::ipc::wake_kind::{WakeKind, decode_wake_kind, encode_wake_kind};

    // -----------------------------------------------------------------------
    // Sequential model
    // -----------------------------------------------------------------------

    /// Simplified sequential model of the bound-notification recv path.
    ///
    /// This is intentionally not lock-free; the correctness of the sequential
    /// interleaving is what we are proving. The kernel's actual implementation
    /// (Track B) must produce the same observable outcomes.
    ///
    /// The `bound` flag models whether the receiver (TCB) is currently bound to
    /// a notification object. Signals always OR-accumulate into `signal_bits`
    /// regardless of binding state, but `recv()` only dispatches them when
    /// `bound == true`. This matches the seL4 semantics where the notification
    /// pending word is always updated but only wakes the bound TCB.
    #[derive(Debug, Clone)]
    struct RecvModel {
        /// Pending messages, FIFO order.
        message_queue: alloc::vec::Vec<u64>,
        /// Pending notification bits, OR-accumulated regardless of bound state.
        signal_bits: u64,
        /// Whether the receiver is currently bound to a notification object.
        bound: bool,
    }

    impl RecvModel {
        fn new() -> Self {
            Self {
                message_queue: alloc::vec::Vec::new(),
                signal_bits: 0,
                bound: false,
            }
        }

        fn bind(&mut self) {
            self.bound = true;
        }

        fn unbind(&mut self) {
            self.bound = false;
        }

        fn send(&mut self, label: u64) {
            self.message_queue.push(label);
        }

        /// OR-accumulate notification bits into the pending word.
        ///
        /// This always updates `signal_bits` regardless of `self.bound`,
        /// because the hardware notification word is always written. The bits
        /// become observable through `recv()` only after `bind()`.
        fn signal(&mut self, bits: u64) {
            self.signal_bits |= bits;
        }

        /// Dispatch one wake, or return `None` if nothing is dispatchable.
        ///
        /// Messages take priority over notifications (seL4-style: a waiting
        /// sender unblocks before a queued IRQ signal).
        /// Notification bits are only dispatched when `self.bound == true`.
        fn recv(&mut self) -> Option<WakeKind> {
            if !self.message_queue.is_empty() {
                Some(WakeKind::Message(self.message_queue.remove(0)))
            } else if self.bound && self.signal_bits != 0 {
                let bits = self.signal_bits;
                self.signal_bits = 0;
                Some(WakeKind::Notification(bits))
            } else {
                None
            }
        }

        /// True when `recv()` has nothing to dispatch.
        ///
        /// Notification bits that exist but are unobservable because `bound ==
        /// false` are NOT considered pending from `recv()`'s perspective.
        fn is_recv_empty(&self) -> bool {
            self.message_queue.is_empty() && (!self.bound || self.signal_bits == 0)
        }
    }

    // -----------------------------------------------------------------------
    // Proptest strategies
    // -----------------------------------------------------------------------

    #[derive(Debug, Clone)]
    enum Op {
        /// Bind the receiver to a notification object: signals become observable.
        Bind,
        /// Unbind the receiver: signals continue accumulating but are not dispatched.
        Unbind,
        Signal(u64),
        Send(u64),
        Recv,
    }

    fn any_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            // Bind and Unbind have equal probability to each other, combined
            // roughly equal to Signal, Send, and Recv so that bind/unbind
            // transitions are well-exercised across the sequence.
            Just(Op::Bind),
            Just(Op::Unbind),
            any::<u64>().prop_map(Op::Signal),
            any::<u64>().prop_map(Op::Send),
            Just(Op::Recv),
        ]
    }

    fn op_sequence(max_len: usize) -> impl Strategy<Value = alloc::vec::Vec<Op>> {
        proptest::collection::vec(any_op(), 0..=max_len)
    }

    // -----------------------------------------------------------------------
    // Property tests
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        /// After every recv(), the returned wake was actually pending, and
        /// nothing that was pending is silently dropped.
        ///
        /// The generator emits arbitrary sequences of `Bind`, `Unbind`,
        /// `Signal`, `Send`, and `Recv` so that binding-state transitions are
        /// continuously exercised. Each generated `Op::Recv` performs exactly
        /// one `model.recv()` call. Additional assertions (A-R2) verify that
        /// when recv() dispatches a message while a notification was also
        /// pending, the notification survives (immediate check) and is still
        /// observable at the very next generated `Op::Recv` (deferred check),
        /// without consuming extra recv() calls inside the first handler.
        #[test]
        fn bound_notif_race_safety(ops in op_sequence(24)) {
            let mut model = RecvModel::new();

            // Track what labels are pending so we can validate no-loss.
            let mut pending_labels: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
            // Track all accumulated signal bits (regardless of bound state) so
            // we can validate no-merge.
            let mut accumulated_bits: u64 = 0;

            // A-R2 deferred observability state:
            // When a message is dispatched while a notification was also pending,
            // signal_bits must survive and be observable at the very next
            // generated Op::Recv.  We carry the expected non-zero signal_bits
            // value here and check it at the start of the next Op::Recv without
            // consuming any extra recv() calls inside the current handler.
            let mut deferred_check: Option<u64> = None;

            for op in ops {
                match op {
                    Op::Bind => {
                        model.bind();
                    }
                    Op::Unbind => {
                        model.unbind();
                    }
                    Op::Signal(bits) => {
                        model.signal(bits);
                        // Track every bit ever signaled; recv() only dispatches
                        // a subset when bound, so this is a superset bound.
                        accumulated_bits |= bits;
                    }
                    Op::Send(label) => {
                        model.send(label);
                        pending_labels.push(label);
                    }
                    Op::Recv => {
                        // A-R2 deferred check: the previous Op::Recv dispatched
                        // a message while a notification was also pending.  Those
                        // signal_bits must still be present now — they can only
                        // grow (from Op::Signal) or stay the same between recvs.
                        // No intermediate Op::Recv ran because take() clears the
                        // flag on the very next Op::Recv.
                        if let Some(saved_bits) = deferred_check.take() {
                            prop_assert_ne!(
                                model.signal_bits, 0,
                                "signal_bits must survive until the next Op::Recv \
                                 (saved={:#x}, current signal_bits={:#x})",
                                saved_bits,
                                model.signal_bits
                            );
                        }

                        // Snapshot observability state before the single dispatch
                        // so we can set the deferred check if needed.
                        let pending_signal_before = model.bound && model.signal_bits != 0;
                        let signal_bits_before = model.signal_bits;

                        // Exactly one model.recv() per Op::Recv.
                        match model.recv() {
                            Some(WakeKind::Message(label)) => {
                                // No-loss: the label must have been in the pending queue.
                                let pos = pending_labels.iter().position(|&l| l == label);
                                prop_assert!(
                                    pos.is_some(),
                                    "message label {:#x} was not in the pending queue",
                                    label
                                );
                                pending_labels.remove(pos.unwrap());

                                // A-R2 — no-lost-wake: if a notification was also pending
                                // when this message was dispatched, signal_bits must still
                                // be non-zero now (immediate check) and must remain
                                // observable at the next generated Op::Recv (deferred check).
                                if pending_signal_before {
                                    prop_assert_ne!(
                                        model.signal_bits, 0,
                                        "signal_bits={:#x} must survive a message dispatch \
                                         (no-lost-wake)",
                                        signal_bits_before
                                    );
                                    // Carry forward: the next Op::Recv must still see
                                    // signal_bits != 0.  Do NOT consume extra recv() calls
                                    // here — deferred state carries the obligation forward.
                                    deferred_check = Some(model.signal_bits);
                                }
                            }
                            Some(WakeKind::Notification(bits)) => {
                                // The bits must be non-zero.
                                prop_assert_ne!(
                                    bits, 0,
                                    "notification wake must carry non-zero bits"
                                );
                                // The bits must be a subset of what was ever accumulated
                                // (signals can only drain what was previously OR'd in).
                                prop_assert_eq!(
                                    bits & accumulated_bits,
                                    bits,
                                    "notification bits {:#x} must be a subset of \
                                     accumulated signals {:#x}",
                                    bits,
                                    accumulated_bits
                                );
                                // Observability invariant: notification dispatch only
                                // occurs when bound.  (By the model's recv() logic this
                                // is always satisfied; the assertion documents the contract.)
                                prop_assert!(
                                    model.bound,
                                    "notification dispatched while unbound (model invariant violated)"
                                );
                                // After a notification wake, signal state is drained.
                                accumulated_bits = model.signal_bits;
                            }
                            None => {
                                // recv() returns None iff nothing is dispatchable:
                                // message queue is empty AND (unbound OR signal_bits == 0).
                                prop_assert!(
                                    model.is_recv_empty(),
                                    "recv returned None but model has something dispatchable \
                                     (bound={}, signal_bits={:#x}, msg_count={})",
                                    model.bound,
                                    model.signal_bits,
                                    model.message_queue.len()
                                );
                            }
                        }
                    }
                }
            }
        }

        /// Signals arriving during a blocked recv are never merged with an
        /// earlier send's label: notification bits and message labels stay
        /// in independent pools and never cross-contaminate.
        ///
        /// The model is explicitly bound before the send/signal so that both
        /// sources are observable, directly exercising the bound path.
        #[test]
        fn signals_never_merge_with_message_labels(
            label in any::<u64>(),
            bits in any::<u64>(),
        ) {
            let mut model = RecvModel::new();

            // Bind first so that both messages and notifications are observable.
            model.bind();

            // Send a message first, then signal.
            model.send(label);
            model.signal(bits);

            // First recv must return the message (messages have priority).
            match model.recv() {
                Some(WakeKind::Message(got_label)) => {
                    prop_assert_eq!(got_label, label, "message label must be unmodified");
                }
                other => prop_assert!(
                    false,
                    "expected Message wake, got {:?}",
                    other
                ),
            }

            // If bits != 0, the notification must still be pending (A-R2 guarantee)
            // and must be returned by the very next recv() without contamination.
            if bits != 0 {
                match model.recv() {
                    Some(WakeKind::Notification(got_bits)) => {
                        prop_assert_eq!(got_bits, bits, "notification bits must be unmodified");
                    }
                    other => prop_assert!(
                        false,
                        "expected Notification wake after message, got {:?}",
                        other
                    ),
                }
            }
        }

        /// Arbitrary `u64` labels round-trip through the WakeKind ABI.
        #[test]
        fn arbitrary_label_round_trips(label in any::<u64>()) {
            let wake = WakeKind::Message(label);
            let (kind, msg) = encode_wake_kind(wake);
            let decoded = decode_wake_kind(kind, msg);
            prop_assert_eq!(decoded, WakeKind::Message(label));
        }

        /// Arbitrary `u64` bit masks round-trip through the WakeKind ABI.
        #[test]
        fn arbitrary_bits_round_trips(bits in any::<u64>()) {
            let wake = WakeKind::Notification(bits);
            let (kind, msg) = encode_wake_kind(wake);
            let decoded = decode_wake_kind(kind, msg);
            prop_assert_eq!(decoded, WakeKind::Notification(bits));
        }

        /// Mixed interleaving: message then notification kind tags differ.
        #[test]
        fn mixed_interleaving_kind_tags_are_distinct(
            label in any::<u64>(),
            bits in any::<u64>(),
        ) {
            let (km, _) = encode_wake_kind(WakeKind::Message(label));
            let (kn, _) = encode_wake_kind(WakeKind::Notification(bits));
            prop_assert_ne!(km, kn, "message and notification kind tags must differ");
        }
    }
}
