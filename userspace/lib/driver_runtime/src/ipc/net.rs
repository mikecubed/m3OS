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

use super::{EndpointCap, IpcBackend, RecvResult, SyscallBackend};

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
/// The kernel ingress endpoint is optional so a driver can construct a
/// `NetServer` before the kernel has stood it up; `publish_rx_frame` returns
/// [`DriverRuntimeError::Device(DeviceHostError::NotClaimed)`] if
/// the ingress endpoint is missing. Drivers are expected to wire the
/// ingress endpoint before the first RX frame or link-state event arrives.
pub struct NetServer<B: IpcBackend = SyscallBackend> {
    endpoint: EndpointCap,
    ingress_endpoint: Option<EndpointCap>,
    pub(crate) backend: Mutex<B>,
}

impl NetServer<SyscallBackend> {
    /// Construct a net server bound to the given Phase 50 endpoint
    /// capability using the real syscall backend.
    pub fn new(endpoint: EndpointCap) -> Self {
        Self {
            endpoint,
            ingress_endpoint: None,
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
            ingress_endpoint: None,
            backend: Mutex::new(backend),
        }
    }

    /// Register the kernel ingress endpoint the driver publishes both RX
    /// frames and link-state events to.
    pub fn with_ingress_endpoint(mut self, ingress: EndpointCap) -> Self {
        self.ingress_endpoint = Some(ingress);
        self
    }

