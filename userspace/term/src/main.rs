//! `term` binary entry point — Phase 57 Track G.1 scaffold.
//!
//! G.1 only wires the four-step new-binary convention so the binary is
//! built, embedded in the ramdisk, and described by `term.conf`. The
//! event loop, surface registration, PTY host, screen state machine,
//! and input handler land in G.2..G.6 in order. Until then, the
//! binary writes its boot marker, signals readiness, and exits zero
//! so the supervisor records a clean start.
//!
//! `cfg(not(test))` gates protect the OS-only entry point so
//! `cargo test -p term --target x86_64-unknown-linux-gnu --lib`
//! continues to compile on the host.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;

#[cfg(not(test))]
use term::{BOOT_LOG_MARKER, READY_SENTINEL};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "term: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "term: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);
    // G.1 scaffold: post-G.5 lands the surface registration, focus
    // dispatch, PTY host, and renderer. Until then we just signal
    // readiness so the supervisor records a clean spawn and exit-zero
    // so the supervisor's restart budget is not consumed.
    syscall_lib::write_str(STDOUT_FILENO, READY_SENTINEL);
    0
}
