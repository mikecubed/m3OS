#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! QEMU integration test stubs for Phase 57d Track A — voluntary preemption.
//!
//! All tests are stubs that return immediately.  They will be activated in
//! Track G once the kernel-side preemption wiring (syscall-return hook,
//! `preempt_enable` zero-crossing handler) is in place.
//!
//! | Test | Track | Scenario |
//! |---|---|---|
//! | `preempt_user_loop` | G | user-mode task is preempted mid-loop |
//! | `no_preempt_when_count_nonzero` | G | preempt_count > 0 blocks preemption |
//! | `no_preempt_when_kernel_mode` | G | kernel-mode path skips preemption |
//! | `preempt_enable_zero_crossing` | G | zero-crossing sets resched pending |
//!
//! # Test harness
//!
//! Matches the pattern of `kernel/tests/bound_recv.rs`: custom test framework
//! with a stub `GlobalAlloc`, `panic_handler`, and QEMU ISA debug-exit device
//! for pass/fail signalling.
//!
//! Source ref: phase-57d-track-A.2
//! Depends on: phase-57d-track-A.1 (preempt_model pure-logic)

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use x86_64::instructions::{hlt, port::Port};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(preempt_voluntary_kernel_test, config = &BOOTLOADER_CONFIG);

fn preempt_voluntary_kernel_test(_boot_info: &'static mut BootInfo) -> ! {
    test_main();
    qemu_exit(0x10);
}

// ---------------------------------------------------------------------------
// Stub global allocator — satisfies the linker; stubs do not allocate.
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
// Stub tests — activate in Track G
// ---------------------------------------------------------------------------

/// Stub: user-mode task is preempted mid-loop when the scheduler sets
/// `reschedule` and `preempt_count == 0` at the syscall-return boundary.
///
/// TODO: activate in Track G once the syscall-return preemption hook lands.
#[test_case]
fn preempt_user_loop() {
    // TODO: activate in Track G
}

/// Stub: `preempt_count > 0` prevents preemption even when `reschedule` is set.
///
/// TODO: activate in Track G once the preempt_disable / preempt_enable kernel
/// wiring and the preemption eligibility check are in place.
#[test_case]
fn no_preempt_when_count_nonzero() {
    // TODO: activate in Track G
}

/// Stub: kernel-mode paths (from_user == false) do not trigger preemption at
/// the user-mode-return boundary check.
///
/// TODO: activate in Track G once the IRQ-return-to-ring-3 path is wired.
#[test_case]
fn no_preempt_when_kernel_mode() {
    // TODO: activate in Track G
}

/// Stub: `preempt_enable` dropping the count to zero while `reschedule == true`
/// sets `preempt_resched_pending` so the scheduler yields at the next safe point.
///
/// TODO: activate in Track G once the zero-crossing hook in `preempt_enable` lands.
#[test_case]
fn preempt_enable_zero_crossing() {
    // TODO: activate in Track G
}
