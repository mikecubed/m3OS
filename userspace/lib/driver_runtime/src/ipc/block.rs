//! Block-driver IPC client helper — Phase 55b Track C.4.
//!
//! [`BlockServer`] is the driver-side companion to the kernel's
//! `RemoteBlockDevice` facade (Phase 55b Track D.4). It owns the
//! Phase 50 endpoint capability the kernel calls into, pulls one
//! [`BlkRequestHeader`] at a time off the wire, dispatches to a
//! caller-supplied closure, and serialises the matching
//! [`BlkReplyHeader`] + optional bulk read data back to the kernel.
//!
//! The point of factoring this out is the NVMe driver (Track D) and
//! any future block driver (AHCI, VirtIO-blk, ...) all share this
//! exact skeleton — they differ only inside the closure that turns a
//! [`BlkRequest`] into a [`BlkReply`]. Drivers never touch
//! `syscall_lib::ipc_*` directly for the block path.
//!
//! # DRY
//!
//! Every message type re-exported at the bottom of this module lives
//! once, in [`kernel_core::driver_ipc::block`]. This module only
//! wraps the send / recv / reply plumbing.

use alloc::vec::Vec;

use spin::Mutex;

use super::{EndpointCap, IpcBackend, RecvFrame, RecvResult, SyscallBackend};

pub use kernel_core::driver_ipc::block::{
    BLK_READ, BLK_REPLY_HEADER_SIZE, BLK_REQUEST_HEADER_SIZE, BLK_STATUS, BLK_WRITE,
    BlkReplyHeader, BlkRequestHeader, BlockDriverError, DecodeError, MAX_SECTORS_PER_REQUEST,
    decode_blk_reply, decode_blk_request, encode_blk_reply, encode_blk_request,
};

use crate::DriverRuntimeError;

// ---------------------------------------------------------------------------
// BlkRequest / BlkReply — domain types the closure sees.
// ---------------------------------------------------------------------------

/// Decoded request the server hands to the user closure.
///
/// Bundles the header, the bulk payload the kernel copied into the
/// server's recv buffer (write data on `BLK_WRITE`; empty on
/// `BLK_READ` / `BLK_STATUS`), and the payload-grant handle the
/// schema carries alongside the header. Drivers pattern-match on
/// `header.kind` to branch between read / write / status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlkRequest {
    /// Decoded request header.
    pub header: BlkRequestHeader,
    /// IPC grant handle that referred to bulk payload, as encoded in
    /// the wire payload. Forwarded to drivers that want to validate
    /// or log it; an in-process driver satisfies writes by reading
    /// `bulk` directly without touching the grant.
    pub payload_grant: u32,
    /// Bulk write-data payload the kernel copied alongside the
    /// header. Empty for `BLK_READ` and `BLK_STATUS`.
    pub bulk: Vec<u8>,
}

/// Reply the user closure returns.
///
/// Split into the header and the bulk read-data payload so
/// `handle_next` can stage the bulk before calling `reply` and can
/// stamp the reply header's `payload_grant` slot honestly on the
/// wire. For write replies and error replies the closure simply
/// leaves `bulk` empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlkReply {
    /// Reply header — see [`BlkReplyHeader`].
    pub header: BlkReplyHeader,
    /// Payload-grant handle the driver wants written alongside the
    /// reply header (see [`BLK_REPLY_HEADER_SIZE`] for offsets).
    /// Drivers that attach read data via `bulk` typically pass `0`
    /// here and let the kernel resolve the grant from the bulk slot.
    pub payload_grant: u32,
    /// Bulk read-data payload. Length must equal `header.bytes` on
    /// successful reads; zero on writes / errors.
    pub bulk: Vec<u8>,
}

// ---------------------------------------------------------------------------
// BlockServer — closure-dispatch server helper.
// ---------------------------------------------------------------------------

/// Driver-side block server: pulls one request at a time off
/// `endpoint`, dispatches to a caller-supplied closure, replies.
///
/// The concrete backend is pluggable so tests can substitute a
/// crate-private mock; production drivers use the default
/// [`SyscallBackend`]. The backend sits behind a [`Mutex`] so all
/// methods take `&self` — a driver's interrupt wake-loop and its
/// `handle_next` dispatch loop can share the same server reference
/// (though in practice only one thread calls `handle_next` at a
/// time per driver).
pub struct BlockServer<B: IpcBackend = SyscallBackend> {
    endpoint: EndpointCap,
    pub(crate) backend: Mutex<B>,
}

