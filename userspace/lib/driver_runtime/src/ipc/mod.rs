//! Driver-side IPC client helpers — Phase 55b Track C.4.
//!
//! This module factors the send / receive / reply cycle shared by every
//! block- and net-driver process into a single closure-dispatch helper,
//! so `userspace/drivers/nvme/` and `userspace/drivers/e1000/` contain
//! only device semantics. The authoritative schemas ([`block`] and
//! [`net`]) live in [`kernel_core::driver_ipc`]; this module's job is
//! to turn those schemas into ergonomic server loops.
//!
//! # Backend abstraction
//!
//! [`IpcBackend`] abstracts the minimum set of IPC primitives the
//! helpers consume (`recv_msg`, `reply`, `store_reply_bulk`, `send`,
//! `send_buf`, `signal_notification`). In production a driver process
//! passes the unit-like [`SyscallBackend`] which forwards to
//! `syscall_lib::*`. In tests the suite below uses a pure-logic
//! [`MockBackend`] that records inputs and returns queued replies —
//! this is how `BlockServer::handle_next` and `NetServer::handle_next`
//! are exercised without a real kernel endpoint underneath them.
//!
//! # DRY
//!
//! The schema types themselves live exactly once in
//! [`kernel_core::driver_ipc`] per the Phase 55b DRY rule; this module
//! only *consumes* them.

pub mod block;
pub mod net;

// ---------------------------------------------------------------------------
// EndpointCap — ring-3 handle to a Phase 50 endpoint capability.
// ---------------------------------------------------------------------------

/// Opaque newtype around a Phase 50 capability-table handle pointing at
/// an endpoint capability.
///
/// Phase 50's userspace IPC wrappers (`syscall_lib::ipc_recv_msg`,
/// `ipc_reply`, `ipc_send_buf`) take the raw `u32` handle directly —
/// `EndpointCap` is the thin typed wrapper the C.4 helpers (and, by
/// extension, the D / E drivers) use so a random `u32` cannot be passed
/// where an endpoint handle is expected. The raw handle is accessed via
/// [`EndpointCap::raw`] for interop with [`SyscallBackend`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EndpointCap(u32);

impl EndpointCap {
    /// Wrap a raw capability handle. The handle must have been obtained
    /// from `sys_create_endpoint` / `sys_ipc_lookup_service` — this
    /// newtype does not validate it.
    pub const fn new(handle: u32) -> Self {
        Self(handle)
    }

    /// Raw Phase 50 capability-table handle. Drivers should not need
    /// this; it exists for [`SyscallBackend`] and for
    /// interoperability with `syscall_lib`.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Same newtype, but for a notification capability handle. Drivers
/// publish RX / link-state events on a notification separate from the
/// command endpoint so those events do not block behind an in-flight
/// `handle_next` reply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NotificationCap(u32);

impl NotificationCap {
    /// Wrap a raw capability handle. The handle must have been obtained
    /// from `sys_create_notification` / `sys_device_irq_subscribe`.
    pub const fn new(handle: u32) -> Self {
        Self(handle)
    }

    /// Raw Phase 50 notification-capability handle.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// IpcBackend — abstraction over the Phase 50 IPC syscall surface.
// ---------------------------------------------------------------------------

/// A received IPC frame decoded into the shape the server helpers care
/// about: the message label, the first data word, and a bulk-data
/// payload the kernel copied into the server's recv buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecvFrame {
    /// Message label the peer sent.
    pub label: u64,
    /// First inline data word. Unused by the Phase 55b block / net
    /// protocols (all state rides in the bulk buffer), but surfaced
    /// anyway so future protocols can piggyback on the same backend.
    pub data0: u64,
    /// Bulk-data payload; matches the layout in
    /// [`kernel_core::driver_ipc::block`] or
    /// [`kernel_core::driver_ipc::net`] depending on the peer.
    pub bulk: alloc::vec::Vec<u8>,
}

/// Minimum IPC surface consumed by [`block::BlockServer`] and
/// [`net::NetServer`]. A production driver uses [`SyscallBackend`];
/// tests use the crate-private `MockBackend` in the `#[cfg(test)]`
/// block below.
pub trait IpcBackend {
    /// Block on the endpoint until the next request arrives. Returns
    /// the decoded frame on success or
    /// `Err(DriverRuntimeError::Device(DeviceHostError::Internal))`
    /// if the underlying syscall reports an error. Implementations
    /// must not panic on transient errors.
    fn recv(&mut self, endpoint: EndpointCap) -> Result<RecvFrame, crate::DriverRuntimeError>;

