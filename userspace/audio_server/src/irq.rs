//! IRQ multiplex via Phase 55c bound notifications — Phase 57 Track D.4.
//!
//! Mirrors `userspace/drivers/e1000/src/io.rs::run_io_loop`: subscribe
//! to the audio IRQ, bind the notification into the command-endpoint
//! `recv` loop, and dispatch through `RecvResult` arms. Track D.1
//! lands the API shell + a pure-logic dispatch helper that decodes
//! a `RecvResult` into a typed action; the real IRQ path lands in
//! Tracks D.4 (subscribe + bind) and D.5 (single-client policy).

#![allow(dead_code)] // D.4/D.5 consume every symbol; see module docs.

use kernel_core::audio::{AudioError, ClientMessage, ProtocolError};

#[cfg(not(test))]
use crate::client::ClientRegistry;
use crate::device::AudioBackend;
use crate::stream::StreamRegistry;

#[cfg(not(test))]
use driver_runtime::{
    DeviceCapHandle, DeviceHandle, DriverRuntimeError, IrqNotification, SyscallBackend,
    ipc::EndpointCap,
};

// ---------------------------------------------------------------------------
// IoAction — pure decoded outcome of a single recv arm
// ---------------------------------------------------------------------------

/// Decoded outcome of a single `recv_multi` arm. The io loop turns
/// the raw `RecvResult` into one of these so the dispatch logic is
/// testable on the host without a real kernel endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IoAction {
    /// Notification wake — call `backend.handle_irq()` and ack the bits.
    HandleIrq { bits: u64 },
    /// Decoded protocol message — dispatch via the codec.
    HandleMessage { msg: ClientMessage },
    /// Decode error — log and reply with `OpenError`/`SubmitError`.
    DecodeError { err: ProtocolError },
}

/// Translate a raw `bulk` payload into an [`IoAction::HandleMessage`]
/// or `IoAction::DecodeError`. Pure logic, exercised on the host.
pub fn decode_message(bulk: &[u8]) -> IoAction {
    match ClientMessage::decode(bulk) {
        Ok((msg, _consumed)) => IoAction::HandleMessage { msg },
        Err(err) => IoAction::DecodeError { err },
    }
}

// ---------------------------------------------------------------------------
// dispatch_message — pure logic that routes a decoded message into the
// stream + client registries.
// ---------------------------------------------------------------------------

/// Possible outcomes from dispatching a single decoded `ClientMessage`.
///
/// The variants name the wire-level reply the io loop should encode
/// back to the client. `Closed` carries no reply because the protocol
/// `Close` reply is `ServerMessage::Closed`, not a return value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    Opened { stream_id: u32 },
    OpenError(AudioError),
    SubmitAck { frames_consumed: u64 },
    SubmitError(AudioError),
    DrainAck,
    DrainError(AudioError),
    Closed,
    CloseError(AudioError),
    StatsRequested,
    InvalidArgument,
}

