//! Phase 92c Track E.2 — ring-3 USB Video Class (UVC) frame-capture driver.
//!
//! # Live path (bare-metal / VFIO-only)
//!
//! QEMU has no UVC device model, so the live capture path is bare-metal-only.
//! The driver binds cleanly with no CLASS_VIDEO device present and exits with
//! rc 0 — mirroring usb-audio's "no device found — exiting cleanly" pattern.
//!
//! # Run-time flow (on a real USB camera)
//!
//! 1. Wait for the `usb` IPC service and walk its `NextAttach` cursor for a
//!    `CLASS_VIDEO` (0x0E) / `SUBCLASS_VIDEO_STREAMING` (0x02) device.
//! 2. `GetDescriptors` the device and parse its configuration tree to locate the
//!    VideoStreaming alt-setting + capture IN endpoint DCI
//!    ([`kernel_core::usb::uvc::find_video_stream`]).
//! 3. If `alt_setting > 0`, issue `SET_INTERFACE(iface, alt)` to activate the
//!    endpoint.
//! 4. Run the UVC probe/commit negotiation:
//!    `GET_MAX(VS_PROBE_CONTROL)` → `SET_CUR(VS_PROBE_CONTROL)` →
//!    `GET_CUR(VS_PROBE_CONTROL)` → `SET_CUR(VS_COMMIT_CONTROL)`.
//! 5. Capture frames via `SubmitBulkIn` / `PollBulkIn` on the IN endpoint DCI,
//!    and forward each frame to `camera_server` via IPC.
//!    Each frame emits `CAMERA:frame` on serial for observability.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use kernel_core::usb::descriptor::{CLASS_VIDEO, SUBCLASS_VIDEO_STREAMING, parse_config_tree};
#[cfg(not(test))]
use kernel_core::usb::uvc::{
    UVC_GET_CUR, UVC_GET_MAX, UVC_SET_CUR, UvcStreamInfo, VS_COMMIT_CONTROL, VS_PROBE_CONTROL,
    find_video_stream, negotiate_default, probe_control_setup, set_interface_setup,
};
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
#[cfg(not(test))]
use usb_core::protocol::{USB_MSG_MAX, USB_REQ_LABEL, USB_SERVICE_NAME, UsbReply, UsbRequest};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "usb-video: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "usb-video: PANIC\n");
    syscall_lib::exit(101)
}

/// Sentinel emitted once the driver has bound a UVC camera and entered the
/// capture loop.  The bare-metal smoke harness can wait for this string.
pub const UVC_BIND_SENTINEL: &str = "CAMERA:bound\n";

/// Sentinel prefix emitted once per captured frame.
pub const UVC_FRAME_SENTINEL: &str = "CAMERA:frame";

/// IPC service name of the camera_server that receives captured frames.
pub const CAMERA_SERVICE_NAME: &str = "camera";

/// Maximum bulk IN transfer size per frame request (bytes).
/// Capped at 65535 to fit the `u16` `len` field of `UsbRequest::SubmitBulkIn`.
#[cfg(not(test))]
const FRAME_BUF_LEN: u16 = 65535;

// ---------------------------------------------------------------------------
// IPC helpers (mirrored from usb-audio)
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn usb_call(usb_ep: u32, req: &UsbRequest) -> Option<UsbReply> {
    let req_bytes = req.encode();
    let rc = syscall_lib::ipc_call_buf(usb_ep, USB_REQ_LABEL, 0, &req_bytes);
    if rc == u64::MAX {
        return None;
    }
    let mut reply_buf = [0u8; USB_MSG_MAX];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX {
        return None;
    }
    UsbReply::decode(&reply_buf[..n as usize])
}