    /// Back-compat alias for older call sites that described the same kernel
    /// endpoint as the RX endpoint.
    pub fn with_rx_endpoint(self, rx: EndpointCap) -> Self {
        self.with_ingress_endpoint(rx)
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
    /// [`NET_RX_FRAME`]-labelled fire-and-forget send to the kernel ingress
    /// endpoint the caller registered via [`Self::with_ingress_endpoint`].
    pub fn publish_rx_frame(&self, frame: &[u8]) -> Result<(), DriverRuntimeError> {
        self.publish_rx_frames(core::slice::from_ref(&frame))
    }

    /// Batch variant of [`Self::publish_rx_frame`] — concatenates multiple
    /// RX frames into a single `NET_RX_FRAME` bulk so the driver pays one
    /// synchronous IPC rendezvous per drain pass instead of one per frame.
    ///
    /// The kernel-side ingress parser (`RemoteNic::inject_rx_frame`) walks
    /// the bulk record-by-record, so a single-record bulk is identical to
    /// the pre-batch protocol. Frames whose combined header+payload would
    /// exceed the kernel's IPC bulk limit are split into multiple sends
    /// (each sub-batch small enough to fit).
    ///
    /// Returns the first error encountered on the underlying `send_buf`
    /// call. Frames in sub-batches before the failure were already delivered.
    pub fn publish_rx_frames(&self, frames: &[&[u8]]) -> Result<(), DriverRuntimeError> {
        if frames.is_empty() {
            return Ok(());
        }
        for frame in frames {
            if frame.len() > MAX_FRAME_BYTES as usize {
                return Err(DriverRuntimeError::Device(
                    kernel_core::device_host::DeviceHostError::Internal,
                ));
            }
        }
        let ingress = match self.ingress_endpoint {
            Some(ep) => ep,
            None => {
                return Err(DriverRuntimeError::Device(
                    kernel_core::device_host::DeviceHostError::NotClaimed,
                ));
            }
        };

        // Kernel `ipc_send_buf` caps bulk at 4 KiB. Group frames into
        // sub-batches small enough to fit; the common case for interactive
        // workloads is 1–2 frames per drain, which fits in a single send.
        const MAX_BULK_BYTES: usize = 4096;
        let mut bulk: Vec<u8> = Vec::new();

        for frame in frames {
            let record_size = NET_FRAME_HEADER_SIZE + frame.len();
            if !bulk.is_empty() && bulk.len() + record_size > MAX_BULK_BYTES {
                self.backend
                    .lock()
                    .send_buf(ingress, NET_RX_FRAME as u64, 0, &bulk)?;
                bulk.clear();
            }
            let header = NetFrameHeader {
                kind: NET_RX_FRAME,
                frame_len: frame.len() as u16,
                flags: 0,
            };
            bulk.extend_from_slice(&encode_net_rx_notify(header));
            bulk.extend_from_slice(frame);
        }

        if !bulk.is_empty() {
            self.backend
                .lock()
                .send_buf(ingress, NET_RX_FRAME as u64, 0, &bulk)?;
        }
        Ok(())
    }

    /// Publish a `NET_LINK_STATE` event to the kernel net stack.
    pub fn publish_link_state(&self, state: NetLinkEvent) -> Result<(), DriverRuntimeError> {
        let ingress = match self.ingress_endpoint {
            Some(ep) => ep,
            None => {
                return Err(DriverRuntimeError::Device(
                    kernel_core::device_host::DeviceHostError::NotClaimed,
                ));
            }
        };
        let bulk = encode_net_link_event(state);
        self.backend
            .lock()
            .send_buf(ingress, NET_LINK_STATE as u64, 0, &bulk)
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
    use crate::ipc::{EndpointCap, RecvFrame};

    fn ep() -> EndpointCap {
        EndpointCap::new(11)
    }
    fn rx_ep() -> EndpointCap {
        EndpointCap::new(12)
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
        let server =
            NetServer::with_backend(ep(), MockBackend::new()).with_ingress_endpoint(rx_ep());

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
    fn publish_rx_frames_batches_multiple_frames_into_one_send() {
        let server =
            NetServer::with_backend(ep(), MockBackend::new()).with_ingress_endpoint(rx_ep());

        let frame_a: Vec<u8> = (0u8..16).collect();
        let frame_b: Vec<u8> = (16u8..48).collect();
        let frame_c: Vec<u8> = (48u8..80).collect();
        let frames: Vec<&[u8]> =
            alloc::vec![frame_a.as_slice(), frame_b.as_slice(), frame_c.as_slice()];
        server.publish_rx_frames(&frames).expect("batch publish");

        let mock = server.backend.lock();
        // Three small frames easily fit in one 4 KiB bulk — one send_buf.
        assert_eq!(mock.sends.len(), 1);
        let send = &mock.sends[0];
        assert_eq!(send.label, NET_RX_FRAME as u64);

        // Walk the bulk: each record is [header, frame_bytes], concatenated.
        let mut pos = 0;
        for expected in [&frame_a, &frame_b, &frame_c] {
            let hdr = decode_net_rx_notify(&send.bulk[pos..pos + NET_FRAME_HEADER_SIZE])
                .expect("header decodes");
            assert_eq!(hdr.kind, NET_RX_FRAME);
            assert_eq!(hdr.frame_len as usize, expected.len());
            pos += NET_FRAME_HEADER_SIZE;
            assert_eq!(&send.bulk[pos..pos + expected.len()], expected.as_slice());
            pos += expected.len();
        }
        assert_eq!(pos, send.bulk.len());
    }

    #[test]
    fn publish_rx_frames_splits_into_sub_batches_when_bulk_would_overflow() {
        let server =
            NetServer::with_backend(ep(), MockBackend::new()).with_ingress_endpoint(rx_ep());

        // Two max-size frames would exceed 4 KiB when concatenated with their
        // headers, so the publisher must split them across two send_buf calls.
        let big = alloc::vec![0x55u8; MAX_FRAME_BYTES as usize];
        let frames: Vec<&[u8]> = alloc::vec![big.as_slice(), big.as_slice(), big.as_slice()];
        server
            .publish_rx_frames(&frames)
            .expect("split-batch publish");

        let mock = server.backend.lock();
        // 3 × (1522 + 8) = 4590 bytes total — must be at least two sub-batches
        // since a single 4 KiB bulk holds at most two full-size frames.
        assert!(
            mock.sends.len() >= 2,
            "expected >= 2 sub-batches, got {}",
            mock.sends.len()
        );
        for send in &mock.sends {
            assert!(
                send.bulk.len() <= 4096,
                "sub-batch bulk exceeds kernel limit: {}",
                send.bulk.len()
            );
        }
    }

    #[test]
    fn publish_rx_frames_rejects_oversized_frame_and_sends_nothing() {
        let server =
            NetServer::with_backend(ep(), MockBackend::new()).with_ingress_endpoint(rx_ep());
        let oversized = alloc::vec![0u8; (MAX_FRAME_BYTES as usize) + 1];
        let frames: Vec<&[u8]> = alloc::vec![oversized.as_slice()];
        assert!(server.publish_rx_frames(&frames).is_err());
        let mock = server.backend.lock();
        assert_eq!(mock.sends.len(), 0);
    }

    #[test]
    fn publish_link_state_emits_link_event_on_ingress_endpoint() {
        let server =
            NetServer::with_backend(ep(), MockBackend::new()).with_ingress_endpoint(rx_ep());

        let event = NetLinkEvent {
            up: true,
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            speed_mbps: 1000,
        };
        server
            .publish_link_state(event)
            .expect("link-state publish must send on the ingress endpoint");

        let mock = server.backend.lock();
        assert_eq!(mock.sends.len(), 1);
        let send = &mock.sends[0];
        assert_eq!(send.endpoint, rx_ep());
        assert_eq!(send.label, NET_LINK_STATE as u64);
        let decoded = decode_net_link_event(&send.bulk).expect("link-state payload decodes");
        assert_eq!(decoded, event);
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
