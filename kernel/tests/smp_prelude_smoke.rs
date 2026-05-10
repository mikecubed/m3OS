#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Phase 61 Track 0b — smoke test for `kernel::test_prelude`.
//!
//! Proves that an integration test under `kernel/tests/*.rs` can:
//!
//! 1. link against the `kernel` library crate;
//! 2. inherit the kernel's `#[global_allocator]` (no stub allocator);
//! 3. drive the live boot sequence via `init_minimal_smp`;
//! 4. observe live kernel state through the public API surface
//!    (`smp::core_count`, `smp::get_core_data(_).with_run_queue(...)`,
//!    heap allocation through `Box`/`Vec`).
//!
//! If this test passes, the harness is ready for Phase 61 Tracks B / C.2 /
//! D / E.2-test / E.3-test / E.4-test / F-test.

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::panic::PanicInfo;

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(smp_prelude_smoke_test, config = &BOOTLOADER_CONFIG);

fn smp_prelude_smoke_test(boot_info: &'static mut BootInfo) -> ! {
    kernel::test_prelude::init_minimal_smp(boot_info);
    test_main();
    kernel::test_prelude::qemu_exit_success()
}

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

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    kernel::test_prelude::qemu_exit_failure()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test_case]
fn smp_core_count_is_at_least_one() {
    let cores = kernel::smp::core_count();
    assert!(cores >= 1, "core_count returned 0");
}

#[test_case]
fn smp_per_core_ready_after_init() {
    assert!(
        kernel::smp::is_per_core_ready(),
        "init_minimal_smp did not set is_per_core_ready"
    );
}

#[test_case]
fn smp_bsp_run_queue_initially_empty() {
    let bsp = kernel::smp::get_core_data(0).expect("BSP core data should exist");
    let len = bsp.with_run_queue(|q| q.len());
    assert_eq!(len, 0, "BSP run queue should be empty before any spawns");
}

#[test_case]
fn heap_alloc_via_kernel_global_allocator() {
    // Linking against `kernel` lib means we inherit its #[global_allocator].
    // Box and Vec must work post-init_minimal_smp (mm::init has run).
    let b = Box::new(0xDEADBEEFu64);
    assert_eq!(*b, 0xDEADBEEF);

    let mut v: Vec<u32> = Vec::with_capacity(64);
    for i in 0..64 {
        v.push(i);
    }
    assert_eq!(v.len(), 64);
    assert_eq!(v[63], 63);
}

#[test_case]
fn balance_threshold_constant_is_named_and_two() {
    // Phase 61 Track A renamed the load-balance hysteresis threshold from
    // a magic `2` into a named constant; this test pins the value.
    assert_eq!(kernel::task::scheduler::BALANCE_THRESHOLD, 2);
}
