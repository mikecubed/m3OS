#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! QEMU integration-test scaffold for Phase 55c Track B.
//!
//! This bootable no_std test binary keeps the `kernel/tests/` slot compiling
//! under the QEMU harness while the behavioral coverage still lives in the
//! in-crate `#[test_case]` suites. The intended end-to-end scenarios are:
//!
//! - signal during blocked recv → notification wake
//! - message during blocked recv → message wake

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::panic::PanicInfo;
use x86_64::instructions::{hlt, port::Port};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(bound_recv_kernel_test, config = &BOOTLOADER_CONFIG);

fn bound_recv_kernel_test(_boot_info: &'static mut BootInfo) -> ! {
    test_main();
    qemu_exit(0x10);
}

trait Testable {
    fn run(&self);
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        self();
    }
}

fn test_runner(tests: &[&dyn Testable]) {
    for test in tests {
        test.run();
    }
}

fn qemu_exit(code: u32) -> ! {
    unsafe { Port::new(0xf4).write(code) };
    loop {
        hlt();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    qemu_exit(0x11);
}

#[test_case]
fn bound_recv_qemu_scenarios_are_documented() {}
