extern crate alloc;

use alloc::vec::Vec;

/// Structured kernel trace events for scheduler, fork, and IPC paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug)]
pub struct TraceEntry {
    pub tick: u64,
    pub core: u8,
    pub event: TraceEvent,
}

impl TraceEntry {
    pub const EMPTY: Self = Self {
        tick: 0,
        core: 0,
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
            event: TraceEvent::ForkCtxPublish {
                pid: 42,
                rip: 0x1000,
                rsp: 0x2000,
            },
        });
        ring.push(TraceEntry {
            tick: 2,
            core: 1,
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
}
