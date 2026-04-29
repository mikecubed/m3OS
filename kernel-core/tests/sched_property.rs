//! Property-based fuzz harness for the v2 scheduler state machine.
//!
//! Track A.6 — verifies three invariants over ≥ 10,000 random event sequences:
//!
//! 1. **No lost wake.** Every `Block` is eventually followed by a transition
//!    out of `Blocked*` if at least one `Wake` or matching `ScanExpired`
//!    follows it.
//! 2. **Idempotent wake.** Two consecutive `Wake` events on a `Blocked*`
//!    state produce exactly one `Ready` transition; the second is a no-op.
//! 3. **No spurious transitions.** Every (state, event) result matches the
//!    oracle derived from the v2 transition table.
//!
//! The harness is automatically picked up by `cargo test -p kernel-core`.

use kernel_core::sched_model::{apply_event, BlockKind, BlockState, Event, SideEffects};
use proptest::prelude::*;

// ── Arbitrary strategies ──────────────────────────────────────────────────────

fn arb_block_kind() -> impl Strategy<Value = BlockKind> {
    prop_oneof![
        Just(BlockKind::Recv),
        Just(BlockKind::Send),
        Just(BlockKind::Reply),
        Just(BlockKind::Notif),
        Just(BlockKind::Futex),
    ]
}

fn arb_deadline() -> impl Strategy<Value = Option<u64>> {
    prop_oneof![
        Just(None),
        (1u64..10_000u64).prop_map(Some),
    ]
}

/// Generate a single event that is valid from any state.
/// `Block` events are included; callers filter them based on the current state.
fn arb_event() -> impl Strategy<Value = Event> {
    prop_oneof![
        (arb_block_kind(), arb_deadline())
            .prop_map(|(kind, deadline)| Event::Block { kind, deadline }),
        Just(Event::Wake),
        (0u64..20_000u64).prop_map(|now| Event::ScanExpired { now }),
        Just(Event::ConditionTrue),
    ]
}

/// Generate a sequence of events that will be replayed against a task starting
/// in `Running` state. `Block` events are only valid from `Running`; we model
/// this by filtering the sequence during replay (not generation) so proptest
/// can shrink freely.
fn arb_event_sequence(len: usize) -> impl Strategy<Value = Vec<Event>> {
    prop::collection::vec(arb_event(), len)
}

// ── Test oracle (v2 transition table) ────────────────────────────────────────