    /// Reply to the in-flight request on the reply capability the
    /// kernel staged for the peer. Implementations stamp any
    /// pre-staged bulk payload via `store_reply_bulk` before the
    /// reply so read replies carry the data grant.
    fn reply(&mut self, label: u64, data0: u64) -> Result<(), crate::DriverRuntimeError>;

    /// Stage bulk data to accompany the next [`Self::reply`]. The
    /// default implementation is a no-op so backends that never
    /// carry bulk data do not need to override it — production
    /// [`SyscallBackend`] overrides.
    fn store_reply_bulk(&mut self, _bulk: &[u8]) -> Result<(), crate::DriverRuntimeError> {
        Ok(())
    }

    /// Publish a fire-and-forget message on `endpoint` carrying a
    /// bulk payload. Used by [`net::NetServer::publish_rx_frame`] to
    /// push received frames back to the kernel net stack.
    fn send_buf(
        &mut self,
        endpoint: EndpointCap,
        label: u64,
        data0: u64,
        bulk: &[u8],
    ) -> Result<(), crate::DriverRuntimeError>;

    /// Signal a notification capability. Used by
    /// [`net::NetServer::publish_link_state`] to wake the net stack
    /// on link-up / link-down transitions without blocking behind
    /// the command endpoint.
    fn signal_notification(
        &mut self,
        notif: NotificationCap,
        bits: u64,
    ) -> Result<(), crate::DriverRuntimeError>;
}

// ---------------------------------------------------------------------------
// SyscallBackend — production backend bridging to `syscall_lib`.
// ---------------------------------------------------------------------------

/// Production [`IpcBackend`] that forwards to the Phase 50 userspace
/// IPC wrappers in `syscall_lib`. The unit struct carries no state —
/// every call maps 1:1 to a syscall.
///
/// Every `Err(DriverRuntimeError::Device(DeviceHostError::Internal))`
/// returned here corresponds to a `u64::MAX` sentinel out of the
/// underlying syscall, which Phase 50 uses as "either the endpoint
/// handle was bad or the kernel refused the operation"; drivers
/// treating this as a fatal error is acceptable per the task's
/// error-discipline bullet.
pub struct SyscallBackend;

impl SyscallBackend {
    /// The single bulk-recv buffer size for driver servers. Matches
    /// the block / net schemas: the biggest driver-side message is a
    /// frame-sized net payload, bounded by `MAX_FRAME_BYTES`.
    pub const MAX_BULK_RECV: usize = kernel_core::driver_ipc::net::MAX_FRAME_BYTES as usize;

    /// The one-shot reply-cap handle convention — Phase 50 stages
    /// the peer's reply capability at this fixed slot when the
    /// server returns from `ipc_recv_msg`. Matches the `vfs_server`
    /// / `net_server` convention documented in Phase 54.
    const REPLY_CAP_HANDLE: u32 = 1;
}

impl IpcBackend for SyscallBackend {
    fn recv(&mut self, endpoint: EndpointCap) -> Result<RecvFrame, crate::DriverRuntimeError> {
        use alloc::vec;
        let mut msg = syscall_lib::IpcMessage::new(0);
        let mut buf = vec![0u8; Self::MAX_BULK_RECV];
        let rc = syscall_lib::ipc_recv_msg(endpoint.raw(), &mut msg, &mut buf);
        if rc == u64::MAX {
            return Err(crate::DriverRuntimeError::Device(
                kernel_core::device_host::DeviceHostError::Internal,
            ));
        }
        Ok(RecvFrame {
            label: msg.label,
            data0: msg.data[0],
            bulk: buf,
        })
    }

    fn reply(&mut self, label: u64, data0: u64) -> Result<(), crate::DriverRuntimeError> {
        let rc = syscall_lib::ipc_reply(Self::REPLY_CAP_HANDLE, label, data0);
        if rc == u64::MAX {
            return Err(crate::DriverRuntimeError::Device(
                kernel_core::device_host::DeviceHostError::Internal,
            ));
        }
        Ok(())
    }

    fn store_reply_bulk(&mut self, bulk: &[u8]) -> Result<(), crate::DriverRuntimeError> {
        let rc = syscall_lib::ipc_store_reply_bulk(bulk);
        if rc == u64::MAX {
            return Err(crate::DriverRuntimeError::Device(
                kernel_core::device_host::DeviceHostError::Internal,
            ));
        }
        Ok(())
    }