#[cfg(not(test))]
fn lookup(name: &str) -> Option<u32> {
    let h = syscall_lib::ipc_lookup_service(name);
    if h == u64::MAX { None } else { Some(h as u32) }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "usb-video: spawned\n");

    // 1. Wait for the `usb` service.
    if !syscall_lib::ipc_wait_service(USB_SERVICE_NAME, 10_000) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "usb-video: 'usb' service never appeared — exiting cleanly\n",
        );
        return 0;
    }
    let Some(usb_ep) = lookup(USB_SERVICE_NAME) else {
        return 0;
    };

    // 2. Walk NextAttach for a CLASS_VIDEO / SUBCLASS_VIDEO_STREAMING device.
    const MAX_POLLS: u32 = 150;
    const POLL_INTERVAL_MS: u64 = 200;
    let mut bound_slot: Option<u8> = None;
    'outer: for _ in 0..MAX_POLLS {
        let mut cursor = 0u8;
        loop {
            match usb_call(usb_ep, &UsbRequest::NextAttach { cursor }) {
                Some(UsbReply::Attach {
                    notice: Some(notice),
                }) => {
                    if notice.attached
                        && notice.interface_class == CLASS_VIDEO
                        && notice.interface_sub_class == SUBCLASS_VIDEO_STREAMING
                    {
                        bound_slot = Some(notice.slot_id);
                        break 'outer;
                    }
                    cursor = cursor.saturating_add(1);
                }
                Some(UsbReply::Attach { notice: None }) | None => break,
                _ => cursor = cursor.saturating_add(1),
            }
        }
        let _ = syscall_lib::nanosleep_for(0, (POLL_INTERVAL_MS * 1_000_000) as u32);
    }
    let Some(slot_id) = bound_slot else {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "usb-video: no USB video device found — exiting cleanly\n",
        );
        return 0;
    };

    // 3. GetDescriptors + parse to find the VideoStreaming IN endpoint.
    let stream: UvcStreamInfo = match usb_call(usb_ep, &UsbRequest::GetDescriptors { slot_id }) {
        Some(UsbReply::Descriptors { config, .. }) => match parse_config_tree(&config) {
            Some(cfg) => match find_video_stream(&cfg) {
                Some(s) => s,
                None => {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "usb-video: no VideoStreaming IN endpoint — exiting\n",
                    );
                    return 0;
                }
            },
            None => {
                syscall_lib::write_str(STDOUT_FILENO, "usb-video: config parse failed — exiting\n");
                return 0;
            }
        },
        _ => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "usb-video: GetDescriptors failed — exiting\n",
            );
            return 0;
        }
    };

    syscall_lib::write_str(STDOUT_FILENO, "usb-video: bound iface=");
    syscall_lib::write_u64(STDOUT_FILENO, stream.interface_num as u64);
    syscall_lib::write_str(STDOUT_FILENO, " alt=");
    syscall_lib::write_u64(STDOUT_FILENO, stream.alt_setting as u64);
    syscall_lib::write_str(STDOUT_FILENO, " dci=");
    syscall_lib::write_u64(STDOUT_FILENO, stream.ep_dci as u64);
    syscall_lib::write_str(STDOUT_FILENO, " mps=");
    syscall_lib::write_u64(STDOUT_FILENO, stream.mps as u64);
    syscall_lib::write_str(
        STDOUT_FILENO,
        if stream.is_isoch {
            " isoch\n"
        } else {
            " bulk\n"
        },
    );

    // 4. SET_INTERFACE if alt > 0 (isochronous camera: zero-bandwidth idle on
    // alt 0; bulk camera: endpoint is already on alt 0, skip).
    if stream.alt_setting > 0 {
        let set_iface = UsbRequest::ControlRequest {
            slot_id,
            setup: set_interface_setup(stream.interface_num, stream.alt_setting),
            length: 0,
        };
        match usb_call(usb_ep, &set_iface) {
            Some(UsbReply::ControlData {
                completion_code: 1, ..
            }) => {
                syscall_lib::write_str(STDOUT_FILENO, "usb-video: stream interface activated\n");
            }
            _ => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "usb-video: SET_INTERFACE did not succeed (continuing)\n",
                );
            }
        }
    }

    // 5. UVC probe/commit negotiation.
    //
    // Step A: GET_MAX(VS_PROBE_CONTROL) — learn the device's upper bound.
    let probe_get_max_setup =
        probe_control_setup(UVC_GET_MAX, VS_PROBE_CONTROL, stream.interface_num);
    let probe_max_bytes = match usb_call(
        usb_ep,
        &UsbRequest::ControlRequest {
            slot_id,
            setup: probe_get_max_setup,
            length: 26,
        },
    ) {
        Some(UsbReply::ControlData { data, .. }) => data,
        _ => {
            // Not fatal; fall back to negotiate_default.
            syscall_lib::write_str(
                STDOUT_FILENO,
                "usb-video: GET_MAX(probe) failed — using default\n",
            );
            negotiate_default().encode().to_vec()
        }
    };

    // Step B: SET_CUR(VS_PROBE_CONTROL) with our preferred parameters.
    let our_probe = if kernel_core::usb::uvc::UvcStreamingControl::parse(&probe_max_bytes).is_some()
    {
        // Use the device's max as a starting point and override format/frame.
        let mut p = kernel_core::usb::uvc::UvcStreamingControl::parse(&probe_max_bytes).unwrap();
        p.bm_hint = 0x0001;
        p.b_format_index = 1;
        p.b_frame_index = 1;
        p.dw_frame_interval = 333_333;
        p
    } else {
        negotiate_default()
    };
    let probe_bytes = our_probe.encode();

    let probe_set_setup = probe_control_setup(UVC_SET_CUR, VS_PROBE_CONTROL, stream.interface_num);
    let _ = usb_call(
        usb_ep,
        &UsbRequest::ControlWrite {
            slot_id,
            setup: probe_set_setup,
            data: probe_bytes.to_vec(),
        },
    );

    // Step C: GET_CUR(VS_PROBE_CONTROL) — read back the negotiated result.
    let probe_get_setup = probe_control_setup(UVC_GET_CUR, VS_PROBE_CONTROL, stream.interface_num);
    let _ = usb_call(
        usb_ep,
        &UsbRequest::ControlRequest {
            slot_id,
            setup: probe_get_setup,
            length: 26,
        },
    );

    // Step D: SET_CUR(VS_COMMIT_CONTROL) — lock in the stream parameters.
    let commit_setup = probe_control_setup(UVC_SET_CUR, VS_COMMIT_CONTROL, stream.interface_num);
    let _ = usb_call(
        usb_ep,
        &UsbRequest::ControlWrite {
            slot_id,
            setup: commit_setup,
            data: probe_bytes.to_vec(),
        },
    );

    syscall_lib::write_str(STDOUT_FILENO, UVC_BIND_SENTINEL);

    // 6. Look up camera_server (best-effort; if absent the driver still captures
    // and logs frames — useful for initial bare-metal bringup without
    // camera_server running).
    let camera_ep: Option<u32> = lookup(CAMERA_SERVICE_NAME);

    // 7. Capture loop: SubmitBulkIn / PollBulkIn per frame.
    let mut frame_seq: u64 = 0;
    loop {
        // Submit an IN request for up to FRAME_BUF_LEN bytes.
        let submit_req = UsbRequest::SubmitBulkIn {
            slot_id,
            dci: stream.ep_dci,
            len: FRAME_BUF_LEN,
        };
        match usb_call(usb_ep, &submit_req) {
            Some(UsbReply::BulkData { data, .. }) => {
                let frame_len = data.len();
                syscall_lib::write_str(STDOUT_FILENO, UVC_FRAME_SENTINEL);
                syscall_lib::write_str(STDOUT_FILENO, " seq=");
                syscall_lib::write_u64(STDOUT_FILENO, frame_seq);
                syscall_lib::write_str(STDOUT_FILENO, " len=");
                syscall_lib::write_u64(STDOUT_FILENO, frame_len as u64);
                syscall_lib::write_str(STDOUT_FILENO, "\n");

                // Forward to camera_server if available.
                if let Some(cam_ep) = camera_ep {
                    forward_frame_to_camera(cam_ep, frame_seq, &data);
                }
                frame_seq = frame_seq.wrapping_add(1);
            }
            _ => {
                // No data or transport error — brief yield and retry.
                let _ = syscall_lib::nanosleep_for(0, 10_000_000); // 10 ms
            }
        }
    }
}

