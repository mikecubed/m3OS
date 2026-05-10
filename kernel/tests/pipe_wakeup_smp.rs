#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Phase 61 Track D.1 — cross-core pipe wakeup regression test.
//!
//! Validates that a reader task pinned to one core blocked on
//! `PIPE_WAITQUEUES[pipe_id]` is woken within a few ticks of a writer
//! task on a different core calling `pipe_write` + `wake_pipe`. The
//! `WaitQueue.wake_all` path runs `wake_task_v2`, which sends a
//! reschedule IPI to the reader's core when the reader is parked
//! there — this test pins that contract.
//!
//! Track F's syscall-layer refactor (`sys_read` / `sys_write` PipeRead /
//! PipeWrite arms swapped from `yield_now()` polling to
//! `WaitQueue.sleep`) means a userspace `read(pipe_fd)` blocked on an
//! empty pipe wakes within a few ticks of the writer's write rather
//! than on next scheduler dispatch (~10 ms at 100 Hz). This test
//! exercises the kernel-side mechanism the syscall arm now uses;
//! end-to-end userspace pipe-blocking validation lives in
//! `kernel/tests/pipe_blocking_no_busy_wait.rs`.

extern crate alloc;

use alloc::sync::Arc;
use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(pipe_wakeup_smp_test, config = &BOOTLOADER_CONFIG);

