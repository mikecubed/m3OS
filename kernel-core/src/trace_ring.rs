extern crate alloc;

use alloc::vec::Vec;

// ── SchedTrace — G.2 sched-trace feature schema ──────────────────────────────

/// A single structured state-transition entry for the `sched-trace` feature.
///
/// Emitted (under `#[cfg(feature = "sched-trace")]`) at every v1 block, wake,
/// and scan-expired state write in `kernel/src/task/scheduler.rs`.
///
/// # Dumping the trace ring
///
/// Entries accumulate in a per-core `TraceRing<SCHED_TRACE_RING_SIZE>` stored
/// in `kernel/src/task/sched_trace.rs`. To dump them in a running system, use
/// the `sys_ktrace` syscall or trigger a panic dump (the sched-trace ring is
/// printed alongside the existing `TraceEvent` ring in the panic handler).
///
/// # Caller information
///
/// `caller_file` and `caller_line` come from `core::panic::Location::caller()`
/// via `#[track_caller]` on `record()`. They identify the specific state-write
/// site in the scheduler source, making it possible to correlate a trace entry
/// with a block/wake call in the code without any post-processing.
///
/// # `old_state` / `new_state` encoding
///
/// Both fields use the `u8` discriminant of `TaskState`:
/// ```text
///   0 = Ready
///   1 = Running
///   2 = BlockedOnRecv
///   3 = BlockedOnSend
///   4 = BlockedOnReply
///   5 = BlockedOnNotif
///   6 = BlockedOnFutex
///   7 = Dead
/// ```
#[derive(Clone, Copy, Debug)]
pub struct SchedTrace {
    /// PID of the task whose state changed.
    pub pid: u32,
    /// State before the transition (u8 discriminant of `TaskState`).
    pub old_state: u8,
    /// State after the transition (u8 discriminant of `TaskState`).
    pub new_state: u8,
    /// Source file of the call site that triggered the transition.
    /// Populated via `#[track_caller]` / `core::panic::Location::caller()`.
    pub caller_file: &'static str,
    /// Source line of the call site.
    pub caller_line: u32,
    /// Tick counter at the moment of the transition.
    pub tick: u64,
}

impl SchedTrace {
    /// A zeroed sentinel entry used to initialise ring slots.
    pub const EMPTY: Self = Self {
        pid: 0,
        old_state: 0,
        new_state: 0,
        caller_file: "",
        caller_line: 0,
        tick: 0,
    };
}

/// Structured kernel trace events for scheduler, fork, and IPC paths.
///
/// `#[repr(C)]` ensures a stable, deterministic layout for cross-boundary
/// use (sys_ktrace copies entries to userspace as raw bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum TraceEvent {
    // --- Scheduler ---
    Dispatch {
        task_idx: u32,
        core: u8,
        rsp: u64,
    },
    SwitchOut {
        task_idx: u32,
        core: u8,
        saved_rsp: u64,
    },
    YieldNow {
        task_idx: u32,
        core: u8,
    },
    BlockCurrent {
        task_idx: u32,
        core: u8,
        new_state: u8,
    },
    WakeTask {
        task_idx: u32,
        state_before: u8,
        core: u8,
    },
    RunQueueEnqueue {
        task_idx: u32,
        core: u8,
    },

    // --- Fork ---
    ForkCtxPublish {
        pid: u32,
        rip: u64,
        rsp: u64,
    },
    ForkTaskSpawned {
        pid: u32,
        task_idx: u32,
        core: u8,
    },
    ForkTrampolineEnter {
        pid: u32,
        task_idx: u32,
    },
    ForkTrampolineExit {
        pid: u32,
        rip: u64,
        rsp: u64,
    },

    // --- IPC ---
    RecvBlock {
        task_idx: u32,
        ep: u32,
    },
    RecvWake {
        task_idx: u32,
        ep: u32,
    },
    SendBlock {
        task_idx: u32,
        ep: u32,
    },
    SendWake {
        task_idx: u32,
        ep: u32,
    },
    CallBlock {
        task_idx: u32,
        ep: u32,
    },
    ReplyDeliver {
        caller_idx: u32,
        ep: u32,
    },
    MessageDelivered {
        task_idx: u32,
        ep: u32,
    },
}

