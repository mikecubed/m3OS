//! Block-driver IPC client helper (Phase 55b Track C.1 stub — Track C.4 fills in).
//!
//! Track C.1 re-exports the block-driver IPC schema from
//! `kernel-core::driver_ipc::block` so the eventual Track C.4 helper
//! and the Track D.4 `RemoteBlockDevice` kernel facade speak exactly
//! the same types — per the Phase 55b DRY rule that each schema lives
//! once. The ergonomic request / reply round-trip helper
//! (`BlkClient::read_sectors`, `write_sectors`) lands in Track C.4.

pub use kernel_core::driver_ipc::block::{
    BLK_READ, BLK_REPLY_HEADER_SIZE, BLK_REQUEST_HEADER_SIZE, BLK_STATUS, BLK_WRITE,
    BlkReplyHeader, BlkRequestHeader, BlockDriverError, DecodeError, MAX_SECTORS_PER_REQUEST,
    decode_blk_reply, decode_blk_request, encode_blk_reply, encode_blk_request,
};
