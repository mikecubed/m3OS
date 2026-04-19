//! Phase 55b Track E.1 — ring-3 e1000 driver crate scaffold.
//!
//! Stub binary whose `program_main` writes a fixed boot-log marker and
//! exits, so Track F.1 can register it under the service manager and
//! Track E.2 / E.3 can replace the body with the real bring-up and
//! RX/TX paths. Four-place wiring lives in root `Cargo.toml`,
//! `xtask/src/main.rs`, and `kernel/src/fs/ramdisk.rs`; the service
//! config (place 4) is deferred to Track F.1.

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

/// Boot-log marker written to stdout when the driver scaffold starts.
///
/// F.1's service-config smoke test greps the boot log for this line,
/// so the exact spelling is load-bearing.
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
        // Track E.1 acceptance: `cargo xtask run` boot log records
        // `e1000_driver: spawned`. The Red commit wires in an incorrect
        // marker; the Green commit flips it to the real value.
        assert_eq!(BOOT_LOG_MARKER, "e1000_driver: spawned\n");
    }
}
