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
//! * `LABEL_PIXELS` (= 2) — `bulk` carries a raw BGRA8888 pixel buffer.
//!   `data0` carries the [`BufferId`] the client wants to attach.
//!   `data[1]` carries the bulk byte length.
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

use kernel_core::display::protocol::{
    BufferId, ClientMessage, MAX_FRAME_BODY_LEN, ProtocolError, ServerMessage,
};
use syscall_lib::IpcMessage;

use crate::surface::{CommittedBuffer, SurfaceRegistry, SurfaceShimError};

/// IPC label indicating an encoded `ClientMessage` follows in the bulk.
pub const LABEL_VERB: u64 = 1;
/// IPC label indicating raw pixel bytes follow in the bulk; `data0` is
/// the [`BufferId`] the next `AttachBuffer` will reference.
pub const LABEL_PIXELS: u64 = 2;

/// Maximum bulk size accepted by the dispatcher (matches the kernel's
/// `MAX_BULK_LEN`).
pub const MAX_BULK_BYTES: usize = 4096;

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
            // Stash the bulk into the registry's pending-bulk slot. The
            // next AttachBuffer with a matching BufferId will consume it.
            let buffer_id = BufferId(frame.header.data[0] as u32);
            registry.receive_bulk(CommittedBuffer {
                buffer_id,
                width: frame.header.data[2] as u32,
                height: frame.header.data[3] as u32,
                pixels: frame.bulk.to_vec(),
            });
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
                    Err(SurfaceShimError::StateMachine(_))
                    | Err(SurfaceShimError::UnknownSurface(_))
                    | Err(SurfaceShimError::DuplicateSurface(_))
                    | Err(SurfaceShimError::NoPendingBulk) => {
                        // Recoverable: log and continue. The protocol
                        // explicitly allows the server to reply with an
                        // error message rather than disconnect for these.
                        // Phase 56 minimum: silently drop and let the
                        // client recover.
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
    let (msg, _consumed) = ClientMessage::decode(bulk)?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::display::protocol::{Rect, SurfaceId};

    fn encode_to_vec(msg: &ClientMessage) -> Vec<u8> {
        let mut buf = alloc::vec![0u8; 256];
        let n = msg.encode(&mut buf).expect("encode");
        buf.truncate(n);
        buf
    }

    #[test]
    fn hello_returns_welcome() {
        let mut reg = SurfaceRegistry::new();
        let bulk = encode_to_vec(&ClientMessage::Hello {
            protocol_version: kernel_core::display::protocol::PROTOCOL_VERSION,
            capabilities: 0,
        });
        let mut header = IpcMessage::new(LABEL_VERB);
        header.data[1] = bulk.len() as u64;
        let outcome = dispatch(
            InboundFrame {
                header,
                bulk: &bulk,
            },
            &mut reg,
        );
        assert!(!outcome.fatal);
        assert!(!outcome.closed);
        assert_eq!(outcome.outbound.len(), 1);
        match outcome.outbound[0] {
            ServerMessage::Welcome { .. } => {}
            ref other => panic!("expected Welcome, got {other:?}"),
        }
    }

    #[test]
    fn goodbye_sets_closed_flag() {
        let mut reg = SurfaceRegistry::new();
        let bulk = encode_to_vec(&ClientMessage::Goodbye);
        let mut header = IpcMessage::new(LABEL_VERB);
        header.data[1] = bulk.len() as u64;
        let outcome = dispatch(
            InboundFrame {
                header,
                bulk: &bulk,
            },
            &mut reg,
        );
        assert!(outcome.closed);
        assert!(!outcome.fatal);
    }

    #[test]
    fn malformed_bulk_is_fatal() {
        let mut reg = SurfaceRegistry::new();
        let bulk = [0xFFu8, 0xFE, 0xFD]; // garbage opcode + truncated body
        let mut header = IpcMessage::new(LABEL_VERB);
        header.data[1] = bulk.len() as u64;
        let outcome = dispatch(
            InboundFrame {
                header,
                bulk: &bulk,
            },
            &mut reg,
        );
        assert!(outcome.fatal);
    }

    #[test]
    fn create_surface_and_commit_emits_configured() {
        let mut reg = SurfaceRegistry::new();
        // 1. Create surface.
        let bulk = encode_to_vec(&ClientMessage::CreateSurface {
            surface_id: SurfaceId(1),
        });
        let mut header = IpcMessage::new(LABEL_VERB);
        header.data[1] = bulk.len() as u64;
        let _ = dispatch(
            InboundFrame {
                header,
                bulk: &bulk,
            },
            &mut reg,
        );
        // 2. Set role.
        let bulk = encode_to_vec(&ClientMessage::SetSurfaceRole {
            surface_id: SurfaceId(1),
            role: kernel_core::display::protocol::SurfaceRole::Toplevel,
        });
        let mut header = IpcMessage::new(LABEL_VERB);
        header.data[1] = bulk.len() as u64;
        let _ = dispatch(
            InboundFrame {
                header,
                bulk: &bulk,
            },
            &mut reg,
        );
        // 3. Pixel bulk + AttachBuffer.
        let pixels = alloc::vec![0xAAu8; 32 * 32 * 4];
        let mut hdr = IpcMessage::new(LABEL_PIXELS);
        hdr.data[0] = 7;
        hdr.data[2] = 32;
        hdr.data[3] = 32;
        hdr.data[1] = pixels.len() as u64;
        let _ = dispatch(
            InboundFrame {
                header: hdr,
                bulk: &pixels,
            },
            &mut reg,
        );
        let bulk = encode_to_vec(&ClientMessage::AttachBuffer {
            surface_id: SurfaceId(1),
            buffer_id: BufferId(7),
        });
        let mut header = IpcMessage::new(LABEL_VERB);
        header.data[1] = bulk.len() as u64;
        let _ = dispatch(
            InboundFrame {
                header,
                bulk: &bulk,
            },
            &mut reg,
        );
        // 4. Damage + Commit. Commit must produce a SurfaceConfigured.
        let bulk = encode_to_vec(&ClientMessage::DamageSurface {
            surface_id: SurfaceId(1),
            rect: Rect {
                x: 0,
                y: 0,
                w: 32,
                h: 32,
            },
        });
        let mut header = IpcMessage::new(LABEL_VERB);
        header.data[1] = bulk.len() as u64;
        let _ = dispatch(
            InboundFrame {
                header,
                bulk: &bulk,
            },
            &mut reg,
        );
        let bulk = encode_to_vec(&ClientMessage::CommitSurface {
            surface_id: SurfaceId(1),
        });
        let mut header = IpcMessage::new(LABEL_VERB);
        header.data[1] = bulk.len() as u64;
        let outcome = dispatch(
            InboundFrame {
                header,
                bulk: &bulk,
            },
            &mut reg,
        );
        assert!(!outcome.fatal);
        let configured = outcome
            .outbound
            .iter()
            .any(|m| matches!(m, ServerMessage::SurfaceConfigured { .. }));
        assert!(configured, "expected SurfaceConfigured after commit");
        assert!(
            reg.has_damage(),
            "registry should report damage post-commit"
        );
    }
}
