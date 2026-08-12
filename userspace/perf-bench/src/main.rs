//! Syscall round-trip perf harness (Phase 110 Track A.5, Block 3).
//!
//! Measures the wall-clock cost of a tight syscall loop so the A.5 PCID bound
//! can be checked on real silicon: with PCID active (`M3OS_MITIGATIONS=full`)
//! the smoke workload must be **≤30 %** slower than `M3OS_MITIGATIONS=off` — the
//! Phase 84 bound the naive full-flush KPTI cannot meet, so beating it proves the
//! tagged-CR3 no-flush trampolines buy the cost back.
//!
//! The hot loop is `getpid()` — the cheapest real syscall, a pure
//! ring3→ring0→ring3 round trip — so it isolates the KPTI/PCID entry/exit
//! CR3-switch cost with no I/O or allocation noise. It reports total wall time
//! and derived nanoseconds-per-syscall.
//!
//! Bench use (Block 3): run on image C (`full`, PCID active) and image B
//! (`off`), then compare `ns_per_syscall`:
//!   `(ns_full - ns_off) / ns_off  ≤ 0.30`  ⇒ PASS.
//! For a same-boot A/B, mask PCID in `probe_pcid` to force the full-flush
//! fallback and measure exactly what the tags recover.
//!
//! `ITERS` is tunable — raise it if a run finishes too fast to time cleanly.
#![no_std]
#![no_main]

use core::hint::black_box;
use syscall_lib::{CLOCK_MONOTONIC, STDOUT_FILENO, clock_gettime, getpid, write_str, write_u64};

syscall_lib::entry_point!(main);

/// Iterations of the syscall hot loop. Tunable at the bench.
const ITERS: u64 = 3_000_000;

fn now_ns() -> u64 {
    let (sec, nsec) = clock_gettime(CLOCK_MONOTONIC);
    if sec < 0 {
        return 0;
    }
    (sec as u64) * 1_000_000_000 + (nsec as u64)
}

fn main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "PERF_BENCH:start iters=");
    write_u64(STDOUT_FILENO, ITERS);
    write_str(STDOUT_FILENO, "\n");

    let t0 = now_ns();
    let mut acc = 0u64;
    for _ in 0..ITERS {
        // getpid(): pure ring3->ring0->ring3 round trip; this loop measures the
        // KPTI/PCID trampoline + CR3-switch cost and nothing else.
        acc = acc.wrapping_add(getpid() as u64);
    }
    black_box(acc);
    let t1 = now_ns();

    let elapsed_ns = t1.saturating_sub(t0);
    let elapsed_ms = elapsed_ns / 1_000_000;
    let ns_per = elapsed_ns.checked_div(ITERS).unwrap_or(0);

    write_str(STDOUT_FILENO, "PERF_BENCH:elapsed_ms=");
    write_u64(STDOUT_FILENO, elapsed_ms);
    write_str(STDOUT_FILENO, " ns_per_syscall=");
    write_u64(STDOUT_FILENO, ns_per);
    write_str(STDOUT_FILENO, "\nPERF_BENCH:done\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "perf-bench: PANIC\n");
    syscall_lib::exit(101)
}
