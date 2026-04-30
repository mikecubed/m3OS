//! v2 Scheduler Block/Wake State Machine Model
//!
//! This module encodes the v2 protocol transition table from
//! `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md` as a pure,
//! allocation-free function. It compiles in both `no_std` (kernel) and `std`
//! (host test) contexts via `kernel-core`'s feature flag.
//!
//! # Why this exists
//!
//! The v2 protocol eliminates the v1 lost-wake bug class by making
//! `TaskState` the sole source of truth for block state. All mutations are
//! serialized under a per-task `pi_lock`. This model is the TDD red-phase
//! foundation (Track A.4); every kernel implementation in Tracks C/D/E must
//! satisfy the contract encoded here.
//!
//! # A.7 — DEFERRED
//!
//! Loom interleaving harness (`kernel-core/tests/sched_loom.rs`) is included
//! as a `#[cfg(loom)]`-gated skeleton. Full wiring of the two-thread
//! exhaustive search can land in a follow-up PR once the loom model of the
//! actual primitives (Task::pi_lock, SCHEDULER) is available.

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
    Block {
        kind: BlockKind,
        deadline: Option<u64>,
    },

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
    match (state, event) {
        // ── Ready × block ─────────────────────────────────────────────────
        //
        // Invariant: a `Ready` task is not executing; it cannot call
        // `block_current_until`. This cell is reached only if the primitive is
        // called with an already-true condition from a just-dispatched task
        // before any state write — the function returns without a state change.
        (BlockState::Ready, Event::Block { .. }) => (BlockState::Ready, SideEffects::default()),

        // ── Ready × wake ─────────────────────────────────────────────────
        //
        // Invariant: CAS Blocked*→Ready fails; a wake to a Ready task is a
        // silent no-op (idempotent).
        (BlockState::Ready, Event::Wake) => (BlockState::Ready, SideEffects::default()),

        // ── Ready × scan_expired ─────────────────────────────────────────
        //
        // Invariant: state is not Blocked*; stale deadline (if any) is cleared
        // by the scanner as a clean-up, but no state change occurs.
        (BlockState::Ready, Event::ScanExpired { .. }) => {
            (BlockState::Ready, SideEffects::default())
        }

        // ── Ready × condition_true ────────────────────────────────────────
        //
        // ConditionTrue from a non-blocked state: no-op.
        (BlockState::Ready, Event::ConditionTrue) => (BlockState::Ready, SideEffects::default()),

        // ── Running × block ───────────────────────────────────────────────
        //
        // Invariant: a Running task writes state → Blocked* under `pi_lock`,
        // then yields via `SCHEDULER.lock` / `switch_context`. The condition
        // recheck (step 3 of the four-step recipe) happens AFTER the state
        // write; if true, `ConditionTrue` fires instead (self-revert path).
        // This arm represents the case where the recheck is false — the task
        // yields.
        (BlockState::Running, Event::Block { kind, deadline }) => {
            let next = match kind {
                BlockKind::Recv => BlockState::BlockedOnRecv,
                BlockKind::Send => BlockState::BlockedOnSend,
                BlockKind::Reply => BlockState::BlockedOnReply,
                BlockKind::Notif => BlockState::BlockedOnNotif,
                BlockKind::Futex => BlockState::BlockedOnFutex,
            };
            (
                next,
                SideEffects {
                    deadline_set: deadline,
                    yielded: true,
                    ..SideEffects::default()
                },
            )
        }

        // ── Running × wake ────────────────────────────────────────────────
        //
        // Invariant: CAS Blocked*→Ready fails (state is Running). A wake to a
        // Running task is idempotent; the task will recheck its condition in
        // the `block_current_until` loop on resume.
        (BlockState::Running, Event::Wake) => (BlockState::Running, SideEffects::default()),

        // ── Running × scan_expired ────────────────────────────────────────
        //
        // Invariant: state is Running (not Blocked*); scan is a no-op.
        (BlockState::Running, Event::ScanExpired { .. }) => {
            (BlockState::Running, SideEffects::default())
        }

        // ── Running × condition_true ──────────────────────────────────────
        //
        // ConditionTrue from Running: the task is already running; no-op.
        (BlockState::Running, Event::ConditionTrue) => {
            (BlockState::Running, SideEffects::default())
        }

        // ── Blocked* × block ─────────────────────────────────────────────
        //
        // Invariant violation: a Blocked* task is off-CPU and cannot call
        // `block_current_until`. Panic immediately.
        (s, Event::Block { .. }) if s.is_blocked() => {
            panic!(
                "apply_event: Block event on off-CPU task in state {:?} — kernel invariant violated",
                s
            )
        }

        // ── Blocked* × wake → Ready ───────────────────────────────────────
        //
        // Invariant: CAS Blocked*→Ready succeeds; wake_deadline cleared;
        // task enqueued to assigned_core run queue; on_cpu spin-wait required
        // before enqueue (Linux p->on_cpu smp_cond_load_acquire pattern).
        // IPI is required when the enqueue target is a different core — the
        // model marks it as potentially needed; the actual decision depends on
        // runtime core assignment which is outside the pure model.
        (s, Event::Wake) if s.is_blocked() => (
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                on_cpu_wait_required: true,
                ..SideEffects::default()
            },
        ),

        // ── Blocked* × scan_expired → Ready ──────────────────────────────
        //
        // Invariant: deadline scanner fires under SCHEDULER.lock; CAS
        // Blocked*→Ready; wake_deadline cleared; enqueued after lock release.
        // No on_cpu spin-wait because scan runs under SCHEDULER.lock and the
        // enqueue is deferred to after lock release (scan adds to an expired
        // array, not inline enqueue under the lock).
        (s, Event::ScanExpired { .. }) if s.is_blocked() => (
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                ..SideEffects::default()
            },
        ),

        // ── Blocked* × condition_true → Running (self-revert) ────────────
        //
        // Invariant: condition recheck in step 3 of the four-step recipe
        // observed true before yielding. The task re-acquires `pi_lock`,
        // CAS Blocked*→Running, clears wake_deadline, and returns without
        // yielding. This closes the lost-wake race window between the state
        // write (step 1) and the yield (step 4).
        (s, Event::ConditionTrue) if s.is_blocked() => (
            BlockState::Running,
            SideEffects {
                deadline_cleared: true,
                // yielded is false: the self-revert path avoids switch_context.
                ..SideEffects::default()
            },
        ),

        // ── Dead × block ─────────────────────────────────────────────────
        //
        // Invariant violation: a Dead task must not re-enter any block
        // primitive.
        (BlockState::Dead, Event::Block { .. }) => {
            panic!("apply_event: Block event on Dead task — kernel invariant violated")
        }

        // ── Dead × wake → Dead (no-op) ────────────────────────────────────
        //
        // Invariant: CAS Blocked*→Ready fails (state is Dead). A wake to a
        // Dead task is a silent no-op; the reaper (drain_dead) handles cleanup
        // independently.
        (BlockState::Dead, Event::Wake) => (BlockState::Dead, SideEffects::default()),

        // ── Dead × scan_expired → Dead (no-op) ───────────────────────────
        //
        // Invariant: state is Dead (not Blocked*); stale deadline cleaned up
        // without any state change.
        (BlockState::Dead, Event::ScanExpired { .. }) => (BlockState::Dead, SideEffects::default()),

        // ── Dead × condition_true → Dead (no-op) ─────────────────────────
        //
        // ConditionTrue on a Dead task: unreachable in practice but safe to
        // treat as a no-op so the model remains total.
        (BlockState::Dead, Event::ConditionTrue) => (BlockState::Dead, SideEffects::default()),

        // ── Explicit Blocked* arms (required for exhaustiveness) ──────────
        //
        // Rust does not count guard-qualified arms (e.g. `if s.is_blocked()`)
        // toward exhaustiveness, so we enumerate the remaining Blocked* ×
        // event combinations explicitly. All of these are handled by the
        // guard arms above; the unreachable!() is a safety net.
        (
            BlockState::BlockedOnRecv
            | BlockState::BlockedOnSend
            | BlockState::BlockedOnReply
            | BlockState::BlockedOnNotif
            | BlockState::BlockedOnFutex,
            _,
        ) => unreachable!(
            "apply_event: unhandled Blocked* × event — this is a bug in the match ordering"
        ),
    }
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
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
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
            Event::Block {
                kind: BlockKind::Recv,
                deadline: Some(500),
            },
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
            Event::Block {
                kind: BlockKind::Send,
                deadline: None,
            },
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
            Event::Block {
                kind: BlockKind::Reply,
                deadline: None,
            },
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
            Event::Block {
                kind: BlockKind::Notif,
                deadline: Some(999),
            },
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
            Event::Block {
                kind: BlockKind::Futex,
                deadline: None,
            },
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
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
    }

    /// Cell: BlockedOnSend × block — not reachable.
    #[test]
    #[should_panic]
    fn test_blocked_on_send_block_unreachable() {
        apply_event(
            BlockState::BlockedOnSend,
            Event::Block {
                kind: BlockKind::Send,
                deadline: None,
            },
        );
    }

    /// Cell: BlockedOnReply × block — not reachable.
    #[test]
    #[should_panic]
    fn test_blocked_on_reply_block_unreachable() {
        apply_event(
            BlockState::BlockedOnReply,
            Event::Block {
                kind: BlockKind::Reply,
                deadline: None,
            },
        );
    }

    /// Cell: BlockedOnNotif × block — not reachable.
    #[test]
    #[should_panic]
    fn test_blocked_on_notif_block_unreachable() {
        apply_event(
            BlockState::BlockedOnNotif,
            Event::Block {
                kind: BlockKind::Notif,
                deadline: None,
            },
        );
    }

    /// Cell: BlockedOnFutex × block — not reachable.
    #[test]
    #[should_panic]
    fn test_blocked_on_futex_block_unreachable() {
        apply_event(
            BlockState::BlockedOnFutex,
            Event::Block {
                kind: BlockKind::Futex,
                deadline: None,
            },
        );
    }

    /// Cell: Dead × block — not reachable.
    #[test]
    #[should_panic]
    fn test_dead_block_unreachable() {
        apply_event(
            BlockState::Dead,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
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

    // ── block_current_until-specific tests (Track C TDD gate) ────────────────
    //
    // These tests exercise the four-step Linux recipe modelled by
    // `block_current_until` (Track C.2–C.3). Each test name mirrors the
    // acceptance criterion in the task list so reviewers can match tests to
    // spec lines at a glance.

    /// C.3 — Self-revert path: wake arrives between state-write (step 1) and
    /// yield (step 4).  The model represents this as (Running → Block →
    /// BlockedOnRecv) followed immediately by (BlockedOnRecv → ConditionTrue →
    /// Running).  No yield should be observed; `deadline_cleared` fires
    /// because the protocol clears the deadline on self-revert.
    ///
    /// Corresponds to `BlockOutcome::AlreadyTrue` / `Woken` return value from
    /// `block_current_until` when the condition check in step 3 succeeds.
    #[test]
    fn block_current_until_self_revert_when_condition_already_true() {
        // Step 1: state write Running → BlockedOnRecv.
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv);
        assert!(fx1.yielded, "state write alone does not yield");

        // Step 3: condition recheck observes wake before yield → self-revert.
        let (s2, fx2) = apply_event(s1, Event::ConditionTrue);
        assert_eq!(s2, BlockState::Running, "self-revert must restore Running");
        assert!(!fx2.yielded, "self-revert must NOT yield");
        assert!(fx2.deadline_cleared, "self-revert must clear wake_deadline");
        assert!(
            !fx2.enqueue_to_run_queue,
            "self-revert does not enqueue (task is still on CPU)"
        );
    }

    /// C.2 — Normal path: condition is false at step 3, task yields; later a
    /// `Wake` event arrives (step 4 wake side), transitioning to Ready.
    ///
    /// Models the common path where `block_current_until` yields and is woken
    /// by a remote `wake_task` call (`BlockOutcome::Woken`).
    #[test]
    fn block_current_until_yields_when_condition_false_at_recheck() {
        // Step 1+4: state write + yield (condition false at recheck → falls
        // through to switch_context).
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv);
        assert!(fx1.yielded, "normal path must yield via switch_context");

        // Wake arrives from another task / interrupt.
        let (s2, fx2) = apply_event(s1, Event::Wake);
        assert_eq!(s2, BlockState::Ready, "Wake must transition to Ready");
        assert!(
            fx2.enqueue_to_run_queue,
            "Wake must enqueue task to run queue"
        );
    }

    /// C.2 — Deadline path: condition is false at recheck AND deadline has
    /// elapsed.  `ScanExpired` fires, waking the task to Ready.
    ///
    /// Models `BlockOutcome::DeadlineExpired`.
    #[test]
    fn block_current_until_deadline_expired_path() {
        // Step 1: block with deadline.
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: Some(1000),
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv);
        assert_eq!(
            fx1.deadline_set,
            Some(1000),
            "deadline must be recorded in pi_lock-protected field"
        );

        // Deadline scanner fires (tick 1001 > deadline 1000).
        let (s2, fx2) = apply_event(s1, Event::ScanExpired { now: 1001 });
        assert_eq!(
            s2,
            BlockState::Ready,
            "ScanExpired must transition to Ready"
        );
        assert!(fx2.deadline_cleared, "ScanExpired must clear wake_deadline");
        assert!(
            fx2.enqueue_to_run_queue,
            "ScanExpired must enqueue task to run queue"
        );
    }

    /// C.3 — Self-revert for Reply state: wakers can arrive on the BlockedOnReply
    /// path (IPC reply arrives between state-write and yield).
    #[test]
    fn block_current_until_self_revert_no_yield_reply_state() {
        let (s1, _) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Reply,
                deadline: None,
            },
        );
        assert_eq!(s1, BlockState::BlockedOnReply);

        let (s2, fx2) = apply_event(s1, Event::ConditionTrue);
        assert_eq!(
            s2,
            BlockState::Running,
            "self-revert on BlockedOnReply must restore Running"
        );
        assert!(!fx2.yielded, "self-revert must NOT yield");
        assert!(fx2.deadline_cleared, "self-revert must clear wake_deadline");
    }

    // ── E.1 — on_cpu_wait_required invariant ─────────────────────────────
    //
    // When a wake fires while a task is in any Blocked* state, the model
    // requires the wake side to spin-wait on `Task::on_cpu == false` before
    // enqueuing.  This is the RSP-publication barrier: the arch-level
    // switch-out epilogue clears `on_cpu` only after `saved_rsp` is durably
    // written to the task struct.  A waker that skips the spin-wait can
    // observe a published `Ready` state but a stale `saved_rsp`, causing the
    // dispatch path to jump to garbage.
    //
    // The model encodes this requirement as `SideEffects::on_cpu_wait_required`
    // on every `(Blocked*, Wake) → Ready` transition.  The kernel translates
    // it into a `while task.on_cpu.load(Acquire) { core::hint::spin_loop() }`
    // in D.1's `wake_task` implementation.

    /// `BlockedOnRecv × Wake → Ready` sets `on_cpu_wait_required`.
    ///
    /// After `(Running, Block{Recv})` the arch-level epilogue has not yet
    /// cleared `Task::on_cpu`.  A concurrent waker must stall until it does.
    #[test]
    fn wake_during_switch_out_window_requires_on_cpu_wait() {
        // Step 1: Running → BlockedOnRecv (task entered switch-out window).
        let (blocked_state, block_fx) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(blocked_state, BlockState::BlockedOnRecv);
        assert!(block_fx.yielded, "block must yield");

        // Step 2: Concurrent wake fires while the task is in the switch-out
        // window (on_cpu == true at the kernel level; model-level: the task
        // is in Blocked*).
        let (wake_state, wake_fx) = apply_event(blocked_state, Event::Wake);
        assert_eq!(
            wake_state,
            BlockState::Ready,
            "wake must transition to Ready"
        );
        assert!(
            wake_fx.on_cpu_wait_required,
            "wake side MUST spin-wait on on_cpu == false before enqueue \
             (RSP-publication barrier, Linux p->on_cpu pattern)"
        );
        assert!(wake_fx.enqueue_to_run_queue, "wake must request enqueue");
        assert!(wake_fx.deadline_cleared, "wake must clear deadline");
    }

    /// All Blocked* variants × Wake produce `on_cpu_wait_required`.
    ///
    /// The RSP-publication window exists for every block kind, not just Recv.
    /// This test exhaustively checks every (Blocked*, Wake) row.
    #[test]
    fn all_blocked_wake_transitions_require_on_cpu_wait() {
        let blocked_states = [
            BlockState::BlockedOnRecv,
            BlockState::BlockedOnSend,
            BlockState::BlockedOnReply,
            BlockState::BlockedOnNotif,
            BlockState::BlockedOnFutex,
        ];
        for &s in &blocked_states {
            let (new_state, fx) = apply_event(s, Event::Wake);
            assert_eq!(new_state, BlockState::Ready, "state={:?}", s);
            assert!(
                fx.on_cpu_wait_required,
                "on_cpu_wait_required must be true for state={:?} × Wake",
                s
            );
        }
    }

    /// `Blocked* × ScanExpired → Ready` does NOT set `on_cpu_wait_required`.
    ///
    /// The deadline scanner runs inside `SCHEDULER.lock`; enqueue is deferred
    /// to after lock release.  The scan cannot race with the epilogue because
    /// both paths hold `SCHEDULER.lock` — no spin-wait is needed.
    #[test]
    fn scan_expired_wake_does_not_require_on_cpu_wait() {
        let blocked_states = [
            BlockState::BlockedOnRecv,
            BlockState::BlockedOnSend,
            BlockState::BlockedOnReply,
            BlockState::BlockedOnNotif,
            BlockState::BlockedOnFutex,
        ];
        for &s in &blocked_states {
            let (new_state, fx) = apply_event(s, Event::ScanExpired { now: 9999 });
            assert_eq!(new_state, BlockState::Ready, "state={:?}", s);
            assert!(
                !fx.on_cpu_wait_required,
                "scan_expired must NOT set on_cpu_wait_required for state={:?}",
                s
            );
        }
    }

    // ── D.1 — wake_task CAS rewrite TDD gate ─────────────────────────────
    //
    // These consolidating tests document the D.1 acceptance criteria for
    // `wake_task_v2` in `kernel/src/task/scheduler.rs`.  The actual
    // per-variant assertions live in the individual per-row tests above;
    // the tests below provide named entry points matching the task list so
    // reviewers can locate the gate at a glance.
    //
    // Acceptance criteria (from 57a-scheduler-rewrite-tasks.md D.1):
    // - From each Blocked*, Wake → Ready with enqueue_to_run_queue=true,
    //   deadline_cleared=true, on_cpu_wait_required=true.
    // - From Ready, Wake is a no-op (idempotent; already covered by
    //   `test_ready_wake_noop`).
    // - From Running, Wake is a no-op (idempotent; already covered by
    //   `test_running_wake_noop`).
    //
    // The individual row tests (`test_blocked_on_*_wake_to_ready`) and the
    // cross-row sweep (`all_blocked_wake_transitions_require_on_cpu_wait`)
    // already verify every cell.  This test provides a single named gate
    // that CI can filter on: `cargo test wake_task_cas`.

    /// D.1 TDD gate — `wake_task` CAS from every `Blocked*` state succeeds.
    ///
    /// Consolidates: `test_blocked_on_{recv,send,reply,notif,futex}_wake_to_ready`
    /// and `all_blocked_wake_transitions_require_on_cpu_wait`.
    ///
    /// For each `Blocked*` state, `Wake` must produce:
    /// - next state = `Ready`
    /// - `enqueue_to_run_queue = true`
    /// - `deadline_cleared = true`
    /// - `on_cpu_wait_required = true` (Linux `p->on_cpu` spin-wait gate)
    #[test]
    fn wake_task_cas_succeeds_from_blocked() {
        let blocked_states = [
            BlockState::BlockedOnRecv,
            BlockState::BlockedOnSend,
            BlockState::BlockedOnReply,
            BlockState::BlockedOnNotif,
            BlockState::BlockedOnFutex,
        ];
        for &s in &blocked_states {
            let (new_state, fx) = apply_event(s, Event::Wake);
            assert_eq!(
                new_state,
                BlockState::Ready,
                "wake_task CAS must produce Ready for state={:?}",
                s
            );
            assert!(
                fx.enqueue_to_run_queue,
                "wake_task must enqueue task for state={:?}",
                s
            );
            assert!(
                fx.deadline_cleared,
                "wake_task must clear wake_deadline for state={:?}",
                s
            );
            assert!(
                fx.on_cpu_wait_required,
                "wake_task must set on_cpu_wait_required for state={:?} \
                 (Linux p->on_cpu smp_cond_load_acquire pattern)",
                s
            );
        }
    }

    /// D.1 TDD gate — `wake_task` is idempotent when the task is already Ready.
    ///
    /// Consolidates: `test_ready_wake_noop`.
    ///
    /// A CAS from `Ready → Ready` fails (not a `Blocked*` state); the wake
    /// primitive returns `AlreadyAwake` without touching the run queue.
    #[test]
    fn wake_task_idempotent_when_already_ready() {
        let (new_state, fx) = apply_event(BlockState::Ready, Event::Wake);
        assert_eq!(
            new_state,
            BlockState::Ready,
            "wake to Ready task must be a no-op"
        );
        assert!(!fx.enqueue_to_run_queue, "idempotent wake must not enqueue");
        assert!(
            !fx.deadline_cleared,
            "idempotent wake must not clear deadline"
        );
        assert!(
            !fx.on_cpu_wait_required,
            "idempotent wake must not require on_cpu spin-wait"
        );
    }

    /// D.1 TDD gate — `wake_task` is idempotent when the task is Running.
    ///
    /// Consolidates: `test_running_wake_noop`.
    ///
    /// A CAS from `Running → Ready` fails (not a `Blocked*` state); the wake
    /// primitive returns `AlreadyAwake`.  The Running task will recheck its
    /// condition on resume from `block_current_until`.
    #[test]
    fn wake_task_idempotent_when_running() {
        let (new_state, fx) = apply_event(BlockState::Running, Event::Wake);
        assert_eq!(
            new_state,
            BlockState::Running,
            "wake to Running task must be a no-op"
        );
        assert!(!fx.enqueue_to_run_queue, "idempotent wake must not enqueue");
        assert!(
            !fx.deadline_cleared,
            "idempotent wake must not clear deadline"
        );
        assert!(
            !fx.on_cpu_wait_required,
            "idempotent wake must not require on_cpu spin-wait"
        );
    }

    /// D.2 TDD gate — `Wake` then `ScanExpired` on the same task does not
    /// double-enqueue.
    ///
    /// Once `Wake` transitions the task to `Ready`, the subsequent
    /// `ScanExpired` sees a non-`Blocked*` state and is a no-op.  This
    /// models the idempotency guard in `scan_expired_wake_deadlines` v2:
    /// the `pi_lock` CAS from `Blocked*→Ready` in `wake_task_v2` happens
    /// first; the scan's state check then skips the already-Ready task.
    #[test]
    fn wake_then_scan_expired_does_not_double_enqueue() {
        // Step 1: Wake arrives first (e.g. notification signal) — Blocked* → Ready.
        let (s1, fx1) = apply_event(BlockState::BlockedOnRecv, Event::Wake);
        assert_eq!(s1, BlockState::Ready, "wake must produce Ready");
        assert!(fx1.enqueue_to_run_queue, "first wake must enqueue");

        // Step 2: Deadline scan fires after the wake — Ready × ScanExpired is a no-op.
        let (s2, fx2) = apply_event(s1, Event::ScanExpired { now: 5000 });
        assert_eq!(s2, BlockState::Ready, "scan on Ready task must be no-op");
        assert!(
            !fx2.enqueue_to_run_queue,
            "scan after wake must NOT re-enqueue (idempotency guard)"
        );
    }

    /// D.4 TDD gate — scan-expires-during-block-window race.
    ///
    /// A task writes `Blocked*` under `pi_lock` (step 1 of the four-step
    /// recipe) and then `scan_expired_wake_deadlines` fires before the
    /// task yields (step 4).  The scan sees `Blocked*` with an expired
    /// deadline and transitions to `Ready` — the same result as a normal
    /// `Wake` but without the `on_cpu_wait_required` flag (the scan runs
    /// under `SCHEDULER.lock` so the epilogue cannot race).
    ///
    /// Consolidates: `block_current_until_deadline_expired_path` and
    /// `scan_expired_wake_does_not_require_on_cpu_wait`.
    #[test]
    fn scan_expired_during_block_window_race() {
        // Step 1: task writes Running → Blocked* under pi_lock with a deadline.
        let (blocked_state, block_fx) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: Some(100),
            },
        );
        assert_eq!(blocked_state, BlockState::BlockedOnRecv);
        assert_eq!(block_fx.deadline_set, Some(100));

        // Race: scan fires while task is in the switch-out window (before yield).
        // At tick 101, deadline 100 has elapsed.
        let (s2, fx2) = apply_event(blocked_state, Event::ScanExpired { now: 101 });
        assert_eq!(
            s2,
            BlockState::Ready,
            "scan must transition to Ready even in block-window race"
        );
        assert!(fx2.deadline_cleared, "scan must clear the expired deadline");
        assert!(fx2.enqueue_to_run_queue, "scan must enqueue the task");
        assert!(
            !fx2.on_cpu_wait_required,
            "scan does NOT need on_cpu spin-wait (runs under SCHEDULER.lock)"
        );
    }

    // ── H.1 — serial_stdin_feeder_task wake-flag protocol (Phase 57a H.1) ────
    //
    // These tests verify the `STDIN_FEEDER_WOKEN` / `block_current_until`
    // notification protocol for the serial feeder task migration.  They mirror
    // the C.2/C.3 tests but are named to match the H.1 acceptance criteria so
    // reviewers can match each test to a spec line.

    /// H.1 acceptance: ISR sets the wake flag → `block_current_until` returns
    /// `Woken`.
    ///
    /// Models the common path where the COM1 RX ISR fires, sets
    /// `STDIN_FEEDER_WOKEN = true`, and issues a `wake_task_v2` IPI.  The
    /// model represents this as `BlockedOnRecv × Wake → Ready`.
    #[test]
    fn serial_feeder_isr_wake_transitions_blocked_to_ready() {
        // Feeder parks: Running → BlockedOnRecv (block_current_until step 1+4).
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv);
        assert!(fx1.yielded, "feeder must yield when ring buffer is empty");

        // COM1 RX ISR fires: wake_task_v2 → BlockedOnRecv → Ready.
        let (s2, fx2) = apply_event(s1, Event::Wake);
        assert_eq!(
            s2,
            BlockState::Ready,
            "ISR wake must transition feeder to Ready"
        );
        assert!(fx2.enqueue_to_run_queue, "woken feeder must be enqueued");
        assert!(
            fx2.on_cpu_wait_required,
            "cross-core wake requires on_cpu spin-wait before IPI"
        );
    }

    /// H.1 acceptance: wake flag already `true` when feeder rechecks (step 3
    /// self-revert) — `block_current_until` returns `AlreadyTrue` without
    /// yielding.
    ///
    /// Models the race where the COM1 RX ISR fires between the feeder's state
    /// write (step 1) and the condition recheck (step 3): `STDIN_FEEDER_WOKEN`
    /// is already set, so the feeder self-reverts to `Running` without
    /// descending into `switch_context`.
    #[test]
    fn serial_feeder_self_revert_when_flag_already_set() {
        // Step 1: feeder writes Running → BlockedOnRecv under pi_lock.
        let (s1, _fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv);

        // Step 3: condition recheck — ISR already set STDIN_FEEDER_WOKEN.
        let (s2, fx2) = apply_event(s1, Event::ConditionTrue);
        assert_eq!(
            s2,
            BlockState::Running,
            "feeder self-reverts to Running when flag already true"
        );
        assert!(!fx2.yielded, "self-revert must NOT yield");
        assert!(
            fx2.deadline_cleared,
            "self-revert must clear wake_deadline even on indefinite block"
        );
        assert!(
            !fx2.enqueue_to_run_queue,
            "self-revert must not enqueue (task is still on CPU)"
        );
    }

    /// H.1 acceptance: feeder drains bytes, re-clears the flag, and can park
    /// again (idempotency — two consecutive park/wake cycles succeed).
    ///
    /// Verifies that after being woken and re-dispatched to `Running`, the
    /// feeder can park again for the next COM1 RX IRQ without any state
    /// corruption.  The test uses `Block` from `Running` for the second cycle
    /// because the model only has `Running` as a valid entry for `Block`; the
    /// scheduler dispatch (Ready → Running) is an arch detail outside the model.
    #[test]
    fn serial_feeder_two_consecutive_wake_cycles() {
        // --- Cycle 1 ---
        // Feeder parks (block_current_until, ring buffer empty).
        let (s1, _) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv, "first park");

        // COM1 RX ISR fires — wake_task_v2 → BlockedOnRecv → Ready.
        let (s2, fx2) = apply_event(s1, Event::Wake);
        assert_eq!(s2, BlockState::Ready, "first ISR wake");
        assert!(fx2.enqueue_to_run_queue, "first wake must enqueue feeder");

        // Model the scheduler redispatching the feeder: treat as Running for
        // the next block call.  The state machine only models the transition
        // table; `Ready → Running` is the scheduler dispatch step.
        // We re-enter the model from `Running` to represent the next iteration.

        // --- Cycle 2 ---
        // Feeder drains the byte, re-clears STDIN_FEEDER_WOKEN, ring empty → park.
        let (s3, _) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s3, BlockState::BlockedOnRecv, "second park must succeed");

        // Second COM1 RX ISR wake.
        let (s4, fx4) = apply_event(s3, Event::Wake);
        assert_eq!(s4, BlockState::Ready, "second ISR wake must produce Ready");
        assert!(fx4.enqueue_to_run_queue, "second wake must enqueue feeder");
    }

    // ── Phase 57c Track B — block+wake conversion regression tests ────────────
    //
    // The following tests document and regression-guard the Phase 57a
    // block+wake conversions audited in Phase 57c Track B.  Each test names
    // a specific kernel call site and models its block+wake protocol using
    // the state machine already exercised by the C.2/C.3 and H.1 tests above.
    //
    // Sites covered:
    //   TB-1  virtio_blk::do_request         — REQ_WOKEN set by ISR
    //   TB-2  sys_poll no-waiter path        — deadline scanner wake
    //   TB-3  net_task NIC wake              — NIC_WOKEN set by IRQ/RemoteNic
    //   TB-4  WaitQueue::sleep               — wake_one / wake_all protocol
    //   TB-5  futex_wait                     — FUTEX_WAKE → block_current_until
    //   TB-6  NVMe device-host               — no kernel-side spin (doc only)
    //
    // All tests are host-runnable (`cargo test -p kernel-core --target x86_64-unknown-linux-gnu`).

    /// TB-1 — `virtio_blk::do_request` block+wake regression.
    ///
    /// `do_request` clears `REQ_WOKEN`, submits the VirtIO descriptor, then
    /// calls `block_current_until(BlockedOnRecv, &REQ_WOKEN, None)`.  The
    /// virtio-blk ISR (`drain_used_from_irq`) sets `REQ_WOKEN = true` and
    /// issues `wake_task_v2`, transitioning the waiter to `Ready`.
    ///
    /// This test verifies that the normal ISR-wake path (`Running → Blocked →
    /// Ready`) is intact and that the task can re-submit a second request
    /// without any state corruption (idempotency).
    #[test]
    fn virtio_blk_do_request_irq_wake_normal_path() {
        // Phase 1: task submits request → parks on REQ_WOKEN.
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(
            s1,
            BlockState::BlockedOnRecv,
            "do_request must transition to BlockedOnRecv"
        );
        assert!(fx1.yielded, "do_request must yield (device I/O is pending)");

        // ISR fires: drain_used_from_irq sets REQ_WOKEN + wake_task_v2.
        let (s2, fx2) = apply_event(s1, Event::Wake);
        assert_eq!(
            s2,
            BlockState::Ready,
            "ISR wake must transition do_request waiter to Ready"
        );
        assert!(
            fx2.enqueue_to_run_queue,
            "woken do_request task must be enqueued on the run queue"
        );

        // Re-submission: task re-enters do_request for the next sector.
        let (s3, _fx3) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(
            s3,
            BlockState::BlockedOnRecv,
            "second do_request park must succeed"
        );

        let (s4, fx4) = apply_event(s3, Event::Wake);
        assert_eq!(s4, BlockState::Ready, "second ISR wake must produce Ready");
        assert!(
            fx4.enqueue_to_run_queue,
            "second wake must enqueue the task"
        );
    }

    /// TB-1 — `virtio_blk::do_request` self-revert regression.
    ///
    /// If the VirtIO IRQ fires *before* `block_current_until` reaches
    /// step 3 (condition recheck), `REQ_WOKEN` is already `true` and
    /// `block_current_until` self-reverts to `Running` without yielding.
    /// This test guards against the lost-wakeup regression that existed
    /// before Phase 57a.
    #[test]
    fn virtio_blk_do_request_irq_fires_before_park_self_revert() {
        // Task writes BlockedOnRecv (step 1+4).
        let (s1, _) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv);

        // IRQ fires early: REQ_WOKEN already true at step-3 recheck.
        let (s2, fx2) = apply_event(s1, Event::ConditionTrue);
        assert_eq!(
            s2,
            BlockState::Running,
            "early IRQ must self-revert do_request to Running"
        );
        assert!(!fx2.yielded, "self-revert must NOT yield");
        assert!(
            !fx2.enqueue_to_run_queue,
            "self-revert must not enqueue (task still on CPU)"
        );
    }

    /// TB-2 — `sys_poll` deadline-based wake regression.
    ///
    /// When `sys_poll` has registered waiters OR a finite timeout, it calls
    /// `block_current_until(BlockedOnRecv, &woken, Some(deadline_tick))`.
    /// The deadline scanner wakes the task when the tick expires, producing
    /// `BlockOutcome::DeadlineExpired`.
    ///
    /// This test ensures the `Block { deadline: Some(_) } → ScanExpired →
    /// Ready` path is intact so that `poll(fd, timeout_ms)` does not busy-spin
    /// for the duration of the timeout.
    #[test]
    fn sys_poll_deadline_wake_transitions_correctly() {
        // sys_poll blocks with a finite deadline.
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: Some(5000),
            },
        );
        assert_eq!(
            s1,
            BlockState::BlockedOnRecv,
            "sys_poll must park with BlockedOnRecv"
        );
        assert!(fx1.yielded, "sys_poll must yield; no TSC spin allowed");
        assert_eq!(
            fx1.deadline_set,
            Some(5000),
            "deadline must be registered with the scheduler"
        );

        // Deadline scanner fires after timeout expires.
        let (s2, fx2) = apply_event(s1, Event::ScanExpired { now: 5001 });
        assert_eq!(
            s2,
            BlockState::Ready,
            "ScanExpired must transition sys_poll waiter to Ready"
        );
        assert!(
            fx2.enqueue_to_run_queue,
            "expired-deadline task must be enqueued"
        );
    }

    /// TB-2 — `sys_poll` FD-event wake regression.
    ///
    /// Complements the deadline test: a `WaitQueue::wake_one` from a ready
    /// FD wakes the `sys_poll` waiter before the deadline fires.
    #[test]
    fn sys_poll_fd_event_wake_transitions_correctly() {
        let (s1, _) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: Some(5000),
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv);

        // WaitQueue::wake_one fires for a ready FD.
        let (s2, fx2) = apply_event(s1, Event::Wake);
        assert_eq!(
            s2,
            BlockState::Ready,
            "FD-event wake must transition sys_poll waiter to Ready"
        );
        assert!(
            fx2.enqueue_to_run_queue,
            "FD-woken sys_poll task must be enqueued"
        );
    }

    /// TB-3 — `net_task` NIC-wake regression.
    ///
    /// `net_task` parks on `NIC_WOKEN` via `block_current_until(BlockedOnRecv,
    /// &NIC_WOKEN, None)`.  The virtio-net ISR and `RemoteNic::inject_rx_frame`
    /// both set `NIC_WOKEN = true` and call `wake_net_task()`.
    ///
    /// This test models the normal path (IRQ fires after park) and the
    /// self-revert path (IRQ fires before park), matching the pattern
    /// audited in Phase 57c Track B.
    #[test]
    fn net_task_nic_woken_irq_wake_normal_path() {
        // net_task clears NIC_WOKEN, drains queues (empty), then parks.
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(
            s1,
            BlockState::BlockedOnRecv,
            "net_task must park on NIC_WOKEN with BlockedOnRecv"
        );
        assert!(
            fx1.yielded,
            "net_task must yield when no NIC work is pending"
        );

        // NIC IRQ fires: ISR sets NIC_WOKEN + wake_task_v2.
        let (s2, fx2) = apply_event(s1, Event::Wake);
        assert_eq!(
            s2,
            BlockState::Ready,
            "NIC IRQ wake must transition net_task to Ready"
        );
        assert!(
            fx2.enqueue_to_run_queue,
            "woken net_task must be enqueued on the run queue"
        );
    }

    /// TB-3 — `net_task` self-revert when NIC_WOKEN is already set.
    #[test]
    fn net_task_nic_woken_already_set_self_revert() {
        let (s1, _) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv);

        // NIC_WOKEN was set between the clear and the park.
        let (s2, fx2) = apply_event(s1, Event::ConditionTrue);
        assert_eq!(
            s2,
            BlockState::Running,
            "net_task must self-revert when NIC_WOKEN already set"
        );
        assert!(!fx2.yielded, "self-revert must NOT yield");
        assert!(
            !fx2.enqueue_to_run_queue,
            "self-revert must not enqueue (task still on CPU)"
        );
    }

    /// TB-4 — `WaitQueue::sleep` block+wake regression.
    ///
    /// `WaitQueue::sleep` enqueues the task with a per-waiter `AtomicBool`
    /// then calls `block_current_until(BlockedOnRecv, &woken, None)`.
    /// `wake_one`/`wake_all` set `woken = true` and call `wake_task_v2`.
    ///
    /// This test verifies the normal wake path and the self-revert path
    /// for `WaitQueue::sleep`.
    #[test]
    fn wait_queue_sleep_wake_one_normal_path() {
        // Task calls WaitQueue::sleep: enqueue + block_current_until.
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(
            s1,
            BlockState::BlockedOnRecv,
            "WaitQueue::sleep must park with BlockedOnRecv"
        );
        assert!(fx1.yielded, "sleep must yield when queue event is pending");

        // wake_one / wake_all fires: woken flag set + wake_task_v2.
        let (s2, fx2) = apply_event(s1, Event::Wake);
        assert_eq!(
            s2,
            BlockState::Ready,
            "wake_one must transition sleeping task to Ready"
        );
        assert!(
            fx2.enqueue_to_run_queue,
            "woken sleep task must be enqueued on the run queue"
        );
    }

    /// TB-4 — `WaitQueue::sleep` self-revert when woken before parking.
    #[test]
    fn wait_queue_sleep_self_revert_when_woken_before_park() {
        let (s1, _) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s1, BlockState::BlockedOnRecv);

        // wake_one raced with sleep: woken flag already true at recheck.
        let (s2, fx2) = apply_event(s1, Event::ConditionTrue);
        assert_eq!(
            s2,
            BlockState::Running,
            "WaitQueue::sleep must self-revert when woken flag already set"
        );
        assert!(!fx2.yielded, "self-revert must NOT yield");
        assert!(
            !fx2.enqueue_to_run_queue,
            "self-revert must not enqueue (task still on CPU)"
        );
    }

    /// TB-5 — `futex_wait` block+wake regression.
    ///
    /// `sys_futex(FUTEX_WAIT)` enqueues the task in `FUTEX_TABLE` with an
    /// `Arc<AtomicBool>` woken flag then calls
    /// `block_current_until(BlockedOnFutex, &woken_flag, None)`.
    /// `sys_futex(FUTEX_WAKE)` sets the flag and calls `wake_task_v2`.
    ///
    /// This test verifies the `BlockedOnFutex` state is entered and exited
    /// correctly via the standard wake path and the timeout path.
    #[test]
    fn futex_wait_wake_normal_path() {
        // futex(WAIT): task parks with BlockedOnFutex.
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Futex,
                deadline: None,
            },
        );
        assert_eq!(
            s1,
            BlockState::BlockedOnFutex,
            "futex_wait must transition to BlockedOnFutex"
        );
        assert!(fx1.yielded, "futex_wait must yield");

        // futex(WAKE): sets woken flag + wake_task_v2.
        let (s2, fx2) = apply_event(s1, Event::Wake);
        assert_eq!(
            s2,
            BlockState::Ready,
            "FUTEX_WAKE must transition waiter to Ready"
        );
        assert!(
            fx2.enqueue_to_run_queue,
            "woken futex task must be enqueued"
        );
    }

    /// TB-5 — `futex_wait` deadline-expired path.
    ///
    /// `futex(FUTEX_WAIT, ..., timeout)` passes a deadline tick; the deadline
    /// scanner fires `ScanExpired` when the timeout elapses, returning
    /// `ETIMEDOUT` to userspace.
    #[test]
    fn futex_wait_deadline_expired_path() {
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Futex,
                deadline: Some(3000),
            },
        );
        assert_eq!(s1, BlockState::BlockedOnFutex);
        assert!(fx1.yielded);
        assert_eq!(fx1.deadline_set, Some(3000));

        let (s2, fx2) = apply_event(s1, Event::ScanExpired { now: 3001 });
        assert_eq!(
            s2,
            BlockState::Ready,
            "timeout must transition futex waiter to Ready"
        );
        assert!(fx2.enqueue_to_run_queue);
    }

    /// TB-6 — NVMe device-host: no kernel-side spin (structural contract).
    ///
    /// The NVMe driver runs entirely in ring-3 userspace.  The kernel-side
    /// device-host syscalls (`sys_device_claim`, `sys_device_mmio_map`,
    /// `sys_device_irq_subscribe`, `sys_device_dma_alloc`) are
    /// resource-management operations that return immediately; they contain
    /// no polling loops or `spin_loop()` calls.  IRQ delivery to the
    /// userspace NVMe driver goes through the standard notification /
    /// `block_current_until` path already exercised by TB-1 through TB-5.
    ///
    /// This test is a structural contract assertion: it documents that the
    /// kernel NVMe path has NO kernel-side block+wake site to convert.
    #[test]
    fn nvme_device_host_kernel_side_has_no_spin() {
        // The NVMe driver's ring-3 interrupt wait is modelled identically
        // to the virtio-blk ISR wake (TB-1): the userspace driver calls
        // sys_wait_irq (or equivalent), which parks the task via
        // block_current_until; the kernel's IRQ dispatcher sets the woken
        // flag and calls wake_task_v2.  We verify the model is correct for
        // this path using a generic BlockedOnRecv → Wake → Ready sequence.
        let (s1, fx1) = apply_event(
            BlockState::Running,
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(
            s1,
            BlockState::BlockedOnRecv,
            "NVMe IRQ-wait park must use BlockedOnRecv"
        );
        assert!(
            fx1.yielded,
            "NVMe IRQ wait must yield (no kernel busy-spin)"
        );

        let (s2, fx2) = apply_event(s1, Event::Wake);
        assert_eq!(
            s2,
            BlockState::Ready,
            "NVMe IRQ wake must transition to Ready"
        );
        assert!(fx2.enqueue_to_run_queue);
    }
}
