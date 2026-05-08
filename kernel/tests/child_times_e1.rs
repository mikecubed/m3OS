#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Phase 61 Track E.1 — recursive child-time accumulation rule.
//!
//! Validates `Task::child_user_ticks` / `child_system_ticks` semantics:
//! when a parent reaps a zombie, the parent absorbs both the zombie's own
//! per-task tick counts AND the zombie's already-accumulated descendant
//! tick counts. POSIX `times(2)` requires this — a parent who reaps a
//! child inherits the entire reaped subtree's CPU time.
//!
//! This test exercises `current_task_accumulate_child_times` directly from
//! inside a kernel task so `current_task_id()` resolves to a real entry in
//! the scheduler's task table. The full userspace fork → execve → exit →
//! waitpid → sys_times round-trip lives in `sys_times_children.rs`
//! (post-Track E.3, when `sys_wait4` lands).

extern crate alloc;

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::panic::PanicInfo;

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(child_times_e1_test, config = &BOOTLOADER_CONFIG);

fn child_times_e1_test(boot_info: &'static mut BootInfo) -> ! {
    kernel::test_prelude::init_minimal_smp(boot_info);

    kernel::task::spawn(test_runner_task, "e1-test-runner");
    kernel::test_prelude::spawn_idle();
    kernel::task::run()
}

fn test_runner_task() -> ! {
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
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(loc) = info.location() {
        kernel::serial_println!(
            "[e1-test] PANIC at {}:{}: {}",
            loc.file(),
            loc.line(),
            info.message(),
        );
    } else {
        kernel::serial_println!("[e1-test] PANIC: {}", info.message());
    }
    kernel::test_prelude::qemu_exit_failure()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test_case]
fn child_times_recursive_accumulation_rule() {
    use kernel::task::scheduler::{
        current_task_accumulate_child_times, current_task_child_times, current_task_id,
    };

    assert!(
        current_task_id().is_some(),
        "test must run inside a task context"
    );

    // Baseline: a freshly-spawned task's child_* fields start at zero.
    let (cu, cs) = current_task_child_times().expect("current task should exist");
    assert_eq!(cu, 0, "child_user_ticks should start at 0");
    assert_eq!(cs, 0, "child_system_ticks should start at 0");

    // First reap: zombie ran 5 user / 2 system ticks of its own and had
    // already accumulated 3 user / 1 system from a reaped grandchild.
    // Recursive rule: parent absorbs zombie.own_* + zombie.child_*.
    assert!(current_task_accumulate_child_times(5, 2, 3, 1));
    let (cu, cs) = current_task_child_times().unwrap();
    assert_eq!(cu, 5 + 3, "first reap: child_user_ticks");
    assert_eq!(cs, 2 + 1, "first reap: child_system_ticks");

    // Second reap on top: another zombie with 10 / 4 own and zero
    // accumulated. Verifies the accumulator combines without overwriting.
    assert!(current_task_accumulate_child_times(10, 4, 0, 0));
    let (cu, cs) = current_task_child_times().unwrap();
    assert_eq!(cu, 8 + 10, "second reap: child_user_ticks");
    assert_eq!(cs, 3 + 4, "second reap: child_system_ticks");

    // Third reap: zombie with mixed own + child contributions, larger
    // values to confirm there's no silent overflow / saturation in the
    // common range.
    assert!(current_task_accumulate_child_times(100, 50, 25, 10));
    let (cu, cs) = current_task_child_times().unwrap();
    assert_eq!(cu, 18 + 100 + 25, "third reap: child_user_ticks");
    assert_eq!(cs, 7 + 50 + 10, "third reap: child_system_ticks");

    kernel::serial_println!("[e1-test] recursive child-time accumulation rule verified");
}

