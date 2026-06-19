//! Phase 92c Track E.2 — ring-3 camera IPC server.
//!
//! `camera_server` is the policy / aggregation hub for USB video capture.
//! It registers an IPC endpoint named `"camera"` and serves two request
//! types defined by the host-tested
//! [`kernel_core::usb::uvc::camera_ipc`] codec:
//!
//! * `QueryFormat` — reply with the current capture format (width × height +
//!   fourcc). Before the first frame arrives the format is unknown (0 × 0).
//! * `PushFrame { seq, len }` — accept a frame-arrival notification from
//!   `usb-video` and update the latest-frame record.  Reply with `Ack`.
//!
//! A separate `GetFrame` flow (consumer polling) is out of scope for this
//! sub-phase; the consumer path is documented as a deferral.  The live
//! frame flow is bare-metal-only (QEMU has no UVC device model).
//!
//! # Skip-with-reason note
//!
//! There is no always-on QEMU gate for the live capture path.  The
//! CI-verifiable deliverable is the host-tested IPC codec in
//! `kernel_core::usb::uvc::camera_ipc` and the compilation of this crate
//! for the `x86_64-m3os` target (proven by `cargo xtask check`).

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use driver_runtime::ipc::{EndpointCap, IpcBackend, RecvResult, SyscallBackend};
#[cfg(not(test))]
use kernel_core::usb::uvc::camera_ipc::{
    CAMERA_MSG_MAX, CAMERA_REQ_LABEL, CameraReply, CameraRequest,
};
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "camera_server: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "camera_server: PANIC\n");
    syscall_lib::exit(101)
}

/// IPC service name this daemon registers under.
pub const CAMERA_SERVICE_NAME: &str = "camera";

/// Sentinel emitted once the service is registered and accepting requests.
pub const CAMERA_READY_SENTINEL: &str = "CAMERA_SERVER:ready\n";

// ---------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------

/// Runtime state held by the server loop.
#[cfg(not(test))]
struct CameraState {
    /// Sequence number of the latest frame received from `usb-video`.
    latest_seq: u64,
    /// Byte length of the latest frame (0 = no frame yet).
    latest_len: u32,
    /// Capture width in pixels (0 = unknown / pre-first-frame).
    width: u16,
    /// Capture height in pixels (0 = unknown / pre-first-frame).
    height: u16,
    /// Fourcc of the pixel format (default b"YUY2" until overridden).
    fmt: [u8; 4],
    /// Total frames received since boot.
    total_frames: u64,
}

#[cfg(not(test))]
impl CameraState {
    const fn new() -> Self {
        CameraState {
            latest_seq: 0,
            latest_len: 0,
            width: 0,
            height: 0,
            fmt: *b"YUY2",
            total_frames: 0,
        }
    }

    /// Handle one decoded `CameraRequest` and return the appropriate reply.
    fn handle(&mut self, req: CameraRequest) -> CameraReply {
        match req {
            CameraRequest::QueryFormat => CameraReply::Format {
                width: self.width,
                height: self.height,
                fmt: self.fmt,
            },
            CameraRequest::PushFrame { seq, len } => {
                self.latest_seq = seq;
                self.latest_len = len;
                self.total_frames = self.total_frames.wrapping_add(1);
                CameraReply::Ack
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "camera_server: spawned\n");

    // Register the "camera" IPC service endpoint.
    let ep = syscall_lib::create_endpoint();
    let Ok(ep_u32) = u32::try_from(ep) else {
        syscall_lib::write_str(STDOUT_FILENO, "camera_server: endpoint create failed\n");
        return 4;
    };
    if syscall_lib::ipc_register_service(ep_u32, CAMERA_SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "camera_server: service register failed\n");
        return 5;
    }

    syscall_lib::write_str(STDOUT_FILENO, CAMERA_READY_SENTINEL);

    let mut state = CameraState::new();
    let mut backend = SyscallBackend::new();
    let endpoint = EndpointCap::new(ep_u32);

    loop {
        match backend.recv_with_capacity(endpoint, CAMERA_MSG_MAX) {
            Ok(RecvResult::Message(frame)) => {
                let reply = match CameraRequest::decode(&frame.bulk) {
                    Some(req) => state.handle(req),
                    None => CameraReply::NoFrame,
                };
                // Only reply when the client used call-shaped IPC (a reply cap
                // is present). `usb-video` pushes frames with send-shaped IPC
                // (no reply cap), for which `SyscallBackend::reply` would error
                // on a zero handle — so staging + replying would be wasted work
                // every frame. Gate on the frame's reply cap.
                if frame.reply_cap_handle != 0 {
                    let reply_bytes = reply.encode();
                    // Stage bulk payload then reply (mirrors audio_server / blk
                    // drivers: store_reply_bulk → reply).
                    let _ = backend.store_reply_bulk(&reply_bytes);
                    let _ = backend.reply(CAMERA_REQ_LABEL, 0);
                }
            }
            Ok(RecvResult::Notification(_)) => {
                // No notifications bound; nothing to service.
            }
            Err(_) => {
                // Transient receive error — continue.
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{CAMERA_READY_SENTINEL, CAMERA_SERVICE_NAME};

    #[test]
    fn service_name_is_camera() {
        assert_eq!(CAMERA_SERVICE_NAME, "camera");
    }

    #[test]
    fn ready_sentinel_contains_camera_server() {
        assert!(CAMERA_READY_SENTINEL.contains("CAMERA_SERVER:ready"));
    }
}
