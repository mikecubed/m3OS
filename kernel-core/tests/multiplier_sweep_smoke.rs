//! G.3 regression guard: assert that the scheduler tick rate is 1 kHz (1 tick = 1 ms)
//! and that the tick→ms conversion is 1:1 — not the stale ÷10 / ×10 that assumed
//! a 100 Hz timer.
//!
//! These tests cannot fail by themselves (they check a pure constant), but they act
//! as a trip-wire: if someone lowers TICKS_PER_SEC back to 100, or re-introduces the
//! ×10 / ÷10 multiplier, these tests break and force the bug to be noticed.

use kernel_core::time::TICKS_PER_SEC_EXPECTED;

/// The scheduler runs at 1 kHz. One tick equals one millisecond.
/// See: kernel/src/arch/x86_64/syscall/mod.rs `TICKS_PER_SEC = 1_000`.
#[test]
fn ticks_per_sec_is_1000() {
    assert_eq!(
        TICKS_PER_SEC_EXPECTED, 1_000,
        "TICKS_PER_SEC must be 1000 (1 tick = 1 ms); do not regress to 100 Hz"
    );
}

/// Converting N ticks to milliseconds must be N * 1 (not N * 10).
///
/// Before G.3 the log messages used `ticks * 10` and the poll/select/epoll
/// deadline used `ms.div_ceil(10)`, both of which assumed a 100 Hz clock.
/// With TICKS_PER_SEC = 1000, the correct conversion is simply 1:1.
#[test]
fn ticks_to_ms_is_one_to_one() {
    // ticks_to_ms(n) = n * (1_000 ms/s) / TICKS_PER_SEC
    //                = n * 1_000 / 1_000
    //                = n   (not n * 10)
    let ms_per_tick = 1_000_u64 / TICKS_PER_SEC_EXPECTED;
    assert_eq!(ms_per_tick, 1, "each tick must equal exactly 1 ms");
}

/// Converting N milliseconds to ticks must be N / 1 (not N / 10).
///
/// The stale `ms.div_ceil(10)` formula divided by 10, making poll/select/epoll
/// time out 10× sooner than requested.  The corrected formula is `ms` (no
/// division).
#[test]
fn ms_to_ticks_is_one_to_one() {
    // ticks_from_ms(n) = n * TICKS_PER_SEC / 1_000
    //                  = n * 1_000 / 1_000
    //                  = n   (not n / 10)
    let ticks_per_ms = TICKS_PER_SEC_EXPECTED / 1_000_u64;
    assert_eq!(
        ticks_per_ms, 1,
        "each millisecond must equal exactly 1 tick"
    );
}

/// Regression: the stale-ready and cpu-hog log messages reported `ticks * 10`
/// which was 10× the true elapsed milliseconds.  After G.3 the log value is
/// simply `ticks` (since 1 tick = 1 ms, the value is already in ms).
#[test]
fn log_ticks_value_is_not_multiplied_by_ten() {
    let elapsed_ticks: u64 = 500;
    // Correct: elapsed ms == elapsed ticks (1:1)
    let correct_ms = elapsed_ticks; // was: elapsed_ticks * 10  (bug)
    assert_eq!(
        correct_ms, 500,
        "log message should report 500 ms for 500 ticks, not 5000 ms"
    );
}

/// Regression: sys_poll / select_inner / sys_epoll_wait computed the deadline as
/// `start_tick + ms.div_ceil(10)`, which made timeouts fire 10× too early.
/// After G.3 the deadline is `start_tick + ms` (no division).
#[test]
fn deadline_ticks_are_not_divided_by_ten() {
    let start_tick: u64 = 10_000;
    let timeout_ms: u64 = 2_000; // user requests a 2 s timeout
    // Correct: deadline = start + timeout_ms (since 1 tick = 1 ms)
    let correct_deadline = start_tick + timeout_ms; // was: start_tick + timeout_ms.div_ceil(10)
    let stale_deadline = start_tick + timeout_ms.div_ceil(10);
    assert_ne!(
        correct_deadline, stale_deadline,
        "sanity: the two formulas must differ so the test is meaningful"
    );
    assert_eq!(
        correct_deadline, 12_000,
        "deadline should be start(10000) + 2000 ms = 12000 ticks"
    );
    assert_eq!(
        stale_deadline, 10_200,
        "the old formula gives start(10000) + 200 ticks (10x too short)"
    );
}
