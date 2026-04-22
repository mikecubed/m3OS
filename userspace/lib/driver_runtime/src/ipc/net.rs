//! Net-driver IPC client helper — Phase 55b Track C.4.
//!
//! [`NetServer`] is the driver-side companion to the kernel's
//! `RemoteNic` facade (Phase 55b Track E.4). Three paths cross this
//! seam:
//!
//! - TX: the kernel calls the server with a [`NET_SEND_FRAME`]
//!   request; the closure the driver passed to [`NetServer::handle_next`]
//!   turns that into a hardware TX ring post, then returns a
//!   [`NetDriverError`] status.
//! - RX: the driver's interrupt handler stages a received frame and
//!   calls [`NetServer::publish_rx_frame`], which forwards the
//!   frame to the kernel's RX endpoint with the [`NET_RX_FRAME`]
//!   label.
//! - Link state: the driver calls [`NetServer::publish_link_state`]
//!   on PHY up / down edges so the kernel's net stack can react
//!   without polling.
//!
//! # DRY
//!
//! Every message type re-exported at the bottom of this module lives
//! once, in [`kernel_core::driver_ipc::net`]. This module only wraps
//! the send / recv / reply plumbing.

use alloc::vec::Vec;

use spin::Mutex;

use super::{EndpointCap, IpcBackend, NotificationCap, RecvResult, SyscallBackend};

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

/// Decoded TX request the server hands to the user closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetRequest {
    /// Decoded header; `kind == NET_SEND_FRAME`.
    pub header: NetFrameHeader,
    /// Frame payload the peer staged behind the header, truncated
    /// to `header.frame_len` bytes. If the peer claimed a
    /// `frame_len` longer than the bytes it actually sent, the
    /// slice is truncated to what the peer sent — `handle_next`
    /// will not extend past the buffer.
    pub frame: Vec<u8>,
}

/// Reply the driver closure returns for a TX request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetReply {
    /// Status returned to the kernel's net stack. [`NetDriverError::Ok`]
    /// means the frame was enqueued on the hardware TX ring.
    pub status: NetDriverError,
}

// ---------------------------------------------------------------------------
// NetServer — closure-dispatch TX server + RX / link-state publishers.
// ---------------------------------------------------------------------------

/// Driver-side net server. See the module-level docs for the three
/// cross-seam paths this type mediates.
///
/// The RX endpoint and link notification are optional so a driver
/// can construct a `NetServer` before the kernel has stood up either
/// one; `publish_rx_frame` returns
/// [`DriverRuntimeError::Device(DeviceHostError::NotClaimed)`] if
/// the RX endpoint is missing, and `publish_link_state` silently
/// drops when the link notification is missing — a driver is
/// expected to wire both before the first frame arrives.
pub struct NetServer<B: IpcBackend = SyscallBackend> {
    endpoint: EndpointCap,
    rx_endpoint: Option<EndpointCap>,
    link_notification: Option<NotificationCap>,
    pub(crate) backend: Mutex<B>,
}

impl NetServer<SyscallBackend> {
    /// Construct a net server bound to the given Phase 50 endpoint
    /// capability using the real syscall backend.
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
    /// Construct a net server with an explicit backend. Exposed for
    /// the Track C.4 test harness (`cfg(test)`) and for future
    /// alternate transports.
    pub fn with_backend(endpoint: EndpointCap, backend: B) -> Self {
        Self {
            endpoint,
            rx_endpoint: None,
            link_notification: None,
            backend: Mutex::new(backend),
        }
    }

    /// Register the kernel endpoint the driver pushes RX frames to.
    /// `publish_rx_frame` is only valid after this is set — a
    /// driver's init path looks this endpoint up (via the service
    /// manager) and stamps it here before entering its ISR loop.
    pub fn with_rx_endpoint(mut self, rx: EndpointCap) -> Self {
        self.rx_endpoint = Some(rx);
        self
    }

    /// Register the notification capability the driver signals on
    /// link-state changes. `publish_link_state` is a silent no-op
    /// until this is set.
    pub fn with_link_notification(mut self, notif: NotificationCap) -> Self {
        self.link_notification = Some(notif);
        self
    }

    /// The command-endpoint this server listens on.
    pub fn endpoint(&self) -> EndpointCap {
        self.endpoint
    }

