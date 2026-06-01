//! Phase 80 Track A.5 — AC'97 out-of-process ring-3 driver.
//!
//! Run-time flow: write the boot marker, claim the AC'97 PCI device at
//! BDF 0:05.0, initialise [`Ac97Backend`], create an IPC endpoint, register
//! it as `"audio.hw"`, emit the `AC97_SMOKE:server:READY` sentinel, and enter
//! the `audio.hw` server loop — a non-returning IPC / notification dispatch
//! loop serving the `driver_ipc::audio` protocol.
//!
//! `audio_server` (Track B) will discover the `audio.hw` endpoint via
//! `ipc_lookup_service("audio.hw")` and forward all client PCM requests here.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use ac97_driver::Ac97Backend;
#[cfg(not(test))]
use driver_runtime::ipc::{EndpointCap, RecvResult, SyscallBackend};
#[cfg(not(test))]
use driver_runtime::{DeviceCapKey, DeviceHandle};
#[cfg(not(test))]
use kernel_core::audio::AudioError;
#[cfg(not(test))]
use kernel_core::driver_ipc::audio::{
    AUDIO_REQUEST_MAX_SIZE, AudioDriverError, AudioRequest, AudioResponse, caps_v1,
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
    syscall_lib::write_str(STDOUT_FILENO, "ac97_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "ac97_driver: PANIC\n");
    syscall_lib::exit(101)
}

/// Boot-log marker written to stdout when the driver starts.
pub const BOOT_LOG_MARKER: &str = "ac97_driver: spawned\n";

/// Sentinel emitted immediately before entering the IPC server loop.
pub const SERVER_READY_SENTINEL: &str = "AC97_SMOKE:server:READY\n";

/// Service name under which the driver registers its `audio.hw` endpoint.
pub const SERVICE_NAME: &str = "audio.hw";

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    // Claim the AC'97 device at PCI BDF 0:05.0.
    let key = DeviceCapKey::new(0, 0x00, 0x05, 0x00);
    let device = match DeviceHandle::claim(key) {
        Ok(d) => d,
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "ac97_driver: no AC'97 device at BDF 0:05.0 — exiting cleanly\n",
            );
            return 0;
        }
    };

    let mut backend = match Ac97Backend::init(device) {
        Ok(b) => b,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "ac97_driver: AC'97 init failed\n");
            return 3;
        }
    };

    // Create IPC endpoint and register as "audio.hw".
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "ac97_driver: endpoint create failed\n");
        return 4;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(id) => id,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "ac97_driver: endpoint id out of u32 range\n");
            return 6;
        }
    };
    let rc = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
    if rc == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "ac97_driver: service register failed\n");
        return 5;
    }

    syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);

    // Enter the audio.hw server loop — never returns on the happy path.
    run_server_loop(&mut backend, ep_u32)
}

#[cfg(not(test))]
fn run_server_loop(backend: &mut Ac97Backend, ep_u32: u32) -> i32 {
    use driver_runtime::audio_pcm::{PCM_RING_BYTES, PcmReceiver};
    use driver_runtime::ipc::audio::{decode_request_bulk, reply_response};

    let endpoint = EndpointCap::new(ep_u32);
    let mut backend_ipc = SyscallBackend::new();
    let mut receiver = PcmReceiver::new();
    let mut scratch = alloc::vec![0u8; PCM_RING_BYTES];

    loop {
        match backend_ipc.recv_with_capacity(endpoint, AUDIO_REQUEST_MAX_SIZE) {
            Ok(RecvResult::Message(frame)) => {
                let rsp = match decode_request_bulk(&frame.bulk) {
                    Ok(AudioRequest::QueryCaps) => AudioResponse::Caps(caps_v1()),
                    Ok(AudioRequest::OpenStream {
                        format,
                        rate,
                        layout,
                    }) => match backend.open_stream(format, layout, rate) {
                        Ok(id) => AudioResponse::StreamOpened(id),
                        Err(AudioError::Busy) => AudioResponse::Err(AudioDriverError::Busy),
                        Err(AudioError::InvalidFormat) => {
                            AudioResponse::Err(AudioDriverError::InvalidFormat)
                        }
                        Err(_) => AudioResponse::Err(AudioDriverError::Internal),
                    },
                    Ok(AudioRequest::SubmitFrames {
                        stream_id,
                        grant_handle,
                        offset,
                        len,
                    }) => {
                        let safe_len = (len as usize).min(scratch.len());
                        match receiver.recv_and_copy(
                            grant_handle,
                            offset,
                            len,
                            &mut scratch[..safe_len],
                        ) {
                            Ok(n) => match backend.submit_frames(stream_id, &scratch[..n]) {
                                Ok(_) => AudioResponse::Ack {
                                    frames_consumed: backend.poll_frames_consumed(),
                                },
                                Err(AudioError::WouldBlock) => AudioResponse::WouldBlock,
                                Err(_) => AudioResponse::Err(AudioDriverError::Internal),
                            },
                            Err(_) => AudioResponse::Err(AudioDriverError::InvalidArgument),
                        }
                    }
                    Ok(AudioRequest::Drain { stream_id }) => match backend.drain(stream_id) {
                        Ok(()) => AudioResponse::Ok,
                        Err(_) => AudioResponse::Err(AudioDriverError::Internal),
                    },
                    Ok(AudioRequest::CloseStream { stream_id }) => {
                        let r = backend.close_stream(stream_id);
                        receiver.release();
                        match r {
                            Ok(()) => AudioResponse::Ok,
                            Err(_) => AudioResponse::Err(AudioDriverError::Internal),
                        }
                    }
                    Err(_) => AudioResponse::Err(AudioDriverError::InvalidArgument),
                };
                let _ = reply_response(&mut backend_ipc, &rsp);
            }
            Ok(RecvResult::Notification(_)) => {
                // AC'97 IRQ: advance completed-buffer counters.
                let _ = backend.handle_irq();
            }
            Err(_) => {
                // Transient receive error — continue.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BOOT_LOG_MARKER, SERVER_READY_SENTINEL, SERVICE_NAME};

    #[test]
    fn boot_log_marker_matches_acceptance() {
        assert_eq!(BOOT_LOG_MARKER, "ac97_driver: spawned\n");
    }

    #[test]
    fn server_ready_sentinel_matches_acceptance() {
        assert_eq!(SERVER_READY_SENTINEL, "AC97_SMOKE:server:READY\n");
    }

    #[test]
    fn service_name_matches_acceptance() {
        assert_eq!(SERVICE_NAME, "audio.hw");
    }
}
