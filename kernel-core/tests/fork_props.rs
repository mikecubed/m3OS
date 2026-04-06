//! Property-based tests for fork context handoff invariants.
//!
//! Tests an extracted model of the fork child context queue to verify
//! FIFO ordering, nonzero PIDs, and context integrity under varied
//! interleaved push/pop sequences. (Single-threaded proptest — concurrent
//! multi-threaded testing would require loom.)

use proptest::prelude::*;
use std::collections::VecDeque;

/// Simplified fork context model.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ForkCtx {
    pid: u32,
    user_rip: u64,
    user_rsp: u64,
}

/// Model of the fork child context queue (mirrors kernel's VecDeque usage).
struct ForkQueue {
    queue: VecDeque<ForkCtx>,
}

impl ForkQueue {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    fn push(&mut self, ctx: ForkCtx) {
        self.queue.push_back(ctx);
    }

    fn pop(&mut self) -> Option<ForkCtx> {
        self.queue.pop_front()
    }
}

proptest! {
    /// N push_fork_ctx followed by N pop_front returns correct PIDs in FIFO order.
    #[test]
    fn fifo_ordering(pids in prop::collection::vec(1u32..10000, 1..=16)) {
        let mut queue = ForkQueue::new();

        // Push all contexts.
        for &pid in &pids {
            queue.push(ForkCtx {
                pid,
                user_rip: 0x400000 + (pid as u64) * 0x100,
                user_rsp: 0x7FFF_0000 - (pid as u64) * 0x1000,
            });
        }

        // Pop and verify FIFO order.
        for &expected_pid in &pids {
            let ctx = queue.pop().expect("queue should not be empty");
            assert_eq!(ctx.pid, expected_pid, "FIFO order violated");
            assert_ne!(ctx.user_rip, 0, "user_rip must be nonzero");
            assert_ne!(ctx.user_rsp, 0, "user_rsp must be nonzero");
        }

        // Queue should now be empty.
        assert!(queue.pop().is_none());
    }

    /// Interleaved push/pop never returns a context with PID=0.
    #[test]
    fn no_zero_pid(
        ops in prop::collection::vec(
            prop::bool::ANY,
            1..=32
        ),
        pids in prop::collection::vec(1u32..10000, 32..=32),
    ) {
        let mut queue = ForkQueue::new();
        let mut push_idx = 0;
        let mut popped = Vec::new();

        for &do_push in &ops {
            if do_push && push_idx < pids.len() {
                queue.push(ForkCtx {
                    pid: pids[push_idx],
                    user_rip: 0x400000,
                    user_rsp: 0x7FFF_0000,
                });
                push_idx += 1;
            } else if let Some(ctx) = queue.pop() {
                assert_ne!(ctx.pid, 0, "popped context must not have PID=0");
                popped.push(ctx.pid);
            }
        }

        // Drain remaining.
        while let Some(ctx) = queue.pop() {
            assert_ne!(ctx.pid, 0);
            popped.push(ctx.pid);
        }

        // All popped PIDs must match a prefix of the pushed PIDs in FIFO order.
        for (i, &pid) in popped.iter().enumerate() {
            assert_eq!(pid, pids[i], "FIFO order violated at index {i}");
        }
    }

    /// Each push/pop pair preserves the full context (RIP, RSP).
    #[test]
    fn context_integrity(
        pid in 1u32..10000,
        rip in 0x400000u64..0x800000,
        rsp in 0x7F00_0000u64..0x8000_0000,
    ) {
        let mut queue = ForkQueue::new();
        let original = ForkCtx {
            pid,
            user_rip: rip,
            user_rsp: rsp,
        };
        queue.push(original.clone());

        let popped = queue.pop().expect("should have one entry");
        assert_eq!(popped, original, "context must round-trip exactly");
    }
}
