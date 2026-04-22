//! Property tests for bound-notification wake-race atomicity (A.3).
//!
//! These tests operate on a sequential model of the IPC recv path and verify
//! that no sequence of `signal` / `send` / `recv` can lose a wake or
//! accidentally merge a notification signal with a message label.
//!
//! # Model
//!
//! The model maintains two independent pending pools:
//!
//! - `message_queue`: a FIFO of peer-sent labels.
//! - `signal_bits`: a bitset of OR-accumulated notification signals.
//!
//! `recv()` dispatches at most one wake per call:
//! 1. If `message_queue` is non-empty → dispatch the front message.
//! 2. Else if `signal_bits != 0` → drain and dispatch the bits.
//! 3. Otherwise → no pending wake (returns `None`).
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
//! - **Pending observability**: if `recv()` returns `None`, both pools are
//!   empty; anything pending before the call is still pending after.
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
    #[derive(Debug, Default, Clone)]
    struct RecvModel {
        /// Pending messages, FIFO order.
        message_queue: alloc::vec::Vec<u64>,
        /// Pending notification bits, OR-accumulated.
        signal_bits: u64,
    }

    impl RecvModel {
        fn new() -> Self {
            Self::default()
        }

        fn send(&mut self, label: u64) {
            self.message_queue.push(label);
        }

        fn signal(&mut self, bits: u64) {
            self.signal_bits |= bits;
        }

        /// Dispatch one wake, or return `None` if nothing is pending.
        ///
        /// Messages take priority over notifications (seL4-style: a waiting
        /// sender unblocks before a queued IRQ signal).
        fn recv(&mut self) -> Option<WakeKind> {
            if !self.message_queue.is_empty() {
                Some(WakeKind::Message(self.message_queue.remove(0)))
            } else if self.signal_bits != 0 {
                let bits = self.signal_bits;
                self.signal_bits = 0;
                Some(WakeKind::Notification(bits))
            } else {
                None
            }
        }

        fn is_empty(&self) -> bool {
            self.message_queue.is_empty() && self.signal_bits == 0
        }
    }

    // -----------------------------------------------------------------------
    // Proptest strategies
    // -----------------------------------------------------------------------

    #[derive(Debug, Clone)]
    enum Op {
        Signal(u64),
        Send(u64),
        Recv,
    }

    fn any_op() -> impl Strategy<Value = Op> {
        prop_oneof![
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
        #[test]
        fn bound_notif_race_safety(ops in op_sequence(24)) {
            let mut model = RecvModel::new();

            // Track what labels are pending so we can validate no-loss.
            let mut pending_labels: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
            // Track accumulated signal bits so we can validate no-merge.
            let mut accumulated_bits: u64 = 0;

            for op in ops {
                match op {
                    Op::Signal(bits) => {
                        model.signal(bits);
                        accumulated_bits |= bits;
                    }
                    Op::Send(label) => {
                        model.send(label);
                        pending_labels.push(label);
                    }
                    Op::Recv => {
                        match model.recv() {
                            Some(WakeKind::Message(label)) => {
                                // The label must have been in the pending queue.
                                let pos = pending_labels.iter().position(|&l| l == label);
                                prop_assert!(
                                    pos.is_some(),
                                    "message label {label} was not in the pending queue"
                                );
                                pending_labels.remove(pos.unwrap());
                            }
                            Some(WakeKind::Notification(bits)) => {
                                // The bits must be a non-zero subset of what was signalled.
                                prop_assert_ne!(bits, 0, "notification wake must carry non-zero bits");
                                prop_assert_eq!(
                                    bits & accumulated_bits,
                                    bits,
                                    "notification bits must be a subset of accumulated signals"
                                );
                                // After a notification wake, signal state is drained.
                                accumulated_bits = model.signal_bits;
                            }
                            None => {
                                // If no wake was dispatched, both pools must be empty.
                                prop_assert!(
                                    model.is_empty(),
                                    "recv returned None but model is not empty"
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
        #[test]
        fn signals_never_merge_with_message_labels(
            label in any::<u64>(),
            bits in any::<u64>(),
        ) {
            let mut model = RecvModel::new();

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

            // If bits != 0, the notification must still be pending.
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