/// Expected (next_state, side_effects) for every reachable (state, event)
/// pair per the v2 transition table. Returns `None` for "not reachable" cells
/// (blocked tasks calling block, dead tasks calling block).
fn expected_transition(state: BlockState, event: &Event) -> Option<(BlockState, SideEffects)> {
    match (state, event) {
        // ── Ready ──────────────────────────────────────────────────────────
        (BlockState::Ready, Event::Block { .. }) => {
            Some((BlockState::Ready, SideEffects::default()))
        }
        (BlockState::Ready, Event::Wake) => Some((BlockState::Ready, SideEffects::default())),
        (BlockState::Ready, Event::ScanExpired { .. }) => {
            Some((BlockState::Ready, SideEffects::default()))
        }
        // ConditionTrue is only meaningful from Blocked*; Ready is a no-op.
        (BlockState::Ready, Event::ConditionTrue) => {
            Some((BlockState::Ready, SideEffects::default()))
        }

        // ── Running ────────────────────────────────────────────────────────
        (BlockState::Running, Event::Block { kind, deadline }) => {
            let next = match kind {
                BlockKind::Recv => BlockState::BlockedOnRecv,
                BlockKind::Send => BlockState::BlockedOnSend,
                BlockKind::Reply => BlockState::BlockedOnReply,
                BlockKind::Notif => BlockState::BlockedOnNotif,
                BlockKind::Futex => BlockState::BlockedOnFutex,
            };
            Some((
                next,
                SideEffects {
                    deadline_set: *deadline,
                    yielded: true,
                    ..SideEffects::default()
                },
            ))
        }
        (BlockState::Running, Event::Wake) => {
            Some((BlockState::Running, SideEffects::default()))
        }
        (BlockState::Running, Event::ScanExpired { .. }) => {
            Some((BlockState::Running, SideEffects::default()))
        }
        // ConditionTrue from Running: already running, no-op.
        (BlockState::Running, Event::ConditionTrue) => {
            Some((BlockState::Running, SideEffects::default()))
        }

        // ── BlockedOn* × block — not reachable ────────────────────────────
        (s, Event::Block { .. }) if s.is_blocked() => None,
        (BlockState::Dead, Event::Block { .. }) => None,

        // ── BlockedOn* × wake → Ready ─────────────────────────────────────
        (s, Event::Wake) if s.is_blocked() => Some((
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                on_cpu_wait_required: true,
                ..SideEffects::default()
            },
        )),

        // ── BlockedOn* × scan_expired → Ready ─────────────────────────────
        (s, Event::ScanExpired { .. }) if s.is_blocked() => Some((
            BlockState::Ready,
            SideEffects {
                deadline_cleared: true,
                enqueue_to_run_queue: true,
                ..SideEffects::default()
            },
        )),

        // ── BlockedOn* × ConditionTrue → Running (self-revert) ────────────
        (s, Event::ConditionTrue) if s.is_blocked() => Some((
            BlockState::Running,
            SideEffects {
                deadline_cleared: true,
                ..SideEffects::default()
            },
        )),

        // ── Dead ───────────────────────────────────────────────────────────
        (BlockState::Dead, Event::Wake) => Some((BlockState::Dead, SideEffects::default())),
        (BlockState::Dead, Event::ScanExpired { .. }) => {
            Some((BlockState::Dead, SideEffects::default()))
        }
        (BlockState::Dead, Event::ConditionTrue) => {
            Some((BlockState::Dead, SideEffects::default()))
        }

        // All patterns are exhaustive; this arm is unreachable.
        _ => unreachable!("oracle: unhandled (state={:?}, event={:?})", state, event),
    }
}

// ── Property: no spurious transitions ────────────────────────────────────────

/// Every (state, event) pair produces the transition allowed by the v2 table.
/// This is the most fundamental property: `apply_event` must be a faithful
/// encoding of the oracle.
///
/// We skip "not reachable" cells (they panic, which is the correct behavior).
proptest! {
    #![proptest_config(ProptestConfig {
        cases: 10_000,
        ..ProptestConfig::default()
    })]

    #[test]
    fn prop_no_spurious_transitions(
        state_idx in 0usize..8,
        event in arb_event(),
    ) {
        let state = idx_to_state(state_idx);

        // Skip "not reachable" cells — they panic by contract.
        let is_unreachable = matches!(
            (&state, &event),
            (s, Event::Block { .. }) if s.is_blocked() || *s == BlockState::Dead
        );
        if is_unreachable {
            return Ok(());
        }

        if let Some((expected_state, expected_fx)) = expected_transition(state, &event) {
            let (actual_state, actual_fx) = apply_event(state, event);
            prop_assert_eq!(actual_state, expected_state,
                "state mismatch for {:?} + {:?}", state, event);
            prop_assert_eq!(actual_fx.deadline_cleared, expected_fx.deadline_cleared,
                "deadline_cleared mismatch for {:?} + {:?}", state, event);
            prop_assert_eq!(actual_fx.enqueue_to_run_queue, expected_fx.enqueue_to_run_queue,
                "enqueue_to_run_queue mismatch for {:?} + {:?}", state, event);
            prop_assert_eq!(actual_fx.yielded, expected_fx.yielded,
                "yielded mismatch for {:?} + {:?}", state, event);
            prop_assert_eq!(actual_fx.on_cpu_wait_required, expected_fx.on_cpu_wait_required,
                "on_cpu_wait_required mismatch for {:?} + {:?}", state, event);
            // deadline_set is event-data-dependent; check it only when the oracle expects Some.
            if expected_fx.deadline_set.is_some() {
                prop_assert_eq!(actual_fx.deadline_set, expected_fx.deadline_set,
                    "deadline_set mismatch for {:?} + {:?}", state, event);
            }
        }
    }
}

// ── Property: no lost wake ────────────────────────────────────────────────────

