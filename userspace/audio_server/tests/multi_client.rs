//! Phase 57 Track H.4 — multi-client audio policy integration test.
//!
//! Acceptance:
//!
//! - Two `audio_client`-equivalent connections attempt admission. The
//!   first is admitted; the second receives `AudioError::Busy` (which
//!   the io loop encodes as `ServerMessage::OpenError(Busy)` →
//!   `-EBUSY` to the client) and is recorded by the rate-limited
//!   rejection log path.
//! - The first client's `frames_consumed` continues to advance across
//!   the second-client attempt — the in-flight stream is not disturbed
//!   by a rejection.
//! - Test runs under `cargo xtask test` (the standard test harness).
//!
//! ## Choice of test surface
//!
//! This test is implemented as a **host integration test** against the
//! `audio_server` library targets — `ClientRegistry`, `StreamRegistry`,
//! and `AudioBackend`. The cross-process integration (two real client
//! processes connecting to a live `audio_server` over the kernel IPC
//! transport) is harder to wire up: it would require a forked smoke
//! harness similar to `display-server-crash-smoke` plus a second
//! `audio-demo`-equivalent binary. The host integration approach
//! covers the same logical surface — admit / reject / first-client-
//! continues — by composing the same registries the production io
//! loop wires together in `userspace/audio_server/src/irq.rs`.
//!
//! The test runs on `x86_64-unknown-linux-gnu` via `cargo test
//! -p audio_server --target x86_64-unknown-linux-gnu`, which `cargo
//! xtask check`'s pipeline already invokes through the lib target.

extern crate alloc;

use alloc::vec::Vec;
use core::cell::RefCell;

use audio_server::client::{ClientRegistry, ClientState};
use audio_server::device::{Ac97Logic, AudioBackend, IrqEvent};
use audio_server::stream::StreamRegistry;
use kernel_core::audio::{AudioError, ChannelLayout, PcmFormat, SampleRate};

// ---------------------------------------------------------------------------
// FakeBackend — minimal AudioBackend used to drive the registries through
// the same surface the production io loop consumes. Mirrors the FakeBackend
// in `userspace/audio_server/src/stream.rs::tests` but pared down for the
// multi-client scenario.
// ---------------------------------------------------------------------------

struct FakeBackend {
    logic: RefCell<Ac97Logic>,
    next_id: RefCell<u32>,
    submit_calls: RefCell<Vec<(u32, usize)>>,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            logic: RefCell::new(Ac97Logic::new()),
            next_id: RefCell::new(1),
            submit_calls: RefCell::new(Vec::new()),
        }
    }
}

impl AudioBackend for FakeBackend {
    fn init(&mut self) -> Result<(), AudioError> {
        Ok(())
    }
    fn open_stream(
        &mut self,
        _format: PcmFormat,
        _layout: ChannelLayout,
        _rate: SampleRate,
    ) -> Result<u32, AudioError> {
        let id = *self.next_id.borrow();
        *self.next_id.borrow_mut() += 1;
        Ok(id)
    }
    fn submit_frames(&mut self, stream_id: u32, bytes: &[u8]) -> Result<usize, AudioError> {
        self.submit_calls
            .borrow_mut()
            .push((stream_id, bytes.len()));
        // Drive the BDL ring math too so a regression in
        // `Ac97Logic::submit_buffer` surfaces here.
        self.logic
            .borrow_mut()
            .submit_buffer(0x1000, 0xCAFE_F00D, bytes.len() / 2)?;
        Ok(bytes.len())
    }
    fn drain(&mut self, _stream_id: u32) -> Result<(), AudioError> {
        Ok(())
    }
    fn close_stream(&mut self, _stream_id: u32) -> Result<(), AudioError> {
        Ok(())
    }
    fn handle_irq(&mut self) -> Result<IrqEvent, AudioError> {
        Ok(IrqEvent::None)
    }
}

// ---------------------------------------------------------------------------
// Acceptance — second_client_returns_ebusy
// ---------------------------------------------------------------------------

/// First client admit succeeds, opens a stream, submits frames; the
/// second client's admit attempt is rejected with `AudioError::Busy`
/// (mapped to `-EBUSY` at the wire). The first client's stats continue
/// advancing across the rejection.
///
/// Mirrors what the production io loop does in `irq.rs::run_io_loop`:
/// `ClientRegistry::try_admit` gates entry to the dispatch path; when
/// it returns false, the loop encodes `ServerMessage::OpenError(Busy)`
/// and replies without touching `StreamRegistry`. This test pins that
/// the in-flight stream is undisturbed by the rejection — `frames_consumed`
/// advances on every IRQ-driven `record_consumed` regardless of whether
/// rejected admits are happening alongside.
#[test]
fn second_client_returns_ebusy() {
    let mut clients = ClientRegistry::new();
    let mut streams = StreamRegistry::new();
    let mut backend = FakeBackend::new();

    // First client admits, opens, submits.
    const CLIENT_A: u32 = 0xA;
    const CLIENT_B: u32 = 0xB;

    assert!(
        clients.try_admit(CLIENT_A),
        "first admit must succeed on idle slot"
    );
    assert_eq!(
        clients.state(),
        ClientState::Owned {
            client_id: CLIENT_A
        }
    );

    let stream_id = streams
        .try_open(
            &mut backend,
            PcmFormat::S16Le,
            ChannelLayout::Stereo,
            SampleRate::Hz48000,
        )
        .expect("first client open");

    let n = streams
        .submit(&mut backend, stream_id, &[0u8; 1024])
        .expect("first client submit");
    assert_eq!(n, 1024);

    // Simulate hardware progress: the IRQ path calls `record_consumed`
    // on every BCIS wake; here we drive it directly to mirror the
    // io-loop sequence.  `frames_consumed` advances by 256 frames
    // (one buffer's worth at the test parameters).
    streams.record_consumed(256);
    let stats_before = streams.stats();
    assert_eq!(stats_before.frames_submitted, 1024);
    assert_eq!(stats_before.frames_consumed, 256);

    // Second client attempts admit — must be rejected.
    assert!(
        !clients.try_admit(CLIENT_B),
        "second admit with a different id must be rejected"
    );
    // Slot still owned by the first client.
    assert_eq!(
        clients.state(),
        ClientState::Owned {
            client_id: CLIENT_A
        }
    );
    // Rejection logged once via the rate-limited path.
    assert_eq!(
        clients.rejects_since_last_log(),
        1,
        "rejection counter must advance per failed admit"
    );
    assert!(
        clients.should_log_reject(0),
        "first rejection must always be loggable"
    );

    // First client continues advancing across the second-client attempt.
    let m = streams
        .submit(&mut backend, stream_id, &[0u8; 512])
        .expect("first client second submit must still succeed after B's rejection");
    assert_eq!(m, 512);
    streams.record_consumed(128);

    let stats_after = streams.stats();
    assert_eq!(
        stats_after.frames_submitted,
        1024 + 512,
        "first client's frames_submitted must advance across the rejection"
    );
    assert!(
        stats_after.frames_consumed > stats_before.frames_consumed,
        "first client's frames_consumed must continue advancing across the rejection"
    );

    // Cleanup — close the first client's stream and release the slot.
    streams
        .close(&mut backend, stream_id)
        .expect("first client close");
    clients.release(CLIENT_A);
    assert_eq!(clients.state(), ClientState::Idle);

    // After release, the next admit can succeed (single-client slot
    // re-arms after a clean close).
    assert!(
        clients.try_admit(CLIENT_B),
        "after first-client release, the next admit must succeed"
    );
}