#[test_case]
fn rusage_counters_increment_and_accumulate() {
    use kernel::task::scheduler::{
        RusageCounters, current_task_accumulate_child_rusage, current_task_record_ctxsw,
        current_task_record_page_fault, current_task_rusage_children, current_task_rusage_self,
    };

    // Snapshot the starting counters — the test runner may have already
    // executed yield_now and other paths that increment counters before
    // this body runs, so we treat them as a baseline rather than asserting
    // they are zero. Phase 61 Track E.4.
    let (start_minor, start_major, start_vol, start_invol) =
        current_task_rusage_self().expect("current task should exist");

    // Two minor faults, one major.
    current_task_record_page_fault(false);
    current_task_record_page_fault(false);
    current_task_record_page_fault(true);

    // Three voluntary, one involuntary.
    current_task_record_ctxsw(true);
    current_task_record_ctxsw(true);
    current_task_record_ctxsw(true);
    current_task_record_ctxsw(false);

    let (minor, major, vol, invol) = current_task_rusage_self().unwrap();
    assert_eq!(
        minor.saturating_sub(start_minor),
        2,
        "minor_faults delta after 2 minor records",
    );
    assert_eq!(
        major.saturating_sub(start_major),
        1,
        "major_faults delta after 1 major record",
    );
    assert_eq!(
        vol.saturating_sub(start_vol),
        3,
        "voluntary_ctxsw delta after 3 records",
    );
    assert_eq!(
        invol.saturating_sub(start_invol),
        1,
        "involuntary_ctxsw delta after 1 record",
    );

    // Now exercise the recursive accumulation rule for the children path.
    let (cm0, cM0, cv0, cI0) = current_task_rusage_children().unwrap();

    let zombie_a = RusageCounters {
        minor_faults: 7,
        major_faults: 1,
        voluntary_ctxsw: 4,
        involuntary_ctxsw: 2,
        child_minor_faults: 3, // already-reaped grandchild
        child_major_faults: 0,
        child_voluntary_ctxsw: 1,
        child_involuntary_ctxsw: 0,
    };
    assert!(current_task_accumulate_child_rusage(zombie_a));

    let (cm1, cM1, cv1, cI1) = current_task_rusage_children().unwrap();
    assert_eq!(
        cm1 - cm0,
        7 + 3,
        "recursive minor: zombie own + zombie child"
    );
    assert_eq!(cM1 - cM0, 1 + 0, "recursive major");
    assert_eq!(cv1 - cv0, 4 + 1, "recursive voluntary ctxsw");
    assert_eq!(cI1 - cI0, 2 + 0, "recursive involuntary ctxsw");

    kernel::serial_println!("[e4-test] rusage counter increment + recursive accumulation verified");
}

// ---------------------------------------------------------------------------
// Phase 61 Track E.2 — per-tick CS-based user/system tick split
// ---------------------------------------------------------------------------

#[test_case]
fn per_tick_sampling_increments_system_ticks_for_kernel_task() {
    // The test runner task is a kernel task — when it executes, it does so
    // in ring 0. Per-tick CPU sampling at the timer IRQ
    // (`tick_account_current_task(false)` when the interrupted CS is
    // ring 0) increments `system_ticks` for every timer tick that hits
    // while this task is running. Sample wall-clock ticks; spin
    // (non-yielding) for several timer ticks; verify `system_ticks`
    // increased.
    //
    // Pre-Phase 61 (Track E.2), `accumulate_ticks` attributed all elapsed
    // time to `user_ticks` regardless of ring, making this assertion
    // false. The Phase 35 H.2 acceptance line was stale until now.

    let (u0, s0) = kernel::task::scheduler::current_task_times().unwrap();
    let start = kernel::arch::x86_64::interrupts::tick_count();

    // Stay running (no yield) for ~150 ticks. Timer IRQ fires every tick;
    // each fire while we're executing here is attributed to system_ticks.
    while kernel::arch::x86_64::interrupts::tick_count().saturating_sub(start) < 150 {
        for _ in 0..1_000 {
            core::hint::spin_loop();
        }
    }

    let (u1, s1) = kernel::task::scheduler::current_task_times().unwrap();
    kernel::serial_println!(
        "[e2-test] before: user={} system={}; after: user={} system={}",
        u0,
        s0,
        u1,
        s1
    );

    // system_ticks must have advanced — the runner is a kernel task that
    // ran continuously for ~150 timer ticks of wall time, every tick
    // attributed to ring 0.
    assert!(
        s1 > s0,
        "system_ticks did not advance during kernel-mode execution: \
         {s0} -> {s1} (Track E.2 — per-tick CS sampling)",
    );

    // user_ticks must not increase from kernel-only execution. (A small
    // delta could in principle occur if the test runner was somehow
    // scheduled in ring 3 — but kernel tasks never enter ring 3, so this
    // upper bound pins the contract.)
    assert_eq!(
        u0, u1,
        "user_ticks should not advance during pure kernel-mode execution: \
         {u0} -> {u1} (Track E.2 — per-tick CS sampling)",
    );

    kernel::serial_println!(
        "[e2-test] per-tick CS sampling for kernel-mode tasks verified: \
         system_ticks {} -> {} (delta {})",
        s0,
        s1,
        s1 - s0
    );
}
