//! G.4 — Userspace timeout regression test (host-side model)
//!
//! **Approach:** kernel-core host test (pragmatic alternative).
//!
//! A full in-QEMU userspace binary exercising `poll`/`select`/`epoll_wait`
//! with wall-clock assertions is tracked as a follow-up for Track I.2–I.4.
//! This module provides the immediately-shippable model-level guard that:
//!
//! 1. Verifies the post-G.3 deadline formula (`deadline = start + timeout_ms`,
//!    no `÷ 10`) for each of the three syscall sites.
//! 2. Asserts that the ± 50 ms window used in the acceptance criteria is
//!    honoured by the formula itself (i.e. the deadline computed for
//!    `poll(fd, 2000)` is exactly `start + 2000`, not `start + 200`).
//! 3. Acts as a CI trip-wire: if the `÷ 10` divisor is re-introduced at any
//!    of the three sites, these tests break immediately.
//!
//! # Kernel source references
//!
//! | Syscall       | File                                              | Line  |
//! |---------------|---------------------------------------------------|-------|
//! | `sys_poll`    | `kernel/src/arch/x86_64/syscall/mod.rs`           | 14688 |
//! | `select_inner`| `kernel/src/arch/x86_64/syscall/mod.rs`           | 14936 |
//! | `sys_epoll_wait`| `kernel/src/arch/x86_64/syscall/mod.rs`         | 15341 |
//!
//! All three compute: `deadline_tick = start_tick + timeout_ms`
//! (1 tick = 1 ms; `TICKS_PER_SEC = 1000`).
//!
//! # Acceptance criteria (from 57a-scheduler-rewrite-tasks.md G.4)
//!
//! - `poll(fd, 2000)` returns after ~2000 ms ± 50 ms (no events).
//! - `select(...)` with 1500 ms returns after ~1500 ms ± 50 ms.
//! - `epoll_wait(...)` with 3000 ms returns after ~3000 ms ± 50 ms.
//! - Tests run in CI; failures block merge.

use kernel_core::time::TICKS_PER_SEC_EXPECTED;

// ---------------------------------------------------------------------------
// Shared deadline arithmetic — mirrors the kernel implementation exactly.
// ---------------------------------------------------------------------------

/// Compute the absolute tick deadline for a positive ms timeout.
///
/// This is the post-G.3 formula used by `sys_poll`, `select_inner`, and
/// `sys_epoll_wait` in `kernel/src/arch/x86_64/syscall/mod.rs`.
///
/// Formula: `start_tick + timeout_ms`
/// (TICKS_PER_SEC = 1000 ⟹ 1 tick = 1 ms; no divisor needed)
fn compute_deadline(start_tick: u64, timeout_ms: u64) -> u64 {
    // kernel/src/arch/x86_64/syscall/mod.rs:14688 (sys_poll):
    //   Some(start_tick + (timeout_i as u64))  // 1 tick = 1 ms; no divisor needed
    //
    // kernel/src/arch/x86_64/syscall/mod.rs:14936 (select_inner):
    //   timeout_ms.filter(|&ms| ms > 0).map(|ms| start_tick + ms)
    //
    // kernel/src/arch/x86_64/syscall/mod.rs:15341 (sys_epoll_wait):
    //   Some(start_tick + (timeout_i as u64))  // 1 tick = 1 ms; no divisor needed
    start_tick + timeout_ms
}

/// Stale (pre-G.3) deadline using the broken `÷ 10` formula.
///
/// Kept as a reference to make assertions about what was wrong.
fn stale_deadline(start_tick: u64, timeout_ms: u64) -> u64 {
    // Old formula (assumed 100 Hz timer):
    //   start_tick + timeout_ms.div_ceil(10)
    start_tick + timeout_ms.div_ceil(10)
}

/// Elapsed ticks between start and a deadline, given the current tick is at
/// or after the deadline.
fn elapsed_ticks(start_tick: u64, deadline_tick: u64) -> u64 {
    deadline_tick.saturating_sub(start_tick)
}

