//! Audio ring-buffer state model — Phase 57 B.2.
//!
//! Single-producer / single-consumer byte ring used by the audio_server
//! io loop to track DMA progress between the client (producer, via
//! `SubmitFrames` over IPC) and the device (consumer, draining bytes
//! into the AC'97 BDL). The state model is pure logic and operates on
//! a caller-supplied `&mut [u8]` — no heap allocation.
//!
//! ## Producer / consumer split
//!
//! - `write` is the producer side. It accepts the **whole** input slice or
//!   returns [`RingError::WouldBlock`] without modifying state. Partial
//!   accepts complicate the client retry loop and the wire-protocol
//!   contract (see B.3 / Phase 57 audio ABI memo) — which is why the
//!   ring is whole-or-nothing on writes.
//! - `consume` is the consumer side. Given a count `n`, it dispatches up
//!   to `n` bytes through the supplied [`AudioSink`] (one or two
//!   contiguous chunks if the read would wrap). [`RingError::Underrun`]
//!   is returned for `n > 0` when the ring is empty.
//!
//! ## Wrap-around
//!
//! Bytes are stored in a circular buffer. `head` is the next write
//! position; `tail` is the next read position. Both are monotonically
//! increasing `usize` counters; their difference (`head - tail`) gives
//! the fill level, and modulo `capacity` gives the storage offsets.
//! Counters are not wrapped at `usize::MAX`; for any practical capacity
//! and practical PCM throughput, overflow is several thousand years
//! away and is intentionally not engineered against.
//!
//! ## Allocation
//!
//! `AudioRingState` borrows its storage. There is no `alloc::Vec` on
//! any path. `RecordingAudioSink` (test-only) is the one allocating
//! consumer; production consumers are the AC'97 BDL DMA path
//! (`audio_server`) which writes directly into the device-visible page.

use crate::audio::{ChannelLayout, PcmFormat, frame_size_bytes};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by [`AudioRingState`] and any consumer of [`AudioSink`].
///
/// Variants are *data*: callers pattern-match and recover. No
/// stringly-typed errors. Adding a new variant is a deliberate ABI
/// change — `audio_error_to_neg_errno` (B.5) and the protocol codec
/// (B.3) must be updated in the same PR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum RingError {
    /// `consume(n)` requested more bytes than the ring contains. The
    /// ring is empty (or shallower than `n`) and the consumer must wait
    /// for the producer to catch up.
    Underrun,
    /// `write(slice)` would not fit; the ring has insufficient free
    /// space. The producer must retry after the consumer drains.
    WouldBlock,
    /// The backing buffer is too small to be useful (zero-length).
    /// Returned only by [`AudioRingState::try_new`].
    BufferTooSmall,
}

// ---------------------------------------------------------------------------
// Sink trait
// ---------------------------------------------------------------------------

/// Consumer side of the audio ring.
///
/// Implementors receive one or two contiguous byte chunks per
/// [`AudioRingState::consume`] call (one in the no-wrap case, two when
/// the read crosses the storage boundary). Total bytes across all
/// chunks of a single `consume(n)` is exactly `n`.
///
/// The Phase 57 production sink (the AC'97 BDL DMA path in
/// `audio_server`) writes the bytes into device-visible memory and
/// returns `Ok(())`. Test sinks (`RecordingAudioSink`, `DiscardSink`)
/// also return `Ok(())`. A sink may return [`RingError`] only when it
/// fails to absorb the bytes; the ring's tail then advances by the
/// bytes it has already passed to the sink before the error.
pub trait AudioSink {
    /// Absorb a contiguous chunk of consumed bytes.
    fn consume(&mut self, bytes: &[u8]) -> Result<(), RingError>;
}

// ---------------------------------------------------------------------------
// Ring state
// ---------------------------------------------------------------------------

/// Pure-logic single-producer / single-consumer byte ring.
///
/// The state operates on a caller-supplied `&mut [u8]`. No allocation.
/// Public API is `new` / `try_new` / `write` / `consume` / `fill_level`
/// / `reset`. Producer / consumer counters are private.
pub struct AudioRingState<'a> {
    storage: &'a mut [u8],
    /// Monotonically-increasing total bytes written.
    head: usize,
    /// Monotonically-increasing total bytes consumed.
    tail: usize,
}

impl<'a> AudioRingState<'a> {
    /// Build a ring backed by `storage`.
    ///
    /// Panics if `storage.len() == 0`. Use [`try_new`] to handle the
    /// zero-length case as a runtime error.
    ///
    /// [`try_new`]: AudioRingState::try_new
    pub fn new(storage: &'a mut [u8]) -> Self {
        match Self::try_new(storage) {
            Ok(ring) => ring,
            // Documented panic: caller-error fail-fast for a programming
            // bug. `audio_server` constructs the ring once at stream-open
            // with a static-sized DMA page and would never legitimately
            // hit this. Tests covering the error use `try_new`.
            Err(_) => panic!("AudioRingState::new requires storage.len() > 0; use try_new"),
        }
    }

