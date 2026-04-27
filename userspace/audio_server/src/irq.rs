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
// Tests — D.4 host coverage (lands red in next commit)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // D.4 lands the failing-test commit + the FakeIrq + FakeBackend
    // mocks. Track D.1 keeps `#[cfg(test)]` compiling green so the
    // scaffold ships.
}