    fn send_buf(
        &mut self,
        endpoint: EndpointCap,
        label: u64,
        data0: u64,
        bulk: &[u8],
    ) -> Result<(), crate::DriverRuntimeError> {
        let rc = syscall_lib::ipc_send_buf(endpoint.raw(), label, data0, bulk);
        if rc == u64::MAX {
            return Err(crate::DriverRuntimeError::Device(
                kernel_core::device_host::DeviceHostError::Internal,
            ));
        }
        Ok(())
    }

    fn signal_notification(
        &mut self,
        notif: NotificationCap,
        bits: u64,
    ) -> Result<(), crate::DriverRuntimeError> {
        let rc = syscall_lib::notify_signal(notif.raw(), bits);
        if rc == u64::MAX {
            return Err(crate::DriverRuntimeError::Device(
                kernel_core::device_host::DeviceHostError::Internal,
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Test-only MockBackend, shared by block.rs and net.rs unit tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod mock {
    //! Pure-logic `IpcBackend` implementation used by C.4 unit tests.
    //!
    //! Tests queue up `RecvFrame`s the server will pull via `recv`,
    //! then assert on the recorded replies / sends / signals the
    //! helper produced. No syscalls involved — every test runs on
    //! the host via `cargo test -p driver_runtime --target
    //! x86_64-unknown-linux-gnu`.

    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    /// A reply the server made via `IpcBackend::reply`, captured for
    /// assertion. Includes any bulk staged with `store_reply_bulk`
    /// immediately before the reply so the test can verify both the
    /// header label and the bulk payload landed together.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct RecordedReply {
        pub label: u64,
        pub data0: u64,
        pub bulk: Vec<u8>,
    }

    /// A fire-and-forget send the server made via
    /// `IpcBackend::send_buf`, captured for assertion. Used to
    /// verify `NetServer::publish_rx_frame`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct RecordedSend {
        pub endpoint: EndpointCap,
        pub label: u64,
        pub data0: u64,
        pub bulk: Vec<u8>,
    }

    /// A notification signal captured for assertion.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct RecordedSignal {
        pub notif: NotificationCap,
        pub bits: u64,
    }

    pub struct MockBackend {
        pub incoming: VecDeque<RecvFrame>,
        pub replies: Vec<RecordedReply>,
        pub sends: Vec<RecordedSend>,
        pub signals: Vec<RecordedSignal>,
        pending_bulk: Vec<u8>,
    }

    impl MockBackend {
        pub fn new() -> Self {
            Self {
                incoming: VecDeque::new(),
                replies: Vec::new(),
                sends: Vec::new(),
                signals: Vec::new(),
                pending_bulk: Vec::new(),
            }
        }

        pub fn push_request(&mut self, frame: RecvFrame) {
            self.incoming.push_back(frame);
        }
    }

    impl IpcBackend for MockBackend {
        fn recv(&mut self, _endpoint: EndpointCap) -> Result<RecvFrame, crate::DriverRuntimeError> {
            match self.incoming.pop_front() {
                Some(f) => Ok(f),
                None => Err(crate::DriverRuntimeError::Device(
                    kernel_core::device_host::DeviceHostError::Internal,
                )),
            }
        }

        fn reply(&mut self, label: u64, data0: u64) -> Result<(), crate::DriverRuntimeError> {
            let bulk = core::mem::take(&mut self.pending_bulk);
            self.replies.push(RecordedReply { label, data0, bulk });
            Ok(())
        }

        fn store_reply_bulk(&mut self, bulk: &[u8]) -> Result<(), crate::DriverRuntimeError> {
            self.pending_bulk.clear();
            self.pending_bulk.extend_from_slice(bulk);
            Ok(())
        }

        fn send_buf(
            &mut self,
            endpoint: EndpointCap,
            label: u64,
            data0: u64,
            bulk: &[u8],
        ) -> Result<(), crate::DriverRuntimeError> {
            self.sends.push(RecordedSend {
                endpoint,
                label,
                data0,
                bulk: bulk.to_vec(),
            });
            Ok(())
        }

        fn signal_notification(
            &mut self,
            notif: NotificationCap,
            bits: u64,
        ) -> Result<(), crate::DriverRuntimeError> {
            self.signals.push(RecordedSignal { notif, bits });
            Ok(())
        }
    }
}