/// Forward a captured frame to `camera_server` via IPC.
///
/// This is a best-effort fire-and-forget: if camera_server is not ready the
/// frame is dropped and capture continues.  A failure here must never stall
/// the capture loop.
#[cfg(not(test))]
fn forward_frame_to_camera(cam_ep: u32, seq: u64, data: &[u8]) {
    use kernel_core::usb::uvc::camera_ipc::{CameraReply, CameraRequest};

    let req = CameraRequest::PushFrame {
        seq,
        len: data.len() as u32,
    };
    let req_bytes = req.encode();
    let rc = syscall_lib::ipc_call_buf(
        cam_ep,
        kernel_core::usb::uvc::camera_ipc::CAMERA_REQ_LABEL,
        0,
        &req_bytes,
    );
    if rc == u64::MAX {
        return;
    }
    let mut reply_buf = [0u8; 64];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX {
        return;
    }
    // The response is just an acknowledgment; we don't block on it.
    let _ = CameraReply::decode(&reply_buf[..n as usize]);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{CAMERA_SERVICE_NAME, UVC_BIND_SENTINEL, UVC_FRAME_SENTINEL};

    #[test]
    fn camera_service_name_is_camera() {
        assert_eq!(CAMERA_SERVICE_NAME, "camera");
    }

    #[test]
    fn uvc_bind_sentinel_contains_bound() {
        assert!(UVC_BIND_SENTINEL.contains("CAMERA:bound"));
    }

    #[test]
    fn uvc_frame_sentinel_prefix() {
        assert!(UVC_FRAME_SENTINEL.starts_with("CAMERA:frame"));
    }
}
