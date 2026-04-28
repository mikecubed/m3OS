//! Phase 56 Track C.5 — client connection / IPC dispatcher.
//!
//! Phase 56 ships an **IPC-endpoint** transport for the client protocol
//! rather than AF_UNIX sockets. This is the explicit pivot the task doc's
//! "AF_UNIX (or IPC)" foundation note allows: AF_UNIX SCM_RIGHTS-equivalent
//! capability transfer is not yet implemented in m3OS, and the existing
//! IPC bulk-transport primitive (`ipc_send_buf` / `ipc_call_buf`) gives us
//! everything we need for a single-client protocol-reference demo. The
//! *protocol types* live in `kernel-core::display::protocol` and are
//! transport-agnostic, so a future swap to AF_UNIX is a wiring change in
//! this file alone.
//!
//! # Wire framing
//!
//! Every protocol-bearing IPC message uses two label values:
//!
//! * `LABEL_VERB` (= 1) — `bulk` carries an encoded [`ClientMessage`].
//!   `data0` is unused. `data[1]` carries the bulk byte length (kernel
//!   convention — see `kernel/src/ipc/mod.rs::ipc_send_with_bulk`).
//! * `LABEL_PIXELS` (= 2) — `bulk` is `[w: u32 LE | h: u32 LE | pixel_bytes...]`.
//!   `data0` carries the [`BufferId`] the next `AttachBuffer` will reference.
//!   `data[1]` carries the bulk byte length. The geometry-in-bulk shape
//!   exists because the IPC bulk-send syscalls only let clients populate
//!   `data0`; `data[2..]` are written by the kernel and unreachable from
//!   the sender side.
//!
//! Both labels travel on the same `display` endpoint. The dispatcher
//! routes by label and forwards into the [`SurfaceRegistry`].
//!
//! # Resource bounds
//!
//! Per-client bounds are enforced by the registry today (one client in
//! Phase 56). Outbound events accumulate in [`Vec<ServerMessage>`] and are
//! flushed each iteration; if a future multi-client world introduces
//! per-client bounded queues, this module is the place to enforce them.

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::display::pixel_chunk::{CHUNK_HEADER_LEN, PixelChunkHeader};
use kernel_core::display::protocol::{
    BufferId, ClientMessage, MAX_FRAME_BODY_LEN, ProtocolError, ServerMessage,
};
use syscall_lib::IpcMessage;

use crate::surface::{CommittedBuffer, SurfaceRegistry};

/// IPC label indicating an encoded `ClientMessage` follows in the bulk.
pub const LABEL_VERB: u64 = 1;
/// IPC label indicating raw pixel bytes follow in the bulk; `data0` is
/// the [`BufferId`] the next `AttachBuffer` will reference.
///
/// Bulk wire: `[w: u32 LE | h: u32 LE | pixel_bytes...]`. The whole
/// buffer travels in one IPC bulk and must fit in the kernel's
/// `MAX_BULK_LEN` (4 KB). For surfaces larger than ~32×32 BGRA, use
/// [`LABEL_PIXELS_CHUNK`] instead.
pub const LABEL_PIXELS: u64 = 2;
/// IPC label indicating one chunk of a multi-chunk surface buffer
/// follows in the bulk; `data0` is the [`BufferId`] the chunk
/// contributes to. Once the server has received chunks whose
/// cumulative `chunk_len` reaches the header's `total_bytes`, the
/// completed buffer is moved into the same `pending_bulk` slot the
/// `LABEL_PIXELS` path uses, and the next `AttachBuffer { buffer_id
/// }` consumes it. See
/// [`kernel_core::display::pixel_chunk`] for the wire format and the
/// `ChunkAccumulator` reassembly contract.
pub const LABEL_PIXELS_CHUNK: u64 = 5;
/// IPC label a client sends to drain one queued `ServerMessage` from
/// `display_server`'s per-client outbound queue. The reply carries the
/// next pending message in its reply-bulk slot, or replies with
/// [`LABEL_CLIENT_EVENT_NONE`] when the queue is empty.
///
/// Phase 56 C.5 close-out: the dispatcher routes `KeyEvent` /
/// `PointerEvent` deliveries into `ServerMessage::Key` / `Pointer`
/// outbound entries; the per-client queue accumulates them between
/// frame ticks; the client pulls them one at a time by sending a
/// `LABEL_CLIENT_EVENT_PULL` `ipc_call` (no bulk).
pub const LABEL_CLIENT_EVENT_PULL: u64 = 3;
/// Reply label used by the server when the client's pull request finds
/// the per-client outbound queue empty. Distinct from `u64::MAX` so the
/// caller can distinguish "no events this tick" from a transport-level
/// error. Mirrors the `KBD_EVENT_NONE` / `MOUSE_EVENT_NONE` convention
/// established by the input services in Phase 56 D.3.
pub const LABEL_CLIENT_EVENT_NONE: u64 = 4;
/// Per-client outbound event-queue cap, per the documented Phase 56
/// resource bounds (`docs/56-display-and-input-architecture.md`:180
/// "Outbound event-queue depth per client | 128"). Once the queue is
/// full the oldest event is dropped and a `display_server: outbound
/// queue full; oldest dropped` log line is emitted; the design favours
/// timely-but-lossy delivery over open-ended growth.
pub const MAX_CLIENT_EVENT_QUEUE: usize = 128;

