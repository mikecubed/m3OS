//! Ring-3 NVMe driver — Phase 55b Track D.1 scaffold.
//!
//! This crate is the userspace home for the NVMe block driver. Track D.1
//! lands the crate shell so the four-place userspace-binary wiring
//! (workspace member, xtask bins, ramdisk embedding, future service
//! config) is proven before any real driver logic ships. Track D.2 fills
//! in controller bring-up; D.3 wires the I/O queue pair and block IPC
//! path.
//!
//! The stub `program_main` logs a `spawned` line, attempts to claim a
//! sentinel BDF via [`driver_runtime::DeviceHandle::claim`], records the
//! outcome on the serial console, and exits zero. Exiting zero is
//! deliberate — Track F.2's crash-and-restart regression depends on a
//! predictable clean exit path before D.2's run-forever bring-up lands.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use driver_runtime::{DeviceCapKey, DeviceHandle};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

/// Sentinel PCI BDF the D.1 stub tries to claim. The real D.2 driver
/// discovers NVMe controllers via PCI class `0x01` subclass `0x08`
/// programming interface `0x02`; for the scaffold we probe QEMU's
/// default `-device nvme` location (`0000:00:04.0`) so a missing device
/// is observable in the boot log rather than silently skipped.
const SENTINEL_BDF: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x04, 0);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: spawned\n");

    match DeviceHandle::claim(SENTINEL_BDF) {
        Ok(_handle) => {
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: claimed sentinel BDF\n");
        }
        Err(_) => {
            // No device at the sentinel BDF, or the kernel refused the
            // claim. The scaffold deliberately does not panic — D.2
            // lands real discovery + bring-up and will turn this into a
            // fatal error when appropriate.
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: no sentinel device, exiting\n");
        }
    }

    0
}
