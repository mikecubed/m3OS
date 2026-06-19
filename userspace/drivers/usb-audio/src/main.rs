//! Phase 92c Track E.1 — ring-3 USB Audio Class (UAC) isochronous PCM-out driver.
//!
//! Run-time flow:
//! 1. Wait for the `usb` IPC service and walk its `NextAttach` cursor for a
//!    `CLASS_AUDIO` (0x01) device — the AudioStreaming interface the xHCI server
//!    surfaces because it carries an isochronous OUT endpoint (Phase 92c server
//!    change).
//! 2. `GetDescriptors` the device and parse its configuration tree to locate the
//!    AudioStreaming alt-setting + isochronous OUT endpoint DCI
//!    ([`kernel_core::usb::uac::find_isoch_out_stream`]). The xHCI enumerator
//!    already configured that endpoint context, so the ring exists.
//! 3. `SET_INTERFACE(alt)` to activate the isochronous endpoint, then a
//!    best-effort UAC `SET_CUR(SAMPLING_FREQ_CONTROL)` to pin 48 kHz.
//! 4. Register an IPC endpoint as `"audio.hw"`, emit `AUDIO:usb-sink`, and serve
//!    the `driver_ipc::audio` protocol — forwarding `audio_server`'s mixed PCM
//!    out to the device as isochronous TRBs ([`UsbRequest::SubmitIsochOut`]).
//!
//! `audio_server` discovers the `"audio.hw"` endpoint exactly as it does the
//! AC'97 / HDA drivers (`ipc_lookup_service`), so a USB speaker presents through
//! the same policy/mixer seam as the on-board codecs.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use driver_runtime::ipc::{EndpointCap, RecvResult, SyscallBackend};
#[cfg(not(test))]
use kernel_core::driver_ipc::audio::{
    AUDIO_REQUEST_MAX_SIZE, AudioDriverError, AudioRequest, AudioResponse, caps_v1,
};
#[cfg(not(test))]
use kernel_core::usb::descriptor::{CLASS_AUDIO, parse_config_tree};
#[cfg(not(test))]
use kernel_core::usb::uac::{
    UacStreamInfo, find_isoch_out_stream, sample_rate_bytes, set_interface_setup,
    set_sample_rate_setup,
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
    syscall_lib::write_str(STDOUT_FILENO, "usb-audio: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "usb-audio: PANIC\n");
    syscall_lib::exit(101)
}

/// Service name the driver registers its `audio.hw` endpoint under.
pub const AUDIO_SERVICE_NAME: &str = "audio.hw";

/// Sentinel emitted once the USB sink is bound and registered.
pub const USB_SINK_SENTINEL: &str = "AUDIO:usb-sink ready\n";

/// Target playback rate (Hz) — matches the fixed `caps_v1` 48 kHz stereo S16LE.
#[cfg(not(test))]
const SAMPLE_RATE_HZ: u32 = 48_000;

/// Per-isoch-TRB PCM chunk size (bytes). A SubmitFrames payload (up to a 64 KiB
/// `audio_server` ring window) is dribbled out in pieces this size so each fits
/// the inline `SubmitIsochOut` budget (≤ `USB_MSG_MAX` minus wire overhead) and
/// stays a whole number of 48 kHz stereo-S16 frames (multiple of 4 bytes).
#[cfg(not(test))]
const ISOCH_CHUNK: usize = 3840;

/// 48 kHz stereo S16LE: 4 bytes per audio frame.
#[cfg(not(test))]
const BYTES_PER_FRAME: u64 = 4;

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

/// Bound USB-audio device state threaded into the `audio.hw` server loop.
#[cfg(not(test))]
struct UsbSink {
    usb_ep: u32,
    slot_id: u8,
    stream: UacStreamInfo,
    /// Cumulative frames forwarded to the device (the `Ack` flow-control clock).
    frames_consumed: u64,
}

#[cfg(not(test))]
impl UsbSink {
    /// Forward `pcm` to the device as a sequence of isochronous OUT TRBs.
    /// Returns the number of bytes accepted. Isoch is lossy by design — a failed
    /// chunk is dropped (not retried) and submission continues.
    fn submit_pcm(&mut self, pcm: &[u8]) -> usize {
        let mut sent = 0usize;
        // Build the request once and reuse its `data` Vec across chunks: each
        // iteration clears and re-fills it (reusing the capacity after the first
        // chunk grows it to ISOCH_CHUNK) instead of allocating a fresh
        // `chunk.to_vec()` per interval on this continuous-stream hot path.
        let mut req = UsbRequest::SubmitIsochOut {
            slot_id: self.slot_id,
            dci: self.stream.ep_dci,
            data: alloc::vec::Vec::with_capacity(ISOCH_CHUNK),
        };
        for chunk in pcm.chunks(ISOCH_CHUNK) {
            if let UsbRequest::SubmitIsochOut { data, .. } = &mut req {
                data.clear();
                data.extend_from_slice(chunk);
            }
            match usb_call(self.usb_ep, &req) {
                Some(UsbReply::TransferComplete { transferred, .. }) => {
                    sent += transferred;
                }
                // Dropped interval / transport hiccup — keep streaming.
                _ => {}
            }
        }
        self.frames_consumed += (sent as u64) / BYTES_PER_FRAME;
        sent
    }
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "usb-audio: spawned\n");

    // 1. Wait for the `usb` service and locate it.
    if !syscall_lib::ipc_wait_service(USB_SERVICE_NAME, 10_000) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "usb-audio: 'usb' service never appeared — exiting cleanly\n",
        );
        return 0;
    }
    let Some(usb_ep) = lookup(USB_SERVICE_NAME) else {
        return 0;
    };

    // 2. Walk NextAttach for a CLASS_AUDIO device.
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
                    if notice.attached && notice.interface_class == CLASS_AUDIO {
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
            "usb-audio: no USB audio device found — exiting cleanly\n",
        );
        return 0;
    };

    // 3. GetDescriptors + parse to find the isoch OUT stream.
    let stream = match usb_call(usb_ep, &UsbRequest::GetDescriptors { slot_id }) {
        Some(UsbReply::Descriptors { config, .. }) => match parse_config_tree(&config) {
            Some(cfg) => match find_isoch_out_stream(&cfg) {
                Some(s) => s,
                None => {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "usb-audio: device has no isochronous OUT stream — exiting\n",
                    );
                    return 0;
                }
            },
            None => {
                syscall_lib::write_str(STDOUT_FILENO, "usb-audio: config parse failed — exiting\n");
                return 0;
            }
        },
        _ => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "usb-audio: GetDescriptors failed — exiting\n",
            );
            return 0;
        }
    };

    syscall_lib::write_str(STDOUT_FILENO, "usb-audio: bound iface=");
    syscall_lib::write_u64(STDOUT_FILENO, stream.interface_num as u64);
    syscall_lib::write_str(STDOUT_FILENO, " alt=");
    syscall_lib::write_u64(STDOUT_FILENO, stream.alt_setting as u64);
    syscall_lib::write_str(STDOUT_FILENO, " dci=");
    syscall_lib::write_u64(STDOUT_FILENO, stream.ep_dci as u64);
    syscall_lib::write_str(STDOUT_FILENO, " mps=");
    syscall_lib::write_u64(STDOUT_FILENO, stream.mps as u64);
    syscall_lib::write_str(STDOUT_FILENO, "\n");

    // 4. SET_INTERFACE to activate the isochronous endpoint's alt-setting.
    // (alt 0 is the zero-bandwidth idle setting; the isoch endpoint only exists
    // on alt ≥ 1.) Log but continue on failure — the downstream isoch submits
    // and the WAV-capture gate will surface a genuinely inactive endpoint.
    let set_iface = UsbRequest::ControlRequest {
        slot_id,
        setup: set_interface_setup(stream.interface_num, stream.alt_setting),
        length: 0,
    };
    match usb_call(usb_ep, &set_iface) {
        Some(UsbReply::ControlData {
            completion_code: 1, ..
        }) => {
            syscall_lib::write_str(STDOUT_FILENO, "usb-audio: stream interface activated\n");
        }
        _ => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "usb-audio: SET_INTERFACE did not succeed (continuing)\n",
            );
        }
    }

    // Best-effort: pin the sample rate (UAC 1.0 endpoint SET_CUR). QEMU's
    // usb-audio defaults to 48 kHz and may STALL this; a failure is non-fatal.
    let set_rate = UsbRequest::ControlWrite {
        slot_id,
        setup: set_sample_rate_setup(stream.ep_address),
        data: sample_rate_bytes(SAMPLE_RATE_HZ).to_vec(),
    };
    let _ = usb_call(usb_ep, &set_rate);

    // 5. Register as the `audio.hw` backend.
    let ep = syscall_lib::create_endpoint();
    let Ok(ep_u32) = u32::try_from(ep) else {
        syscall_lib::write_str(STDOUT_FILENO, "usb-audio: endpoint create failed\n");
        return 4;
    };
    if syscall_lib::ipc_register_service(ep_u32, AUDIO_SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "usb-audio: service register failed\n");
        return 5;
    }

    syscall_lib::write_str(STDOUT_FILENO, USB_SINK_SENTINEL);

    let mut sink = UsbSink {
        usb_ep,
        slot_id,
        stream,
        frames_consumed: 0,
    };
    run_server_loop(&mut sink, EndpointCap::new(ep_u32))
}

