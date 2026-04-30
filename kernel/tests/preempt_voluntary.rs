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

/// Stub: `timer_entry` user path saves all 15 GPRs and `PreemptTrapFrameUser`
/// layout matches the on-stack layout laid down by the asm stub.
///
/// TODO: activate in Track G once the QEMU single-step harness can inspect
/// register state before and after the handler returns via `iretq`.
#[test_case]
fn timer_entry_user_path_saves_gprs() {
    // TODO: activate in Track G
}

/// Stub: `timer_entry` kernel path saves all 15 GPRs into
/// `PreemptTrapFrameKernel` and `captured_kernel_rsp` equals the interrupted
/// RSP (rsp + 15*8 + 3*8 at the point of the `lea`).
///
/// TODO: activate in Track G.
#[test_case]
fn timer_entry_kernel_path_saves_gprs() {
    // TODO: activate in Track G
}

/// Stub: the `mov rdi, rsp` + `call timer_handler_user` sequence lands with
/// RSP 16-byte aligned so any `movaps` in the Rust handler does not fault.
///
/// TODO: activate in Track G once the alignment invariant is verified via
/// QEMU memory access breakpoints.
#[test_case]
fn timer_entry_movaps_alignment() {
    // TODO: activate in Track G
}

/// Stub: `reschedule_ipi_entry` kernel path round-trip — GPRs saved before
/// `call reschedule_ipi_handler_kernel` are intact after `restore_gprs_all`
/// + `iretq`.
///
/// TODO: activate in Track G.
#[test_case]
fn reschedule_ipi_entry_kernel_round_trip() {
    // TODO: activate in Track G
}

/// Stub: `peek_preempt_count_irq()` returns a value matching the lock-acquired
/// path's read of the current task's `preempt_count`.
///
/// With preempts disabled, `peek_preempt_count_irq()` must equal the task's
/// own `preempt_count` field (read atomically through the scheduler lock).
///
/// TODO: activate in Track G when the scheduler is fully wired up and we can
/// run with a real current task context in the QEMU harness.
#[test_case]
#[ignore = "requires QEMU + full scheduler init"]
fn peek_preempt_count_matches_task_count() {
    // Full impl in Track G when the scheduler is wired up.
}

// ---------------------------------------------------------------------------
// Phase 57d Track C+D stubs — activate in Track G
// ---------------------------------------------------------------------------

/// Stub: `preempt_to_scheduler` correctly saves all 15 GPRs and the iretq
/// fields (rip, cs, rflags, rsp, ss) into `Task::preempt_frame`.
///
/// Full verification requires QEMU + the full scheduler running a real
/// userspace task so the frame can be compared before and after preemption.
///
/// TODO: activate in Track G.
#[test_case]
#[ignore = "requires QEMU + full scheduler init"]
fn preempt_to_scheduler_saves_frame_correctly() {
    // TODO: activate in Track G.
}

/// Stub: `preempt_resume_to_user` restores rip and all GPRs from
/// `Task::preempt_frame` and executes iretq to the original user instruction.
///
/// TODO: activate in Track G once the QEMU single-step harness can inspect
/// register state immediately after the iretq.
#[test_case]
#[ignore = "requires QEMU + full scheduler init"]
fn preempt_resume_restores_rip_and_registers() {
    // TODO: activate in Track G.
}

/// Stub: a cooperative yield (via `yield_now`) still uses `switch_context`
/// (resume_mode == Cooperative) rather than `preempt_resume_to_user`.
///
/// TODO: activate in Track G once dispatch-path tracing is available.
#[test_case]
#[ignore = "requires QEMU + full scheduler init"]
fn cooperative_yield_still_uses_switch_context() {
    // TODO: activate in Track G.
}

// ---------------------------------------------------------------------------
// Phase 57d Track G stubs — IRQ-return voluntary preemption
// ---------------------------------------------------------------------------

/// Stub: timer ISR preempts a user-mode tight loop within 1 ms.
///
/// Full implementation requires QEMU + `preempt-voluntary` feature + a ring-3
/// tight-loop binary that the kernel spawns and then observes is preempted.
#[test_case]
#[ignore = "requires QEMU + preempt-voluntary feature + userspace tight loop"]
fn timer_handler_preempts_user_within_1ms() {
    // Feature-on: spawn a tight ring-3 loop, observe preemption fires within 1ms.
}

/// Stub: reschedule IPI preempts a user-mode task on core 1 within 1 ms.
///
/// Full implementation requires QEMU + SMP + `preempt-voluntary` feature.
#[test_case]
#[ignore = "requires QEMU + SMP + preempt-voluntary feature"]
fn reschedule_ipi_preempts_user_within_1ms() {
    // Core 1 tight loop; core 0 sends wake IPI; core 1 preempts within 1ms.
}

/// Stub: `preempt_count != 0` suppresses preemption at the IRQ-return boundary.
///
/// Full implementation requires QEMU + `preempt-voluntary` feature.
#[test_case]
#[ignore = "requires QEMU + preempt-voluntary feature"]
fn preempt_count_nonzero_suppresses_preemption() {
    // preempt_disable() held: timer fires, preempt_count != 0, no preemption.
}
