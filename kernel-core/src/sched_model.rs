/// v2 Scheduler Block/Wake State Machine Model
///
/// This module encodes the v2 protocol transition table from
/// `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md` as a pure,
/// allocation-free function. It compiles in both `no_std` (kernel) and `std`
/// (host test) contexts via `kernel-core`'s feature flag.
///
/// # Why this exists
///
/// The v2 protocol eliminates the v1 lost-wake bug class by making
/// `TaskState` the sole source of truth for block state. All mutations are
/// serialized under a per-task `pi_lock`. This model is the TDD red-phase
/// foundation (Track A.4); every kernel implementation in Tracks C/D/E must
/// satisfy the contract encoded here.
///
/// # A.7 — DEFERRED:
/// Loom interleaving harness (`kernel-core/tests/sched_loom.rs`) is included
/// as a `#[cfg(loom)]`-gated skeleton. Full wiring of the two-thread
/// exhaustive search can land in a follow-up PR once the loom model of the
/// actual primitives (Task::pi_lock, SCHEDULER) is available.

// ── Block state ──────────────────────────────────────────────────────────────

/// Mirror of `TaskState`'s blocked variants plus the non-blocked states.
///
/// Each variant corresponds to a row in the v2 transition table. The
/// invariants below describe what it means for a task to be in each state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState {
    /// The task is enqueued on a run queue and eligible to be dispatched.
    /// It is not currently executing. No `wake_deadline` is set.
    Ready,

    /// The task is currently executing on a CPU core.
    /// Only the running task may call `block_current_until`.
    Running,

    /// The task is off-CPU and waiting for an IPC message to arrive at its
    /// receive endpoint, or for a deadline to expire.
    BlockedOnRecv,

    /// The task is off-CPU and waiting for a send operation to be accepted by
    /// a receiver, or for a deadline to expire.
    BlockedOnSend,

    /// The task is off-CPU and waiting for a reply to an outstanding IPC call,
    /// or for a deadline to expire.
    BlockedOnReply,

    /// The task is off-CPU and waiting for a notification object to be
    /// signalled, or for a deadline to expire.
    BlockedOnNotif,

    /// The task is off-CPU and waiting for a futex word to change value,
    /// or for a deadline to expire.
    BlockedOnFutex,

    /// The task has exited. No further state transitions are valid.
    /// A `wake` to a `Dead` task is a silent no-op; the reaper handles cleanup.
    Dead,
}

impl BlockState {
    /// Returns `true` if the state is any `Blocked*` variant.
    #[inline]
    pub fn is_blocked(self) -> bool {
        matches!(
            self,
            BlockState::BlockedOnRecv
                | BlockState::BlockedOnSend
                | BlockState::BlockedOnReply
                | BlockState::BlockedOnNotif
                | BlockState::BlockedOnFutex
        )
    }
}

// ── Block kind ───────────────────────────────────────────────────────────────

/// The kind of block operation being requested, used in `Event::Block`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// Block waiting for an IPC receive.
    Recv,
    /// Block waiting for an IPC send.
    Send,
    /// Block waiting for an IPC reply.
    Reply,
    /// Block waiting for a notification.
    Notif,
    /// Block waiting for a futex.
    Futex,
}

// ── Events ───────────────────────────────────────────────────────────────────

/// An event that drives a state transition in the v2 scheduler model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The running task calls `block_current_until`.
    ///
    /// Only valid from the `Running` state. The `kind` field selects which
    /// `Blocked*` variant the task transitions to. `deadline` is an optional
    /// absolute tick value after which `scan_expired` will fire a wake.
    Block { kind: BlockKind, deadline: Option<u64> },

    /// An external caller (ISR, another task, the notification path) calls
    /// `wake_task`. Under `pi_lock`, performs a CAS from any `Blocked*` to
    /// `Ready`. If the CAS fails, returns `AlreadyAwake` (idempotent).
    Wake,

    /// `scan_expired_wake_deadlines` found that `wake_deadline <= now` for
    /// this task. Equivalent to a `Wake` event but driven by the deadline
    /// scanner instead of an external waker. `now` is the current tick count.
    ScanExpired { now: u64 },

    /// The block primitive's condition recheck (step 3 of the Linux four-step
    /// recipe) observed that the wait condition is already true before yielding.
    /// The task self-reverts from `Blocked*` to `Running` without a yield.
    ConditionTrue,
}

