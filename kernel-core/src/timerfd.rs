//! Pure `timerfd(2)` expiry/rearm accounting — Phase 89 Track A.1, host-testable.
//!
//! The kernel object in `kernel/src/timerfd.rs` owns the table, refcounts, and
//! wait queue (all `no_std` kernel types that can't run on the host) and reads
//! the monotonic tick clock. The *expiry arithmetic* — how many times a timer
//! armed at an absolute deadline with a given period has fired by "now", where
//! the next deadline lands, and the nanosecond↔tick rounding at the syscall
//! boundary — is pure integer math with no kernel dependency, so it lives here
//! and is unit-tested on the host (mirroring `kernel_core::eventfd` for the
//! eventfd counter and `kernel_core::epoll` for the epoll edge logic). The
//! kernel object calls straight through to these.
//!
//! Everything here is expressed in **abstract time units**. The kernel uses the
//! 1 kHz scheduler tick (`TICKS_PER_SEC = 1000`, so 1 tick = 1 ms) as the unit,
//! which is also the granularity of the scheduler's `wake_deadline` — the only
//! IRQ-safe way to wake a `poll`/`epoll_wait` blocked on a timer expiry. The
//! nanosecond conversions below pin that 1 ms tick.

/// Nanoseconds per scheduler tick. Mirrors `TICKS_PER_SEC = 1000` in
/// `kernel/src/arch/x86_64/syscall/mod.rs` (1 tick = 1 ms = 1_000_000 ns). If
/// the kernel tick rate ever changes, update this and the call sites that rely
/// on it (the host test `ns_per_tick_matches_ticks_per_sec` guards the link to
/// `kernel_core::time::TICKS_PER_SEC_EXPECTED`).
pub const NS_PER_TICK: u64 = 1_000_000;

/// Convert a nanosecond duration to whole ticks, rounding **up** so that any
/// non-zero sub-tick duration arms at least one tick into the future rather than
/// collapsing to "already expired". A zero duration stays zero (used as the
/// "disarm" / "no interval" sentinel). Saturates instead of overflowing.
pub fn ns_to_ticks_ceil(ns: u64) -> u64 {
    if ns == 0 {
        0
    } else {
        // (ns + NS_PER_TICK - 1) / NS_PER_TICK, overflow-safe.
        (ns.saturating_add(NS_PER_TICK - 1)) / NS_PER_TICK
    }
}

/// Convert a whole-tick duration back to nanoseconds (exact; saturating).
pub fn ticks_to_ns(ticks: u64) -> u64 {
    ticks.saturating_mul(NS_PER_TICK)
}

/// The outcome of evaluating an armed timer against the current time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expiry {
    /// How many times the timer has fired in `(.., now]` — always `>= 1` (this
    /// value is only produced when the timer has expired). This is the `u64` a
    /// `read(2)` on the timerfd returns.
    pub count: u64,
    /// The next absolute deadline strictly after `now` at which an interval
    /// timer will fire — the value a `read(2)` re-bases the timer to. For a
    /// one-shot timer (`interval == 0`) this equals the original `expiry` and is
    /// unused (the caller disarms instead).
    pub next: u64,
}

/// Compute how many times a timer armed at absolute deadline `expiry` with
/// period `interval` (`0` = one-shot) has fired by absolute time `now`.
///
/// Returns `None` when the timer has not yet fired (`now < expiry`).
///
/// For an interval timer the returned `next` is guaranteed to be strictly
/// greater than `now` and at most `interval` beyond it, so re-basing a
/// just-`read` timer to `next` makes the very next `read` correctly return 0
/// until the following period elapses.
pub fn expirations(now: u64, expiry: u64, interval: u64) -> Option<Expiry> {
    if now < expiry {
        return None;
    }
    let elapsed = now - expiry;
    // 1 fire at `expiry`, plus one per full period since. For a one-shot
    // (`interval == 0`) `checked_div` yields `None` → 0 extra fires → `count`
    // collapses to 1 and `next` to `expiry` (unused; the caller disarms).
    let count = 1 + elapsed.checked_div(interval).unwrap_or(0);
    // expiry + count*interval is the first deadline strictly after `now`.
    let next = expiry.saturating_add(count.saturating_mul(interval));
    Some(Expiry { count, next })
}

