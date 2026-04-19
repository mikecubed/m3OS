//! Phase 55b Track E.2 (Red) — tests land here before the bring-up
//! implementation does. The Green commit fills in
//! `init::E1000Device::bring_up` + ring allocators and wires them into
//! `program_main` alongside a MAC-format log line.

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
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "e1000_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "e1000_driver: PANIC\n");
    syscall_lib::exit(101)
}

pub mod init;
pub mod rings;

/// Boot-log marker written to stdout when the driver scaffold starts.
pub const BOOT_LOG_MARKER: &str = "e1000_driver: spawned\n";

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);
    0
}

#[cfg(test)]
mod tests {
    use super::BOOT_LOG_MARKER;

    #[test]
    fn boot_log_marker_matches_acceptance() {
        assert_eq!(BOOT_LOG_MARKER, "e1000_driver: spawned\n");
    }
}
