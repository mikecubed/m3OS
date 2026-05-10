//! m3OS kernel binary shim.
//!
//! This file owns three pieces that must live in the binary crate (they
//! cannot live in `kernel/src/lib.rs` because the bootloader and Rust's
//! lang-item rules require them to be in the linked binary):
//!
//! 1. The bootloader `entry_point!` macro — emits the boot ABI symbol.
//! 2. The `#[panic_handler]` lang item.
//! 3. The `#[alloc_error_handler]` lang item.
//!
//! Everything else lives in the `kernel` library crate so that the
//! integration tests under `kernel/tests/*.rs` can `use kernel::...` to
//! reach scheduler / SMP / pipe internals (Phase 61 Track 0a).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(kernel::testing::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]

extern crate alloc;

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    kernel::kernel_main_entry(boot_info)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kernel::handle_panic(info)
}

#[alloc_error_handler]
fn alloc_error_handler(layout: alloc::alloc::Layout) -> ! {
    kernel::handle_alloc_error(layout)
}