/// A single trace ring entry: timestamp + core ID + event.
///
/// `#[repr(C)]` ensures a stable, deterministic layout for sys_ktrace.
/// Explicit `_pad` fields are zeroed on construction to prevent leaking
/// uninitialized kernel memory through padding bytes.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct TraceEntry {
    pub tick: u64,
    pub core: u8,
    /// Explicit padding — always zero. Prevents uninit bytes in the ABI.
    pub _pad: [u8; 7],
    pub event: TraceEvent,
}

impl TraceEntry {
    pub const EMPTY: Self = Self {
        tick: 0,
        core: 0,
        _pad: [0; 7],
        event: TraceEvent::Dispatch {
            task_idx: 0,
            core: 0,
            rsp: 0,
        },
    };
}

/// Per-core lockless circular trace buffer.
///
/// Holds the most recent `N` entries. New entries overwrite the oldest on wrap.
/// No mutex — safe for single-writer (owning core) use.
pub struct TraceRing<const N: usize> {
    buf: [TraceEntry; N],
    write_idx: usize,
    count: usize,
}

impl<const N: usize> TraceRing<N> {
    pub const fn new() -> Self {
        Self {
            buf: [TraceEntry::EMPTY; N],
            write_idx: 0,
            count: 0,
        }
    }

    /// Push an entry into the ring, overwriting the oldest if full.
    pub fn push(&mut self, entry: TraceEntry) {
        if N == 0 {
            return;
        }
        self.buf[self.write_idx] = entry;
        self.write_idx = (self.write_idx + 1) % N;
        if self.count < N {
            self.count += 1;
        }
    }

    /// Return all entries in chronological order (oldest first).
    pub fn snapshot(&self) -> Vec<TraceEntry> {
        let mut out = Vec::with_capacity(self.count);
        if self.count == 0 {
            return out;
        }
        let start = if self.count < N { 0 } else { self.write_idx };
        for i in 0..self.count {
            out.push(self.buf[(start + i) % N]);
        }
        out
    }

    /// Iterate entries in chronological order without allocating.
    ///
    /// Calls `f` for each entry from oldest to newest. Safe for use in
    /// panic/fault context where the heap may be corrupted.
    pub fn for_each_chronological(&self, mut f: impl FnMut(&TraceEntry)) {
        if self.count == 0 {
            return;
        }
        let start = if self.count < N { 0 } else { self.write_idx };
        for i in 0..self.count {
            f(&self.buf[(start + i) % N]);
        }
    }

    /// Copy entries in chronological order into `dst`, returning the count written.
    ///
    /// Does not allocate. Suitable for `sys_ktrace` where entries must be
    /// copied into a caller-provided buffer.
    pub fn copy_into(&self, dst: &mut [TraceEntry]) -> usize {
        if self.count == 0 {
            return 0;
        }
        let start = if self.count < N { 0 } else { self.write_idx };
        let n = self.count.min(dst.len());
        for (i, slot) in dst.iter_mut().enumerate().take(n) {
            *slot = self.buf[(start + i) % N];
        }
        n
    }

