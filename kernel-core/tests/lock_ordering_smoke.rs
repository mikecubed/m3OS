//! Phase 57a B.3 — Lock-ordering invariant smoke test.
//!
//! Tests that the `pi_lock`-is-outer / `SCHEDULER.lock`-is-inner invariant
//! can be detected by a per-CPU flag.  The kernel's `with_block_state` helper
//! fires a `debug_assert!` when `holds_scheduler_lock` is true; this harness
//! exercises the flag logic in pure-Rust `std` space without needing the
//! full kernel runtime.
//!
//! # What this tests
//!
//! The production invariant is:
//! - A thread may acquire `pi_lock` (via `with_block_state`) while holding
//!   `SCHEDULER.lock`.  ← FORBIDDEN (pi_lock is outer, SCHEDULER is inner).
//! - A thread may acquire `SCHEDULER.lock` while holding `pi_lock`. ← allowed.
//!
//! The assertion modeled here: if `holds_scheduler_lock` is `true` when
//! `with_block_state` is entered, the call panics in debug builds.
//!
//! Because the kernel's `try_per_core()` is not available in `std` space, we
//! model the invariant with a plain `AtomicBool` that plays the same role as
//! `PerCoreData::holds_scheduler_lock`.

use std::sync::atomic::{AtomicBool, Ordering};

/// Simulates the `holds_scheduler_lock` per-CPU flag.
static HOLDS_SCHED_LOCK: AtomicBool = AtomicBool::new(false);

/// Model of the lock-ordering check inside `Task::with_block_state`.
///
/// Returns `Ok(())` when the invariant holds, `Err(&str)` when it is
/// violated (mirrors the `debug_assert!` message).
fn check_pi_lock_ordering() -> Result<(), &'static str> {
    if HOLDS_SCHED_LOCK.load(Ordering::Relaxed) {
        Err("pi_lock acquisition while SCHEDULER.lock is held — \
             Linux p->pi_lock → rq->lock ordering violated")
    } else {
        Ok(())
    }
}

/// Helper: simulate acquiring SCHEDULER.lock (sets the flag).
fn acquire_scheduler_lock() {
    HOLDS_SCHED_LOCK.store(true, Ordering::Relaxed);
}

/// Helper: simulate releasing SCHEDULER.lock (clears the flag).
fn release_scheduler_lock() {
    HOLDS_SCHED_LOCK.store(false, Ordering::Relaxed);
}

/// Invariant holds: no SCHEDULER lock held when acquiring pi_lock.
#[test]
fn pi_lock_without_scheduler_lock_is_ok() {
    // Ensure the flag is clear before we start.
    HOLDS_SCHED_LOCK.store(false, Ordering::Relaxed);
    assert!(
        check_pi_lock_ordering().is_ok(),
        "pi_lock should be acquirable when SCHEDULER.lock is not held"
    );
}

/// Violation detected: acquiring pi_lock while SCHEDULER.lock is held.
///
/// In the kernel this is a `debug_assert!` panic; here we check the
/// `Err` return from our model function.
#[test]
fn pi_lock_while_scheduler_lock_held_is_violation() {
    HOLDS_SCHED_LOCK.store(false, Ordering::Relaxed);
    acquire_scheduler_lock();
    let result = check_pi_lock_ordering();
    release_scheduler_lock();

    assert!(
        result.is_err(),
        "lock-ordering violation should be detected when SCHEDULER.lock is held"
    );
    assert!(
        result.unwrap_err().contains("Linux p->pi_lock"),
        "violation message should reference the Linux lock ordering pattern"
    );
}

/// Allowed sequence: SCHEDULER.lock released before pi_lock acquired.
#[test]
fn pi_lock_after_scheduler_lock_released_is_ok() {
    HOLDS_SCHED_LOCK.store(false, Ordering::Relaxed);
    acquire_scheduler_lock();
    release_scheduler_lock();
    // pi_lock acquired after SCHEDULER.lock is released — allowed.
    assert!(
        check_pi_lock_ordering().is_ok(),
        "pi_lock should be acquirable after SCHEDULER.lock is released"
    );
}

/// Allowed sequence: pi_lock held, then SCHEDULER.lock acquired (outer→inner).
///
/// The pi_lock check runs at the beginning of `with_block_state` *before*
/// locking; at that point `holds_scheduler_lock` is false.  The test
/// simulates acquiring SCHEDULER.lock *after* the pi_lock check passes —
/// the correct outer→inner direction.
#[test]
fn outer_to_inner_direction_is_ok() {
    HOLDS_SCHED_LOCK.store(false, Ordering::Relaxed);
    // 1. Check pi_lock ordering (pi_lock is outer — check passes).
    assert!(
        check_pi_lock_ordering().is_ok(),
        "pi_lock check should pass"
    );
    // 2. Now acquire SCHEDULER.lock (inner lock).
    acquire_scheduler_lock();
    // ... do work while holding both ...
    // 3. Release SCHEDULER.lock (inner first).
    release_scheduler_lock();
    // 4. Release pi_lock (outer last) — simulated by doing nothing here.
    //    Invariant maintained throughout.
}
