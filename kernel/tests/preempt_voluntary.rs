#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! QEMU integration tests for Phase 57d Track H — voluntary preemption logic.
//!
//! # Live kernel-side logic tests (Track H.1)
//!
//! Three tests exercise `kernel_core::preempt_model` — the pure-logic mirror of
//! the kernel's preemption state machine — directly in QEMU.  These run without
//! per-core data or scheduler initialisation because the model lives in
//! `kernel_core` and uses no atomics or kernel-only globals.
//!
//! | Test | What it pins |
//! |---|---|
//! | `test_peek_preempt_count_irq_nonzero_suppresses` | `Counter::disable` makes count non-zero; `enable` restores zero |
//! | `test_preempt_resched_pending_flag_set_and_cleared` | flag survives while depth > 0; `consume_pending` clears it |
//! | `test_preempt_enable_zero_crossing_sets_resched_pending` | zero-crossing with `reschedule=true` sets the pending flag |
//!
//! # Why `kernel_core` models and not live kernel functions?
//!
//! The QEMU test binaries in `kernel/tests/` are standalone `no_std` binaries
//! that boot via `entry_point!`.  The `kernel` crate is a binary crate (only
//! `src/main.rs`; no `src/lib.rs`), so its internal functions — including
//! `preempt_disable`, `preempt_enable`, `peek_preempt_count_irq`, and `per_core()`
//! — are not accessible from these test binaries.  Additionally, `per_core()`
//! reads `IA32_GS_BASE` and panics if it is 0; in the QEMU test harness,
//! `test_main` runs before `smp::init_bsp_per_core`, so `per_core()` would
//! panic unconditionally.  The `kernel_core::preempt_model` types (`Counter`,
//! `DeferredReschedModel`) are the host-testable pure-logic mirror that pins
//! the same contracts.  Any divergence between the model and the live kernel
//! functions would show up in the Track G QEMU integration tests (the
//! `#[ignore]` stubs below) once the kernel is built as a lib.
//!
//! # Existing `#[ignore]` stubs (Tracks A–G)
//!
//! The remaining tests below remain `#[ignore]`.  Each requires one or more of:
//! - A ring-3 userspace process that can be spawned and observed for preemption.
//! - `smp::init_bsp_per_core` to run before `test_main` (needs full UEFI init).
//! - The `preempt-voluntary` Cargo feature to be enabled on the kernel build.
//!
//! # Test harness
//!
//! Matches the pattern of `kernel/tests/bound_recv.rs`: custom test framework
//! with a stub `GlobalAlloc`, `panic_handler`, and QEMU ISA debug-exit device
//! for pass/fail signalling.
//!
//! Source ref: phase-57d-track-H.1
//! Depends on: kernel_core::preempt_model (phase-57d-track-A.1)

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use kernel_core::preempt_model::{Counter, DeferredReschedModel};
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
// Track H.1 — kernel-side preemption logic tests (QEMU-runnable)
//
// These tests exercise kernel_core::preempt_model, the pure-logic mirror of
// the kernel's preemption state machine.  They run without per-core data or
// scheduler init and cannot panic in the QEMU test context.
// ---------------------------------------------------------------------------

/// H.1.1: `Counter::disable` makes the count non-zero; `enable` restores zero.
///
/// This is the model-layer analog of the kernel's `preempt_disable` /
/// `peek_preempt_count_irq` / `preempt_enable` round trip.  It verifies that
/// the pure-logic contract — which the kernel's lock-free AtomicI32 helpers
/// mirror — behaves correctly: a single disable puts the counter above zero,
/// and the matching enable brings it back to zero.
#[test_case]
fn test_peek_preempt_count_irq_nonzero_suppresses() {
    let mut counter = Counter::new();
    assert_eq!(counter.count(), 0, "fresh counter must start at zero");

    counter.disable();
    assert_ne!(
        counter.count(),
        0,
        "after disable, preempt_count must be non-zero (preemption suppressed)"
    );

    let crossed = counter.enable();
    assert!(crossed, "single disable→enable must be a zero-crossing");
    assert_eq!(
        counter.count(),
        0,
        "after enable, preempt_count must return to zero (preemption re-enabled)"
    );
}