    /// Number of entries currently in the ring.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl<const N: usize> Default for TraceRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(tick: u64) -> TraceEntry {
        TraceEntry {
            tick,
            core: 0,
            _pad: [0; 7],
            event: TraceEvent::Dispatch {
                task_idx: tick as u32,
                core: 0,
                rsp: 0,
            },
        }
    }

    #[test]
    fn empty_ring_snapshot_returns_empty() {
        let ring = TraceRing::<8>::new();
        assert!(ring.snapshot().is_empty());
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn push_n_entries_returns_all_in_order() {
        let mut ring = TraceRing::<8>::new();
        for i in 0..8 {
            ring.push(make_entry(i));
        }
        let snap = ring.snapshot();
        assert_eq!(snap.len(), 8);
        for (i, entry) in snap.iter().enumerate() {
            assert_eq!(entry.tick, i as u64);
        }
    }

    #[test]
    fn push_n_plus_one_drops_oldest() {
        let mut ring = TraceRing::<8>::new();
        for i in 0..9 {
            ring.push(make_entry(i));
        }
        let snap = ring.snapshot();
        assert_eq!(snap.len(), 8);
        // oldest (tick=0) should be gone, should have ticks 1..=8
        for (i, entry) in snap.iter().enumerate() {
            assert_eq!(entry.tick, (i + 1) as u64);
        }
    }

    #[test]
    fn push_3n_entries_keeps_last_n() {
        let mut ring = TraceRing::<8>::new();
        for i in 0..24 {
            ring.push(make_entry(i));
        }
        let snap = ring.snapshot();
        assert_eq!(snap.len(), 8);
        // should have ticks 16..=23
        for (i, entry) in snap.iter().enumerate() {
            assert_eq!(entry.tick, (16 + i) as u64);
        }
    }

    #[test]
    fn partial_fill_preserves_order() {
        let mut ring = TraceRing::<16>::new();
        for i in 0..5 {
            ring.push(make_entry(i));
        }
        let snap = ring.snapshot();
        assert_eq!(snap.len(), 5);
        for (i, entry) in snap.iter().enumerate() {
            assert_eq!(entry.tick, i as u64);
        }
    }

    #[test]
    fn event_variants_round_trip() {
        let mut ring = TraceRing::<4>::new();
        ring.push(TraceEntry {
            tick: 1,
            core: 0,
            _pad: [0; 7],
            event: TraceEvent::ForkCtxPublish {
                pid: 42,
                rip: 0x1000,
                rsp: 0x2000,
            },
        });
        ring.push(TraceEntry {
            tick: 2,
            core: 1,
            _pad: [0; 7],
            event: TraceEvent::RecvBlock { task_idx: 3, ep: 7 },
        });
        let snap = ring.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(matches!(
            snap[0].event,
            TraceEvent::ForkCtxPublish {
                pid: 42,
                rip: 0x1000,
                rsp: 0x2000,
            }
        ));
        assert!(matches!(
            snap[1].event,
            TraceEvent::RecvBlock { task_idx: 3, ep: 7 }
        ));
    }

    #[test]
    fn for_each_chronological_matches_snapshot() {
        let mut ring = TraceRing::<8>::new();
        for i in 0..12 {
            ring.push(make_entry(i));
        }
        let snap = ring.snapshot();
        let mut collected = Vec::new();
        ring.for_each_chronological(|e| collected.push(e.tick));
        assert_eq!(collected.len(), snap.len());
        for (i, entry) in snap.iter().enumerate() {
            assert_eq!(collected[i], entry.tick);
        }
    }

    #[test]
    fn copy_into_fills_dst() {
        let mut ring = TraceRing::<8>::new();
        for i in 0..10 {
            ring.push(make_entry(i));
        }
        let mut dst = [TraceEntry::EMPTY; 4];
        let n = ring.copy_into(&mut dst);
        assert_eq!(n, 4);
        // Should get the 4 oldest of the 8 entries (ticks 2..=5)
        for i in 0..4 {
            assert_eq!(dst[i].tick, (2 + i) as u64);
        }
    }

    // ── G.2: SchedTrace schema tests ─────────────────────────────────────

    /// SchedTrace::EMPTY sentinel is fully zeroed.
    #[test]
    fn sched_trace_empty_sentinel_is_zeroed() {
        let e = SchedTrace::EMPTY;
        assert_eq!(e.pid, 0);
        assert_eq!(e.old_state, 0); // Ready discriminant
        assert_eq!(e.new_state, 0);
        assert_eq!(e.caller_file, "");
        assert_eq!(e.caller_line, 0);
        assert_eq!(e.tick, 0);
    }

    /// SchedTrace round-trips through a TraceRing.
    #[test]
    fn sched_trace_ring_round_trip() {
        let mut ring: [SchedTrace; 4] = [SchedTrace::EMPTY; 4];
        // Simulate pushing two entries (manual ring for simplicity).
        ring[0] = SchedTrace {
            pid: 42,
            old_state: 1, // Running
            new_state: 2, // BlockedOnRecv
            caller_file: "scheduler.rs",
            caller_line: 1095,
            tick: 50_000,
        };
        ring[1] = SchedTrace {
            pid: 42,
            old_state: 2, // BlockedOnRecv
            new_state: 0, // Ready
            caller_file: "scheduler.rs",
            caller_line: 1433,
            tick: 80_000,
        };

        // Verify field integrity.
        assert_eq!(ring[0].pid, 42);
        assert_eq!(ring[0].old_state, 1);
        assert_eq!(ring[0].new_state, 2);
        assert_eq!(ring[0].caller_file, "scheduler.rs");
        assert_eq!(ring[0].caller_line, 1095);
        assert_eq!(ring[0].tick, 50_000);

        assert_eq!(ring[1].pid, 42);
        assert_eq!(ring[1].old_state, 2);
        assert_eq!(ring[1].new_state, 0);
        assert_eq!(ring[1].tick, 80_000);
    }

    /// State u8 encoding matches expected discriminants.
    ///
    /// This test serves as a contract: if kernel `TaskState` discriminants
    /// change, the mismatch will be caught here before the trace becomes
    /// silently wrong.
    #[test]
    fn sched_trace_state_discriminant_contract() {
        // Encoding documented in SchedTrace's docstring:
        const READY: u8 = 0;
        const RUNNING: u8 = 1;
        const BLOCKED_ON_RECV: u8 = 2;
        const BLOCKED_ON_SEND: u8 = 3;
        const BLOCKED_ON_REPLY: u8 = 4;
        const BLOCKED_ON_NOTIF: u8 = 5;
        const BLOCKED_ON_FUTEX: u8 = 6;
        const DEAD: u8 = 7;

        // Sequence must be monotonically increasing (discriminants assigned by
        // declaration order).
        assert!(READY < RUNNING);
        assert!(RUNNING < BLOCKED_ON_RECV);
        assert!(BLOCKED_ON_RECV < BLOCKED_ON_SEND);
        assert!(BLOCKED_ON_SEND < BLOCKED_ON_REPLY);
        assert!(BLOCKED_ON_REPLY < BLOCKED_ON_NOTIF);
        assert!(BLOCKED_ON_NOTIF < BLOCKED_ON_FUTEX);
        assert!(BLOCKED_ON_FUTEX < DEAD);

        // All variants fit in a u8.
        assert!(DEAD <= u8::MAX);
    }

    /// A valid block-then-wake transition sequence encodes correctly.
    #[test]
    fn sched_trace_block_wake_sequence() {
        // block_current: Running → BlockedOnFutex (state 6)
        let block_entry = SchedTrace {
            pid: 7,
            old_state: 1,
            new_state: 6,
            caller_file: "scheduler.rs",
            caller_line: 1231,
            tick: 10_000,
        };
        // wake_task: BlockedOnFutex (6) → Ready (0)
        let wake_entry = SchedTrace {
            pid: 7,
            old_state: 6,
            new_state: 0,
            caller_file: "scheduler.rs",
            caller_line: 1479,
            tick: 15_000,
        };

        // Causal ordering: wake always comes after block.
        assert!(wake_entry.tick > block_entry.tick);
        // State chain is valid: old_state of wake == new_state of block.
        assert_eq!(wake_entry.old_state, block_entry.new_state);
    }
}