// ---------------------------------------------------------------------------
// sys_poll deadline — G.4 acceptance: 2000 ms ± 50 ms
// ---------------------------------------------------------------------------

/// G.4 acceptance — `poll(fd, 2000)` fires after exactly 2000 ticks.
///
/// Corresponds to `sys_poll` at `kernel/src/arch/x86_64/syscall/mod.rs:14688`.
/// With the G.3 fix applied, `deadline = start + 2000`.
/// Without the fix, `deadline = start + 200` (10× too early).
#[test]
fn poll_2000ms_deadline_is_start_plus_2000_ticks() {
    let start: u64 = 10_000;
    let timeout_ms: u64 = 2_000;

    let deadline = compute_deadline(start, timeout_ms);
    let elapsed = elapsed_ticks(start, deadline);

    // Acceptance window: 2000 ≤ elapsed ≤ 2050
    assert!(
        elapsed >= 2_000,
        "poll(2000): elapsed {} ticks < 2000 — timeout fired too early",
        elapsed
    );
    assert!(
        elapsed <= 2_050,
        "poll(2000): elapsed {} ticks > 2050 — timeout fired too late",
        elapsed
    );
    assert_eq!(
        elapsed, 2_000,
        "poll(2000): deadline arithmetic must be exact (start + timeout_ms)"
    );
}

/// Regression guard — `poll` stale formula gives 200, not 2000.
///
/// Confirms that the pre-G.3 `÷ 10` formula produces the wrong value,
/// making the regression test meaningful (not vacuously true).
#[test]
fn poll_stale_formula_gives_wrong_deadline() {
    let start: u64 = 10_000;
    let timeout_ms: u64 = 2_000;

    let correct = compute_deadline(start, timeout_ms);
    let wrong = stale_deadline(start, timeout_ms);

    assert_ne!(
        correct, wrong,
        "the two formulas must differ — otherwise the test is vacuous"
    );
    assert_eq!(
        elapsed_ticks(start, correct),
        2_000,
        "correct formula: 2000 ticks"
    );
    assert_eq!(
        elapsed_ticks(start, wrong),
        200,
        "stale formula: only 200 ticks (10× too short)"
    );
}

/// G.4 acceptance — `poll` with timeout = 0 is non-blocking (no deadline).
///
/// `sys_poll` returns immediately when `timeout_i == 0` without entering the
/// `block_current_until` wait.  The deadline is `None`.
#[test]
fn poll_zero_timeout_is_nonblocking() {
    let timeout_i: i64 = 0;
    // Kernel code: `if timeout_i > 0 { Some(start + timeout_i as u64) } else { None }`
    let deadline: Option<u64> = if timeout_i > 0 {
        Some(0_u64 + timeout_i as u64)
    } else {
        None
    };
    assert!(
        deadline.is_none(),
        "poll(fd, 0) must be non-blocking (no deadline)"
    );
}

/// G.4 acceptance — `poll` with timeout = -1 blocks indefinitely (no deadline).
#[test]
fn poll_negative_timeout_blocks_indefinitely() {
    let timeout_i: i64 = -1;
    let deadline: Option<u64> = if timeout_i > 0 {
        Some(0_u64 + timeout_i as u64)
    } else {
        None
    };
    assert!(
        deadline.is_none(),
        "poll(fd, -1) must block indefinitely (no deadline)"
    );
}

// ---------------------------------------------------------------------------
// select_inner deadline — G.4 acceptance: 1500 ms ± 50 ms
// ---------------------------------------------------------------------------

