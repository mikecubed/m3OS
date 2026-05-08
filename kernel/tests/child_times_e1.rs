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