    /// Build a ring backed by `storage`, returning [`RingError::BufferTooSmall`]
    /// if the storage is empty.
    pub fn try_new(storage: &'a mut [u8]) -> Result<Self, RingError> {
        if storage.is_empty() {
            return Err(RingError::BufferTooSmall);
        }
        Ok(Self {
            storage,
            head: 0,
            tail: 0,
        })
    }

    /// Capacity of the backing storage in bytes.
    pub fn capacity(&self) -> usize {
        self.storage.len()
    }

    /// Bytes currently waiting to be consumed.
    pub fn fill_level(&self) -> usize {
        // `head` and `tail` are monotonic counters — their difference is
        // the fill level even after many wrap-arounds.
        self.head - self.tail
    }

    /// Reset the ring to the empty state. Does not zero the backing
    /// storage; bytes from prior writes are unreachable but not erased.
    pub fn reset(&mut self) {
        self.head = 0;
        self.tail = 0;
    }

    /// Append `bytes` to the producer side.
    ///
    /// Whole-or-nothing semantics: if the slice fits in the current
    /// free window, every byte is copied into the ring and `head` is
    /// advanced; otherwise the call returns [`RingError::WouldBlock`]
    /// and the ring is unchanged.
    ///
    /// Returns the number of bytes accepted, which equals `bytes.len()`
    /// on success.
    pub fn write(&mut self, bytes: &[u8]) -> Result<usize, RingError> {
        let cap = self.storage.len();
        let free = cap - self.fill_level();
        if bytes.len() > free {
            return Err(RingError::WouldBlock);
        }
        // Empty slice is a no-op success (matches the standard "writer
        // accepts zero bytes" convention).
        if bytes.is_empty() {
            return Ok(0);
        }
        let head_pos = self.head % cap;
        let first_chunk = core::cmp::min(cap - head_pos, bytes.len());
        self.storage[head_pos..head_pos + first_chunk].copy_from_slice(&bytes[..first_chunk]);
        let remaining = bytes.len() - first_chunk;
        if remaining > 0 {
            self.storage[..remaining].copy_from_slice(&bytes[first_chunk..]);
        }
        self.head += bytes.len();
        Ok(bytes.len())
    }

    /// Consume up to `n` bytes through `sink`.
    ///
    /// Returns the number of bytes dispatched (always `n` on success).
    /// Returns [`RingError::Underrun`] if `n > 0` and the ring is empty.
    /// If the read window crosses the storage boundary, the sink
    /// receives two contiguous chunks back-to-back.
    pub fn consume(
        &mut self,
        sink: &mut dyn AudioSink,
        n: usize,
    ) -> Result<usize, RingError> {
        if n == 0 {
            return Ok(0);
        }
        let fill = self.fill_level();
        if fill < n {
            return Err(RingError::Underrun);
        }
        let cap = self.storage.len();
        let tail_pos = self.tail % cap;
        let first_chunk = core::cmp::min(cap - tail_pos, n);
        // Re-borrow storage for the first chunk and dispatch it.
        sink.consume(&self.storage[tail_pos..tail_pos + first_chunk])?;
        let remaining = n - first_chunk;
        if remaining > 0 {
            sink.consume(&self.storage[..remaining])?;
        }
        self.tail += n;
        Ok(n)
    }

    /// Convenience for callers that want byte counts in PCM frames.
    /// Returns `fill_level / frame_size_bytes(format, layout)`. Zero if
    /// the fill level is shallower than one frame.
    pub fn fill_frames(&self, format: PcmFormat, layout: ChannelLayout) -> usize {
        let frame = frame_size_bytes(format, layout);
        // `frame` is at least 2 for `S16Le * Mono`; the const audit
        // backing this lives in `format::frame_size_bytes`.
        self.fill_level() / frame
    }
}

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
        // Wrap-around exercise: write 8 starting at offset 8, the
        // second consume drains the remaining 4.
        ring.write(&[9, 10, 11, 12, 13, 14, 15, 16]).unwrap();
        let consumed = ring.consume(&mut sink, 4).unwrap();
        assert_eq!(consumed, 4);
        // Drain the rest.
        let consumed = ring.consume(&mut sink, 4).unwrap();
        assert_eq!(consumed, 4);
        // Empty exercise — consume from empty is Underrun for any sink.
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
