//! Pure `eventfd(2)` counter state machine — Phase 86d Track D, host-testable.
//!
//! The kernel object in `kernel/src/eventfd.rs` owns the table, refcounts, and
//! wait queue (all `no_std` kernel types that can't run on the host). The
//! *counter transitions* — read-drain vs `EFD_SEMAPHORE` decrement, the
//! `2^64-2` overflow cap, and the `val == u64::MAX` rejection — are pure
//! arithmetic with no kernel dependency, so they live here and are unit-tested
//! on the host (mirroring how Track A's VMA logic and Track B's epoll edge
//! logic were extracted). The kernel object calls straight through to these.

/// Linux caps the eventfd counter at `2^64 - 2`; a write that would reach
/// `2^64 - 1` blocks (or `EAGAIN`s for non-blocking fds).
pub const EVENTFD_COUNTER_MAX: u64 = u64::MAX - 1;

/// Outcome of an `eventfd` read against the current counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    /// Counter is 0 — nothing to read (`EAGAIN` for non-blocking; block
    /// otherwise).
    Empty,
    /// `(value_returned_to_userspace, new_counter)`.
    Value(u64, u64),
}

/// Compute the result of reading an `eventfd`. With `EFD_SEMAPHORE` a non-zero
/// counter yields `1` and decrements by one; otherwise it yields the whole
/// counter and resets it to `0`. A zero counter yields [`ReadOutcome::Empty`].
pub fn read_outcome(counter: u64, semaphore: bool) -> ReadOutcome {
    if counter == 0 {
        ReadOutcome::Empty
    } else if semaphore {
        ReadOutcome::Value(1, counter - 1)
    } else {
        ReadOutcome::Value(counter, 0)
    }
}

/// Outcome of an `eventfd` write against the current counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The new counter value after adding the written amount.
    Ok(u64),
    /// The value `0xffff_ffff_ffff_ffff` is rejected (`EINVAL`).
    Invalid,
    /// The add would overflow the counter cap (`EAGAIN` for non-blocking;
    /// block otherwise).
    WouldBlock,
}

/// Compute the result of adding `val` to an `eventfd` counter. `val == u64::MAX`
/// is rejected (`EINVAL`, matching Linux); a sum exceeding [`EVENTFD_COUNTER_MAX`]
/// (or overflowing `u64`) yields [`WriteOutcome::WouldBlock`].
pub fn write_outcome(counter: u64, val: u64) -> WriteOutcome {
    if val == u64::MAX {
        return WriteOutcome::Invalid;
    }
    match counter.checked_add(val) {
        Some(sum) if sum <= EVENTFD_COUNTER_MAX => WriteOutcome::Ok(sum),
        _ => WriteOutcome::WouldBlock,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_empty_counter_is_empty() {
        assert_eq!(read_outcome(0, false), ReadOutcome::Empty);
        assert_eq!(read_outcome(0, true), ReadOutcome::Empty);
    }

    #[test]
    fn read_non_semaphore_drains_whole_counter() {
        assert_eq!(read_outcome(7, false), ReadOutcome::Value(7, 0));
        assert_eq!(read_outcome(1, false), ReadOutcome::Value(1, 0));
    }

    #[test]
    fn read_semaphore_returns_one_and_decrements() {
        assert_eq!(read_outcome(3, true), ReadOutcome::Value(1, 2));
        // Draining a semaphore one at a time reaches Empty exactly at 0.
        assert_eq!(read_outcome(1, true), ReadOutcome::Value(1, 0));
    }

    #[test]
    fn write_adds_to_counter() {
        assert_eq!(write_outcome(0, 1), WriteOutcome::Ok(1));
        assert_eq!(write_outcome(5, 10), WriteOutcome::Ok(15));
    }

    #[test]
    fn write_max_value_is_invalid() {
        // u64::MAX (0xffff…ffff) is the sentinel Linux rejects with EINVAL,
        // regardless of the current counter.
        assert_eq!(write_outcome(0, u64::MAX), WriteOutcome::Invalid);
        assert_eq!(write_outcome(100, u64::MAX), WriteOutcome::Invalid);
    }

    #[test]
    fn write_to_the_cap_is_allowed_but_one_more_blocks() {
        // Reaching exactly 2^64-2 is fine; the next unit would hit 2^64-1.
        assert_eq!(
            write_outcome(EVENTFD_COUNTER_MAX - 1, 1),
            WriteOutcome::Ok(EVENTFD_COUNTER_MAX)
        );
        assert_eq!(
            write_outcome(EVENTFD_COUNTER_MAX, 1),
            WriteOutcome::WouldBlock
        );
    }

    #[test]
    fn write_overflow_would_block_not_wrap() {
        // A huge add that overflows u64 must not wrap to a small counter.
        assert_eq!(
            write_outcome(EVENTFD_COUNTER_MAX, EVENTFD_COUNTER_MAX),
            WriteOutcome::WouldBlock
        );
        // Boundary: counter + val == 2^64-1 (the cap+1) blocks.
        assert_eq!(
            write_outcome(1, EVENTFD_COUNTER_MAX),
            WriteOutcome::WouldBlock
        );
    }

    #[test]
    fn write_zero_is_a_noop_ok() {
        // A 0 write is valid and leaves the counter unchanged (no wake needed,
        // but the syscall still succeeds).
        assert_eq!(write_outcome(4, 0), WriteOutcome::Ok(4));
    }
}