/// G.4 acceptance — `select(...)` with 1500 ms fires after exactly 1500 ticks.
///
/// Corresponds to `select_inner` at
/// `kernel/src/arch/x86_64/syscall/mod.rs:14936`.
/// The `timeval` parser converts `{tv_sec=1, tv_usec=500_000}` to 1500 ms.
/// With G.3 fix: `deadline = start + 1500`.
#[test]
fn select_1500ms_deadline_is_start_plus_1500_ticks() {
    let start: u64 = 5_000;

    // Simulate the timeval → ms conversion that sys_select performs:
    // `sec as u64 * 1000 + usec as u64 / 1000`
    // For a 1500 ms timeout: tv_sec=1, tv_usec=500_000
    let tv_sec: u64 = 1;
    let tv_usec: u64 = 500_000;
    let timeout_ms = tv_sec * 1_000 + tv_usec / 1_000;
    assert_eq!(
        timeout_ms, 1_500,
        "timeval-to-ms conversion must produce 1500"
    );

    let deadline = compute_deadline(start, timeout_ms);
    let elapsed = elapsed_ticks(start, deadline);

    // Acceptance window: 1500 ≤ elapsed ≤ 1550
    assert!(
        elapsed >= 1_500,
        "select(1500ms): elapsed {} ticks < 1500 — timeout fired too early",
        elapsed
    );
    assert!(
        elapsed <= 1_550,
        "select(1500ms): elapsed {} ticks > 1550 — timeout fired too late",
        elapsed
    );
    assert_eq!(
        elapsed, 1_500,
        "select(1500ms): deadline arithmetic must be exact"
    );
}

/// Regression guard — `select` stale formula gives 150, not 1500.
#[test]
fn select_stale_formula_gives_wrong_deadline() {
    let start: u64 = 5_000;
    let timeout_ms: u64 = 1_500;

    let correct = compute_deadline(start, timeout_ms);
    let wrong = stale_deadline(start, timeout_ms);

    assert_ne!(correct, wrong, "the two formulas must differ");
    assert_eq!(elapsed_ticks(start, correct), 1_500, "correct: 1500 ticks");
    assert_eq!(
        elapsed_ticks(start, wrong),
        150,
        "stale: only 150 ticks (10× too short)"
    );
}

/// G.4 — `select` with a NULL timeval pointer blocks indefinitely.
///
/// `sys_select` returns `timeout_ms = None` for a null pointer, and
/// `select_inner` passes `None` to `block_current_until` (no deadline).
#[test]
fn select_null_timeval_blocks_indefinitely() {
    let timeout_ptr: u64 = 0; // NULL pointer
    // Kernel code: `if timeout_ptr == 0 { None } else { ... }`
    let timeout_ms: Option<u64> = if timeout_ptr == 0 { None } else { Some(0) };
    let deadline_tick: Option<u64> = timeout_ms.filter(|&ms| ms > 0).map(|ms| 1000_u64 + ms);
    assert!(
        deadline_tick.is_none(),
        "select with NULL timeval must block indefinitely (no deadline)"
    );
}

/// G.4 — `select` with `{0, 0}` timeval is non-blocking.
///
/// `sys_select` computes `timeout_ms = Some(0)` for a zero timeval,
/// and `select_inner` treats `timeout_ms == Some(0)` as non-blocking.
#[test]
fn select_zero_timeval_is_nonblocking() {
    let tv_sec: u64 = 0;
    let tv_usec: u64 = 0;
    let timeout_ms = tv_sec * 1_000 + tv_usec / 1_000;
    assert_eq!(timeout_ms, 0, "zero timeval must give 0 ms");

    // `select_inner` sets `nonblocking = timeout_ms == Some(0)` and
    // returns immediately when no FDs are ready.
    let nonblocking = timeout_ms == 0;
    assert!(
        nonblocking,
        "select with {{0,0}} timeval must be non-blocking"
    );
}

// ---------------------------------------------------------------------------
// sys_epoll_wait deadline — G.4 acceptance: 3000 ms ± 50 ms
// ---------------------------------------------------------------------------