/// Dispatch a decoded `ClientMessage` against a stream registry +
/// audio backend.
///
/// The io loop calls this for every `IoAction::HandleMessage`. The
/// function takes a `&mut dyn AudioBackend` so a pure-logic test
/// double can drive the same dispatch path.
pub fn dispatch_message(
    msg: &ClientMessage,
    streams: &mut StreamRegistry,
    backend: &mut dyn AudioBackend,
) -> DispatchOutcome {
    match msg {
        ClientMessage::Open {
            format,
            layout,
            rate,
        } => match streams.try_open(backend, *format, *layout, *rate) {
            Ok(id) => DispatchOutcome::Opened { stream_id: id },
            Err(e) => DispatchOutcome::OpenError(e),
        },
        ClientMessage::SubmitFrames { len } => {
            // The bulk payload (the actual PCM bytes) rides the same
            // socket immediately after the encoded frame — the io
            // loop is responsible for reading those bytes and
            // calling `streams.submit`. Here we only acknowledge the
            // `len` field's validity; len > MAX is a decoder error
            // and never reaches dispatch. Phase 57 D.1 returns the
            // latest `frames_consumed` value from the stream stats so
            // the protocol shape compiles; D.3 wires the bulk-read
            // path through this arm.
            if streams.open.is_none() {
                return DispatchOutcome::SubmitError(AudioError::InvalidArgument);
            }
            let _ = len;
            let stats = streams.stats();
            DispatchOutcome::SubmitAck {
                frames_consumed: stats.frames_consumed,
            }
        }
        ClientMessage::Drain => {
            let stream_id = match streams.open.as_ref() {
                Some(s) => s.stream_id,
                None => return DispatchOutcome::DrainError(AudioError::InvalidArgument),
            };
            match streams.drain(backend, stream_id) {
                Ok(()) => DispatchOutcome::DrainAck,
                Err(e) => DispatchOutcome::DrainError(e),
            }
        }
        ClientMessage::Close => {
            let stream_id = match streams.open.as_ref() {
                Some(s) => s.stream_id,
                None => return DispatchOutcome::CloseError(AudioError::InvalidArgument),
            };
            match streams.close(backend, stream_id) {
                Ok(()) => DispatchOutcome::Closed,
                Err(e) => DispatchOutcome::CloseError(e),
            }
        }
        ClientMessage::ControlCommand(_) => DispatchOutcome::StatsRequested,
        // `ClientMessage` is `#[non_exhaustive]`. Future variants
        // surface as `InvalidArgument` so the io loop never panics
        // on a forward-compat protocol revision; the protocol
        // version check (Phase 57 ABI memo) gates the new opcode at
        // the codec layer.
        _ => DispatchOutcome::InvalidArgument,
    }
}

// ---------------------------------------------------------------------------
// subscribe_and_bind / run_io_loop — production entry points (Track D.4)
// ---------------------------------------------------------------------------

#[cfg(not(test))]
pub struct DeviceCapView<'a> {
    inner: &'a DeviceHandle,
}

#[cfg(not(test))]
impl<'a> DeviceCapView<'a> {
    pub fn new(inner: &'a DeviceHandle) -> Self {
        Self { inner }
    }
}

#[cfg(not(test))]
impl DeviceCapHandle for DeviceCapView<'_> {
    fn cap_handle(&self) -> u32 {
        self.inner.cap()
    }
}

/// Subscribe to the AC'97 IRQ, bind the resulting [`IrqNotification`]
/// into `endpoint`'s recv loop, and return the notification cap so the
/// io loop can ack each wake.
///
/// Mirrors `e1000_driver::io::subscribe_and_bind`: subscribe → bind →
/// (no separate arm step — AC'97 IRQ-cause arming is done at
/// per-stream open time via `nabm::CR`).
#[cfg(not(test))]
pub fn subscribe_and_bind(
    device: &DeviceHandle,
    endpoint: EndpointCap,
) -> Result<IrqNotification<SyscallBackend>, DriverRuntimeError> {
    let view = DeviceCapView::new(device);
    let notif = IrqNotification::<SyscallBackend>::subscribe(&view, None)?;
    notif.bind_to_endpoint(endpoint)?;
    Ok(notif)
}

