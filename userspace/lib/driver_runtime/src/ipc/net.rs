//! Net-driver IPC client helper — Phase 55b Track C.4.
//!
//! **Red-commit stub.** Lands the public surface with broken bodies
//! so the unit tests below fail. Green commit fills in the real
//! decode / dispatch / publish paths.
//!
//! # DRY
//!
//! Every message type re-exported at the bottom of this module lives
//! once, in [`kernel_core::driver_ipc::net`]. This module only wraps
//! the send / recv / reply plumbing.

use alloc::vec::Vec;

use spin::Mutex;

use super::{EndpointCap, IpcBackend, NotificationCap, SyscallBackend};

pub use kernel_core::driver_ipc::net::{
    MAX_FRAME_BYTES, NET_FRAME_HEADER_SIZE, NET_LINK_EVENT_BODY_SIZE, NET_LINK_EVENT_SIZE,
    NET_LINK_STATE, NET_RX_FRAME, NET_SEND_FRAME, NetDriverError, NetFrameHeader, NetLinkEvent,
    decode_net_link_event, decode_net_rx_notify, decode_net_send, encode_net_link_event,
    encode_net_rx_notify, encode_net_send,
};

use crate::DriverRuntimeError;

// ---------------------------------------------------------------------------
// NetRequest / NetReply
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetRequest {
    /// Decoded header; `kind == NET_SEND_FRAME`.
    pub header: NetFrameHeader,
    /// Frame payload the peer staged behind the header, truncated
    /// to `header.frame_len` bytes.
    pub frame: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetReply {
    /// Status returned to the caller.
    pub status: NetDriverError,
}

// ---------------------------------------------------------------------------
// NetServer — Red stub.
// ---------------------------------------------------------------------------

pub struct NetServer<B: IpcBackend = SyscallBackend> {
    endpoint: EndpointCap,
    rx_endpoint: Option<EndpointCap>,
    link_notification: Option<NotificationCap>,
    pub(crate) backend: Mutex<B>,
}

impl NetServer<SyscallBackend> {
    pub fn new(endpoint: EndpointCap) -> Self {
        Self {
            endpoint,
            rx_endpoint: None,
            link_notification: None,
            backend: Mutex::new(SyscallBackend),
        }
    }
}

impl<B: IpcBackend> NetServer<B> {
    pub fn with_backend(endpoint: EndpointCap, backend: B) -> Self {
        Self {
            endpoint,
            rx_endpoint: None,
            link_notification: None,
            backend: Mutex::new(backend),
        }
    }

    /// Register the kernel endpoint the driver pushes RX frames to.
    /// `publish_rx_frame` is only valid after this is set.
    pub fn with_rx_endpoint(mut self, rx: EndpointCap) -> Self {
        self.rx_endpoint = Some(rx);
        self
    }

    /// Register the notification capability the driver signals on
    /// link-state changes. `publish_link_state` is only observable
    /// on the kernel side once this is set.
    pub fn with_link_notification(mut self, notif: NotificationCap) -> Self {
        self.link_notification = Some(notif);
        self
    }

    pub fn endpoint(&self) -> EndpointCap {
        self.endpoint
    }

    /// Red-commit stub.
    pub fn handle_next<F>(&self, _f: F) -> Result<(), DriverRuntimeError>
    where
        F: FnMut(NetRequest) -> NetReply,
    {
        Err(DriverRuntimeError::Device(
            kernel_core::device_host::DeviceHostError::Internal,
        ))
    }

    /// Red-commit stub.
    pub fn publish_rx_frame(&self, _frame: &[u8]) -> Result<(), DriverRuntimeError> {
        Err(DriverRuntimeError::Device(
            kernel_core::device_host::DeviceHostError::Internal,
        ))
    }

    /// Red-commit stub — no-op.
    pub fn publish_link_state(&self, _state: NetLinkEvent) {
        // Red stub does nothing so the recorded-signals assertions
        // below fail.
    }
}

// ---------------------------------------------------------------------------
// Tests — C.4 Red commit pins behavior the Green implementation must satisfy.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::mock::MockBackend;
    use crate::ipc::{EndpointCap, NotificationCap, RecvFrame};

    fn ep() -> EndpointCap {
        EndpointCap::new(11)
    }
    fn rx_ep() -> EndpointCap {
        EndpointCap::new(12)
    }
    fn link_notif() -> NotificationCap {
        NotificationCap::new(13)
    }

    fn send_frame_bytes(frame_len: u16, frame: &[u8]) -> Vec<u8> {
        let header = NetFrameHeader {
            kind: NET_SEND_FRAME,
            frame_len,
            flags: 0,
        };
        let mut out = encode_net_send(header);
        out.extend_from_slice(frame);
        out
    }

    #[test]
    fn handle_next_passes_decoded_frame_to_closure_and_replies_ok() {
        let frame: Vec<u8> = (0u8..=127).collect();
        let mut mock = MockBackend::new();
        mock.push_request(RecvFrame {
            label: NET_SEND_FRAME as u64,
            data0: 0,
            bulk: send_frame_bytes(frame.len() as u16, &frame),
        });

        let server = NetServer::with_backend(ep(), mock);
        let observed: core::cell::RefCell<Option<NetRequest>> = core::cell::RefCell::new(None);
        let result = server.handle_next(|req| {
            *observed.borrow_mut() = Some(req.clone());
            NetReply {
                status: NetDriverError::Ok,
            }
        });
        assert!(result.is_ok());

        let seen = observed.borrow().clone().expect("closure ran");
        assert_eq!(seen.header.kind, NET_SEND_FRAME);
        assert_eq!(seen.header.frame_len as usize, frame.len());
        assert_eq!(seen.frame, frame);

        let mock = server.backend.lock();
        assert_eq!(mock.replies.len(), 1);
        let rep = &mock.replies[0];
        // The reply bulk carries the encoded NetDriverError byte so
        // the kernel-side `RemoteNic` facade can decode the status.
        assert_eq!(rep.bulk.len(), 1);
        assert_eq!(rep.bulk[0], 0);
    }

    #[test]
    fn handle_next_malformed_request_replies_invalid_frame_without_panicking() {
        let mut mock = MockBackend::new();
        mock.push_request(RecvFrame {
            label: NET_SEND_FRAME as u64,
            data0: 0,
            // Empty bulk — decode_net_send rejects as InvalidFrame.
            bulk: Vec::new(),
        });

        let server = NetServer::with_backend(ep(), mock);
        let closure_called = core::cell::Cell::new(false);
        let result = server.handle_next(|_| {
            closure_called.set(true);
            NetReply {
                status: NetDriverError::Ok,
            }
        });
        assert!(result.is_ok());
        assert!(!closure_called.get(), "closure must not run on malformed");

        let mock = server.backend.lock();
        let rep = &mock.replies[0];
        assert_eq!(rep.bulk.len(), 1);
        // InvalidFrame status byte == 5 per the schema's pinned enum
        // ordering (see the net module tests).
        assert_eq!(rep.bulk[0], 5);
    }

    #[test]
    fn publish_rx_frame_emits_rx_notify_envelope_with_frame_bulk() {
        let server = NetServer::with_backend(ep(), MockBackend::new())
            .with_rx_endpoint(rx_ep())
            .with_link_notification(link_notif());

        let frame: Vec<u8> = (0u8..64).collect();
        let result = server.publish_rx_frame(&frame);
        assert!(result.is_ok());

        let mock = server.backend.lock();
        assert_eq!(mock.sends.len(), 1);
        let send = &mock.sends[0];
        assert_eq!(send.endpoint, rx_ep());
        assert_eq!(send.label, NET_RX_FRAME as u64);
        // The bulk carries the NET_RX_FRAME header followed by the
        // frame bytes.
        assert!(send.bulk.len() >= NET_FRAME_HEADER_SIZE);
        let back =
            decode_net_rx_notify(&send.bulk[..NET_FRAME_HEADER_SIZE]).expect("header decodes");
        assert_eq!(back.kind, NET_RX_FRAME);
        assert_eq!(back.frame_len as usize, frame.len());
        assert_eq!(&send.bulk[NET_FRAME_HEADER_SIZE..], frame.as_slice());
    }

    #[test]
    fn publish_rx_frame_rejects_oversized_frame() {
        let server = NetServer::with_backend(ep(), MockBackend::new()).with_rx_endpoint(rx_ep());
        let oversized = alloc::vec![0u8; (MAX_FRAME_BYTES as usize) + 1];
        let result = server.publish_rx_frame(&oversized);
        assert!(result.is_err());
        let mock = server.backend.lock();
        assert_eq!(mock.sends.len(), 0);
    }

    #[test]
    fn publish_link_state_signals_encoded_link_event_on_notification() {
        let server = NetServer::with_backend(ep(), MockBackend::new())
            .with_link_notification(link_notif());

        let event = NetLinkEvent {
            up: true,
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            speed_mbps: 1000,
        };
        server.publish_link_state(event);

        let mock = server.backend.lock();
        assert_eq!(mock.signals.len(), 1);
        let sig = &mock.signals[0];
        assert_eq!(sig.notif, link_notif());
        // The signal bit word packs the `up` flag so the kernel can
        // wake on link-up / link-down edges without pulling the
        // full event out of a side channel. At minimum the low bit
        // must be set when `up == true`.
        assert_ne!(sig.bits & 0x1, 0);
    }
}