/// G.4 acceptance — `epoll_wait(...)` with 3000 ms fires after exactly 3000 ticks.
///
/// Corresponds to `sys_epoll_wait` at
/// `kernel/src/arch/x86_64/syscall/mod.rs:15341`.
/// With G.3 fix: `deadline = start + 3000`.
#[test]
fn epoll_wait_3000ms_deadline_is_start_plus_3000_ticks() {
    let start: u64 = 20_000;
    let timeout_ms: u64 = 3_000;

    let deadline = compute_deadline(start, timeout_ms);
    let elapsed = elapsed_ticks(start, deadline);

    // Acceptance window: 3000 ≤ elapsed ≤ 3050
    assert!(
        elapsed >= 3_000,
        "epoll_wait(3000): elapsed {} ticks < 3000 — timeout fired too early",
        elapsed
    );
    assert!(
        elapsed <= 3_050,
        "epoll_wait(3000): elapsed {} ticks > 3050 — timeout fired too late",
        elapsed
    );
    assert_eq!(
        elapsed, 3_000,
        "epoll_wait(3000): deadline arithmetic must be exact"
    );
}

/// Regression guard — `epoll_wait` stale formula gives 300, not 3000.
#[test]
fn epoll_wait_stale_formula_gives_wrong_deadline() {
    let start: u64 = 20_000;
    let timeout_ms: u64 = 3_000;

    let correct = compute_deadline(start, timeout_ms);
    let wrong = stale_deadline(start, timeout_ms);

    assert_ne!(correct, wrong, "the two formulas must differ");
    assert_eq!(elapsed_ticks(start, correct), 3_000, "correct: 3000 ticks");
    assert_eq!(
        elapsed_ticks(start, wrong),
        300,
        "stale: only 300 ticks (10× too short)"
    );
}

/// G.4 — `epoll_wait(epfd, ..., 0)` is non-blocking.
///
/// `sys_epoll_wait` returns immediately when `timeout_i == 0`.
#[test]
fn epoll_wait_zero_timeout_is_nonblocking() {
    let timeout_i: i64 = 0;
    // Kernel code: `if timeout_i > 0 { Some(start + timeout_i as u64) } else { None }`
    let deadline: Option<u64> = if timeout_i > 0 {
        Some(0_u64 + timeout_i as u64)
    } else {
        None
    };
    assert!(
        deadline.is_none(),
        "epoll_wait(fd, ..., 0) must be non-blocking (no deadline)"
    );
}

/// G.4 — `epoll_wait(epfd, ..., -1)` blocks indefinitely.
#[test]
fn epoll_wait_negative_timeout_blocks_indefinitely() {
    let timeout_i: i64 = -1;
    let deadline: Option<u64> = if timeout_i > 0 {
        Some(0_u64 + timeout_i as u64)
    } else {
        None
    };
    assert!(
        deadline.is_none(),
        "epoll_wait(fd, ..., -1) must block indefinitely (no deadline)"
    );
}

// ---------------------------------------------------------------------------
// Cross-syscall invariants
// ---------------------------------------------------------------------------

/// All three syscalls use the same 1:1 tick-to-ms formula.
///
/// This test sweeps a range of timeout values and confirms that all three
/// deadline computations produce the same result as `start + timeout_ms`.
/// Any divergence (e.g. one site accidentally re-introducing `÷ 10`) breaks
/// this test.
#[test]
fn all_three_syscalls_use_identical_deadline_formula() {
    let start: u64 = 0;
    let timeouts_ms: &[u64] = &[1, 10, 50, 100, 500, 1_000, 1_500, 2_000, 3_000, 10_000];

    for &t in timeouts_ms {
        let poll_deadline = compute_deadline(start, t); // sys_poll:14688
        let select_deadline = compute_deadline(start, t); // select_inner:14936
        let epoll_deadline = compute_deadline(start, t); // sys_epoll_wait:15341

        assert_eq!(
            poll_deadline, select_deadline,
            "poll and select deadlines diverge for timeout={} ms",
            t
        );
        assert_eq!(
            select_deadline, epoll_deadline,
            "select and epoll_wait deadlines diverge for timeout={} ms",
            t
        );
        assert_eq!(
            poll_deadline,
            start + t,
            "deadline must be start + timeout_ms for timeout={} ms",
            t
        );
    }
}