/// Maximum bulk size accepted by the dispatcher (matches the kernel's
/// `MAX_BULK_LEN`).
pub const MAX_BULK_BYTES: usize = 4096;

/// Bytes per BGRA8888 pixel — used to validate that the bulk length on a
/// `LABEL_PIXELS` frame matches `width * height * BYTES_PER_PIXEL_BGRA8888`.
pub const BYTES_PER_PIXEL_BGRA8888: usize = 4;

/// Length of the geometry header at the front of a `LABEL_PIXELS` bulk.
/// Layout: `[w: u32 LE (4) | h: u32 LE (4)]`. The remaining
/// `bulk.len() - PIXEL_BULK_HEADER_LEN` bytes are pixels.
pub const PIXEL_BULK_HEADER_LEN: usize = 8;

/// Outcome of one dispatch loop iteration.
#[derive(Debug, Default)]
pub struct DispatchOutcome {
    /// Server → client messages produced by the dispatched verb. The caller
    /// (`main.rs`) is responsible for serialising and sending them back.
    pub outbound: Vec<ServerMessage>,
    /// `true` if a `Goodbye` was processed; the caller should exit the
    /// per-client loop.
    pub closed: bool,
    /// `true` if the client violated the wire protocol (decode error,
    /// state-machine error, oversized bulk). The caller should disconnect.
    pub fatal: bool,
}

/// One Phase 56 IPC message from a client. Created by the C.5 dispatch
/// loop after `ipc_recv_msg`. The bulk slice is borrowed from the receive
/// buffer for the lifetime of `dispatch`.
pub struct InboundFrame<'a> {
    pub header: IpcMessage,
    pub bulk: &'a [u8],
}

