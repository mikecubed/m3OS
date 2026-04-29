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
}