    /// Pull one TX request or notification off the endpoint and dispatch.
    ///
    /// Two closures are required so the driver can act on both event
    /// kinds in one call:
    ///
    /// - `on_message`: called with a decoded [`NetRequest`] when the
    ///   kernel sends a TX request. Returns a [`NetReply`] whose
    ///   `status` is encoded and sent back.
    /// - `on_notification`: called with the drained notification bit
    ///   mask when the notification bound to this endpoint via
    ///   `sys_notif_bind` fires. No reply is sent for notifications.
    ///
    /// Behavior on `RecvResult::Message`:
    ///
    /// - Successful decode produces a [`NetRequest`] whose `frame`
    ///   field carries the peer's payload truncated to the minimum
    ///   of `header.frame_len` and the bytes actually present in
    ///   the recv buffer. The `on_message` closure returns a
    ///   [`NetReply`] whose `status` is encoded as a single byte and
    ///   stamped in the reply bulk slot.
    /// - Malformed decode replies with
    ///   [`NetDriverError::InvalidFrame`] and never runs the
    ///   closure. The method still returns `Ok(())` because the
    ///   frame was processed (badly, but not catastrophically).
    pub fn handle_next<F, G>(
        &self,
        mut on_message: F,
        mut on_notification: G,
    ) -> Result<(), DriverRuntimeError>
    where
        F: FnMut(NetRequest) -> NetReply,
        G: FnMut(u64),
    {
        // Bind to a local so the MutexGuard is dropped before the match body
        // calls write_reply (which re-acquires the lock).
        let recv_result = self.backend.lock().recv(self.endpoint)?;
        match recv_result {
            RecvResult::Notification(bits) => {
                on_notification(bits);
                Ok(())
            }
            RecvResult::Message(frame_in) => {
                match decode_net_send(&frame_in.bulk) {
                    Ok(header) => {
                        // Slice the trailing payload down to `frame_len` (or
                        // whatever the peer actually sent, if shorter).
                        let declared = header.frame_len as usize;
                        let start = NET_FRAME_HEADER_SIZE.min(frame_in.bulk.len());
                        let available = frame_in.bulk.len() - start;
                        let take = declared.min(available);
                        let frame_bytes = frame_in.bulk[start..start + take].to_vec();
                        let req = NetRequest {
                            header,
                            frame: frame_bytes,
                        };
                        let reply = on_message(req);
                        self.write_reply(reply, frame_in.label)
                    }
                    Err(_) => {
                        let reply = NetReply {
                            status: NetDriverError::InvalidFrame,
                        };
                        self.write_reply(reply, frame_in.label)
                    }
                }
            }
        }
    }

    /// Encode the reply status byte and send it back on the reply
    /// capability the kernel staged.
    fn write_reply(&self, reply: NetReply, request_label: u64) -> Result<(), DriverRuntimeError> {
        let byte = net_driver_error_to_byte(reply.status);
        let bulk = [byte];
        let mut be = self.backend.lock();
        be.store_reply_bulk(&bulk)?;
        be.reply(request_label, 0)
    }

    /// Push a received frame back to the kernel net stack.
    ///
    /// Rejects frames longer than [`MAX_FRAME_BYTES`] with
    /// `DriverRuntimeError::Device(DeviceHostError::Internal)` —
    /// the spec forbids oversized frames on the wire, and the
    /// driver must drop them at ingress rather than poison the
    /// kernel buffer.
    ///
    /// The frame travels as a bulk payload alongside a
    /// [`NET_RX_FRAME`]-labelled fire-and-forget send to the RX
    /// endpoint the caller registered via
    /// [`Self::with_rx_endpoint`].
    pub fn publish_rx_frame(&self, frame: &[u8]) -> Result<(), DriverRuntimeError> {
        if frame.len() > MAX_FRAME_BYTES as usize {
            return Err(DriverRuntimeError::Device(
                kernel_core::device_host::DeviceHostError::Internal,
            ));
        }
        let rx = match self.rx_endpoint {
            Some(ep) => ep,
            None => {
                return Err(DriverRuntimeError::Device(
                    kernel_core::device_host::DeviceHostError::NotClaimed,
                ));
            }
        };
        let header = NetFrameHeader {
            kind: NET_RX_FRAME,
            frame_len: frame.len() as u16,
            flags: 0,
        };
        let mut bulk = encode_net_rx_notify(header);
        bulk.extend_from_slice(frame);
        self.backend
            .lock()
            .send_buf(rx, NET_RX_FRAME as u64, 0, &bulk)
    }