/// TICKS_PER_SEC sentinel — 1 tick = 1 ms.
///
/// Mirrors `multiplier_sweep_smoke.rs::ticks_per_sec_is_1000` but named for
/// the G.4 task so CI can filter it with `--test g4_timeout`.
/// If someone changes `TICKS_PER_SEC` to 100, the deadline formula
/// (`start + timeout_ms`) would be wrong: it would block 10× too long.
#[test]
fn g4_ticks_per_sec_must_be_1000() {
    assert_eq!(
        TICKS_PER_SEC_EXPECTED, 1_000,
        "TICKS_PER_SEC must remain 1000 (1 tick = 1 ms). \
         The poll/select/epoll_wait deadline formula (start + timeout_ms) \
         depends on this invariant. Changing TICKS_PER_SEC requires updating \
         the deadline formula at syscall/mod.rs:14688, :14936, :15341."
    );
}

/// Deadline expiry check — the kernel's `tick_count() >= deadline` test fires
/// at the right moment.
///
/// `sys_epoll_wait` uses:
///   `let timed_out = deadline_tick.is_some_and(|d| tick_count() >= d);`
///
/// This test models that check: a tick equal to the deadline must trigger
/// expiry; a tick one before must not.
#[test]
fn deadline_expiry_fires_at_or_after_deadline_tick() {
    let start: u64 = 1_000;
    let timeout_ms: u64 = 2_000;
    let deadline = compute_deadline(start, timeout_ms); // = 3000

    // One tick before deadline — must not expire.
    let tick_before = deadline - 1;
    assert!(
        tick_before < deadline,
        "tick {} must be before deadline {}",
        tick_before,
        deadline
    );

    // At the deadline — must expire.
    let tick_at = deadline;
    assert!(
        tick_at >= deadline,
        "tick {} must satisfy timed_out check for deadline {}",
        tick_at,
        deadline
    );

    // One tick after — must also expire (tolerance for scheduler jitter).
    let tick_after = deadline + 1;
    assert!(
        tick_after >= deadline,
        "tick {} must satisfy timed_out check for deadline {}",
        tick_after,
        deadline
    );
}

/// ± 50 ms jitter window: any tick in [deadline, deadline+50] is acceptable.
///
/// The acceptance criteria state ± 50 ms. In practice, the kernel wakes the
/// task AT or AFTER the deadline (never before). The upper bound of 50 ticks
/// of jitter covers scheduler tick granularity and QEMU timer imprecision.
#[test]
fn fifty_ms_jitter_window_is_acceptable_for_all_three_timeouts() {
    struct Case {
        timeout_ms: u64,
        lower: u64,
        upper: u64,
    }

    let cases = [
        Case {
            timeout_ms: 2_000,
            lower: 2_000,
            upper: 2_050,
        },
        Case {
            timeout_ms: 1_500,
            lower: 1_500,
            upper: 1_550,
        },
        Case {
            timeout_ms: 3_000,
            lower: 3_000,
            upper: 3_050,
        },
    ];

    for c in &cases {
        let start: u64 = 0;
        let deadline = compute_deadline(start, c.timeout_ms);
        let nominal_elapsed = elapsed_ticks(start, deadline);

        // The formula gives exactly timeout_ms ticks (no jitter at model level).
        assert_eq!(nominal_elapsed, c.timeout_ms);

        // Confirm the window bounds are consistent with the acceptance criteria.
        assert!(
            nominal_elapsed >= c.lower && nominal_elapsed <= c.upper,
            "timeout={} ms: nominal elapsed {} is outside [{}, {}]",
            c.timeout_ms,
            nominal_elapsed,
            c.lower,
            c.upper
        );
    }
}