/// H.1.2: `preempt_resched_pending` flag survives while depth > 0;
/// `consume_pending` clears it once depth reaches zero.
///
/// Models the contract of `check_deferred_preempt_at_user_return`: when
/// `preempt_count != 0` the function returns without consuming the flag
/// (the kernel check `if pc != 0 { return; }` before the `swap`).  Only
/// when depth is zero does `consume_pending` clear the flag, mirroring
/// the production `swap(false, AcqRel)`.
#[test_case]
fn test_preempt_resched_pending_flag_set_and_cleared() {
    let mut model = DeferredReschedModel::new();

    // Pre-set the pending flag directly (mirrors a prior zero-crossing or
    // an explicit store from the interrupt path).
    model.preempt_resched_pending = true;

    // Disable raises depth to 1 — now check_deferred_preempt_at_user_return
    // would return early (count != 0) without touching the flag.
    model.disable();
    assert_eq!(model.count(), 1, "depth must be 1 after disable");
    assert!(
        model.preempt_resched_pending,
        "flag must survive while preempt_count != 0 (depth-guard not yet released)"
    );

    // Release the depth.  The flag was already set; enable does not clear it.
    let _ = model.enable();
    assert_eq!(model.count(), 0, "depth must return to zero after enable");
    assert!(
        model.preempt_resched_pending,
        "flag must still be set immediately after enable (consume not yet called)"
    );

    // Now consume — mirrors the swap(false) in check_deferred_preempt_at_user_return.
    let was = model.consume_pending();
    assert!(was, "consume_pending must return true (flag was set)");
    assert!(
        !model.preempt_resched_pending,
        "flag must be cleared after consume_pending"
    );
}

/// H.1.3: `preempt_enable` zero-crossing with `reschedule=true` sets
/// `preempt_resched_pending`.
///
/// Models Phase 57d E.2: when `preempt_count` drops from 1 to 0 and the
/// per-core `reschedule` flag is true, `preempt_resched_pending` must be
/// set so the user-mode-return boundary (E.3) can trigger a voluntary yield.
#[test_case]
fn test_preempt_enable_zero_crossing_sets_resched_pending() {
    let mut model = DeferredReschedModel::new();
    model.reschedule = true;

    model.disable();
    assert_eq!(model.count(), 1);
    assert!(
        !model.preempt_resched_pending,
        "flag must start clear before zero-crossing"
    );

    model.enable();
    assert_eq!(model.count(), 0, "depth must be zero after enable");
    assert!(
        model.preempt_resched_pending,
        "E.2: zero-crossing with reschedule=true must set preempt_resched_pending"
    );

    // Cleanup.
    let _ = model.consume_pending();
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
/// To make live this test needs:
/// - The `kernel` crate exposed as a library (`src/lib.rs`) so QEMU test
///   binaries can call `kernel::task::scheduler::preempt_*` directly.
/// - `smp::init_bsp_per_core` called before `test_main` so `per_core()`
///   does not panic (GS_BASE must be set before any scheduler function).
/// - `preempt-voluntary` Cargo feature enabled on the test build.
/// - A ring-3 tight-loop binary the kernel can spawn and observe being
///   preempted within 1 ms of the scheduler timer firing.
#[test_case]
#[ignore = "requires QEMU + preempt-voluntary feature + userspace tight loop"]
fn timer_handler_preempts_user_within_1ms() {
    // Feature-on: spawn a tight ring-3 loop, observe preemption fires within 1ms.
}

/// Stub: reschedule IPI preempts a user-mode task on core 1 within 1 ms.
///
/// To make live this test needs (in addition to the requirements above):
/// - Multi-core QEMU (at least 2 vCPUs).
/// - Core 0 able to send a reschedule IPI to core 1.
/// - Core 1 running a ring-3 tight loop observable for preemption.
#[test_case]
#[ignore = "requires QEMU + SMP + preempt-voluntary feature"]
fn reschedule_ipi_preempts_user_within_1ms() {
    // Core 1 tight loop; core 0 sends wake IPI; core 1 preempts within 1ms.
}

/// Stub: `preempt_count != 0` suppresses preemption at the IRQ-return boundary.
///
/// To make live this test needs (in addition to the requirements above):
/// - Live kernel functions accessible from the test binary.
/// - A ring-3 task running with `preempt_disable()` held so the timer ISR
///   observes `preempt_count != 0` and skips preemption.
#[test_case]
#[ignore = "requires QEMU + preempt-voluntary feature"]
fn preempt_count_nonzero_suppresses_preemption() {
    // preempt_disable() held: timer fires, preempt_count != 0, no preemption.
}
