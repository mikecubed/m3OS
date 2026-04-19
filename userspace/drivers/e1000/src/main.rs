//! Phase 55b Track E.1 — ring-3 e1000 driver crate scaffold (RED).
//!
//! This stub exists so the four-place wiring (workspace, xtask pipeline,
//! ramdisk embedding, service config in F.1) has a concrete binary to
//! point at before Track E.2 lands the real device bring-up. The
//! `program_main` body is a logging shell — it records a boot-log marker
//! so F.1's service-config integration can verify the spawn path, then
//! exits cleanly. Real e1000 register programming, descriptor rings, and
//! RX/TX path land in E.2 / E.3.
//!
//! # RED state
//!
//! The `BOOT_LOG_MARKER` constant is deliberately set to an incorrect
//! value in this commit so the in-crate unit test fails. The Green
//! commit flips the constant to `"e1000_driver: spawned"` and wires up
//! the remaining three places (xtask `bins`, ramdisk `BIN_ENTRIES`,
//! workspace member).

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
pub const BOOT_LOG_MARKER: &str = "e1000_driver: TODO\n";

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
