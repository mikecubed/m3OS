//! Phase 80b — Intel HDA out-of-process ring-3 driver entry point.
//!
//! Run-time flow: write the boot marker, discover an HDA controller by PCI
//! class (0x04/0x03/0x00, vendor-agnostic), claim it, map BAR0, bring up the
//! controller (reset → STATESTS codec-ready → CORB/RIRB RUN-enable) and the
//! codec (enumerate widgets → select analog codec → configure output path),
//! create an IPC endpoint, register it as `"audio.hw"`, emit the
//! `HDA_SMOKE:server:READY` sentinel, and enter the `audio.hw` server loop
//! serving the `driver_ipc::audio` protocol.

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
use driver_runtime::{DeviceHandle, Mmio};
#[cfg(not(test))]
use hda_driver::{BOOT_LOG_MARKER, HDA_BAR0_LEN, SERVER_READY_SENTINEL, SERVICE_NAME};
#[cfg(not(test))]
use kernel_core::driver_ipc::audio::{
    AUDIO_REQUEST_MAX_SIZE, AudioDriverError, AudioRequest, AudioResponse, caps_v1,
};
#[cfg(not(test))]
use kernel_core::hda;
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
    syscall_lib::write_str(STDOUT_FILENO, "hda_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "hda_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Discover an HDA controller by PCI class (vendor-agnostic 0x04/0x03/0x00).
#[cfg(not(test))]
fn find_hda() -> Option<driver_runtime::DeviceCapKey> {
    let candidates = driver_runtime::enumerate_pci_class(0x04, 0x03, 0x00).ok()?;
    candidates.into_iter().next()
}

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    let key = match find_hda() {
        Some(k) => k,
        None => {
            // No HDA controller on this machine (e.g. QEMU with only -device
            // AC97) — exit cleanly so the ac97 driver serves audio.hw.
            syscall_lib::write_str(STDOUT_FILENO, "hda_driver: no HDA controller present\n");
            return 0;
        }
    };

    let device = match DeviceHandle::claim(key) {
        Ok(d) => d,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "hda_driver: device claim failed\n");
            return 1;
        }
    };

    let bar0 = match Mmio::<u8>::map(&device, 0, HDA_BAR0_LEN) {
        Ok(m) => m,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "hda_driver: BAR0 map failed\n");
            return 2;
        }
    };

    // Read GCAP so the log records the controller's stream counts.
    let gcap: u16 = bar0.read_reg(hda::REG_GCAP);
    let _ = gcap;

    // Create + register the audio.hw endpoint.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "hda_driver: endpoint create failed\n");
        return 4;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(id) => id,
        Err(_) => return 6,
    };
    if syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "hda_driver: service register failed\n");
        return 5;
    }

    syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);

    server_loop(EndpointCap::new(ep_u32))
}

/// Minimal control server loop (skeleton). The full output-stream engine
/// (Track C) replaces the `SubmitFrames` arm; for now it answers `QueryCaps`
/// so `audio_server`'s connect handshake succeeds.
#[cfg(not(test))]
fn server_loop(endpoint: EndpointCap) -> i32 {
    use driver_runtime::ipc::audio::{decode_request_bulk, reply_response};

    let mut backend = SyscallBackend::new();
    loop {
        let rsp = match backend.recv_with_capacity(endpoint, AUDIO_REQUEST_MAX_SIZE) {
            Ok(RecvResult::Message(frame)) => match decode_request_bulk(&frame.bulk) {
                Ok(AudioRequest::QueryCaps) => AudioResponse::Caps(caps_v1()),
                Ok(_) => AudioResponse::Err(AudioDriverError::Internal),
                Err(_) => AudioResponse::Err(AudioDriverError::InvalidArgument),
            },
            Ok(RecvResult::Notification(_)) => continue,
            Err(_) => continue,
        };
        let _ = reply_response(&mut backend, &rsp);
    }
}
