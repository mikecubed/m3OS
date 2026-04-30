#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Phase 57d Track H.2 — Voluntary preemption stress tests.
//!
//! These tests require multi-core QEMU with the `preempt-voluntary` feature
//! enabled and full userspace task spawning, which is not yet supported by
//! the QEMU test harness.  They are kept as documented placeholders for the
//! real-hardware validation gate described in the Phase 57d task spec (H.3/H.4).
//!
//! # Why placeholders?
//!
//! The QEMU test binaries in `kernel/tests/` are standalone `no_std` binaries
//! that boot via `entry_point!`.  The `kernel` crate is a binary-only crate
//! (no `src/lib.rs`), so it cannot be imported by these test binaries.  The
//! stress tests below require:
//!
//! - The `preempt-voluntary` Cargo feature enabled on the kernel build.
//! - `smp::init_bsp_per_core` running before `test_main` (GS_BASE must be
//!   set so `per_core()` does not panic).
//! - A 4-core QEMU instance so tight-loop tasks saturate all cores.
//! - A userspace "metronome" task that can be spawned and observed.
//!
//! Until the kernel exposes a `src/lib.rs` (or a dedicated test-helper crate)
//! and `init_bsp_per_core` is called from the test entry point, these tests
//! remain `#[ignore]`.
//!
//! # Hardware validation procedure (H.3 / H.4)
//!
//! To validate on real hardware after enabling `preempt-voluntary` in defaults:
//!
//! ```text
//! cargo xtask run-gui --fresh
//! ```
//!
//! Verify:
//! - Cursor moves without stutter.
//! - Keyboard input echoes correctly in the terminal.
//! - `term` reaches `TERM_SMOKE:ready` in the boot transcript.
//! - Zero `[WARN][sched]` lines in the serial output.
//! - Metronome counter within ±5% of expected tick count after 5 minutes.

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use x86_64::instructions::{hlt, port::Port};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(preempt_user_stress_kernel_test, config = &BOOTLOADER_CONFIG);

fn preempt_user_stress_kernel_test(_boot_info: &'static mut BootInfo) -> ! {
    test_main();
    qemu_exit(0x10);
}

// ---------------------------------------------------------------------------
// Stub global allocator — placeholder tests do not allocate.
// ---------------------------------------------------------------------------

struct NoAlloc;

unsafe impl GlobalAlloc for NoAlloc {
    unsafe fn alloc(&self, _: Layout) -> *mut u8 {
        core::ptr::null_mut()
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

#[global_allocator]
static STUB_ALLOC: NoAlloc = NoAlloc;

// ---------------------------------------------------------------------------
// Test infrastructure
// ---------------------------------------------------------------------------

trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
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

// ---------------------------------------------------------------------------
// H.2: Stress / soak placeholders
// ---------------------------------------------------------------------------

/// H.2: 4-core tight-loop + metronome stress test.
///
/// Requires: `preempt-voluntary` feature on, SMP QEMU (4 cores), full userspace.
///
/// Spawn 4 userspace tight-loop tasks (one per core) plus a "metronome" task
/// that increments a counter every 10 ms.  Run for 5 minutes.  Assert the
/// metronome counter is within ±5% of 30_000 (300 s × 100 ticks/s).  No
/// `[WARN][sched]` lines, no panics.
#[test_case]
#[ignore = "requires 4-core QEMU + preempt-voluntary feature + full userspace"]
fn multicore_preempt_stress_5min() {
    // placeholder — see module doc for hardware validation procedure
}

/// H.3: Real-hardware acceptance gate (procedural).
///
/// Run `cargo xtask run-gui --fresh` with `preempt-voluntary` enabled, 5 times.
/// Verify:
/// - Cursor moves without stutter.
/// - Keyboard echoes correctly.
/// - `term` reaches `TERM_SMOKE:ready`.
/// - Zero `[WARN][sched]` lines in serial output.
#[test_case]
#[ignore = "procedural — requires real hardware, not a QEMU harness test"]
fn real_hardware_acceptance_gate() {
    // placeholder — see module doc for procedure
}

/// H.4: 30+30 min soak (procedural).
///
/// Run `cargo xtask run-gui --fresh` twice in succession, each for 30 minutes,
/// without a reboot between runs.  Assert no kernel panic, no watchdog trips,
/// and metronome counter within ±5% of expected across both windows.
#[test_case]
#[ignore = "procedural — requires 60-min QEMU soak, not a harness test"]
fn soak_30_plus_30_min() {
    // placeholder — see module doc for procedure
}
