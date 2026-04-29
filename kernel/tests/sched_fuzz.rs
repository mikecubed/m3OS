#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! In-QEMU smoke fixture for Phase 57a Track I.3 — multi-core scheduler fuzz.
//!
//! # ⚠️ Scope: model-level only — this is NOT a live-scheduler test
//!
//! This fixture exercises the `kernel_core::sched_model` pure state machine
//! ONLY.  It does NOT call `block_current_until` / `wake_task_v2` /
//! `scan_expired_wake_deadlines` from `kernel/src/task/scheduler.rs` — those
//! live-scheduler primitives are not exercised here.
//!
//! Concretely, this fixture will NOT catch:
//!   - lock-ordering violations between `pi_lock` and `SCHEDULER.lock`
//!   - hardcoded block-kind bugs in `block_current_until`'s `kind` parameter
//!   - dual-source-of-truth divergence between `Task::state` and
//!     `TaskBlockState.state` on the various scheduler paths
//!   - ISR wake-drain bugs, remote dead/reap paths, dispatch-handler races
//!   - any race between real CPU cores in the live scheduler
//!
//! Catching those requires either: (a) a real multi-core in-QEMU integration
//! test that spawns kernel tasks and exercises the live primitives, or
//! (b) `cargo xtask run-gui --fresh` on the user's hardware (Track I.1 in
//! `docs/handoffs/57a-validation-gate.md`).
//!
//! # What this DOES test
//!
//! The four deterministic scenarios from `kernel-core/tests/sched_fuzz_multicore.rs`
//! are replayed here so that the in-QEMU build target compiles and links the
//! model.  This gives CI two complementary checks:
//!
//!   1. **Property-based depth** (kernel-core host tests): `sched_fuzz_multicore`
//!      runs 5 000 random rounds × 32 cross-core actions each — fast, shrinkable.
//!   2. **QEMU build smoke** (this file): the 4 deterministic scenarios boot
//!      on the kernel target without panic, confirming the model compiles for
//!      `x86_64-unknown-none`.
//!
//! # Why deterministic only in-QEMU?
//!
//! The in-QEMU binary is `no_std` + no allocation; there is no random number
//! generator available without the full kernel.  Property-based fuzzing (proptest)
//! requires `std`.  The random/shrinkable depth lives in the kernel-core host
//! tests; the QEMU fixture provides build-target coverage.
//!
//! # Duration / SCHED_FUZZ_DURATION_TICKS
//!
//! The spec mentions `SCHED_FUZZ_DURATION_TICKS` for the full 5-minute variant.
//! This in-QEMU fixture is intentionally bounded: the four deterministic
//! scenarios complete in well under 1 second.  For the full 5-minute soak,
//! use `cargo xtask test --test sched_fuzz --timeout 360` and extend the loop
//! count below (or use the kernel-core model test with PROPTEST_CASES=100000).
//!
//! # Running
//!
//! ```
//! cargo xtask test --test sched_fuzz
//! ```
//!
//! # Passing criteria
//!
//! - All test cases complete without panic (panic_handler calls `qemu_exit(FAIL)`).
//! - QEMU exits with `0x21` (success code for `isa-debug-exit` device value `0x10`).
//! - No `[WARN] [sched]` stuck-task line in serial output (N/A for this fixture:
//!   the stuck-task watchdog runs in the real kernel, not this test binary).

extern crate alloc;

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use kernel_core::sched_model::{BlockKind, BlockState, Event, apply_event};
use x86_64::instructions::{hlt, port::Port};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(sched_fuzz_kernel_test, config = &BOOTLOADER_CONFIG);

fn sched_fuzz_kernel_test(_boot_info: &'static mut BootInfo) -> ! {
    test_main();
    qemu_exit(0x10);
}

// ---------------------------------------------------------------------------
// Stub global allocator — tests must not allocate; this satisfies the linker.
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
// Scenario constants matching the model-level fuzz test.
// ---------------------------------------------------------------------------

const N_CORES: usize = 4;
const N_WORKERS: usize = 4;
const N_TASKS: usize = N_CORES * N_WORKERS;

// ---------------------------------------------------------------------------
// I.3 — Scenario 1: IPC call/reply burst (4 workers, 2 "cores").
//
// Caller workers block on Reply; server workers block on Recv; servers are
// woken (IPC deliver); servers reply — waking callers; second Wake is no-op.
//
// Acceptance: no lost wake, no double-enqueue, idempotent second Wake.
// ---------------------------------------------------------------------------

#[test_case]
fn ipc_call_reply_burst_callers_become_ready_after_server_reply() {
    let mut states = [BlockState::Running; N_TASKS];
    let mut enqueue_counts = [0u32; N_TASKS];

    // Callers (0,1) block on Reply.
    for i in [0usize, 1] {
        let (s, fx) = apply_event(
            states[i],
            Event::Block {
                kind: BlockKind::Reply,
                deadline: None,
            },
        );
        assert_eq!(s, BlockState::BlockedOnReply);
        assert!(fx.yielded);
        states[i] = s;
    }

    // Servers (4,5) block on Recv.
    for i in [4usize, 5] {
        let (s, fx) = apply_event(
            states[i],
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s, BlockState::BlockedOnRecv);
        assert!(fx.yielded);
        states[i] = s;
    }

    // IPC deliver: wake servers.
    for i in [4usize, 5] {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready);
        assert!(fx.enqueue_to_run_queue);
        assert!(fx.on_cpu_wait_required);
        states[i] = s;
        enqueue_counts[i] += 1;
    }

    // Servers reply: wake callers.
    for i in [0usize, 1] {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready);
        assert!(fx.enqueue_to_run_queue);
        states[i] = s;
        enqueue_counts[i] += 1;
    }

    // Second Wake on each Ready task must be a no-op (idempotent).
    for i in [0usize, 1, 4, 5] {
        let prev = states[i];
        assert_eq!(prev, BlockState::Ready);
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready);
        assert!(!fx.enqueue_to_run_queue);
    }

    // Each task enqueued exactly once.
    for i in [0usize, 1, 4, 5] {
        assert_eq!(enqueue_counts[i], 1);
    }
}