/// Main driver loop: blocks on `recv_multi`, fans out IRQ wakes to the
/// backend, and dispatches client messages through the registry.
///
/// Acceptance: `grep "irq.wait" userspace/audio_server/src/` returns
/// no hits — every block lives behind `endpoint.recv_multi(&irq)`.
#[cfg(not(test))]
pub fn run_io_loop(
    backend: &mut Ac97BackendDyn,
    streams: &mut StreamRegistry,
    clients: &mut ClientRegistry,
    endpoint: EndpointCap,
    irq: IrqNotification<SyscallBackend>,
) -> i32 {
    use driver_runtime::ipc::{IpcBackend, RecvResult};
    use kernel_core::audio::ServerMessage;
    use syscall_lib::STDOUT_FILENO;

    let mut transport = driver_runtime::ipc::SyscallBackend;
    loop {
        let result = match transport.recv(endpoint) {
            Ok(r) => r,
            Err(_) => {
                syscall_lib::write_str(STDOUT_FILENO, "audio_server: recv failed\n");
                return 8;
            }
        };
        match result {
            RecvResult::Notification(bits) => {
                let _ = backend.handle_irq();
                let _ = irq.ack(bits);
            }
            RecvResult::Message(frame) => {
                // First-message admit: the connecting client must be
                // admitted into the single-client slot. Phase 57 D.5
                // identifies clients by the message label (kernel-
                // staged sender id); the rate-limited rejection log
                // lives in `ClientRegistry::reject`.
                let client_id = frame.label as u32;
                if !clients.try_admit(client_id) {
                    let mut buf = [0u8; 16];
                    let reply = ServerMessage::OpenError(AudioError::Busy);
                    if let Ok(n) = reply.encode(&mut buf) {
                        let _ = transport.store_reply_bulk(&buf[..n]);
                    }
                    let _ = transport.reply(frame.label, 0);
                    continue;
                }
                let action = decode_message(&frame.bulk);
                let outcome = match action {
                    IoAction::HandleMessage { msg } => dispatch_message(&msg, streams, backend),
                    IoAction::DecodeError { .. } => {
                        DispatchOutcome::OpenError(AudioError::InvalidArgument)
                    }
                    IoAction::HandleIrq { .. } => {
                        // Cannot happen on a Message arm — fall through.
                        continue;
                    }
                };
                let server_msg = encode_outcome(&outcome, streams);
                let mut buf = [0u8; 64];
                if let Ok(n) = server_msg.encode(&mut buf) {
                    let _ = transport.store_reply_bulk(&buf[..n]);
                }
                let _ = transport.reply(frame.label, 0);
                if matches!(outcome, DispatchOutcome::Closed) {
                    clients.release(client_id);
                }
            }
        }
    }
}

/// Convenience type alias — the io loop accepts any `AudioBackend`
/// trait object. Production wiring passes `&mut Ac97Backend`.
#[cfg(not(test))]
type Ac97BackendDyn = dyn AudioBackend;

/// Convert a [`DispatchOutcome`] into a `ServerMessage` reply.
///
/// Pure logic, exercised by the host tests in this module.
pub fn encode_outcome(
    outcome: &DispatchOutcome,
    streams: &StreamRegistry,
) -> kernel_core::audio::ServerMessage {
    use kernel_core::audio::{AudioControlEvent, ServerMessage};
    match outcome {
        DispatchOutcome::Opened { stream_id } => ServerMessage::Opened {
            stream_id: *stream_id,
        },
        DispatchOutcome::OpenError(e) => ServerMessage::OpenError(*e),
        DispatchOutcome::SubmitAck { frames_consumed } => ServerMessage::SubmitAck {
            frames_consumed: *frames_consumed,
        },
        DispatchOutcome::SubmitError(e) => ServerMessage::SubmitError(*e),
        DispatchOutcome::DrainAck => ServerMessage::DrainAck,
        DispatchOutcome::DrainError(e) => ServerMessage::SubmitError(*e),
        DispatchOutcome::Closed => ServerMessage::Closed,
        DispatchOutcome::CloseError(e) => ServerMessage::OpenError(*e),
        DispatchOutcome::StatsRequested => {
            let stats = streams.stats();
            ServerMessage::ControlEvent(AudioControlEvent::Stats {
                underrun_count: stats.underrun_count,
                frames_submitted: stats.frames_submitted,
                frames_consumed: stats.frames_consumed,
            })
        }
        DispatchOutcome::InvalidArgument => ServerMessage::SubmitError(AudioError::InvalidArgument),
    }
}

// ---------------------------------------------------------------------------
// dispatch_irq — pure logic that translates an IRQ outcome into the
// per-stream registry update.  Exercised by host tests in this module
// and consumed by the production io loop.
// ---------------------------------------------------------------------------