fn pipe_wakeup_smp_test(boot_info: &'static mut BootInfo) -> ! {
    kernel::test_prelude::init_minimal_smp(boot_info);
    kernel::test_prelude::boot_aps_if_available();

    kernel::task::spawn(test_runner_task, "pipe-wakeup-runner");
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
    for t in tests {
        t.run();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(loc) = info.location() {
        kernel::serial_println!(
            "[pipe-wakeup-test] PANIC at {}:{}: {}",
            loc.file(),
            loc.line(),
            info.message()
        );
    } else {
        kernel::serial_println!("[pipe-wakeup-test] PANIC: {}", info.message());
    }
    kernel::test_prelude::qemu_exit_failure()
}

// ---------------------------------------------------------------------------
// Shared state between reader / writer / runner tasks
// ---------------------------------------------------------------------------

/// Pipe under test. Set by the runner before spawning workers.
static PIPE_ID: AtomicU64 = AtomicU64::new(u64::MAX);

/// Tick at which the writer wrote — sampled inside the writer immediately
/// before `pipe_write` returns. Used to bound wake latency.
static WRITE_TICK: AtomicU64 = AtomicU64::new(0);

/// Tick at which the reader observed the data. Sampled inside the reader
/// immediately after `pipe_read` returns Ok.
static READ_TICK: AtomicU64 = AtomicU64::new(0);

/// Set true once the reader has confirmed it parked on the wait queue.
/// The runner waits for this flag before spawning the writer so the
/// reader is guaranteed to be blocked at the time of the write — i.e.,
/// the test exercises the cross-core wake path, not a "writer wrote
/// first, reader never blocked" race.
static READER_PARKED: AtomicBool = AtomicBool::new(false);

/// Set true after the reader exits its loop. Lets the runner observe
/// completion without polling the read tick alone.
static READER_DONE: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Reader task
// ---------------------------------------------------------------------------

fn reader_task() -> ! {
    let pipe_id = PIPE_ID.load(Ordering::Acquire) as usize;
    let task_id = kernel::task::scheduler::current_task_id().expect("reader has task id");

    let woken = Arc::new(AtomicBool::new(false));
    let mut buf = [0u8; 16];

    loop {
        woken.store(false, Ordering::Release);
        kernel::pipe::pipe_register_waiter(pipe_id, task_id, &woken);

        match kernel::pipe::pipe_read(pipe_id, &mut buf[..1]) {
            Ok(0) => {
                kernel::pipe::pipe_deregister_waiter(pipe_id, task_id);
                kernel::serial_println!("[pipe-wakeup-test] reader: EOF");
                READER_DONE.store(true, Ordering::Release);
                loop {
                    kernel::task::yield_now();
                }
            }
            Ok(_n) => {
                kernel::pipe::pipe_deregister_waiter(pipe_id, task_id);
                let now = kernel::arch::x86_64::interrupts::tick_count();
                READ_TICK.store(now, Ordering::Release);
                kernel::serial_println!(
                    "[pipe-wakeup-test] reader: got byte {:#x} at tick {}",
                    buf[0],
                    now
                );
                READER_DONE.store(true, Ordering::Release);
                loop {
                    kernel::task::yield_now();
                }
            }
            Err(_would_block) => {
                READER_PARKED.store(true, Ordering::Release);
                let _ = kernel::task::scheduler::block_current_until(
                    kernel::task::TaskState::BlockedOnRecv,
                    &woken,
                    None,
                );
                kernel::pipe::pipe_deregister_waiter(pipe_id, task_id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Writer task
// ---------------------------------------------------------------------------

fn writer_task() -> ! {
    let pipe_id = PIPE_ID.load(Ordering::Acquire) as usize;
    // Wait until the reader has parked on the wait queue. This is the
    // critical synchronisation step — if the writer runs first, the
    // reader's `pipe_read` returns Ok(1) immediately on its first call
    // and the wake path is never exercised. Cap the wait at 500 ticks
    // so a missing reader fails the test rather than hanging.
    let park_deadline = kernel::arch::x86_64::interrupts::tick_count().saturating_add(500);
    while !READER_PARKED.load(Ordering::Acquire) {
        if kernel::arch::x86_64::interrupts::tick_count() >= park_deadline {
            kernel::serial_println!("[pipe-wakeup-test] writer: reader never parked");
            loop {
                kernel::task::yield_now();
            }
        }
        kernel::task::yield_now();
    }

    let payload = [0xA5u8];
    // Sample tick immediately before the write so the latency we measure
    // covers `pipe_write + wake_pipe + cross-core wake_task_v2 +
    // dispatch + read` — not the time it took us to schedule.
    let now = kernel::arch::x86_64::interrupts::tick_count();
    WRITE_TICK.store(now, Ordering::Release);
    let _ = kernel::pipe::pipe_write(pipe_id, &payload);
    kernel::serial_println!("[pipe-wakeup-test] writer: wrote 0xA5 at tick {}", now);
    loop {
        kernel::task::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test_case]
fn cross_core_pipe_wakeup_within_latency_budget() {
    let cores = kernel::smp::core_count() as usize;
    assert!(
        cores >= 2,
        "cross-core pipe wakeup test requires at least 2 cores; got {cores}"
    );

    // Allocate the pipe under test. Add a reader and a writer ref so
    // pipe_close_reader / pipe_close_writer can't free the slot mid-test.
    let pipe_id = kernel::pipe::create_pipe();
    kernel::pipe::pipe_add_reader(pipe_id);
    kernel::pipe::pipe_add_writer(pipe_id);
    PIPE_ID.store(pipe_id as u64, Ordering::Release);

    // Spawn reader on core 0 and writer on core 1 simultaneously. The
    // writer task busy-waits on `READER_PARKED` before issuing the
    // write — robust against any dispatch ordering on the two cores.
    kernel::task::scheduler::spawn_on_core(reader_task, "pipe-reader", 0);
    kernel::task::scheduler::spawn_on_core(writer_task, "pipe-writer", 1);
    kernel::serial_println!("[pipe-wakeup-test] reader + writer spawned, waiting for completion");
    // Wait for reader to observe the data. Bound the wait so a stuck
    // wake path fails the test rather than hanging.
    let read_deadline = kernel::arch::x86_64::interrupts::tick_count().saturating_add(500);
    let mut last_log = kernel::arch::x86_64::interrupts::tick_count();
    while !READER_DONE.load(Ordering::Acquire) {
        let now = kernel::arch::x86_64::interrupts::tick_count();
        if now >= read_deadline {
            panic!(
                "reader did not wake within 500 ticks (write_tick={}, read_tick={})",
                WRITE_TICK.load(Ordering::Acquire),
                READ_TICK.load(Ordering::Acquire),
            );
        }
        if now.saturating_sub(last_log) >= 100 {
            last_log = now;
            kernel::serial_println!(
                "[pipe-wakeup-test] runner waiting at tick {} (read_tick={})",
                now,
                READ_TICK.load(Ordering::Acquire)
            );
        }
        kernel::task::yield_now();
    }
    kernel::serial_println!("[pipe-wakeup-test] runner observed READER_DONE");

    let write_tick = WRITE_TICK.load(Ordering::Acquire);
    let read_tick = READ_TICK.load(Ordering::Acquire);
    let latency = read_tick.saturating_sub(write_tick);

    // Phase 61 Track D.1 acceptance — the task list calls for ≤100 ticks
    // initially, tightened to ≤10 ticks once Track F lands. Track F
    // landed in the same PR; assert the tight bound.
    const MAX_LATENCY_TICKS: u64 = 10;
    assert!(
        latency <= MAX_LATENCY_TICKS,
        "cross-core pipe wakeup latency {latency} ticks exceeds budget {MAX_LATENCY_TICKS} \
         (write_tick={write_tick}, read_tick={read_tick})",
    );

    kernel::serial_println!(
        "[pipe-wakeup-test] PASSED: write_tick={} read_tick={} latency={} ticks",
        write_tick,
        read_tick,
        latency
    );
}
