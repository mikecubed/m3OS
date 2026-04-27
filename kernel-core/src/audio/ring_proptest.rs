//! Property tests for [`AudioRingState`] — Phase 57 B.4.
//!
//! Given an arbitrary sequence of `write(bytes)`, `consume(n)`, and
//! `reset()` operations, the ring must satisfy:
//!
//! 1. `fill_level() == head - tail` (modular interpretation of two
//!    monotonic counters), always.
//! 2. `fill_level() <= capacity()`, always.
//! 3. `fill_level() >= 0` — represented in code by `usize` so this is
//!    enforced by the type system; the property tests document the
//!    intent.
//! 4. No sequence panics or accesses memory out of bounds (the model
//!    does not exercise `unsafe`; bounds-check failures would be
//!    runtime panics that proptest would catch).
//!
//! The harness also runs an indirect oracle for byte-order: the
//! recording sink's accumulated buffer must match the reference
//! producer's emit sequence exactly (modulo bytes that fell on the
//! wrong side of a `reset()`).

#[cfg(test)]
mod tests {
    use super::super::ring::{AudioRingState, AudioSink, RingError};
    use alloc::vec::Vec;
    use proptest::prelude::*;

    /// Reference recording sink: every consumed byte ends up here in
    /// the order the ring delivered it.
    #[derive(Default)]
    struct Recorder {
        bytes: Vec<u8>,
    }

    impl AudioSink for Recorder {
        fn consume(&mut self, bytes: &[u8]) -> Result<(), RingError> {
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
    }

    /// One operation in the random sequence.
    #[derive(Debug, Clone)]
    enum Op {
        Write(Vec<u8>),
        Consume(usize),
        Reset,
    }

    fn any_op(max_write: usize) -> impl Strategy<Value = Op> {
        prop_oneof![
            proptest::collection::vec(any::<u8>(), 0..=max_write).prop_map(Op::Write),
            (0usize..=max_write).prop_map(Op::Consume),
            Just(Op::Reset),
        ]
    }

    fn op_sequence(max_ops: usize, max_write: usize) -> impl Strategy<Value = Vec<Op>> {
        proptest::collection::vec(any_op(max_write), 0..=max_ops)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        /// The ring's reported fill level always equals the difference
        /// between the cumulative bytes written and consumed (modulo
        /// `reset` calls), and never exceeds capacity.
        #[test]
        fn ring_proptest_invariants(ops in op_sequence(48, 32), capacity in 1usize..=32) {
            let mut storage = alloc::vec![0u8; capacity];
            let mut ring = AudioRingState::new(&mut storage);
            let mut recorder = Recorder::default();

            // Reference shadow: bytes the ring should still hold (in
            // order) before each consume. We model this as a deque
            // implemented over a Vec, since alloc::collections is not
            // imported in this crate's test harness.
            let mut shadow: Vec<u8> = Vec::new();

            // The Recorder accumulates every byte that `consume`
            // delivered. We compare it against the bytes we expected
            // to flow out of the shadow on each consume.
            let mut expected_consumed: Vec<u8> = Vec::new();

            for op in ops {
                match op {
                    Op::Write(bytes) => {
                        let result = ring.write(&bytes);
                        match result {
                            Ok(n) => {
                                prop_assert_eq!(n, bytes.len());
                                shadow.extend_from_slice(&bytes);
                            }
                            Err(RingError::WouldBlock) => {
                                // Whole-or-nothing: ring state must be
                                // unchanged. The fill_level matches
                                // shadow.len() invariant catches any
                                // partial write.
                            }
                            Err(other) => {
                                prop_assert!(
                                    false,
                                    "unexpected ring write error: {:?}",
                                    other
                                );
                            }
                        }
                    }
                    Op::Consume(n) => {
                        let result = ring.consume(&mut recorder, n);
                        match result {
                            Ok(consumed) => {
                                prop_assert_eq!(consumed, n);
                                let drained: Vec<u8> = shadow.drain(..n).collect();
                                expected_consumed.extend(drained);
                            }
                            Err(RingError::Underrun) => {
                                // Ring had less than `n`; state must be
                                // unchanged on the consumer side.
                                prop_assert!(shadow.len() < n || n == 0);
                            }
                            Err(other) => {
                                prop_assert!(
                                    false,
                                    "unexpected ring consume error: {:?}",
                                    other
                                );
                            }
                        }
                    }
                    Op::Reset => {
                        ring.reset();
                        // After reset, any bytes still in the shadow
                        // are unreachable; clear them. The Recorder is
                        // not cleared because it holds bytes that were
                        // legitimately consumed before the reset.
                        shadow.clear();
                    }
                }

                // ----- Invariants after every op -----
                let fill = ring.fill_level();
                let cap = ring.capacity();
                prop_assert!(
                    fill <= cap,
                    "fill_level ({}) > capacity ({})",
                    fill,
                    cap
                );
                prop_assert_eq!(
                    fill,
                    shadow.len(),
                    "fill_level ({}) != shadow.len() ({})",
                    fill,
                    shadow.len()
                );
            }

            // ----- Final byte-order invariant -----
            // The recorder's accumulated bytes must match the reference
            // producer's emit-sequence (modulo bytes lost to reset).
            prop_assert_eq!(
                recorder.bytes.len(),
                expected_consumed.len(),
                "recorded {} bytes, expected {}",
                recorder.bytes.len(),
                expected_consumed.len()
            );
            prop_assert_eq!(
                &recorder.bytes,
                &expected_consumed,
                "byte-order mismatch between ring's consume and shadow"
            );
        }

        /// Zero-byte writes never advance head, never error, and never
        /// change fill level.
        #[test]
        fn zero_byte_write_is_noop(capacity in 1usize..=32) {
            let mut storage = alloc::vec![0u8; capacity];
            let mut ring = AudioRingState::new(&mut storage);
            let before = ring.fill_level();
            let result = ring.write(&[]);
            prop_assert!(matches!(result, Ok(0)));
            prop_assert_eq!(ring.fill_level(), before);
        }

        /// Zero-byte consume succeeds even when the ring is empty,
        /// because `Underrun` is only meaningful for `n > 0`.
        #[test]
        fn zero_byte_consume_on_empty_is_ok(capacity in 1usize..=32) {
            let mut storage = alloc::vec![0u8; capacity];
            let mut ring = AudioRingState::new(&mut storage);
            let mut sink = Recorder::default();
            let result = ring.consume(&mut sink, 0);
            prop_assert!(matches!(result, Ok(0)));
        }
    }
}
