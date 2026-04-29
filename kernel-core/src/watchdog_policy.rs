//! Stuck-task watchdog policy — pure, allocation-free, host-testable.
//!
//! This module encodes the decision function for the G.1 stuck-task watchdog:
//! given a task's current tick, the tick at which it entered a `Blocked*`
//! state, and whether a `wake_deadline` is set, should the watchdog emit a
//! warning?
//!
//! Keeping the policy here (in `kernel-core`) and the iteration in the kernel
//! satisfies the SOLID separation principle: the kernel performs the O(n) scan
//! and calls this function; the policy is host-testable without a QEMU harness.
//!
//! # Thresholds
//!
//! Both constants use tick units. At the standard 1 kHz tick rate:
//! - 10 000 ticks = 10 seconds  (watchdog scan interval)
//! - 30 000 ticks = 30 seconds  (stuck threshold)

/// How often the watchdog scans the task table (in ticks).
///
/// At 1 kHz, 10 000 ticks = 10 seconds.
pub const WATCHDOG_SCAN_INTERVAL_TICKS: u64 = 10_000;

/// Threshold after which a `Blocked*` task with no waker is reported as stuck
/// (in ticks).
///
/// At 1 kHz, 30 000 ticks = 30 seconds.
pub const WATCHDOG_STUCK_THRESHOLD_TICKS: u64 = 30_000;

/// The outcome of a watchdog policy check for a single task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogVerdict {
    /// The task is not stuck; no warning should be emitted.
    Ok,
    /// The task has been blocked for more than the threshold with no waker
    /// registered. A `[WARN] [sched]` log line should be emitted.
    StuckNoWaker,
    /// The task has been blocked for more than the threshold AND had a
    /// `wake_deadline` set that is already in the past. This implies the
    /// deadline scanner is also stuck or the deadline was set incorrectly.
    StuckDeadlineExpired,
}