// ── Side effects ─────────────────────────────────────────────────────────────

/// Side effects produced by a state transition.
///
/// These flags represent the scheduler-visible work that the kernel must
/// perform after `apply_event` returns. The model does not execute any of
/// this work itself; it only declares what is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SideEffects {
    /// If `Some(d)`, the `wake_deadline` for this task must be set to `d`.
    pub deadline_set: Option<u64>,

    /// If `true`, the `wake_deadline` for this task must be cleared (set to
    /// `None`) and `ACTIVE_WAKE_DEADLINES` decremented if it was `Some`.
    pub deadline_cleared: bool,

    /// If `true`, the task must be enqueued onto its `assigned_core` run queue.
    pub enqueue_to_run_queue: bool,

    /// If `true`, the task yielded CPU via `switch_context`. The model records
    /// this as a side effect so tests can verify the no-yield (self-revert)
    /// path.
    pub yielded: bool,

    /// If `true`, the enqueue target is a different core than the waker's core
    /// and a reschedule IPI must be sent.
    pub ipi_required: bool,

    /// If `true`, the wake side must spin-wait (`smp_cond_load_acquire`-style)
    /// on `Task::on_cpu == false` before enqueuing, because the task's
    /// `saved_rsp` may not yet be published by the arch-level switch epilogue.
    /// (Linux `p->on_cpu` spin-wait pattern in `try_to_wake_up`.)
    pub on_cpu_wait_required: bool,
}

// ── State machine ─────────────────────────────────────────────────────────────

/// Apply a scheduler event to a `BlockState`, producing the next state and
/// required side effects. Pure function; no allocation; no mutation of
/// arguments.
///
/// This is a mechanical encoding of the v2 transition table in
/// `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md`.
/// Every reachable (state, event) pair has a single, deterministic result.
/// "Not reachable" cells (e.g. `Block` on an off-CPU task) result in a
/// `panic!` because they represent a kernel invariant violation.
pub fn apply_event(state: BlockState, event: Event) -> (BlockState, SideEffects) {
    // Stub implementation — returns unimplemented!() so red-phase tests fail.
    // The full implementation lands in the green-phase commit (A.4).
    unimplemented!(
        "apply_event stub — green-phase commit will implement the v2 transition table"
    )
}

