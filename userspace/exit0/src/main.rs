//! Minimal userspace binary: calls exit(0) immediately.
//!
//! Validation: P11-T019 — load a statically linked ELF, confirm exit code 0.
#![no_std]
#![no_main]

use syscall_lib::exit;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    exit(0)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(101)
}