impl BlockServer<SyscallBackend> {
    /// Construct a block server bound to the given Phase 50 endpoint
    /// capability, using the real syscall backend.
    ///
    /// The server does not call any syscall at construction — that
    /// only happens inside [`BlockServer::handle_next`], so a driver
    /// can build the server early and wire the device up before
    /// entering the dispatch loop.
    pub fn new(endpoint: EndpointCap) -> Self {
        Self {
            endpoint,
            backend: Mutex::new(SyscallBackend),
        }
    }
}

impl<B: IpcBackend> BlockServer<B> {
    /// Construct a block server with an explicit backend. Exposed for
    /// the Track C.4 test harness (`cfg(test)`) and for future
    /// alternate transports (e.g. a user-mode simulator); production
    /// drivers use [`BlockServer::new`].
    pub fn with_backend(endpoint: EndpointCap, backend: B) -> Self {
        Self {
            endpoint,
            backend: Mutex::new(backend),
        }
    }

    /// The endpoint capability this server listens on. Exposed so a
    /// driver can register the endpoint with the service manager
    /// after constructing the server.
    pub fn endpoint(&self) -> EndpointCap {
        self.endpoint
    }

    /// Pull one request off the endpoint, run the closure, reply.
    ///
    /// This is the compatibility wrapper for drivers that do not care about
    /// notification wakes. [`RecvResult::Notification`] is handled by a
    /// default no-op callback so existing callers keep the old single-closure
    /// shape.
    pub fn handle_next<F>(&self, f: F) -> Result<(), DriverRuntimeError>
    where
        F: FnMut(BlkRequest) -> BlkReply,
    {
        self.handle_next_with_notification(f, |_bits| {})
    }
    /// Pull one request or notification off the endpoint, dispatch, reply.
    ///
    /// Acceptance bullets satisfied:
    ///
    /// - Closure receives exactly the decoded request shape: the
    ///   inbound bytes are decoded via [`decode_blk_request`] into a
    ///   [`BlkRequestHeader`] + `payload_grant` pair, then bundled
    ///   with the trailing bulk bytes into [`BlkRequest`].
    /// - Reply serialises back across the same backend via
    ///   [`encode_blk_reply`] + a `store_reply_bulk` for any read
    ///   data the closure returned.
    /// - Malformed requests do not panic: [`DecodeError`] results in
    ///   a [`BlockDriverError::InvalidRequest`] reply; the method
    ///   returns `Ok(())` because it successfully processed the
    ///   (malformed) frame.
    pub fn handle_next_with_notification<F, N>(
        &self,
        mut f: F,
        mut on_notification: N,
    ) -> Result<(), DriverRuntimeError>
    where
        F: FnMut(BlkRequest) -> BlkReply,
        N: FnMut(u64),
    {
        let frame: RecvFrame = match self.backend.lock().recv(self.endpoint)? {
            RecvResult::Notification(bits) => {
                on_notification(bits);
                return Ok(());
            }
            RecvResult::Message(frame) => frame,
        };
        match decode_blk_request(&frame.bulk) {
            Ok((header, payload_grant)) => {
                // Bulk write data rides after the fixed-width header
                // in the same recv buffer; forward whatever the peer
                // sent beyond `BLK_REQUEST_HEADER_SIZE` to the
                // closure. For `BLK_READ` and `BLK_STATUS` the peer
                // legitimately sends nothing past the header, so the
                // slice is empty.
                let bulk = if frame.bulk.len() > BLK_REQUEST_HEADER_SIZE {
                    frame.bulk[BLK_REQUEST_HEADER_SIZE..].to_vec()
                } else {
                    Vec::new()
                };
                let request = BlkRequest {
                    header,
                    payload_grant,
                    bulk,
                };
                let reply = f(request);
                self.write_reply(reply, frame.label)
            }
            Err(_decode_err) => {
                // Malformed peer: build a synthetic InvalidRequest
                // reply referencing the peer's `cmd_id` if we can
                // salvage it, zero otherwise. Either way the peer
                // gets a well-formed reply, not a panic or silent
                // drop.
                let cmd_id = recover_cmd_id(&frame.bulk);
                let reply = BlkReply {
                    header: BlkReplyHeader {
                        cmd_id,
                        status: BlockDriverError::InvalidRequest,
                        bytes: 0,
                    },
                    payload_grant: 0,
                    bulk: Vec::new(),
                };
                self.write_reply(reply, frame.label)
            }
        }
    }

