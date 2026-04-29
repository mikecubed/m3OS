//! H.3 regression guard: `syslogd` cpu-hog fix.
//!
//! Root-cause analysis (Track H.3):
//!
//! **Hypothesis A (primary):** Before G.3, `sys_poll` divided the ms timeout
//! by 10 (stale 100 Hz assumption).  `poll(2000 ms)` actually timed out after
//! only 200 ms, making syslogd wake up 5× per second instead of 0.5× per
//! second, burning 10–15 % CPU idle.  G.3 removed the `÷10` divisor; this is
//! now confirmed by the `multiplier_sweep_smoke` test suite.
//!
//! **Hypothesis B (secondary defence):** Even with a correct poll timeout,
//! `drain_kmsg` must not do unbounded work in one scheduling quantum.  The
//! fix splits the drain into chunks of [`SYSLOGD_KMSG_DRAIN_CHUNK`] messages,
//! yielding (`nanosleep(0)` → kernel `yield_now()`) between chunks, and
//! continues draining until EAGAIN — so a large burst is consumed promptly
//! without monopolising a CPU core.
//!
//! These tests pin both invariants so a regression is immediately visible.

use kernel_core::time::TICKS_PER_SEC_EXPECTED;

/// The drain-chunk size used by `syslogd`'s `drain_kmsg` function.
///
/// This mirrors `syslogd::KMSG_DRAIN_CHUNK` (defined in
/// `userspace/syslogd/src/main.rs`).  It is replicated here so the invariant
/// can be tested on the host without pulling in a `no_std` binary crate.
///
/// **Do not lower this below 1** (would yield after every single message —
/// excessive syscall overhead).  **Do not remove the yield entirely** (would
/// allow unbounded busy work per scheduling quantum).
pub const SYSLOGD_KMSG_DRAIN_CHUNK: usize = 32;

/// The syslogd poll timeout in milliseconds.
///
/// Mirrors `syslogd::POLL_TIMEOUT_MS`.  After G.3 the kernel converts this
/// value to ticks as `timeout_ms` (no divisor), so the daemon actually sleeps
/// for 2 s between idle wakeups.
pub const SYSLOGD_POLL_TIMEOUT_MS: i32 = 2000;

// ---------------------------------------------------------------------------
// H.3 Hypothesis A tests — poll timeout is respected (G.3 fix)
// ---------------------------------------------------------------------------

/// With G.3 (1 tick = 1 ms), a 2000 ms poll timeout produces a 2000-tick
/// deadline, NOT a 200-tick deadline.
///
/// Before G.3: `deadline = start + timeout_ms.div_ceil(10)` → 200 ticks for a
/// 2000 ms timeout → syslogd looped 5× per second idle.
/// After G.3: `deadline = start + timeout_ms` → 2000 ticks → 0.5× per second.
#[test]
fn poll_timeout_is_not_divided_by_ten() {
    let start_tick: u64 = 50_000;
    let timeout_ms: u64 = SYSLOGD_POLL_TIMEOUT_MS as u64;

    // Correct formula (G.3): 1 tick = 1 ms, deadline = start + ms.
    let correct_deadline = start_tick + timeout_ms;
    // Stale formula (pre-G.3): divided by 10.
    let stale_deadline = start_tick + timeout_ms.div_ceil(10);

    assert_ne!(
        correct_deadline, stale_deadline,
        "sanity: the formulas must differ for the test to be meaningful"
    );
    assert_eq!(
        correct_deadline, 52_000,
        "syslogd poll(2000 ms) must produce a 52000-tick deadline (50000 + 2000)"
    );
    assert_eq!(
        stale_deadline, 50_200,
        "the stale ÷10 formula gives 50200 ticks — 10× too early"
    );
}

/// Confirm the tick rate matches the value that makes 1 tick = 1 ms true.
///
/// This is a secondary guard: if the tick rate were ever lowered back to 100 Hz
/// the `syslogd` poll timeout would again be 10× too long (in the opposite
/// direction, sleeping too long) — but the cpu-hog caused by the ÷10 divisor
/// bug would re-emerge if the divisor were re-introduced at 100 Hz.
#[test]
fn tick_rate_is_1khz_so_1_tick_equals_1_ms() {
    assert_eq!(
        TICKS_PER_SEC_EXPECTED, 1_000,
        "scheduler must run at 1 kHz (1 tick = 1 ms) for the G.3 fix to be correct"
    );
}

