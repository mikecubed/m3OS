//! `audio_server` binary entry point — Phase 57 Track D.1.
//!
//! Run-time flow: write the boot marker, claim the sentinel AC'97 BDF,
//! initialise the controller via [`device::Ac97Backend::init`], create
//! the command endpoint, register it under `audio.cmd`, subscribe to
//! the audio IRQ via [`irq::subscribe_and_bind`], emit the
//! `AUDIO_SMOKE:server:READY` sentinel, and enter [`irq::run_io_loop`]
//! — a non-returning IRQ / IPC dispatch loop. Bring-up failures log a
//! marker and exit with a stable non-zero code so the service manager's
//! restart path observes the failure.
//!
//! Track D.1 lands the scaffold; Tracks D.2..D.5 land the real backend,
//! stream registry, IRQ multiplex, and single-client policy.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use audio_server::{
    BOOT_LOG_MARKER, SENTINEL_BUS, SENTINEL_DEVICE, SENTINEL_FUNCTION, SERVER_READY_SENTINEL,
    SERVICE_NAME, client::ClientRegistry, device::Ac97Backend, irq, stream::StreamRegistry,
};

#[cfg(not(test))]
use driver_runtime::{DeviceCapKey, DeviceHandle, ipc::EndpointCap};
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
    syscall_lib::write_str(STDOUT_FILENO, "audio_server: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "audio_server: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    let key = DeviceCapKey::new(0, SENTINEL_BUS, SENTINEL_DEVICE, SENTINEL_FUNCTION);
    let device = match DeviceHandle::claim(key) {
        Ok(d) => d,
        Err(_) => {
            // Either the AC'97 controller is absent (QEMU launched without
            // `-device AC97`) or something else owns the slot. Both reduce
            // to "no audio device present" from this driver's perspective.
            syscall_lib::write_str(
                STDOUT_FILENO,
                "audio_server: no AC'97 device at sentinel BDF — exiting cleanly\n",
            );
            return 0;
        }
    };

    let mut backend = match Ac97Backend::init(device) {
        Ok(b) => b,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "audio_server: AC'97 init failed\n");
            return 3;
        }
    };

    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "audio_server: endpoint create failed\n");
        return 4;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(id) => id,
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "audio_server: endpoint id out of u32 range\n",
            );
            return 6;
        }
    };
    let rc = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
    if rc == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "audio_server: service register failed\n");
        return 5;
    }

    let endpoint = EndpointCap::new(ep_u32);

    let irq_notif = match irq::subscribe_and_bind(backend.device(), endpoint) {
        Ok(n) => n,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "audio_server: IRQ bind failed\n");
            return 7;
        }
    };

    syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);

    let mut streams = StreamRegistry::new();
    let mut clients = ClientRegistry::new();
    irq::run_io_loop(
        &mut backend,
        &mut streams,
        &mut clients,
        endpoint,
        irq_notif,
    )
}