// ---------------------------------------------------------------------------
// I.3 — Scenario 2: futex wait/wake + ScanExpired race — no double-enqueue.
//
// 4 workers park on Futex with a deadline. A cross-core Wake arrives.
// A ScanExpired then races (deadline elapsed but task is Ready) — must be a
// no-op: no second enqueue.
//
// Acceptance: exactly one enqueue per waiter; ScanExpired on Ready is no-op.
// ---------------------------------------------------------------------------

#[test_case]
fn futex_wait_wake_then_scan_expired_no_double_enqueue() {
    let mut states = [BlockState::Running; N_TASKS];
    let mut enqueue_counts = [0u32; N_TASKS];

    let waiters = [0usize, 4, 8, 12]; // one per core

    // Park on Futex.
    for i in waiters {
        let (s, fx) = apply_event(
            states[i],
            Event::Block {
                kind: BlockKind::Futex,
                deadline: Some(5_000),
            },
        );
        assert_eq!(s, BlockState::BlockedOnFutex);
        assert!(fx.yielded);
        assert_eq!(fx.deadline_set, Some(5_000));
        states[i] = s;
    }

    // Cross-core futex_wake.
    for i in waiters {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready);
        assert!(fx.enqueue_to_run_queue);
        assert!(fx.deadline_cleared);
        assert!(fx.on_cpu_wait_required);
        states[i] = s;
        enqueue_counts[i] += 1;
    }

    // ScanExpired races — deadline elapsed but task is already Ready.
    for i in waiters {
        let prev = states[i];
        assert_eq!(prev, BlockState::Ready);
        let (s, fx) = apply_event(prev, Event::ScanExpired { now: 10_000 });
        assert_eq!(s, BlockState::Ready);
        assert!(!fx.enqueue_to_run_queue); // no double-enqueue
    }

    // Exactly one enqueue per waiter.
    for i in waiters {
        assert_eq!(enqueue_counts[i], 1);
    }
}

// ---------------------------------------------------------------------------
// I.3 — Scenario 3: notification signal/wait — all 4 cores, triple race.
//
// 8 workers across 4 cores block on Notif with a deadline.
// A cross-core signal (Wake) arrives; ScanExpired races (no-op); second Wake
// races (idempotent no-op).
//
// Acceptance: exactly one enqueue per waiter; both races are no-ops.
// ---------------------------------------------------------------------------

#[test_case]
fn notif_signal_wait_triple_race_all_cores() {
    let mut states = [BlockState::Running; N_TASKS];
    let mut enqueue_counts = [0u32; N_TASKS];

    let waiters = [0usize, 1, 4, 5, 8, 9, 12, 13];

    // Park on Notif.
    for i in waiters {
        let (s, fx) = apply_event(
            states[i],
            Event::Block {
                kind: BlockKind::Notif,
                deadline: Some(1_000),
            },
        );
        assert_eq!(s, BlockState::BlockedOnNotif);
        assert!(fx.yielded);
        states[i] = s;
    }

    // Signal (cross-core Wake).
    for i in waiters {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready);
        assert!(fx.enqueue_to_run_queue);
        assert!(fx.deadline_cleared);
        states[i] = s;
        enqueue_counts[i] += 1;
    }

    // ScanExpired races (task already Ready — no-op).
    for i in waiters {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::ScanExpired { now: 2_000 });
        assert_eq!(s, BlockState::Ready);
        assert!(!fx.enqueue_to_run_queue);
    }

    // Second Wake races (idempotent — no-op).
    for i in waiters {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready);
        assert!(!fx.enqueue_to_run_queue);
    }

    // One enqueue per waiter.
    for i in waiters {
        assert_eq!(enqueue_counts[i], 1);
    }
}

// ---------------------------------------------------------------------------
// I.3 — Scenario 4: self-revert (ConditionTrue) on all 4 cores × all kinds.
//
// 16 workers (N_TASKS) block on each of the 4 block kinds (Recv, Notif,
// Futex, Reply — one kind per core).  Immediately after the state write, a
// ConditionTrue fires (wake arrived between step 1 and step 3 of the
// four-step recipe).  Each worker must self-revert to Running without
// yielding and without enqueuing.
//
// Acceptance: reverted_state == Running; yielded == false; enqueue == false.
// ---------------------------------------------------------------------------

#[test_case]
fn condition_true_self_revert_all_16_workers_no_yield_no_enqueue() {
    let block_kinds = [
        BlockKind::Recv,
        BlockKind::Notif,
        BlockKind::Futex,
        BlockKind::Reply,
    ];

    for (core, kind) in block_kinds.iter().enumerate() {
        for worker in 0..N_WORKERS {
            let _task = core * N_WORKERS + worker;

            // Step 1: state write Running → Blocked*.
            let (blocked, block_fx) = apply_event(
                BlockState::Running,
                Event::Block {
                    kind: *kind,
                    deadline: None,
                },
            );
            assert!(blocked.is_blocked());
            assert!(block_fx.yielded);

            // Step 3: condition already true → self-revert.
            let (reverted, revert_fx) = apply_event(blocked, Event::ConditionTrue);
            assert_eq!(reverted, BlockState::Running);
            assert!(!revert_fx.yielded);
            assert!(revert_fx.deadline_cleared);
            assert!(!revert_fx.enqueue_to_run_queue);
        }
    }
}