/// Replay a sequence of events against a task starting in `Running` state.
/// After the sequence, if at least one `Wake` or `ScanExpired` appeared after
/// the most recent `Block`, the task must not still be in a `Blocked*` state.
///
/// We build a valid replay: `Block` is only applied from `Running`; other
/// events are applied from whatever the current state is. Invalid transitions
/// (e.g. `Block` from `Ready`) are skipped to keep the oracle clean.
proptest! {
    #![proptest_config(ProptestConfig {
        cases: 10_000,
        ..ProptestConfig::default()
    })]

    #[test]
    fn prop_no_lost_wake(events in arb_event_sequence(20)) {
        let mut state = BlockState::Running;

        // Track whether a wake-class event followed the most recent Block.
        let mut last_block_idx: Option<usize> = None;
        let mut wake_after_last_block = false;

        for (i, event) in events.iter().enumerate() {
            // Skip events that would hit "not reachable" cells.
            let skip = matches!(
                (&state, event),
                (s, Event::Block { .. }) if s.is_blocked() || *s == BlockState::Dead
            ) || matches!(
                (&state, event),
                (BlockState::Dead, Event::Block { .. })
            );
            if skip {
                continue;
            }

            // Track Block / wake events for the no-lost-wake invariant.
            match event {
                Event::Block { .. } => {
                    last_block_idx = Some(i);
                    wake_after_last_block = false;
                }
                Event::Wake | Event::ScanExpired { .. } => {
                    if last_block_idx.is_some() {
                        wake_after_last_block = true;
                    }
                }
                Event::ConditionTrue => {
                    if last_block_idx.is_some() {
                        wake_after_last_block = true;
                    }
                }
            }

            let (next_state, _) = apply_event(state, *event);
            state = next_state;
        }

        // Invariant: if a Block was followed by a wake-class event, the task
        // must not still be blocked.
        if last_block_idx.is_some() && wake_after_last_block {
            prop_assert!(
                !state.is_blocked(),
                "lost wake: task still in {:?} after wake event", state
            );
        }
    }
}

// ── Property: idempotent wake ─────────────────────────────────────────────────

/// Two consecutive `Wake` events on a `Blocked*` state:
/// - First wake: CAS succeeds, state → Ready, enqueue.
/// - Second wake: CAS fails (state is Ready), no-op.
proptest! {
    #![proptest_config(ProptestConfig {
        cases: 10_000,
        ..ProptestConfig::default()
    })]

    #[test]
    fn prop_idempotent_wake(state_idx in 0usize..5) {
        // Only test Blocked* states (indices 2–6).
        let state = idx_to_blocked_state(state_idx);

        // First wake: must transition to Ready.
        let (state_after_first, fx1) = apply_event(state, Event::Wake);
        prop_assert_eq!(state_after_first, BlockState::Ready,
            "first wake from {:?} must go to Ready", state);
        prop_assert!(fx1.enqueue_to_run_queue,
            "first wake from {:?} must enqueue", state);

        // Second wake: must be a no-op (state is already Ready).
        let (state_after_second, fx2) = apply_event(state_after_first, Event::Wake);
        prop_assert_eq!(state_after_second, BlockState::Ready,
            "second wake must keep state Ready");
        prop_assert!(!fx2.enqueue_to_run_queue,
            "second wake must not re-enqueue");
        prop_assert!(!fx2.deadline_cleared,
            "second wake must not clear deadline");
    }
}

// ── Helper: index → BlockState ────────────────────────────────────────────────

fn idx_to_state(idx: usize) -> BlockState {
    match idx {
        0 => BlockState::Ready,
        1 => BlockState::Running,
        2 => BlockState::BlockedOnRecv,
        3 => BlockState::BlockedOnSend,
        4 => BlockState::BlockedOnReply,
        5 => BlockState::BlockedOnNotif,
        6 => BlockState::BlockedOnFutex,
        7 => BlockState::Dead,
        _ => unreachable!(),
    }
}

fn idx_to_blocked_state(idx: usize) -> BlockState {
    match idx {
        0 => BlockState::BlockedOnRecv,
        1 => BlockState::BlockedOnSend,
        2 => BlockState::BlockedOnReply,
        3 => BlockState::BlockedOnNotif,
        4 => BlockState::BlockedOnFutex,
        _ => unreachable!(),
    }
}