// ── Tests (A.5) ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: assert both the new state and every SideEffects flag in one call.
    // This keeps each test concise and makes regressions immediately obvious.
    fn assert_transition(
        initial: BlockState,
        event: Event,
        expected_state: BlockState,
        expected_fx: SideEffects,
    ) {
        let (new_state, fx) = apply_event(initial, event);
        assert_eq!(
            new_state, expected_state,
            "state mismatch for {:?} + {:?}",
            initial, event
        );
        assert_eq!(
            fx.deadline_set, expected_fx.deadline_set,
            "deadline_set mismatch for {:?} + {:?}",
            initial, event
        );
        assert_eq!(
            fx.deadline_cleared, expected_fx.deadline_cleared,
            "deadline_cleared mismatch for {:?} + {:?}",
            initial, event
        );
        assert_eq!(
            fx.enqueue_to_run_queue, expected_fx.enqueue_to_run_queue,
            "enqueue_to_run_queue mismatch for {:?} + {:?}",
            initial, event
        );
        assert_eq!(
            fx.yielded, expected_fx.yielded,
            "yielded mismatch for {:?} + {:?}",
            initial, event
        );
        assert_eq!(
            fx.ipi_required, expected_fx.ipi_required,
            "ipi_required mismatch for {:?} + {:?}",
            initial, event
        );
        assert_eq!(
            fx.on_cpu_wait_required, expected_fx.on_cpu_wait_required,
            "on_cpu_wait_required mismatch for {:?} + {:?}",
            initial, event
        );
    }

    // ── Row: Ready ────────────────────────────────────────────────────────

    /// Cell: Ready × block → Ready (no-op early return).
    ///
    /// A task in `Ready` is not executing; `block_current_until` must not
    /// be called, but if it is called with an already-true condition the
    /// function returns without a state write.
    #[test]
    fn test_ready_block_noop() {
        assert_transition(
            BlockState::Ready,
            Event::Block { kind: BlockKind::Recv, deadline: None },
            BlockState::Ready,
            SideEffects::default(),
        );
    }

    /// Cell: Ready × wake → Ready (no-op; CAS Blocked*→Ready fails).
    #[test]
    fn test_ready_wake_noop() {
        assert_transition(
            BlockState::Ready,
            Event::Wake,
            BlockState::Ready,
            SideEffects::default(),
        );
    }

    /// Cell: Ready × scan_expired → Ready (no-op; stale deadline cleaned up).
    #[test]
    fn test_ready_scan_expired_noop() {
        assert_transition(
            BlockState::Ready,
            Event::ScanExpired { now: 100 },
            BlockState::Ready,
            SideEffects::default(),
        );
    }

    // ── Row: Running ──────────────────────────────────────────────────────

    /// Cell: Running × block → BlockedOnRecv (happy path — condition false,
    /// task yields). deadline is Some so deadline_set is populated.
    #[test]
    fn test_running_block_to_blocked_on_recv_with_deadline() {
        assert_transition(
            BlockState::Running,
            Event::Block { kind: BlockKind::Recv, deadline: Some(500) },
            BlockState::BlockedOnRecv,
            SideEffects {
                deadline_set: Some(500),
                yielded: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: Running × block → BlockedOnSend (no deadline).
    #[test]
    fn test_running_block_to_blocked_on_send_no_deadline() {
        assert_transition(
            BlockState::Running,
            Event::Block { kind: BlockKind::Send, deadline: None },
            BlockState::BlockedOnSend,
            SideEffects {
                yielded: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: Running × block → BlockedOnReply.
    #[test]
    fn test_running_block_to_blocked_on_reply() {
        assert_transition(
            BlockState::Running,
            Event::Block { kind: BlockKind::Reply, deadline: None },
            BlockState::BlockedOnReply,
            SideEffects {
                yielded: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: Running × block → BlockedOnNotif.
    #[test]
    fn test_running_block_to_blocked_on_notif() {
        assert_transition(
            BlockState::Running,
            Event::Block { kind: BlockKind::Notif, deadline: Some(999) },
            BlockState::BlockedOnNotif,
            SideEffects {
                deadline_set: Some(999),
                yielded: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: Running × block → BlockedOnFutex.
    #[test]
    fn test_running_block_to_blocked_on_futex() {
        assert_transition(
            BlockState::Running,
            Event::Block { kind: BlockKind::Futex, deadline: None },
            BlockState::BlockedOnFutex,
            SideEffects {
                yielded: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: Running × wake → Running (no-op; CAS Blocked*→Ready fails).
    #[test]
    fn test_running_wake_noop() {
        assert_transition(
            BlockState::Running,
            Event::Wake,
            BlockState::Running,
            SideEffects::default(),
        );
    }

    /// Cell: Running × scan_expired → Running (no-op).
    #[test]
    fn test_running_scan_expired_noop() {
        assert_transition(
            BlockState::Running,
            Event::ScanExpired { now: 200 },
            BlockState::Running,
            SideEffects::default(),
        );
    }

    // ── Row: BlockedOnRecv ────────────────────────────────────────────────

    /// Cell: BlockedOnRecv × wake → Ready.
    ///
    /// CAS `BlockedOnRecv → Ready`; deadline cleared; enqueue to run queue;
    /// on_cpu spin-wait required.
    #[test]
    fn test_blocked_on_recv_wake_to_ready() {
        assert_transition(
            BlockState::BlockedOnRecv,
            Event::Wake,
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                on_cpu_wait_required: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: BlockedOnRecv × scan_expired → Ready.
    ///
    /// Deadline scanner fires; `wake_deadline.take()`; state ← Ready;
    /// enqueue after lock release.
    #[test]
    fn test_blocked_on_recv_scan_expired_to_ready() {
        assert_transition(
            BlockState::BlockedOnRecv,
            Event::ScanExpired { now: 600 },
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                ..SideEffects::default()
            },
        );
    }

    // ── Row: BlockedOnSend ────────────────────────────────────────────────

    /// Cell: BlockedOnSend × wake → Ready.
    #[test]
    fn test_blocked_on_send_wake_to_ready() {
        assert_transition(
            BlockState::BlockedOnSend,
            Event::Wake,
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                on_cpu_wait_required: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: BlockedOnSend × scan_expired → Ready.
    #[test]
    fn test_blocked_on_send_scan_expired_to_ready() {
        assert_transition(
            BlockState::BlockedOnSend,
            Event::ScanExpired { now: 700 },
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                ..SideEffects::default()
            },
        );
    }

    // ── Row: BlockedOnReply ───────────────────────────────────────────────

    /// Cell: BlockedOnReply × wake → Ready.
    ///
    /// This is the fix for the display_server / mouse_server race documented
    /// in `docs/handoff/2026-04-28-graphical-stack-startup.md`.
    #[test]
    fn test_blocked_on_reply_wake_to_ready() {
        assert_transition(
            BlockState::BlockedOnReply,
            Event::Wake,
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                on_cpu_wait_required: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: BlockedOnReply × scan_expired → Ready.
    #[test]
    fn test_blocked_on_reply_scan_expired_to_ready() {
        assert_transition(
            BlockState::BlockedOnReply,
            Event::ScanExpired { now: 800 },
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                ..SideEffects::default()
            },
        );
    }

    // ── Row: BlockedOnNotif ───────────────────────────────────────────────

    /// Cell: BlockedOnNotif × wake → Ready.
    #[test]
    fn test_blocked_on_notif_wake_to_ready() {
        assert_transition(
            BlockState::BlockedOnNotif,
            Event::Wake,
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                on_cpu_wait_required: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: BlockedOnNotif × scan_expired → Ready.
    #[test]
    fn test_blocked_on_notif_scan_expired_to_ready() {
        assert_transition(
            BlockState::BlockedOnNotif,
            Event::ScanExpired { now: 900 },
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                ..SideEffects::default()
            },
        );
    }

    // ── Row: BlockedOnFutex ───────────────────────────────────────────────

    /// Cell: BlockedOnFutex × wake → Ready.
    #[test]
    fn test_blocked_on_futex_wake_to_ready() {
        assert_transition(
            BlockState::BlockedOnFutex,
            Event::Wake,
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                on_cpu_wait_required: true,
                ..SideEffects::default()
            },
        );
    }

    /// Cell: BlockedOnFutex × scan_expired → Ready.
    #[test]
    fn test_blocked_on_futex_scan_expired_to_ready() {
        assert_transition(
            BlockState::BlockedOnFutex,
            Event::ScanExpired { now: 1000 },
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                ..SideEffects::default()
            },
        );
    }

    // ── Row: Dead ─────────────────────────────────────────────────────────

    /// Cell: Dead × wake → Dead (no-op; CAS fails; reaper handles cleanup).
    #[test]
    fn test_dead_wake_noop() {
        assert_transition(
            BlockState::Dead,
            Event::Wake,
            BlockState::Dead,
            SideEffects::default(),
        );
    }

    /// Cell: Dead × scan_expired → Dead (no-op; stale deadline cleaned up).
    #[test]
    fn test_dead_scan_expired_noop() {
        assert_transition(
            BlockState::Dead,
            Event::ScanExpired { now: 1100 },
            BlockState::Dead,
            SideEffects::default(),
        );
    }

    // ── "Not reachable" cells — panic guard ──────────────────────────────
    //
    // The v2 table marks these cells as "Not reachable" because a blocked or
    // dead task cannot call `block_current_until` (it is off-CPU).
    // `apply_event` panics on these inputs; verified by `#[should_panic]`.

    /// Cell: BlockedOnRecv × block — not reachable.
    #[test]
    #[should_panic]
    fn test_blocked_on_recv_block_unreachable() {
        apply_event(
            BlockState::BlockedOnRecv,
            Event::Block { kind: BlockKind::Recv, deadline: None },
        );
    }

    /// Cell: BlockedOnSend × block — not reachable.
    #[test]
    #[should_panic]
    fn test_blocked_on_send_block_unreachable() {
        apply_event(
            BlockState::BlockedOnSend,
            Event::Block { kind: BlockKind::Send, deadline: None },
        );
    }

    /// Cell: BlockedOnReply × block — not reachable.
    #[test]
    #[should_panic]
    fn test_blocked_on_reply_block_unreachable() {
        apply_event(
            BlockState::BlockedOnReply,
            Event::Block { kind: BlockKind::Reply, deadline: None },
        );
    }

    /// Cell: BlockedOnNotif × block — not reachable.
    #[test]
    #[should_panic]
    fn test_blocked_on_notif_block_unreachable() {
        apply_event(
            BlockState::BlockedOnNotif,
            Event::Block { kind: BlockKind::Notif, deadline: None },
        );
    }

    /// Cell: BlockedOnFutex × block — not reachable.
    #[test]
    #[should_panic]
    fn test_blocked_on_futex_block_unreachable() {
        apply_event(
            BlockState::BlockedOnFutex,
            Event::Block { kind: BlockKind::Futex, deadline: None },
        );
    }

    /// Cell: Dead × block — not reachable.
    #[test]
    #[should_panic]
    fn test_dead_block_unreachable() {
        apply_event(
            BlockState::Dead,
            Event::Block { kind: BlockKind::Recv, deadline: None },
        );
    }

    // ── ConditionTrue self-revert path ────────────────────────────────────

    /// The self-revert path: after state write under `pi_lock`, the condition
    /// recheck observes true before yielding. The task returns to Running
    /// without a yield (Linux four-step recipe, step 3).
    ///
    /// Valid from any Blocked* state (the state write happened; condition
    /// was already satisfied).
    #[test]
    fn test_blocked_on_recv_condition_true_self_revert() {
        assert_transition(
            BlockState::BlockedOnRecv,
            Event::ConditionTrue,
            BlockState::Running,
            SideEffects {
                deadline_cleared: true,
                // No yield — self-revert takes place before switch_context.
                ..SideEffects::default()
            },
        );
    }

    #[test]
    fn test_blocked_on_send_condition_true_self_revert() {
        assert_transition(
            BlockState::BlockedOnSend,
            Event::ConditionTrue,
            BlockState::Running,
            SideEffects {
                deadline_cleared: true,
                ..SideEffects::default()
            },
        );
    }

    #[test]
    fn test_blocked_on_reply_condition_true_self_revert() {
        assert_transition(
            BlockState::BlockedOnReply,
            Event::ConditionTrue,
            BlockState::Running,
            SideEffects {
                deadline_cleared: true,
                ..SideEffects::default()
            },
        );
    }

    #[test]
    fn test_blocked_on_notif_condition_true_self_revert() {
        assert_transition(
            BlockState::BlockedOnNotif,
            Event::ConditionTrue,
            BlockState::Running,
            SideEffects {
                deadline_cleared: true,
                ..SideEffects::default()
            },
        );
    }

    #[test]
    fn test_blocked_on_futex_condition_true_self_revert() {
        assert_transition(
            BlockState::BlockedOnFutex,
            Event::ConditionTrue,
            BlockState::Running,
            SideEffects {
                deadline_cleared: true,
                ..SideEffects::default()
            },
        );
    }
}