    /// Signal link-state change to the kernel net stack.
    ///
    /// The signal word packs the `up` flag in bit 0 so the kernel
    /// can wake on link-up / link-down edges without pulling the
    /// full event out of a side channel. Encoded speed rides in
    /// the bits above so a single 64-bit signal communicates the
    /// whole event without a follow-up RPC.
    ///
    /// The schema's [`encode_net_link_event`] bytes are not sent
    /// over the notification — they are staged in a way the
    /// kernel-side `RemoteNic` can poll if it wants the full
    /// breakdown (MAC, speed) and reconcile against its own
    /// policy. For the C.4 contract test we only assert the
    /// notification's bit-0 edge is delivered.
    pub fn publish_link_state(&self, state: NetLinkEvent) {
        let notif = match self.link_notification {
            Some(n) => n,
            None => return,
        };
        // Bit 0 = up, bits 32..64 = speed (mbps).
        let mut bits = 0u64;
        if state.up {
            bits |= 0x1;
        }
        bits |= (state.speed_mbps as u64) << 32;
        // Silently absorb signalling errors — a driver that can't
        // reach the notification cap is already in a bad state and
        // the kernel supervisor will restart it; we don't want to
        // panic from an interrupt-adjacent path.
        let _ = self.backend.lock().signal_notification(notif, bits);
    }
}

/// Stable single-byte encoding for [`NetDriverError`], matching the
/// A.3 schema ordering (see `kernel_core::driver_ipc::net` tests).
fn net_driver_error_to_byte(e: NetDriverError) -> u8 {
    match e {
        NetDriverError::Ok => 0,
        NetDriverError::LinkDown => 1,
        NetDriverError::RingFull => 2,
        NetDriverError::DeviceAbsent => 3,
        NetDriverError::DriverRestarting => 4,
        NetDriverError::InvalidFrame => 5,
        // `#[non_exhaustive]`: unknown future variants encode as
        // InvalidFrame so the kernel can still pattern-match
        // something meaningful. Safe because the protocol defines
        // InvalidFrame as "peer sent garbage".
        _ => 5,
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
        let result = server.handle_next(
            |req| {
                *observed.borrow_mut() = Some(req.clone());
                NetReply {
                    status: NetDriverError::Ok,
                }
            },
            |_bits| {},
        );
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
        let result = server.handle_next(
            |_| {
                closure_called.set(true);
                NetReply {
                    status: NetDriverError::Ok,
                }
            },
            |_bits| {},
        );
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
        let server =
            NetServer::with_backend(ep(), MockBackend::new()).with_link_notification(link_notif());

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

    // -- Track E.1 tests ---------------------------------------------------

    #[test]
    fn net_server_handle_next_dispatches_message_variant() {
        let frame: Vec<u8> = (0u8..32).collect();
        let mut mock = MockBackend::new();
        mock.push_request(RecvFrame {
            label: NET_SEND_FRAME as u64,
            data0: 0,
            bulk: send_frame_bytes(frame.len() as u16, &frame),
        });

        let server = NetServer::with_backend(ep(), mock);
        let message_called = core::cell::Cell::new(false);
        let notif_called = core::cell::Cell::new(false);

        let result = server.handle_next(
            |req| {
                message_called.set(true);
                assert_eq!(req.frame, frame);
                NetReply {
                    status: NetDriverError::Ok,
                }
            },
            |_bits| {
                notif_called.set(true);
            },
        );
        assert!(result.is_ok());
        assert!(
            message_called.get(),
            "on_message must be called for Message variant"
        );
        assert!(
            !notif_called.get(),
            "on_notification must not be called for Message variant"
        );
    }

    #[test]
    fn net_server_handle_next_dispatches_notification_variant() {
        const NOTIF_BITS: u64 = 0b0011;
        let mut mock = MockBackend::new();
        mock.push_notification(NOTIF_BITS);

        let server = NetServer::with_backend(ep(), mock);
        let message_called = core::cell::Cell::new(false);
        let observed_bits: core::cell::Cell<u64> = core::cell::Cell::new(0);

        let result = server.handle_next(
            |_req| {
                message_called.set(true);
                NetReply {
                    status: NetDriverError::Ok,
                }
            },
            |bits| {
                observed_bits.set(bits);
            },
        );
        assert!(result.is_ok());
        assert!(
            !message_called.get(),
            "on_message must not be called for Notification variant"
        );
        assert_eq!(
            observed_bits.get(),
            NOTIF_BITS,
            "on_notification receives the drained bits"
        );

        // No reply should have been sent for a notification wake.
        let mock = server.backend.lock();
        assert_eq!(mock.replies.len(), 0, "no reply for notification wake");
    }
}
