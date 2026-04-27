//! Audio ring-buffer state model — Phase 57 B.2.
//!
//! Tests-only commit. The implementation lands in the next commit; these
//! tests are committed first to satisfy the red-before-green TDD rule
//! (see `docs/roadmap/tasks/57-audio-and-local-session-tasks.md`).
//!
//! References: phase-57 audio target memo (B.2 acceptance bullets).

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Recording sink — the test double exercised by the contract suite.
    /// Records every byte its `consume` method receives so the suite can
    /// assert byte-order preservation across producer/consumer interleavings.
    #[derive(Debug, Default)]
    struct RecordingAudioSinkLocal {
        recorded: Vec<u8>,
    }

    impl AudioSink for RecordingAudioSinkLocal {
        fn consume(&mut self, bytes: &[u8]) -> Result<(), RingError> {
            self.recorded.extend_from_slice(bytes);
            Ok(())
        }
    }

    /// Discard sink — second contract impl that drops every consumed byte.
    /// Used by the contract suite to prove `AudioSink` impls are
    /// behaviorally interchangeable for `AudioRingState::consume`.
    #[derive(Debug, Default)]
    struct DiscardSink {
        bytes_seen: usize,
    }

    impl AudioSink for DiscardSink {
        fn consume(&mut self, bytes: &[u8]) -> Result<(), RingError> {
            self.bytes_seen += bytes.len();
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // Required B.2 acceptance unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn write_advances_head() {
        let mut storage = [0u8; 16];
        let mut ring = AudioRingState::new(&mut storage);
        let written = ring.write(&[1, 2, 3, 4]).expect("write should accept bytes");
        assert_eq!(written, 4);
        assert_eq!(ring.fill_level(), 4);
    }

    #[test]
    fn consume_advances_tail() {
        let mut storage = [0u8; 16];
        let mut ring = AudioRingState::new(&mut storage);
        ring.write(&[10, 20, 30, 40]).unwrap();
        let mut sink = RecordingAudioSinkLocal::default();
        let consumed = ring.consume(&mut sink, 3).expect("consume should succeed");
        assert_eq!(consumed, 3);
        assert_eq!(sink.recorded, vec![10, 20, 30]);
        assert_eq!(ring.fill_level(), 1);
    }

    #[test]
    fn write_into_full_returns_wouldblock() {
        let mut storage = [0u8; 4];
        let mut ring = AudioRingState::new(&mut storage);
        let n = ring.write(&[1, 2, 3, 4]).unwrap();
        assert_eq!(n, 4);
        // The ring is now exactly full — a further write must report
        // WouldBlock; partial writes that fit are accepted up to the
        // remaining capacity.
        let result = ring.write(&[99]);
        assert_eq!(result, Err(RingError::WouldBlock));
    }

    #[test]
    fn consume_from_empty_returns_underrun() {
        let mut storage = [0u8; 8];
        let mut ring = AudioRingState::new(&mut storage);
        let mut sink = RecordingAudioSinkLocal::default();
        let result = ring.consume(&mut sink, 1);
        assert_eq!(result, Err(RingError::Underrun));
    }

    #[test]
    fn wrap_around_preserves_byte_order() {
        // Capacity 6: write 4, consume 4, write 6, consume 6 — the second
        // write is forced to wrap around the storage. The recording sink
        // observes bytes in the exact order written.
        let mut storage = [0u8; 6];
        let mut ring = AudioRingState::new(&mut storage);
        ring.write(&[1, 2, 3, 4]).unwrap();
        let mut sink = RecordingAudioSinkLocal::default();
        ring.consume(&mut sink, 4).unwrap();
        // Now head == tail == 4, capacity 6: a 6-byte write wraps.
        ring.write(&[5, 6, 7, 8, 9, 10]).unwrap();
        ring.consume(&mut sink, 6).unwrap();
        assert_eq!(sink.recorded, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn fill_level_is_consistent_with_head_tail() {
        let mut storage = [0u8; 8];
        let mut ring = AudioRingState::new(&mut storage);
        assert_eq!(ring.fill_level(), 0);
        ring.write(&[1, 2, 3]).unwrap();
        assert_eq!(ring.fill_level(), 3);
        let mut sink = RecordingAudioSinkLocal::default();
        ring.consume(&mut sink, 2).unwrap();
        assert_eq!(ring.fill_level(), 1);
        ring.write(&[10, 20, 30, 40, 50]).unwrap();
        assert_eq!(ring.fill_level(), 6);
    }

    // -----------------------------------------------------------------------
    // Additional sanity tests (still part of the same red commit)
    // -----------------------------------------------------------------------

    #[test]
    fn reset_clears_state() {
        let mut storage = [0u8; 8];
        let mut ring = AudioRingState::new(&mut storage);
        ring.write(&[1, 2, 3, 4]).unwrap();
        ring.reset();
        assert_eq!(ring.fill_level(), 0);
        let mut sink = RecordingAudioSinkLocal::default();
        // After reset the ring is empty — consuming any byte underruns.
        let result = ring.consume(&mut sink, 1);
        assert_eq!(result, Err(RingError::Underrun));
    }

    #[test]
    fn write_with_partial_room_is_rejected_as_wouldblock() {
        // The Phase 57 contract is "either accept the whole slice or
        // return WouldBlock"; partial accepts complicate the client retry
        // loop. A 4-byte slice into a ring with 2 bytes free returns
        // WouldBlock and leaves the ring unchanged.
        let mut storage = [0u8; 4];
        let mut ring = AudioRingState::new(&mut storage);
        ring.write(&[1, 2]).unwrap();
        let result = ring.write(&[10, 20, 30, 40]);
        assert_eq!(result, Err(RingError::WouldBlock));
        assert_eq!(ring.fill_level(), 2);
    }

    #[test]
    fn buffer_too_small_for_zero_capacity() {
        // A zero-length backing buffer is rejected at construction time —
        // callers cannot accidentally produce a zero-capacity ring.
        let result = AudioRingState::try_new(&mut [] as &mut [u8]);
        assert_eq!(result.err(), Some(RingError::BufferTooSmall));
    }

    // -----------------------------------------------------------------------
    // Contract test (B.2 acceptance: ≥1 contract test)
    // -----------------------------------------------------------------------

    /// Behavioral spec the `AudioSink` trait obeys: every byte written to
    /// the ring is delivered to the sink in producer order, and the
    /// `bytes_consumed` count matches `n` for any successful consume.
    /// Both impls (`RecordingAudioSinkLocal`, `DiscardSink`) must satisfy it.
    fn run_contract_suite<S: AudioSink + Default>() {
        let mut storage = [0u8; 16];
        let mut ring = AudioRingState::new(&mut storage);
        let mut sink = S::default();
        // Standard exercise: write 8, consume 8, no errors.
        ring.write(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let consumed = ring.consume(&mut sink, 8).unwrap();
        assert_eq!(consumed, 8);
        // Wrap-around exercise.
        ring.write(&[9, 10, 11, 12, 13, 14, 15, 16]).unwrap();
        let consumed = ring.consume(&mut sink, 4).unwrap();
        assert_eq!(consumed, 4);
        // Empty exercise — consume from empty is Underrun for any sink.
        ring.consume(&mut sink, 8).unwrap();
        let result = ring.consume(&mut sink, 1);
        assert_eq!(result, Err(RingError::Underrun));
    }

    #[test]
    fn audio_sink_contract_recording_double() {
        run_contract_suite::<RecordingAudioSinkLocal>();
    }

    #[test]
    fn audio_sink_contract_discard_sink() {
        run_contract_suite::<DiscardSink>();
    }

    #[test]
    fn ring_error_variants_are_distinct() {
        assert_ne!(RingError::Underrun, RingError::WouldBlock);
        assert_ne!(RingError::Underrun, RingError::BufferTooSmall);
        assert_ne!(RingError::WouldBlock, RingError::BufferTooSmall);
    }
}