/// Decide whether a blocked task should be reported as stuck.
///
/// # Arguments
///
/// - `now_ticks`: the current tick counter value.
/// - `blocked_since_ticks`: the tick at which the task entered its current
///   `Blocked*` state. Set when `block_current` writes the new state.
/// - `wake_deadline`: the task's `wake_deadline`, if any.
///
/// # Returns
///
/// - [`WatchdogVerdict::Ok`] when the task has been blocked for fewer than
///   [`WATCHDOG_STUCK_THRESHOLD_TICKS`].
/// - [`WatchdogVerdict::StuckNoWaker`] when the task has been blocked for
///   more than the threshold and has no deadline set (nothing will ever wake
///   it automatically).
/// - [`WatchdogVerdict::StuckDeadlineExpired`] when the task has been blocked
///   for more than the threshold and the deadline is in the past (meaning the
///   deadline scanner is not advancing, or the deadline was set incorrectly).
#[inline]
pub fn watchdog_verdict(
    now_ticks: u64,
    blocked_since_ticks: u64,
    wake_deadline: Option<u64>,
) -> WatchdogVerdict {
    let blocked_for = now_ticks.saturating_sub(blocked_since_ticks);
    if blocked_for <= WATCHDOG_STUCK_THRESHOLD_TICKS {
        return WatchdogVerdict::Ok;
    }
    match wake_deadline {
        None => WatchdogVerdict::StuckNoWaker,
        Some(deadline) if deadline <= now_ticks => WatchdogVerdict::StuckDeadlineExpired,
        Some(_) => {
            // Deadline is in the future — the deadline scanner will eventually
            // wake this task. Not considered stuck (yet).
            WatchdogVerdict::Ok
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Ok paths ──────────────────────────────────────────────────────────

    /// Task that just blocked — no warning.
    #[test]
    fn test_just_blocked_is_ok() {
        let now = 50_000u64;
        let blocked_since = 49_999u64; // 1 tick ago
        assert_eq!(
            watchdog_verdict(now, blocked_since, None),
            WatchdogVerdict::Ok
        );
    }

    /// Task blocked for exactly the threshold — boundary: not yet stuck.
    #[test]
    fn test_exactly_at_threshold_is_ok() {
        let now = WATCHDOG_STUCK_THRESHOLD_TICKS;
        let blocked_since = 0u64;
        assert_eq!(
            watchdog_verdict(now, blocked_since, None),
            WatchdogVerdict::Ok
        );
    }

    /// Task blocked for threshold - 1 ticks with a future deadline — ok.
    #[test]
    fn test_below_threshold_with_future_deadline_is_ok() {
        let now = 50_000u64;
        let blocked_since = now - (WATCHDOG_STUCK_THRESHOLD_TICKS - 1);
        let deadline = Some(now + 1_000);
        assert_eq!(
            watchdog_verdict(now, blocked_since, deadline),
            WatchdogVerdict::Ok
        );
    }

    /// Task blocked longer than threshold but has a future deadline — ok, the
    /// deadline scanner will eventually wake it.
    #[test]
    fn test_over_threshold_with_future_deadline_is_ok() {
        let now = 100_000u64;
        let blocked_since = 0u64; // 100 000 ticks = well over threshold
        let deadline = Some(now + 5_000); // wakes in 5 seconds
        assert_eq!(
            watchdog_verdict(now, blocked_since, deadline),
            WatchdogVerdict::Ok
        );
    }

    // ── StuckNoWaker paths ─────────────────────────────────────────────────

    /// Task blocked for threshold + 1 ticks with no deadline — stuck.
    #[test]
    fn test_one_over_threshold_no_waker_is_stuck() {
        let now = WATCHDOG_STUCK_THRESHOLD_TICKS + 1;
        let blocked_since = 0u64;
        assert_eq!(
            watchdog_verdict(now, blocked_since, None),
            WatchdogVerdict::StuckNoWaker
        );
    }

    /// Task blocked for a very long time (10× threshold) with no deadline.
    #[test]
    fn test_very_long_block_no_waker_is_stuck() {
        let now = 1_000_000u64;
        let blocked_since = 0u64;
        assert_eq!(
            watchdog_verdict(now, blocked_since, None),
            WatchdogVerdict::StuckNoWaker
        );
    }

    /// Ensure saturating_sub doesn't overflow when blocked_since > now (clock
    /// wrap / misconfiguration: treat as ok, not stuck).
    #[test]
    fn test_blocked_since_greater_than_now_is_ok() {
        let now = 100u64;
        let blocked_since = 1_000u64; // future — shouldn't happen but must not panic
        assert_eq!(
            watchdog_verdict(now, blocked_since, None),
            WatchdogVerdict::Ok
        );
    }

    // ── StuckDeadlineExpired paths ─────────────────────────────────────────

    /// Task blocked over threshold and deadline is exactly now (expired).
    #[test]
    fn test_over_threshold_deadline_exactly_now_is_expired() {
        let now = 100_000u64;
        let blocked_since = 0u64;
        let deadline = Some(now); // deadline == now => expired
        assert_eq!(
            watchdog_verdict(now, blocked_since, deadline),
            WatchdogVerdict::StuckDeadlineExpired
        );
    }

    /// Task blocked over threshold and deadline is in the past.
    #[test]
    fn test_over_threshold_deadline_in_past_is_expired() {
        let now = 100_000u64;
        let blocked_since = 0u64;
        let deadline = Some(now - 1_000);
        assert_eq!(
            watchdog_verdict(now, blocked_since, deadline),
            WatchdogVerdict::StuckDeadlineExpired
        );
    }

    /// Deadline expired but task not yet over threshold — ok.
    #[test]
    fn test_below_threshold_with_expired_deadline_is_ok() {
        let now = 1_000u64;
        let blocked_since = now - 500; // 500 ticks — well below 30 000
        let deadline = Some(now - 100); // expired, but task hasn't been stuck long
        assert_eq!(
            watchdog_verdict(now, blocked_since, deadline),
            WatchdogVerdict::Ok
        );
    }

    // ── Zero / edge values ─────────────────────────────────────────────────

    /// All zeros — no history, ok.
    #[test]
    fn test_all_zeros_is_ok() {
        assert_eq!(watchdog_verdict(0, 0, None), WatchdogVerdict::Ok);
    }

    /// now == blocked_since with no deadline — not stuck (0 ticks elapsed).
    #[test]
    fn test_zero_elapsed_is_ok() {
        let tick = WATCHDOG_STUCK_THRESHOLD_TICKS * 2;
        assert_eq!(watchdog_verdict(tick, tick, None), WatchdogVerdict::Ok);
    }
}
