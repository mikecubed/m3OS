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
use driver_runtime::ipc::EndpointCap;

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
    /// Decoded protocol message. `consumed` is the number of bytes the
    /// decoder used; trailing bytes in the same bulk buffer carry any
    /// out-of-band payload (e.g. PCM data for `SubmitFrames`).
    HandleMessage { msg: ClientMessage, consumed: usize },
    /// Decode error — log and reply with `OpenError`/`SubmitError`.
    DecodeError { err: ProtocolError },
}

/// Translate a raw `bulk` payload into an [`IoAction::HandleMessage`]
/// or `IoAction::DecodeError`. Pure logic, exercised on the host.
///
/// The decoded `consumed` byte count is carried on the
/// `HandleMessage` variant so the io loop can locate the trailing
/// payload that rides the same bulk buffer (currently used by
/// `SubmitFrames` to find its PCM bytes).
pub fn decode_message(bulk: &[u8]) -> IoAction {
    match ClientMessage::decode(bulk) {
        Ok((msg, consumed)) => IoAction::HandleMessage { msg, consumed },
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
        } => {
            // 2026-05-11 stale-stream fix: if a stream is already open
            // (the previous client died without sending `Close` — e.g.,
            // the documented `Io(-32)` intermittency aborts
            // `audio-demo` mid-`SubmitFrames`), close it first so the
            // new `Open` lands on a clean backend. This is the
            // protocol-level companion to the io loop's `force_release`
            // takeover: both paths exist because the audio_server has
            // no cap-revocation hook and uses a fixed
            // `LABEL_AUDIO_CMD` for every audio-demo invocation, so
            // `client_id` (`frame.label`) can't distinguish two
            // consecutive demo processes. Without this branch,
            // `try_open` hits `Ac97Backend.stream_open == true` and
            // returns `Busy` indefinitely until audio_server is
            // restarted.
            if let Some(s) = streams.open.as_ref() {
                let stale_id = s.stream_id;
                let _ = streams.close(backend, stale_id);
            }
            match streams.try_open(backend, *format, *layout, *rate) {
                Ok(id) => DispatchOutcome::Opened { stream_id: id },
                Err(e) => DispatchOutcome::OpenError(e),
            }
        }
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
        ClientMessage::SubmitFramesPageGrant { .. } => {
            // Phase 74 Track F.2 — handled in the io loop's pre-dispatch
            // arm in `run_io_loop` because it needs access to
            // `frame.cap_slots`. The pure-logic `dispatch_message`
            // helper cannot reach the cap-slot scratch, so it surfaces
            // the same `InvalidArgument` shape it would for any
            // forward-compat variant; the real handling happens above
            // before this fallback fires.
            DispatchOutcome::SubmitError(AudioError::InvalidArgument)
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

/// Phase 105 Track D.2 — apply the system master gain to a PCM buffer,
/// returning the slice to forward to the backend.
///
/// At unity the input is returned unchanged (zero-copy — the common case,
/// so an un-attenuated stream and the page-grant fast path pay nothing).
/// Below unity, `pcm` is copied into `scratch` and scaled in place so the
/// source (which may be a read-only page grant from the client) is never
/// mutated. The scratch is caller-owned and reused across submits.
fn gained_pcm<'a>(pcm: &'a [u8], q15_gain: u16, scratch: &'a mut alloc::vec::Vec<u8>) -> &'a [u8] {
    if q15_gain >= kernel_core::audio::MASTER_GAIN_UNITY_Q15 {
        return pcm;
    }
    scratch.clear();
    scratch.extend_from_slice(pcm);
    kernel_core::audio::apply_master_gain_s16le(scratch, q15_gain);
    scratch.as_slice()
}

// ---------------------------------------------------------------------------
// run_io_loop — production entry point
// ---------------------------------------------------------------------------

/// Main server loop: blocks on the command endpoint, dispatches client
/// messages through the registry, and forwards them to the backend.
///
/// Phase 80: `audio_server` no longer owns the audio hardware — the backend
/// is an [`crate::proxy::AudioProxyBackend`] (a `dyn AudioBackend`) forwarding
/// to an out-of-process driver. There is therefore no device IRQ to subscribe
/// to here: completion is observed via the `frames_consumed` the driver
/// returns on each `SubmitFrames` `Ack` (surfaced through
/// `backend.poll_frames_consumed()`). A `RecvResult::Notification` is not
/// expected on this endpoint, but the arm is kept defensively.
#[cfg(not(test))]
pub fn run_io_loop(
    backend: &mut Ac97BackendDyn,
    streams: &mut StreamRegistry,
    clients: &mut ClientRegistry,
    endpoint: EndpointCap,
) -> i32 {
    use driver_runtime::ipc::{IpcBackend, RecvResult};
    use kernel_core::audio::{MAX_SUBMIT_BYTES, ServerMessage};
    use syscall_lib::STDOUT_FILENO;

    // Phase 63 driver-host fix: size the recv bulk buffer for the largest
    // audio request (a `SubmitFrames` PCM payload — up to MAX_SUBMIT_BYTES,
    // 64 KiB — preceded by the small ClientMessage frame header). The
    // default `SyscallBackend::recv` bulk cap is 1522 B (sized for net
    // frames) and would silently truncate the PCM payload, so audio_server
    // must opt into a larger buffer via `recv_with_capacity`.
    //
    // The +256 slack covers the request frame header (currently 16 B)
    // with comfortable margin for ABI evolution.
    const AUDIO_RECV_CAP: usize = MAX_SUBMIT_BYTES + 256;

    let mut transport = driver_runtime::ipc::SyscallBackend::new();
    // Phase 105 Track D.2 — system master volume. Starts at unity (the
    // forward path is a no-op until the settings panel attenuates it) and
    // is updated by the `SetMasterVolume` control verb below; applied to
    // every forwarded PCM buffer via `apply_master_gain_s16le`.
    let mut master_gain_q15: u16 = kernel_core::audio::MASTER_GAIN_UNITY_Q15;
    // Reused scratch for the attenuated PCM copy so a below-unity volume
    // does not allocate per `SubmitFrames`.
    let mut gain_scratch: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    loop {
        let result = match transport.recv_with_capacity(endpoint, AUDIO_RECV_CAP) {
            Ok(r) => r,
            Err(_) => {
                // Phase 63 driver-host fix: a recv error here is currently
                // bound to a deeper Phase 63 IRQ-pipeline issue (the AC'97
                // notification bits race-drain in `recv_msg_with_notif`,
                // returning `u64::MAX` repeatedly without blocking). An
                // earlier "log + continue" branch turned this into a tight
                // hot loop that starved other userspace services; restore
                // the original bounded behavior — exit + let init's
                // `restart=on-failure max_restart=3` policy contain the
                // chaos until the underlying race lands as a kernel fix.
                syscall_lib::write_str(STDOUT_FILENO, "audio_server: recv failed\n");
                return 8;
            }
        };
        match result {
            RecvResult::Notification(_bits) => {
                let _ = backend.handle_irq();
            }
            RecvResult::Message(frame) => {
                // First-message admit: the connecting client must be
                // admitted into the single-client slot. Phase 57 D.5
                // identifies clients by the message label (kernel-
                // staged sender id); the rate-limited rejection log
                // lives in `ClientRegistry::reject`.
                //
                // 2026-05-11 takeover fix: if an `Open` arrives from a
                // client that isn't the current owner, treat it as
                // the previous client having gone away (single-client
                // server — a new IPC sender id means a fresh process,
                // and the audio_server has no other way to learn of
                // a crashed/aborted client). Close the lingering
                // stream on the backend, force-release the slot, and
                // admit the new client. Without this, an
                // `audio-demo` invocation that died mid-`SubmitFrames`
                // (e.g., the documented `Io(-32)` intermittency)
                // leaves the registry pinned to the dead pid, and
                // every subsequent run reports `Server:Busy` at the
                // `Open` stage.
                let client_id = frame.label as u32;
                let action = decode_message(&frame.bulk);
                let is_open = matches!(
                    action,
                    IoAction::HandleMessage {
                        msg: ClientMessage::Open { .. },
                        ..
                    }
                );
                if !clients.try_admit(client_id) {
                    if is_open {
                        if let Some(prev_owner) = clients.force_release() {
                            // Best-effort cleanup of the previous
                            // client's stream so the new opener gets a
                            // fresh backend state. The close itself
                            // may fail (e.g., stream already torn
                            // down) — that's fine, the registry
                            // release is what unblocks the admit.
                            if let Some(s) = streams.open.as_ref() {
                                let _ = streams.close(backend, s.stream_id);
                            }
                            let _ = prev_owner;
                        }
                        let _ = clients.try_admit(client_id);
                    } else {
                        let mut buf = [0u8; 16];
                        let reply = ServerMessage::OpenError(AudioError::Busy);
                        if let Ok(n) = reply.encode(&mut buf) {
                            let _ = transport.store_reply_bulk(&buf[..n]);
                        }
                        let _ = transport.reply(frame.label, 0);
                        continue;
                    }
                }
                let outcome = match action {
                    IoAction::HandleMessage { msg, consumed } => match &msg {
                        // SubmitFrames carries its PCM payload as the
                        // bytes immediately after the encoded frame
                        // header in the same bulk buffer. Extract that
                        // slice and run it through `streams.submit` so
                        // the backend actually programs the BDL —
                        // `dispatch_message` cannot see `frame.bulk` and
                        // would otherwise return `SubmitAck` without
                        // touching the hardware (the original Phase 57
                        // D.1 stub that left `frames_consumed` pinned
                        // at `0` and the AC'97 IRQ silent).
                        ClientMessage::SubmitFrames { len } => {
                            let pcm_len = *len as usize;
                            if streams.open.is_none() {
                                DispatchOutcome::SubmitError(AudioError::InvalidArgument)
                            } else if consumed.saturating_add(pcm_len) > frame.bulk.len() {
                                DispatchOutcome::SubmitError(AudioError::Internal)
                            } else {
                                let stream_id =
                                    streams.open.as_ref().map(|s| s.stream_id).unwrap_or(0);
                                let pcm = &frame.bulk[consumed..consumed + pcm_len];
                                // Phase 105 Track D.2 — apply the system
                                // master gain before forwarding (no-op copy
                                // elision at unity).
                                let forward = gained_pcm(pcm, master_gain_q15, &mut gain_scratch);
                                match streams.submit(backend, stream_id, forward) {
                                    Ok(_) => DispatchOutcome::SubmitAck {
                                        frames_consumed: streams.stats().frames_consumed,
                                    },
                                    Err(e) => DispatchOutcome::SubmitError(e),
                                }
                            }
                        }
                        // Phase 74 Track F.2 — zero-copy PCM submit via
                        // page-grant transport. The client transferred a
                        // `Capability::PageGrant` for the PCM ring via
                        // the IPC `cap_slots`; the kernel populated
                        // `frame.cap_slots[cap_slot_index]` with the
                        // receiver-side handle. We consume the grant via
                        // `sys_page_grant_recv` to map the granted
                        // pages into our address space, then read the
                        // PCM bytes directly from that mapping with no
                        // intervening IPC bulk copy.
                        ClientMessage::SubmitFramesPageGrant {
                            cap_slot_index,
                            n_pages,
                            len,
                        } => {
                            let pcm_len = *len as usize;
                            let n_caps = frame.n_caps as usize;
                            let idx = *cap_slot_index as usize;
                            // Require `n_pages == ceil(pcm_len / 4096)` so the
                            // protocol fields are self-consistent and a client
                            // cannot claim a larger page count than the PCM
                            // payload actually needs. `sys_page_grant_recv`
                            // does not report the actual mapped length, so the
                            // server's only safe anchor is `pcm_len` itself;
                            // bounding `n_pages` to the page count needed to
                            // cover `pcm_len` prevents a malicious client from
                            // tricking the server into computing
                            // `mapped_bytes > kernel_mapping_size` and then
                            // reading past the mapping.
                            let required_pages = pcm_len.div_ceil(4096);
                            if streams.open.is_none() {
                                DispatchOutcome::SubmitError(AudioError::InvalidArgument)
                            } else if idx >= n_caps {
                                DispatchOutcome::SubmitError(AudioError::InvalidArgument)
                            } else if *n_pages as usize != required_pages {
                                DispatchOutcome::SubmitError(AudioError::InvalidArgument)
                            } else {
                                let cap = frame.cap_slots[idx];
                                let user_va = syscall_lib::page_grant_recv(cap);
                                if user_va == u64::MAX {
                                    DispatchOutcome::SubmitError(AudioError::Internal)
                                } else {
                                    let stream_id =
                                        streams.open.as_ref().map(|s| s.stream_id).unwrap_or(0);
                                    // SAFETY: `sys_page_grant_recv` mapped
                                    // exactly `required_pages * 4096` bytes
                                    // (validated against `n_pages` above) at
                                    // `user_va`; we read `pcm_len` bytes which
                                    // is bounded by `required_pages * 4096`
                                    // by construction of `required_pages`.
                                    let pcm = unsafe {
                                        core::slice::from_raw_parts(user_va as *const u8, pcm_len)
                                    };
                                    // Phase 105 Track D.2 — apply the system
                                    // master gain. Below unity this copies
                                    // into `gain_scratch` (the client's
                                    // granted pages are never mutated); at
                                    // unity the zero-copy grant is forwarded
                                    // as-is.
                                    let forward =
                                        gained_pcm(pcm, master_gain_q15, &mut gain_scratch);
                                    match streams.submit(backend, stream_id, forward) {
                                        Ok(_) => DispatchOutcome::SubmitAck {
                                            frames_consumed: streams.stats().frames_consumed,
                                        },
                                        Err(e) => DispatchOutcome::SubmitError(e),
                                    }
                                }
                            }
                        }
                        // Phase 105 Track D.2 — the system master volume is
                        // io-loop state (it scales the forwarded PCM above),
                        // so `dispatch_message` — which cannot reach it —
                        // does not handle this verb. Update the gain here and
                        // reply with the current stats (same reply shape as
                        // `GetStats`, so the control surface is uniform).
                        ClientMessage::ControlCommand(
                            kernel_core::audio::AudioControlCommand::SetMasterVolume { q15_gain },
                        ) => {
                            master_gain_q15 =
                                (*q15_gain).min(kernel_core::audio::MASTER_GAIN_UNITY_Q15);
                            DispatchOutcome::StatsRequested
                        }
                        _ => dispatch_message(&msg, streams, backend),
                    },
                    IoAction::DecodeError { .. } => {
                        DispatchOutcome::OpenError(AudioError::InvalidArgument)
                    }
                    IoAction::HandleIrq { .. } => {
                        // Cannot happen on a Message arm — fall through.
                        continue;
                    }
                };
                // Sync the StreamRegistry's `frames_consumed` from the
                // backend before any reply that includes stats. The
                // backend's IRQ-side state (or, under `-audiodev wav`,
                // its CIV-poll fallback inside `submit_frames`) is
                // authoritative; the StreamRegistry only mirrors the
                // count for the reply path. Without this sync the
                // `GetStats` reply would forever report `0` because
                // `record_consumed` is never called from this loop.
                let device_consumed = backend.poll_frames_consumed();
                let mirrored = streams.stats().frames_consumed;
                if device_consumed > mirrored {
                    streams.record_consumed(device_consumed - mirrored);
                }
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
            // Bump the underrun counter exactly once per event.  The
            // io loop must then call `repost_silence_after_underrun` to
            // re-arm the BDL; those two steps are separate so the
            // underrun_count is never double-counted.
            streams.record_underrun();
        }
        IrqEvent::FifoError => {
            // Programming bug.  The io loop logs and surfaces
            // `AudioError::Internal` to the open client.
        }
        IrqEvent::None => {}
    }
}

/// Re-arm the BDL after an underrun by submitting one silence slot.
///
/// Called by the io loop immediately after [`apply_irq_event`] returns
/// `IrqEvent::Underrun`.  Because [`crate::device::IrqEvent::Underrun`]
/// is only produced when `Ac97Logic` observed FIFOE on an *empty* ring
/// (`head == tail`), posting one silence slot is always safe — the ring
/// has room and the device will start consuming from the new entry.
///
/// The call goes through `StreamRegistry::submit` so
/// `stream.stats.frames_submitted` advances — the underrun-recovery
/// slot counts as a submitted frame from the caller's perspective.
///
/// # No double-count
///
/// `apply_irq_event` already incremented `underrun_count` for this
/// event; this function must *not* bump it again.  It only posts audio
/// data; the stats counter update is the caller's responsibility before
/// this call.
pub fn repost_silence_after_underrun(
    backend: &mut dyn AudioBackend,
    stream_id: u32,
    streams: &mut StreamRegistry,
) {
    use crate::device::SILENCE_FRAME;
    // `submit` may return `WouldBlock` if — against the invariant — the
    // BDL somehow has no room; ignore that gracefully rather than
    // panicking in the IRQ handler path.
    let _ = streams.submit(backend, stream_id, &SILENCE_FRAME);
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
            IoAction::HandleMessage {
                msg: decoded,
                consumed,
            } => {
                assert_eq!(decoded, ClientMessage::Drain);
                assert_eq!(consumed, n);
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
    fn dispatch_open_when_already_open_takes_over_and_returns_opened() {
        // 2026-05-11 stale-stream fix: an `Open` arriving while a
        // stream is already open is treated as a takeover — the
        // previous session is closed and a fresh stream id is
        // allocated. This replaces the old `Busy` semantics so that
        // a client that died mid-protocol (e.g., the documented
        // `Io(-32)` aborting `audio-demo` between `Open` and
        // `Close`) doesn't permanently wedge the server. The fixed
        // `LABEL_AUDIO_CMD` means the io loop's client_id-based
        // `force_release` takeover can't distinguish two
        // consecutive demo processes — this protocol-level path is
        // what actually unblocks them.
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let first_id = open_stereo(&mut reg, &mut b);
        let msg = ClientMessage::Open {
            format: PcmFormat::S16Le,
            layout: ChannelLayout::Stereo,
            rate: SampleRate::Hz48000,
        };
        let outcome = dispatch_message(&msg, &mut reg, &mut b);
        match outcome {
            DispatchOutcome::Opened { stream_id } => {
                assert_ne!(
                    stream_id, first_id,
                    "takeover must allocate a fresh stream id so backend state is reset"
                );
            }
            other => panic!("expected Opened, got {other:?}"),
        }
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

    // -- B.2: repost_silence_after_underrun ----------------------------------

    /// B.2: open → simulate `IrqEvent::Underrun` → assert `frames_submitted`
    /// advances by exactly one slot's worth of zero bytes.
    #[test]
    fn repost_silence_after_underrun_advances_frames_submitted_by_one_slot() {
        use super::repost_silence_after_underrun;
        use crate::device::PCM_SLOT_STRIDE;

        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let stream_id = open_stereo(&mut reg, &mut b);

        // Precondition: no frames submitted yet.
        assert_eq!(reg.stats().frames_submitted, 0);

        // Simulate an underrun event: apply_irq_event bumps underrun_count.
        apply_irq_event(IrqEvent::Underrun, &mut reg);
        assert_eq!(reg.stats().underrun_count, 1, "underrun_count must be 1");

        // Repost one silence slot.
        repost_silence_after_underrun(&mut b, stream_id, &mut reg);

        // frames_submitted must advance by exactly PCM_SLOT_STRIDE bytes.
        assert_eq!(
            reg.stats().frames_submitted,
            PCM_SLOT_STRIDE as u64,
            "frames_submitted must advance by one slot stride of silence"
        );
        // underrun_count must remain at 1 — no double-count.
        assert_eq!(
            reg.stats().underrun_count,
            1,
            "underrun_count must not be double-incremented by repost"
        );
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

    // Phase 105 Track D.2 — the system master-gain forward helper.

    fn s16le(samples: &[i16]) -> Vec<u8> {
        let mut v = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    #[test]
    fn gained_pcm_unity_is_zero_copy() {
        let pcm = s16le(&[100, -200, 300]);
        let mut scratch = Vec::new();
        let out = gained_pcm(
            &pcm,
            kernel_core::audio::MASTER_GAIN_UNITY_Q15,
            &mut scratch,
        );
        // Unity returns the input slice unchanged and never fills scratch.
        assert_eq!(out, pcm.as_slice());
        assert!(scratch.is_empty(), "unity must not copy into scratch");
    }

    #[test]
    fn gained_pcm_attenuates_via_scratch() {
        let pcm = s16le(&[0, 200, -200, 1000]);
        let mut scratch = Vec::new();
        let out = gained_pcm(&pcm, 0x4000, &mut scratch); // half
        let decoded: Vec<i16> = out
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(decoded, [0, 100, -100, 500]);
        // The source PCM is untouched (a page grant must never be mutated).
        assert_eq!(pcm, s16le(&[0, 200, -200, 1000]));
    }

    #[test]
    fn gained_pcm_zero_mutes_without_touching_source() {
        let pcm = s16le(&[1234, -4321]);
        let mut scratch = Vec::new();
        let out = gained_pcm(&pcm, 0, &mut scratch);
        assert!(out.iter().all(|&b| b == 0));
        assert_eq!(pcm, s16le(&[1234, -4321]));
    }
}