/// Dispatch a single inbound frame.
///
/// Returns the outbound message list (which `main.rs` is responsible for
/// transmitting) plus closed/fatal flags. The dispatcher never sends
/// directly — keeping I/O out of this module makes the body host-testable
/// without an IPC harness.
pub fn dispatch(frame: InboundFrame<'_>, registry: &mut SurfaceRegistry) -> DispatchOutcome {
    let mut out = DispatchOutcome::default();
    if frame.bulk.len() > MAX_BULK_BYTES {
        out.fatal = true;
        return out;
    }

    match frame.header.label {
        LABEL_PIXELS => {
            // Bulk wire format: `[w: u32 LE | h: u32 LE | pixel_bytes...]`.
            // The IPC bulk-send syscalls only let clients populate `data0`
            // (the kernel writes `data[1]` with bulk length and zeros the
            // rest) — so geometry has to travel in the bulk itself. The
            // first 8 bytes are the header; the remainder is exactly
            // `w * h * BYTES_PER_PIXEL_BGRA8888` BGRA8888 pixels.
            let buffer_id = BufferId(frame.header.data[0] as u32);
            if frame.bulk.len() < PIXEL_BULK_HEADER_LEN {
                out.fatal = true;
                return out;
            }
            let mut wbuf = [0u8; 4];
            let mut hbuf = [0u8; 4];
            wbuf.copy_from_slice(&frame.bulk[0..4]);
            hbuf.copy_from_slice(&frame.bulk[4..8]);
            let width = u32::from_le_bytes(wbuf);
            let height = u32::from_le_bytes(hbuf);
            let pixels = &frame.bulk[PIXEL_BULK_HEADER_LEN..];
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|wh| wh.checked_mul(BYTES_PER_PIXEL_BGRA8888));
            if expected != Some(pixels.len()) {
                out.fatal = true;
                return out;
            }
            // Resource bound — `receive_bulk` returns `false` if the
            // pending-bulk queue is at the documented cap. Refusing
            // additional buffers protects compositor memory from a
            // client that floods `LABEL_PIXELS` without `AttachBuffer`.
            if !registry.receive_bulk(CommittedBuffer {
                buffer_id,
                width,
                height,
                pixels: pixels.to_vec(),
            }) {
                out.fatal = true;
                return out;
            }
        }
        LABEL_PIXELS_CHUNK => {
            // Bulk wire: 24-byte `PixelChunkHeader` + `chunk_len`
            // bytes of pixel data. The accumulator owns the
            // reassembly state; the dispatcher just decodes the
            // header, splits the body, and forwards.
            if frame.bulk.len() < CHUNK_HEADER_LEN {
                out.fatal = true;
                return out;
            }
            let header = match PixelChunkHeader::decode(&frame.bulk[..CHUNK_HEADER_LEN]) {
                Ok(h) => h,
                Err(_) => {
                    out.fatal = true;
                    return out;
                }
            };
            // The IPC `data0` carries the BufferId; cross-check
            // against the in-bulk header so a confused client cannot
            // accidentally race two buffers' chunks together.
            if frame.header.data[0] as u32 != header.buffer_id {
                out.fatal = true;
                return out;
            }
            let body = &frame.bulk[CHUNK_HEADER_LEN..];
            if registry.receive_chunk(header, body).is_err() {
                out.fatal = true;
                return out;
            }
        }
        LABEL_VERB => match decode_message(frame.bulk) {
            Ok(msg) => match msg {
                ClientMessage::Hello {
                    protocol_version, ..
                } => {
                    out.outbound.push(ServerMessage::Welcome {
                        protocol_version,
                        capabilities: 0,
                    });
                }
                ClientMessage::Goodbye => {
                    out.closed = true;
                }
                ref other => match registry.handle_message(other) {
                    Ok(result) => out.outbound.extend(result.outbound),
                    Err(_) => {
                        // Recoverable surface-shim errors
                        // (UnknownSurface, DuplicateSurface, StateMachine,
                        // PendingBulkIdMismatch). The protocol explicitly
                        // allows the server to reply with an error message
                        // rather than disconnect on these; Phase 56's
                        // minimum behaviour is to log via the dispatcher
                        // and let the client recover.
                    }
                },
            },
            Err(_) => {
                out.fatal = true;
            }
        },
        _ => {
            // Unknown labels are ignored in Phase 56 (forward-compatible
            // for future labels like a control-socket multiplex). Future
            // tightening could close on unknown labels.
        }
    }

    out
}

fn decode_message(bulk: &[u8]) -> Result<ClientMessage, ProtocolError> {
    if bulk.len() > MAX_FRAME_BODY_LEN as usize {
        return Err(ProtocolError::BodyTooLarge);
    }
    let (msg, consumed) = ClientMessage::decode(bulk)?;
    // Phase 56 wire framing is "exactly one frame per IPC bulk" — trailing
    // bytes are a protocol violation, not a forward-compatible extension.
    // Reject so fuzzing / adversarial clients cannot smuggle a half-second
    // frame past the dispatcher and produce ambiguous framing.
    if consumed != bulk.len() {
        return Err(ProtocolError::BodyLengthMismatch);
    }
    Ok(msg)
}

// NB: a `#[cfg(test)]` host-side test module previously lived here, but
// `display_server` is a `no_std` + `no_main` binary crate and cannot be
// compiled with the std `test` harness. Future C.5 work that wants
// host-runnable dispatcher tests should split the pure-logic dispatch
// surface (this file's `dispatch` + `decode_message`) into a small
// library crate. Until then, the dispatcher is exercised end-to-end by
// the Phase 56 G.1 regression test running under QEMU.