#[cfg(not(test))]
fn run_server_loop(sink: &mut UsbSink, endpoint: EndpointCap) -> i32 {
    use driver_runtime::audio_pcm::{PCM_RING_BYTES, PcmReceiver};
    use driver_runtime::ipc::audio::{decode_request_bulk, reply_response};

    let mut backend_ipc = SyscallBackend::new();
    let mut receiver = PcmReceiver::new();
    let mut scratch = alloc::vec![0u8; PCM_RING_BYTES];

    loop {
        match backend_ipc.recv_with_capacity(endpoint, AUDIO_REQUEST_MAX_SIZE) {
            Ok(RecvResult::Message(frame)) => {
                let rsp = match decode_request_bulk(&frame.bulk) {
                    Ok(AudioRequest::QueryCaps) => AudioResponse::Caps(caps_v1()),
                    // A USB sink serves a single isochronous stream.
                    Ok(AudioRequest::OpenStream { .. }) => AudioResponse::StreamOpened(1),
                    Ok(AudioRequest::SubmitFrames {
                        grant_handle,
                        offset,
                        len,
                        ..
                    }) => {
                        let safe_len = (len as usize).min(scratch.len());
                        match receiver.recv_and_copy(
                            grant_handle,
                            offset,
                            len,
                            &mut scratch[..safe_len],
                        ) {
                            Ok(n) => {
                                sink.submit_pcm(&scratch[..n]);
                                AudioResponse::Ack {
                                    frames_consumed: sink.frames_consumed,
                                }
                            }
                            Err(_) => AudioResponse::Err(AudioDriverError::InvalidArgument),
                        }
                    }
                    Ok(AudioRequest::Drain { .. }) => AudioResponse::Ok,
                    Ok(AudioRequest::CloseStream { .. }) => {
                        receiver.release();
                        AudioResponse::Ok
                    }
                    Err(_) => AudioResponse::Err(AudioDriverError::InvalidArgument),
                };
                let _ = reply_response(&mut backend_ipc, &rsp);
            }
            Ok(RecvResult::Notification(_)) => {
                // No device IRQ is bound (the USB IRQ belongs to the xHCI
                // driver); nothing to service here.
            }
            Err(_) => {
                // Transient receive error — continue.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AUDIO_SERVICE_NAME, USB_SINK_SENTINEL};

    #[test]
    fn service_name_matches_audio_hw() {
        assert_eq!(AUDIO_SERVICE_NAME, "audio.hw");
    }

    #[test]
    fn usb_sink_sentinel_is_stable() {
        assert!(USB_SINK_SENTINEL.contains("AUDIO:usb-sink"));
    }
}