/// Remaining ticks until the next expiration, for `timerfd_gettime`'s reported
/// `it_value`. `armed == false` (disarmed) yields `0`. An armed timer that has
/// not yet fired yields `expiry - now`. An armed timer that has already fired
/// yields the time to its *next* fire (interval timer) or `0` (a one-shot that
/// has fired and is awaiting a `read`) — matching Linux, which reports the time
/// to the next expiration, never a negative or absolute value.
pub fn remaining(now: u64, armed: bool, expiry: u64, interval: u64) -> u64 {
    if !armed {
        return 0;
    }
    if now < expiry {
        return expiry - now;
    }
    match expirations(now, expiry, interval) {
        // Interval timer that has fired: time until the next deadline.
        Some(e) if interval != 0 => e.next.saturating_sub(now),
        // One-shot that has already fired (pending read): nothing remaining.
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ns_per_tick_matches_ticks_per_sec() {
        // 1 tick = 1 ms at 1 kHz; this pins NS_PER_TICK to the kernel's
        // TICKS_PER_SEC so the two can't silently drift.
        assert_eq!(
            NS_PER_TICK * crate::time::TICKS_PER_SEC_EXPECTED,
            1_000_000_000
        );
    }

    #[test]
    fn ns_to_ticks_rounds_up_nonzero_keeps_zero() {
        assert_eq!(ns_to_ticks_ceil(0), 0); // disarm/no-interval sentinel
        assert_eq!(ns_to_ticks_ceil(1), 1); // 1 ns still arms next tick
        assert_eq!(ns_to_ticks_ceil(999_999), 1);
        assert_eq!(ns_to_ticks_ceil(1_000_000), 1); // exactly 1 ms
        assert_eq!(ns_to_ticks_ceil(1_000_001), 2);
        assert_eq!(ns_to_ticks_ceil(1_500_000), 2);
        assert_eq!(ns_to_ticks_ceil(2_000_000), 2);
    }

    #[test]
    fn ticks_to_ns_is_exact() {
        assert_eq!(ticks_to_ns(0), 0);
        assert_eq!(ticks_to_ns(1), 1_000_000);
        assert_eq!(ticks_to_ns(250), 250_000_000);
    }

    #[test]
    fn not_yet_expired_is_none() {
        assert_eq!(expirations(99, 100, 0), None);
        assert_eq!(expirations(99, 100, 10), None);
        assert_eq!(expirations(0, 1, 0), None);
    }

    #[test]
    fn one_shot_fires_exactly_once() {
        assert_eq!(
            expirations(100, 100, 0),
            Some(Expiry {
                count: 1,
                next: 100
            })
        );
        // Long after the deadline a one-shot still reports exactly one fire.
        assert_eq!(
            expirations(10_000, 100, 0),
            Some(Expiry {
                count: 1,
                next: 100
            })
        );
    }

    #[test]
    fn interval_counts_fires_and_advances_next() {
        // Armed at 100, period 10.
        assert_eq!(
            expirations(100, 100, 10),
            Some(Expiry {
                count: 1,
                next: 110
            })
        );
        assert_eq!(
            expirations(105, 100, 10),
            Some(Expiry {
                count: 1,
                next: 110
            })
        );
        assert_eq!(
            expirations(110, 100, 10),
            Some(Expiry {
                count: 2,
                next: 120
            })
        );
        assert_eq!(
            expirations(125, 100, 10),
            Some(Expiry {
                count: 3,
                next: 130
            })
        );
        assert_eq!(
            expirations(130, 100, 10),
            Some(Expiry {
                count: 4,
                next: 140
            })
        );
    }

    #[test]
    fn interval_next_is_strictly_after_now_within_one_period() {
        // Property: for any now >= expiry, expiry < next <= now + interval, and
        // re-basing to `next` makes the following read return 0 until `next`.
        let expiry = 1000u64;
        let interval = 7u64;
        for now in expiry..(expiry + 100) {
            let e = expirations(now, expiry, interval).unwrap();
            assert!(e.next > now, "next {} must be > now {}", e.next, now);
            assert!(
                e.next <= now + interval,
                "next {} must be <= now+interval {}",
                e.next,
                now + interval
            );
            // After re-basing to `next`, a read at the same `now` sees nothing.
            assert_eq!(expirations(now, e.next, interval), None);
        }
    }

    #[test]
    fn remaining_disarmed_is_zero() {
        assert_eq!(remaining(50, false, 100, 10), 0);
    }

    #[test]
    fn remaining_before_expiry_counts_down() {
        assert_eq!(remaining(40, true, 100, 0), 60);
        assert_eq!(remaining(99, true, 100, 10), 1);
    }

    #[test]
    fn remaining_after_one_shot_fire_is_zero() {
        // A one-shot that has fired but not been read reports 0 remaining.
        assert_eq!(remaining(150, true, 100, 0), 0);
    }

    #[test]
    fn remaining_interval_reports_time_to_next_fire() {
        // Armed at 100, period 10, now 125: fired at 100/110/120, next at 130.
        assert_eq!(remaining(125, true, 100, 10), 5);
        // Exactly on a period boundary: next fire is one full period away.
        assert_eq!(remaining(120, true, 100, 10), 10);
    }
}
