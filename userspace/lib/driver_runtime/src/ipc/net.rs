//! Net-driver IPC client helper (Phase 55b Track C.1 stub — Track C.4 fills in).
//!
//! Track C.1 re-exports the net-driver IPC schema from
//! `kernel-core::driver_ipc::net` so the eventual Track C.4 helper and
//! the Track E.4 `RemoteNic` kernel facade speak exactly the same
//! types. The send-frame / rx-notify helper with bulk-grant plumbing
//! lands in Track C.4.

pub use kernel_core::driver_ipc::net::{
    MAX_FRAME_BYTES, NET_FRAME_HEADER_SIZE, NET_LINK_EVENT_BODY_SIZE, NET_LINK_EVENT_SIZE,
    NET_LINK_STATE, NET_RX_FRAME, NET_SEND_FRAME, NetDriverError, NetFrameHeader, NetLinkEvent,
    decode_net_link_event, decode_net_rx_notify, decode_net_send, encode_net_link_event,
    encode_net_rx_notify, encode_net_send,
};
