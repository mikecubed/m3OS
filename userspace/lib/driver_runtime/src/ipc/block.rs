//! Block-driver IPC client helper — Phase 55b Track C.4.
//!
//! **Red-commit stub.** This file lands the public surface (types,
//! constructors, trait-gated `handle_next` signature) with an
//! intentionally broken body so the unit tests in the `tests` module
//! below fail. The Green commit replaces [`BlockServer::handle_next`]
//! with the real decode / dispatch / encode implementation.
//!
//! # DRY
//!
//! Every message type re-exported at the bottom of this module lives
//! once, in [`kernel_core::driver_ipc::block`]. This module only
//! wraps the send / recv / reply plumbing.

use alloc::vec::Vec;

use spin::Mutex;

use super::{EndpointCap, IpcBackend, SyscallBackend};

pub use kernel_core::driver_ipc::block::{
    BLK_READ, BLK_REPLY_HEADER_SIZE, BLK_REQUEST_HEADER_SIZE, BLK_STATUS, BLK_WRITE,
    BlkReplyHeader, BlkRequestHeader, BlockDriverError, DecodeError, MAX_SECTORS_PER_REQUEST,
    decode_blk_reply, decode_blk_request, encode_blk_reply, encode_blk_request,
};

use crate::DriverRuntimeError;

// ---------------------------------------------------------------------------
// BlkRequest / BlkReply — domain types the closure sees.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlkRequest {
    pub header: BlkRequestHeader,
    pub payload_grant: u32,
    pub bulk: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlkReply {
    pub header: BlkReplyHeader,
    pub payload_grant: u32,
    pub bulk: Vec<u8>,
}

// ---------------------------------------------------------------------------
// BlockServer — closure-dispatch server helper (Red stub).
// ---------------------------------------------------------------------------

pub struct BlockServer<B: IpcBackend = SyscallBackend> {
    endpoint: EndpointCap,
    pub(crate) backend: Mutex<B>,
}

impl BlockServer<SyscallBackend> {
    pub fn new(endpoint: EndpointCap) -> Self {
        Self {
            endpoint,
            backend: Mutex::new(SyscallBackend),
        }
    }
}

impl<B: IpcBackend> BlockServer<B> {
    pub fn with_backend(endpoint: EndpointCap, backend: B) -> Self {
        Self {
            endpoint,
            backend: Mutex::new(backend),
        }
    }

    pub fn endpoint(&self) -> EndpointCap {
        self.endpoint
    }

    /// Red-commit stub: always surfaces an error. Green commit
    /// replaces this with the decode / dispatch / reply pipeline.
    pub fn handle_next<F>(&self, _f: F) -> Result<(), DriverRuntimeError>
    where
        F: FnMut(BlkRequest) -> BlkReply,
    {
        Err(DriverRuntimeError::Device(
            kernel_core::device_host::DeviceHostError::Internal,
        ))
    }
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

    fn read_request_bytes(
        cmd_id: u64,
        lba: u64,
        sector_count: u32,
        payload_grant: u32,
    ) -> Vec<u8> {
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
        let (back, grant) = decode_blk_reply(&rep.bulk[..BLK_REPLY_HEADER_SIZE])
            .expect("reply header round-trips");
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
        let observed_bulk: core::cell::RefCell<Option<Vec<u8>>> =
            core::cell::RefCell::new(None);
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
}