    /// Stage the encoded reply header + any bulk read-data, then
    /// send the reply envelope. The block protocol carries the
    /// real reply state in `store_reply_bulk`; the envelope label
    /// only exists for routing.
    fn write_reply(&self, reply: BlkReply, request_label: u64) -> Result<(), DriverRuntimeError> {
        let header_bytes = encode_blk_reply(reply.header, reply.payload_grant);
        let mut combined = Vec::with_capacity(header_bytes.len() + reply.bulk.len());
        combined.extend_from_slice(&header_bytes);
        combined.extend_from_slice(&reply.bulk);

        let mut be = self.backend.lock();
        be.store_reply_bulk(&combined)?;
        be.reply(request_label, 0)
    }
}

/// Salvage `cmd_id` (bytes 2..10 of the request header) if the peer
/// sent at least that many bytes. Otherwise return zero. Used only
/// for the `InvalidRequest` reply path where the full decode failed
/// but we still want to quote the peer's command id back at them
/// for log correlation. Pure function — no panics on any input.
fn recover_cmd_id(bytes: &[u8]) -> u64 {
    if bytes.len() < 10 {
        return 0;
    }
    u64::from_le_bytes([
        bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
    ])
}

// ---------------------------------------------------------------------------
// Tests — C.4 Red commit pins the behavior `handle_next` must satisfy.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::mock::MockBackend;
    use crate::ipc::{EndpointCap, RecvFrame};
    use alloc::vec;

    fn endpoint() -> EndpointCap {
        EndpointCap::new(7)
    }

    fn read_request_bytes(cmd_id: u64, lba: u64, sector_count: u32, payload_grant: u32) -> Vec<u8> {
        let hdr = BlkRequestHeader {
            kind: BLK_READ,
            cmd_id,
            lba,
            sector_count,
            flags: 0,
        };
        encode_blk_request(hdr, payload_grant).to_vec()
    }

    #[test]
    fn handle_next_passes_decoded_request_shape_to_closure_and_serialises_reply() {
        let mut mock = MockBackend::new();
        mock.push_request(RecvFrame {
            label: 0xABCD,
            data0: 0,
            bulk: read_request_bytes(0x42, 0x1000, 4, 0xdead_beef),
        });

        let server = BlockServer::with_backend(endpoint(), mock);
        let observed: core::cell::RefCell<Option<BlkRequest>> = core::cell::RefCell::new(None);
        let reply_bulk: Vec<u8> = (0u8..(4 * 32)).collect();

        let result = server.handle_next(|req| {
            *observed.borrow_mut() = Some(req.clone());
            BlkReply {
                header: BlkReplyHeader {
                    cmd_id: req.header.cmd_id,
                    status: BlockDriverError::Ok,
                    bytes: reply_bulk.len() as u32,
                },
                payload_grant: 0xfeed_face,
                bulk: reply_bulk.clone(),
            }
        });
        assert!(result.is_ok());

        let seen = observed.borrow().clone().expect("closure ran");
        assert_eq!(seen.header.kind, BLK_READ);
        assert_eq!(seen.header.cmd_id, 0x42);
        assert_eq!(seen.header.lba, 0x1000);
        assert_eq!(seen.header.sector_count, 4);
        assert_eq!(seen.payload_grant, 0xdead_beef);

        let mock = server.backend.lock();
        assert_eq!(mock.replies.len(), 1);
        let rep = &mock.replies[0];
        assert_eq!(rep.label, 0xABCD);
        assert!(rep.bulk.len() >= BLK_REPLY_HEADER_SIZE);
        let (back, grant) =
            decode_blk_reply(&rep.bulk[..BLK_REPLY_HEADER_SIZE]).expect("reply header round-trips");
        assert_eq!(back.cmd_id, 0x42);
        assert_eq!(back.status, BlockDriverError::Ok);
        assert_eq!(back.bytes as usize, reply_bulk.len());
        assert_eq!(grant, 0xfeed_face);
        assert_eq!(&rep.bulk[BLK_REPLY_HEADER_SIZE..], reply_bulk.as_slice());
    }

    #[test]
    fn handle_next_surfaces_write_bulk_to_closure() {
        let hdr = BlkRequestHeader {
            kind: BLK_WRITE,
            cmd_id: 9,
            lba: 0x200,
            sector_count: 1,
            flags: 0,
        };
        let header_bytes = encode_blk_request(hdr, 0x1234_5678).to_vec();
        let write_data: Vec<u8> = (0u8..128).collect();
        let mut full = header_bytes;
        full.extend_from_slice(&write_data);

        let mut mock = MockBackend::new();
        mock.push_request(RecvFrame {
            label: 0,
            data0: 0,
            bulk: full,
        });

        let server = BlockServer::with_backend(endpoint(), mock);
        let observed_bulk: core::cell::RefCell<Option<Vec<u8>>> = core::cell::RefCell::new(None);
        let _ = server.handle_next(|req| {
            *observed_bulk.borrow_mut() = Some(req.bulk.clone());
            BlkReply {
                header: BlkReplyHeader {
                    cmd_id: req.header.cmd_id,
                    status: BlockDriverError::Ok,
                    bytes: 0,
                },
                payload_grant: 0,
                bulk: Vec::new(),
            }
        });
        assert_eq!(
            observed_bulk.borrow().as_deref(),
            Some(write_data.as_slice())
        );
    }

    #[test]
    fn handle_next_malformed_request_replies_invalid_request_without_panicking() {
        // Build a wire payload with a garbage `kind` so decode fails
        // but `cmd_id` bytes are recoverable.
        let mut garbage = vec![0u8; BLK_REQUEST_HEADER_SIZE];
        garbage[0] = 0xff;
        garbage[1] = 0xee;
        let cmd_id = 0xdead_beef_cafe_f00d_u64.to_le_bytes();
        garbage[2..10].copy_from_slice(&cmd_id);

        let mut mock = MockBackend::new();
        mock.push_request(RecvFrame {
            label: 0x1234,
            data0: 0,
            bulk: garbage,
        });

        let server = BlockServer::with_backend(endpoint(), mock);
        let closure_called = core::cell::Cell::new(false);
        let result = server.handle_next(|_| {
            closure_called.set(true);
            BlkReply {
                header: BlkReplyHeader {
                    cmd_id: 0,
                    status: BlockDriverError::Ok,
                    bytes: 0,
                },
                payload_grant: 0,
                bulk: Vec::new(),
            }
        });
        assert!(result.is_ok());
        assert!(!closure_called.get(), "closure must not run on malformed");

        let mock = server.backend.lock();
        let rep = &mock.replies[0];
        assert_eq!(rep.label, 0x1234);
        let (back, grant) =
            decode_blk_reply(&rep.bulk[..BLK_REPLY_HEADER_SIZE]).expect("reply decodes");
        assert_eq!(back.status, BlockDriverError::InvalidRequest);
        assert_eq!(back.cmd_id, 0xdead_beef_cafe_f00d);
        assert_eq!(grant, 0);
    }

    #[test]
    fn handle_next_surfaces_backend_recv_error() {
        let server = BlockServer::with_backend(endpoint(), MockBackend::new());
        let result = server.handle_next(|_req| BlkReply {
            header: BlkReplyHeader {
                cmd_id: 0,
                status: BlockDriverError::Ok,
                bytes: 0,
            },
            payload_grant: 0,
            bulk: Vec::new(),
        });
        assert!(result.is_err());
    }

    // -- Track E.1 test ----------------------------------------------------

    #[test]
    fn block_server_handle_next_ignores_notification_variant() {
        let mut mock = MockBackend::new();
        mock.push_notification(0b1111);

        let server = BlockServer::with_backend(endpoint(), mock);
        let closure_called = core::cell::Cell::new(false);

        let result = server.handle_next(|_req| {
            closure_called.set(true);
            BlkReply {
                header: BlkReplyHeader {
                    cmd_id: 0,
                    status: BlockDriverError::Ok,
                    bytes: 0,
                },
                payload_grant: 0,
                bulk: Vec::new(),
            }
        });

        assert!(result.is_ok(), "notification wake must return Ok");
        assert!(
            !closure_called.get(),
            "closure must not be invoked on notification wake"
        );

        // No reply must have been sent.
        let mock = server.backend.lock();
        assert_eq!(mock.replies.len(), 0, "no reply for notification wake");
    }

    #[test]
    fn block_server_handle_next_dispatches_notification_variant() {
        const NOTIF_BITS: u64 = 0b1111;
        let mut mock = MockBackend::new();
        mock.push_notification(NOTIF_BITS);

        let server = BlockServer::with_backend(endpoint(), mock);
        let closure_called = core::cell::Cell::new(false);
        let observed_bits = core::cell::Cell::new(0u64);

        let result = server.handle_next_with_notification(
            |_req| {
                closure_called.set(true);
                BlkReply {
                    header: BlkReplyHeader {
                        cmd_id: 0,
                        status: BlockDriverError::Ok,
                        bytes: 0,
                    },
                    payload_grant: 0,
                    bulk: Vec::new(),
                }
            },
            |bits| observed_bits.set(bits),
        );

        assert!(result.is_ok(), "notification wake must return Ok");
        assert!(
            !closure_called.get(),
            "message closure must not be invoked on notification wake"
        );
        assert_eq!(
            observed_bits.get(),
            NOTIF_BITS,
            "notification callback must receive drained bits"
        );

        let mock = server.backend.lock();
        assert_eq!(mock.replies.len(), 0, "no reply for notification wake");
    }
}