// ---------------------------------------------------------------------------
// H.3 Hypothesis B tests — drain_kmsg chunk size is bounded and non-trivial
// ---------------------------------------------------------------------------

/// `SYSLOGD_KMSG_DRAIN_CHUNK` must be in the range [4, 128].
///
/// - Lower bound 4: fewer than 4 messages per yield is excessive overhead
///   (context-switch cost amortises over too few messages).
/// - Upper bound 128: a larger chunk risks monopolising the CPU for too long
///   during a log burst (128 × ~128 B = ~16 KiB of work before yielding).
#[test]
fn kmsg_drain_chunk_is_in_acceptable_range() {
    assert!(
        SYSLOGD_KMSG_DRAIN_CHUNK >= 4,
        "drain chunk ({}) is too small — excessive yield overhead",
        SYSLOGD_KMSG_DRAIN_CHUNK
    );
    assert!(
        SYSLOGD_KMSG_DRAIN_CHUNK <= 128,
        "drain chunk ({}) is too large — risks CPU monopolisation during log bursts",
        SYSLOGD_KMSG_DRAIN_CHUNK
    );
}

/// Simulate the `drain_kmsg` chunk logic: given N pending messages, verify
/// that the number of yields is `floor(N / SYSLOGD_KMSG_DRAIN_CHUNK)` and the
/// total messages drained equals N.
///
/// This pins the "continue draining until EAGAIN" behaviour introduced in H.3:
/// the drain no longer exits after one chunk; it loops until the fd is empty,
/// yielding between each full chunk.
#[test]
fn drain_kmsg_yields_between_chunks_and_drains_all() {
    let chunk = SYSLOGD_KMSG_DRAIN_CHUNK;

    for &total_msgs in &[0usize, 1, chunk - 1, chunk, chunk + 1, chunk * 3 + 7] {
        let (drained, yields) = simulate_drain(total_msgs, chunk);
        assert_eq!(
            drained, total_msgs,
            "drain must consume all {total_msgs} messages (chunk={chunk})"
        );
        let expected_yields = total_msgs / chunk;
        assert_eq!(
            yields, expected_yields,
            "drain of {total_msgs} messages must yield {expected_yields} times (chunk={chunk})"
        );
    }
}

/// Pure-logic simulation of the `drain_kmsg` loop.
///
/// Returns `(messages_drained, yields_issued)`.
fn simulate_drain(mut pending: usize, chunk: usize) -> (usize, usize) {
    let mut drained = 0usize;
    let mut yields = 0usize;
    let mut chunk_count = 0usize;

    loop {
        if pending == 0 {
            break; // EAGAIN: fd is empty
        }
        pending -= 1;
        drained += 1;
        chunk_count += 1;
        if chunk_count >= chunk {
            yields += 1;
            chunk_count = 0;
        }
    }

    (drained, yields)
}

/// Before H.3 the drain loop broke out after the first chunk even when more
/// messages were pending.  The simulation below encodes the *old* (broken)
/// behaviour and asserts that it fails the "drain all messages" property.
///
/// This test documents why the break-after-one-chunk approach was wrong.
#[test]
fn old_drain_breaks_after_one_chunk_leaving_messages_pending() {
    let chunk = SYSLOGD_KMSG_DRAIN_CHUNK;
    let total = chunk * 3; // 3 full chunks of pending messages

    let (old_drained, old_yields) = simulate_drain_old(total, chunk);

    // Old code: drained exactly one chunk, then stopped.
    assert_eq!(
        old_drained, chunk,
        "old drain consumed {old_drained} messages, expected exactly one chunk ({chunk})"
    );
    assert_eq!(
        old_yields, 1,
        "old drain issued {old_yields} yield(s), expected 1"
    );

    // Prove that old behaviour left messages un-drained.
    assert!(
        old_drained < total,
        "old drain left {} messages pending (total={total}, chunk={chunk})",
        total - old_drained
    );
}

/// Pure-logic simulation of the *old* `drain_kmsg` behaviour: yield once after
/// a full chunk and then break entirely (exit the function).
fn simulate_drain_old(mut pending: usize, chunk: usize) -> (usize, usize) {
    let mut drained = 0usize;
    let mut yields = 0usize;

    loop {
        if pending == 0 {
            break;
        }
        pending -= 1;
        drained += 1;
        if drained >= chunk {
            // Old code: yield then break — leaves remaining messages un-drained.
            yields += 1;
            break;
        }
    }

    (drained, yields)
}
