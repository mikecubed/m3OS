//! Ring-3 NVMe driver — Phase 55b Tracks D.1 (scaffold) and D.2 (bring-up).
//!
//! Track D.1 landed the crate shell and the four-place userspace-binary
//! wiring (workspace member, xtask bins, ramdisk embedding, future
//! service config). Track D.2 lands the controller bring-up state
//! machine in [`init`] — the concrete MMIO / DMA path that consumes
//! the state machine ships in the following commit.
//!
//! This module's `program_main` stays on the Track D.1 stub while the
//! D.2 red tests land; the green commit replaces it with a real
//! bring-up driver.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod init;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use driver_runtime::{DeviceCapKey, DeviceHandle};
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
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Sentinel PCI BDF the scaffold claims. The real D.3 driver walks PCI
/// for class `0x01` subclass `0x08` programming interface `0x02`;
/// until that lands we target QEMU's default `-device nvme` location.
#[cfg(not(test))]
const SENTINEL_BDF: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x04, 0);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: spawned\n");

    match DeviceHandle::claim(SENTINEL_BDF) {
        Ok(_handle) => {
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: claimed sentinel BDF\n");
        }
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: no sentinel device, exiting\n");
        }
    }

    0
}