/// Update the stream registry for one IRQ event. Pure logic — the
/// caller still owns the [`AudioBackend`] handle and writes the SR
/// ack via MMIO.
///
/// The fan-out is intentionally narrow: `Underrun` is the only event
/// that bumps a stats counter at this layer.  `Empty` and
/// `LastValidIndex` are handled by the io loop reposting BDL
/// buffers (see `Ac97Logic::observe_irq` for the byte-level state
/// machine); `FifoError` is logged but not double-counted as an
/// underrun.
pub fn apply_irq_event(event: crate::device::IrqEvent, streams: &mut StreamRegistry) {
    use crate::device::IrqEvent;
    match event {
        IrqEvent::Empty => {
            // BCIS — the consumed counter advanced.  The io loop reads
            // the backend's stats snapshot and calls `record_consumed`
            // separately.
        }
        IrqEvent::LastValidIndex => {
            // LVBCI — BDL hit LVI.  The io loop reposts buffers; no
            // stats update at this layer.
        }
        IrqEvent::Underrun => {
            streams.record_underrun();
        }
        IrqEvent::FifoError => {
            // Programming bug.  The io loop logs and surfaces
            // `AudioError::Internal` to the open client.
        }
        IrqEvent::None => {}
    }
}

// ---------------------------------------------------------------------------
// Tests — D.4 host coverage
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Ac97Logic, AudioBackend, IrqEvent};
    use crate::stream::StreamRegistry;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use kernel_core::audio::{
        AudioControlCommand, ChannelLayout, ClientMessage, PcmFormat, SampleRate, ServerMessage,
    };

    // Reuse the FakeBackend shape from `stream.rs` tests; copied
    // locally so the test module owns the mock and the file builds
    // independently.
    struct FakeBackend {
        logic: RefCell<Ac97Logic>,
        next_id: RefCell<u32>,
        irq_events: RefCell<Vec<IrqEvent>>,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                logic: RefCell::new(Ac97Logic::new()),
                next_id: RefCell::new(7),
                irq_events: RefCell::new(Vec::new()),
            }
        }
        fn queue_irq(&self, event: IrqEvent) {
            self.irq_events.borrow_mut().push(event);
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
        fn submit_frames(&mut self, _id: u32, bytes: &[u8]) -> Result<usize, AudioError> {
            self.logic
                .borrow_mut()
                .submit_buffer(0, 0xCAFE_F00D, bytes.len() / 2)?;
            Ok(bytes.len())
        }
        fn drain(&mut self, _id: u32) -> Result<(), AudioError> {
            Ok(())
        }
        fn close_stream(&mut self, _id: u32) -> Result<(), AudioError> {
            Ok(())
        }
        fn handle_irq(&mut self) -> Result<IrqEvent, AudioError> {
            Ok(self.irq_events.borrow_mut().pop().unwrap_or(IrqEvent::None))
        }
    }

    fn open_stereo(reg: &mut StreamRegistry, b: &mut FakeBackend) -> u32 {
        reg.try_open(
            b,
            PcmFormat::S16Le,
            ChannelLayout::Stereo,
            SampleRate::Hz48000,
        )
        .expect("open")
    }

    // -- decode_message ---------------------------------------------------

    #[test]
    fn decode_message_returns_handle_message_on_valid_frame() {
        let msg = ClientMessage::Drain;
        let mut buf = [0u8; 32];
        let n = msg.encode(&mut buf).expect("encode");
        let action = decode_message(&buf[..n]);
        match action {
            IoAction::HandleMessage { msg: decoded } => {
                assert_eq!(decoded, ClientMessage::Drain);
            }
            other => panic!("unexpected action: {:?}", other),
        }
    }

    #[test]
    fn decode_message_returns_decode_error_on_corrupt_frame() {
        // A buffer too small for the frame header.
        let action = decode_message(&[0u8, 0u8]);
        assert!(matches!(action, IoAction::DecodeError { .. }));
    }

    #[test]
    fn decode_message_handles_empty_input_without_panic() {
        let action = decode_message(&[]);
        assert!(matches!(action, IoAction::DecodeError { .. }));
    }

    // -- dispatch_message: every protocol arm ----------------------------

    #[test]
    fn dispatch_open_returns_opened_with_backend_id() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let msg = ClientMessage::Open {
            format: PcmFormat::S16Le,
            layout: ChannelLayout::Stereo,
            rate: SampleRate::Hz48000,
        };
        let outcome = dispatch_message(&msg, &mut reg, &mut b);
        match outcome {
            DispatchOutcome::Opened { stream_id } => assert_eq!(stream_id, 7),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn dispatch_open_when_already_open_returns_open_error_busy() {
        // Second `Open` while the registry holds a stream returns
        // `OpenError(Busy)` — the protocol surface for the
        // single-stream constraint.
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let _ = open_stereo(&mut reg, &mut b);
        let msg = ClientMessage::Open {
            format: PcmFormat::S16Le,
            layout: ChannelLayout::Stereo,
            rate: SampleRate::Hz48000,
        };
        let outcome = dispatch_message(&msg, &mut reg, &mut b);
        assert_eq!(outcome, DispatchOutcome::OpenError(AudioError::Busy));
    }

    #[test]
    fn dispatch_drain_returns_drain_ack_when_open() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let _ = open_stereo(&mut reg, &mut b);
        let outcome = dispatch_message(&ClientMessage::Drain, &mut reg, &mut b);
        assert_eq!(outcome, DispatchOutcome::DrainAck);
    }

    #[test]
    fn dispatch_drain_when_idle_returns_drain_error_invalid_argument() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let outcome = dispatch_message(&ClientMessage::Drain, &mut reg, &mut b);
        assert_eq!(
            outcome,
            DispatchOutcome::DrainError(AudioError::InvalidArgument)
        );
    }

    #[test]
    fn dispatch_close_returns_closed_when_open() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let _ = open_stereo(&mut reg, &mut b);
        let outcome = dispatch_message(&ClientMessage::Close, &mut reg, &mut b);
        assert_eq!(outcome, DispatchOutcome::Closed);
        // Slot released — next open succeeds.
        assert!(reg.is_idle());
    }

    #[test]
    fn dispatch_close_when_idle_returns_close_error_invalid_argument() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let outcome = dispatch_message(&ClientMessage::Close, &mut reg, &mut b);
        assert_eq!(
            outcome,
            DispatchOutcome::CloseError(AudioError::InvalidArgument)
        );
    }

    #[test]
    fn dispatch_submit_when_idle_returns_submit_error_invalid_argument() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let outcome = dispatch_message(&ClientMessage::SubmitFrames { len: 64 }, &mut reg, &mut b);
        assert_eq!(
            outcome,
            DispatchOutcome::SubmitError(AudioError::InvalidArgument)
        );
    }

    #[test]
    fn dispatch_control_command_get_stats_returns_stats_requested() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let outcome = dispatch_message(
            &ClientMessage::ControlCommand(AudioControlCommand::GetStats),
            &mut reg,
            &mut b,
        );
        assert_eq!(outcome, DispatchOutcome::StatsRequested);
    }

    // -- encode_outcome: every arm produces a well-formed ServerMessage --

    #[test]
    fn encode_outcome_opened_round_trips_through_server_message() {
        let reg = StreamRegistry::new();
        let smsg = encode_outcome(&DispatchOutcome::Opened { stream_id: 42 }, &reg);
        assert_eq!(smsg, ServerMessage::Opened { stream_id: 42 });
    }

    #[test]
    fn encode_outcome_open_error_carries_audio_error() {
        let reg = StreamRegistry::new();
        let smsg = encode_outcome(&DispatchOutcome::OpenError(AudioError::Busy), &reg);
        assert_eq!(smsg, ServerMessage::OpenError(AudioError::Busy));
    }

    #[test]
    fn encode_outcome_drain_ack_yields_drain_ack_server_message() {
        let reg = StreamRegistry::new();
        let smsg = encode_outcome(&DispatchOutcome::DrainAck, &reg);
        assert_eq!(smsg, ServerMessage::DrainAck);
    }

    #[test]
    fn encode_outcome_closed_yields_closed_server_message() {
        let reg = StreamRegistry::new();
        let smsg = encode_outcome(&DispatchOutcome::Closed, &reg);
        assert_eq!(smsg, ServerMessage::Closed);
    }

    #[test]
    fn encode_outcome_stats_requested_returns_control_event_stats() {
        // Build a registry with stats so the control-event reply
        // carries non-zero values.
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let _ = open_stereo(&mut reg, &mut b);
        reg.record_consumed(100);
        reg.record_underrun();
        reg.record_underrun();
        let smsg = encode_outcome(&DispatchOutcome::StatsRequested, &reg);
        match smsg {
            ServerMessage::ControlEvent(kernel_core::audio::AudioControlEvent::Stats {
                underrun_count,
                frames_consumed,
                ..
            }) => {
                assert_eq!(underrun_count, 2);
                assert_eq!(frames_consumed, 100);
            }
            other => panic!("expected Stats: {:?}", other),
        }
    }

    // -- apply_irq_event --------------------------------------------------

    #[test]
    fn apply_irq_event_underrun_bumps_registry_underrun_count() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let _ = open_stereo(&mut reg, &mut b);
        apply_irq_event(IrqEvent::Underrun, &mut reg);
        apply_irq_event(IrqEvent::Underrun, &mut reg);
        assert_eq!(reg.stats().underrun_count, 2);
    }

    #[test]
    fn apply_irq_event_lvbci_does_not_touch_stats() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let _ = open_stereo(&mut reg, &mut b);
        apply_irq_event(IrqEvent::LastValidIndex, &mut reg);
        let s = reg.stats();
        assert_eq!(s.frames_submitted, 0);
        assert_eq!(s.underrun_count, 0);
    }

    #[test]
    fn apply_irq_event_none_is_noop() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let _ = open_stereo(&mut reg, &mut b);
        apply_irq_event(IrqEvent::None, &mut reg);
        let s = reg.stats();
        assert_eq!(s.underrun_count, 0);
    }

    // -- io-loop discipline check -----------------------------------------

    #[test]
    fn no_irq_wait_calls_in_audio_server_production_paths() {
        // Acceptance bullet: `grep "irq.wait" userspace/audio_server/src/`
        // returns no hits in the io loop.  We scan the production
        // source files (everything except `irq.rs` itself, which
        // legitimately mentions the symbol in doc comments + this
        // self-check) and confirm zero matches against the production
        // call-site pattern.
        let sources: &[(&str, &str)] = &[
            ("device.rs", include_str!("device.rs")),
            ("stream.rs", include_str!("stream.rs")),
            ("client.rs", include_str!("client.rs")),
            ("lib.rs", include_str!("lib.rs")),
            ("main.rs", include_str!("main.rs")),
        ];
        for (name, s) in sources {
            assert!(
                !s.contains(".wait("),
                "audio_server file {name} must never call .wait( on an IrqNotification — see Phase 55c",
            );
        }
        // For `irq.rs` we strip the `#[cfg(test)]` block before
        // scanning — the doc comment + this self-check legitimately
        // mention the literal symbol.  The production half of the
        // file must remain `.wait(`-free.
        let irq_src = include_str!("irq.rs");
        let prod_section = irq_src
            .split_once("#[cfg(test)]")
            .map(|(prod, _)| prod)
            .unwrap_or(irq_src);
        assert!(
            !prod_section.contains(".wait("),
            "audio_server irq.rs production code must never call .wait() on a notification",
        );
    }
}
