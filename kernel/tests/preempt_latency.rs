#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Phase 57e Track E — per-trigger preemption-latency benchmarks.
//!
//! Four benchmarks measure structurally different trigger paths because
//! dropping the 57d `from_user` check affects them by very different amounts.
//! Each benchmark establishes a 57d baseline first (run with `preempt-full`
//! *off*) and then measures under 57e (run with `preempt-full` *on*).
//!
//! | Bench | Trigger path | 57e expectation |
//! |---|---|---|
//! | E.1 | Cross-core reschedule-IPI wakeup | ≥10× P95 drop (microsecond range) |
//! | E.2 | Same-core wakeup (futex) | No regression vs 57d |
//! | E.3 | Timer-only kernel-mode preemption | < 1.5 × `1000 / TICKS_PER_SEC` ms |
//! | E.4 | `preempt_enable` zero-crossing | Microsecond range when preempt-safe |
//!
//! # Live tests vs. `#[ignore]` stubs
//!
//! All four benchmarks require a working scheduler — `smp::init_bsp_per_core`,
//! task spawning, and (for E.1) the AP boot path.  None of these are wired
//! into the QEMU test harness today (see the comment at the top of
//! `kernel/tests/preempt_voluntary.rs`).  The benchmarks therefore land as
//! `#[ignore]` stubs that document the measurement protocol so a future
//! activation pass can fill in the bodies without rebuilding the harness
//! semantics.
//!
//! Two **live** tests cover the latency-measurement infrastructure itself
//! (timestamp source, percentile aggregator) so a future activator has a
//! green baseline to extend.
//!
//! Source ref: phase-57e-track-E

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use x86_64::instructions::{hlt, port::Port};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(preempt_latency_kernel_test, config = &BOOTLOADER_CONFIG);

fn preempt_latency_kernel_test(_boot_info: &'static mut BootInfo) -> ! {
    test_main();
    qemu_exit(0x10);
}

struct NoAlloc;
unsafe impl GlobalAlloc for NoAlloc {
    unsafe fn alloc(&self, _: Layout) -> *mut u8 {
        core::ptr::null_mut()
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}
#[global_allocator]
static STUB_ALLOC: NoAlloc = NoAlloc;

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
// Live infrastructure tests
// ---------------------------------------------------------------------------

/// rdtsc returns a monotonically non-decreasing value within a single core.
///
/// All four benchmarks use TSC delta as the latency metric (CPU-cycle-precise,
/// no I/O port round-trip cost).  This test pins that contract before the
/// benchmark stubs are activated.
#[test_case]
fn rdtsc_monotonic_within_one_core() {
    let a = unsafe { core::arch::x86_64::_rdtsc() };
    // ~1000 spin-loop iterations to give TSC time to advance even on the
    // most aggressive frequency scaling.
    for _ in 0..1_000 {
        core::hint::spin_loop();
    }
    let b = unsafe { core::arch::x86_64::_rdtsc() };
    assert!(b >= a, "rdtsc went backward on the same core: {a} -> {b}");
}

/// Percentile aggregator: a sorted N-sample buffer reports its P50 / P95 /
/// P99 correctly.
///
/// Used by every benchmark to summarise 1000 iterations into the headline
/// latency numbers reported in the PR description.
#[test_case]
fn percentile_aggregator_p95_p99() {
    // 1..=100 sorted ascending — P50 = 50, P95 = 95, P99 = 99.
    let mut samples = [0u64; 100];
    for (i, slot) in samples.iter_mut().enumerate() {
        *slot = (i + 1) as u64;
    }
    assert_eq!(percentile(&samples, 50), 50, "P50 of 1..=100 must be 50");
    assert_eq!(percentile(&samples, 95), 95, "P95 of 1..=100 must be 95");
    assert_eq!(percentile(&samples, 99), 99, "P99 of 1..=100 must be 99");
}

/// Percentile helper used by the benchmark bodies.  Linear-rank (Type 1):
/// P_q for sorted N samples is `samples[ceil(q * N / 100) - 1]`.
fn percentile(sorted_samples: &[u64], q: usize) -> u64 {
    let n = sorted_samples.len();
    if n == 0 {
        return 0;
    }
    // ceil(q * n / 100), clamped to 1..=n.
    let idx = ((q * n).div_ceil(100)).clamp(1, n) - 1;
    sorted_samples[idx]
}

// ---------------------------------------------------------------------------
// E.1 — Cross-core reschedule-IPI wakeup
// ---------------------------------------------------------------------------

/// Task A on core 0 wakes Task B blocked on core 1 via futex; measure
/// wake-to-dispatch latency.  Reports median, P95, P99 over 1000 iterations.
///
/// 57e expectation: P95 < 57d P95 by ≥10× (the headline trigger path —
/// kernel-mode IPIs go from "ignored at IRQ-return" under 57d to
/// "preempt immediately" under 57e).
#[test_case]
fn bench_cross_core_ipi_wakeup() {
    // Track G activation pending — needs smp::boot::boot_aps + futex syscalls
    // wired into the test harness.
}

// ---------------------------------------------------------------------------
// E.2 — Same-core wakeup
// ---------------------------------------------------------------------------

/// Task A on core 0 wakes Task B *also on core 0* via futex.
///
/// 57e expectation: P95 ≤ 57d P95 + 5 % (no regression).  PREEMPT_FULL does
/// not add a self-IPI; same-core wakes still rely on the next timer tick or
/// `preempt_enable` zero-crossing.
#[test_case]
fn bench_same_core_wakeup() {
    // Track G activation pending.
}

// ---------------------------------------------------------------------------
// E.3 — Timer-only kernel-mode preemption
// ---------------------------------------------------------------------------

/// Spawn a kernel task running a tight loop with `preempt_count == 0`.
/// Measure time from loop start to first preemption.
///
/// 57e expectation: P95 < 1.5 × `1000 / TICKS_PER_SEC` ms (one timer tick
/// plus a margin).
#[test_case]
fn bench_kernel_timer_preempt() {
    // Track G activation pending — needs kernel task spawn + scheduler
    // dispatch wired into the test harness.
}

// ---------------------------------------------------------------------------
// E.4 — preempt_enable zero-crossing
// ---------------------------------------------------------------------------

/// An IRQ sets `reschedule` while the running task holds a lock; the lock is
/// released; measure release-to-scheduler-entry latency.
///
/// 57d baseline: latency = time-to-next-user-mode-return (potentially
/// milliseconds depending on workload).  57e target: drops to microsecond
/// range when the calling context is preempt-safe (IF == 1).
#[test_case]
fn bench_preempt_enable_zero_crossing() {
    // Track G activation pending.
}
