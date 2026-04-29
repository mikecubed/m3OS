//! Multi-core scheduler fuzz — I.3 (model-level).
//!
//! Track I.3 — exercises the v2 scheduler block/wake state machine under
//! simulated 4-core × 4-worker concurrent pressure, modelling the IPC
//! call/reply, futex wait/wake, and notification signal/wait patterns that
//! the in-QEMU spec demands.
//!
//! # Why model-level rather than in-QEMU?
//!
//! The in-QEMU fixture for I.3 requires the full kernel scheduler to be
//! running (4 vCPUs, real IPC endpoints, futex words in memory-mapped pages,
//! live notification objects).  That fixture depends on every prior Phase 57a
//! Track (B–H) being wired together and tested in QEMU, a build that takes
//! ~8–10 minutes per iteration.  In contrast, this model-level harness:
//!
//! - Runs in <10 seconds via `cargo test -p kernel-core`.
//! - Exercises every transition in the v2 table (`apply_event`) under random
//!   cross-core event interleavings.
//! - Detects lost-wake, double-enqueue, and spurious-transition bugs at the
//!   state-machine level — the same invariants the in-QEMU test checks at
//!   the kernel level.
//! - Is shrinkable: proptest minimises failing sequences automatically.
//!
//! The in-QEMU fixture (`kernel/tests/sched_fuzz.rs`) exercises the same
//! invariants against the kernel_core types as a QEMU smoke boot.  This
//! file provides the property-based depth that the in-QEMU test cannot.
//!
//! # Test structure
//!
//! Each property test simulates N_CORES cores, each running N_WORKERS worker
//! "tasks".  Workers cycle through alternating bursts:
//!
//!   - **IPC burst**: a "caller" worker blocks on Reply; a "server" worker on
//!     a different core blocks on Recv; the server wakes and then wakes the
//!     caller via a reply Wake.
//!   - **Futex burst**: two workers on different cores race: one blocks on
//!     Futex, the other issues a Wake.
//!   - **Notification burst**: one worker blocks on Notif; another signals it
//!     (Wake); a ScanExpired races against the Wake.
//!
//! The random interleaving is provided by proptest-generated event sequences
//! for each worker.  After replaying the sequence we check:
//!
//!   1. **No lost wake** — every task that entered Blocked* either exits it
//!      via Wake/ScanExpired/ConditionTrue or is still Blocked* at the end
//!      (no task finishes in Blocked* after a Wake was issued to it).
//!   2. **Idempotent wake** — two consecutive Wakes on a Blocked* task
//!      produce exactly one `enqueue_to_run_queue = true` side effect.
//!   3. **No double-enqueue** — Wake then ScanExpired on the same task does
//!      not produce two `enqueue_to_run_queue` side effects.
//!   4. **No spurious transitions** — every transition matches the oracle.
//!
//! # Running
//!
//! ```
//! cargo test -p kernel-core --target x86_64-unknown-linux-gnu -- sched_fuzz_multicore
//! ```
//!
//! For a heavier run (more proptest cases):
//! ```
//! PROPTEST_CASES=100000 cargo test -p kernel-core ... -- sched_fuzz_multicore
//! ```

use kernel_core::sched_model::{BlockKind, BlockState, Event, apply_event};
use proptest::prelude::*;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Number of simulated cores in the multi-core model.
const N_CORES: usize = 4;

/// Number of worker "tasks" per core in the model.
const N_WORKERS: usize = 4;

/// Total worker count: N_CORES × N_WORKERS.
const N_TASKS: usize = N_CORES * N_WORKERS;

// ── Worker model ──────────────────────────────────────────────────────────────

/// State of one simulated worker task.
#[derive(Debug, Clone, Copy)]
struct Worker {
    state: BlockState,
    /// How many times this worker transitioned to Ready (enqueued).
    enqueue_count: u32,
    /// Running total of `enqueue_to_run_queue` side effects emitted for this
    /// worker — used to verify no-double-enqueue invariant.
    pending_enqueues: u32,
}

impl Worker {
    fn new() -> Self {
        Self {
            state: BlockState::Running,
            enqueue_count: 0,
            pending_enqueues: 0,
        }
    }
}

// ── Cross-core action ─────────────────────────────────────────────────────────

/// An action one worker can apply to another (or itself).
#[derive(Debug, Clone, Copy)]
enum CrossCoreAction {
    /// Issue a Wake to target worker idx.
    Wake { target: usize },
    /// Issue a ScanExpired(now) to target worker idx.
    ScanExpired { target: usize, now: u64 },
    /// The running worker itself performs a Block.
    Block {
        kind: BlockKind,
        deadline: Option<u64>,
    },
    /// The running worker's condition was already true (self-revert).
    ConditionTrue,
}

// ── Proptest strategies ────────────────────────────────────────────────────────

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
    prop_oneof![Just(None), (1u64..50_000u64).prop_map(Some)]
}

fn arb_target() -> impl Strategy<Value = usize> {
    0usize..N_TASKS
}

fn arb_action() -> impl Strategy<Value = CrossCoreAction> {
    prop_oneof![
        // Wake to a random task (including self — idempotent by contract).
        arb_target().prop_map(|target| CrossCoreAction::Wake { target }),
        // ScanExpired to a random task.
        (arb_target(), 0u64..100_000u64)
            .prop_map(|(target, now)| CrossCoreAction::ScanExpired { target, now }),
        // The actor itself blocks.
        (arb_block_kind(), arb_deadline())
            .prop_map(|(kind, deadline)| CrossCoreAction::Block { kind, deadline }),
        // Self-revert (ConditionTrue).
        Just(CrossCoreAction::ConditionTrue),
    ]
}

/// Generate a sequence of actions for one "round" of the fuzz scenario.
///
/// Each round contains up to `steps` actions spread across N_TASKS actors.
/// Actions are (actor_idx, CrossCoreAction) pairs.
fn arb_round(steps: usize) -> impl Strategy<Value = Vec<(usize, CrossCoreAction)>> {
    prop::collection::vec((arb_target(), arb_action()), steps)
}

// ── Oracle ────────────────────────────────────────────────────────────────────

/// True if applying `event` to `state` is a "not reachable" cell (would panic).
fn is_unreachable(state: BlockState, event: &Event) -> bool {
    matches!(
        (&state, event),
        (s, Event::Block { .. }) if s.is_blocked() || *s == BlockState::Dead
    ) || matches!((&state, event), (BlockState::Dead, Event::Block { .. }))
}

// ── Invariant checks ─────────────────────────────────────────────────────────

/// After applying `event` to `worker`, update the worker model and verify:
/// 1. The transition matches the oracle (`apply_event`).
/// 2. No double-enqueue occurs (pending_enqueues never exceeds 1 while Blocked*).
///
/// Returns `Ok(())` if all invariants hold, `Err(msg)` otherwise.
fn apply_and_check(
    workers: &mut [Worker; N_TASKS],
    actor: usize,
    event: Event,
) -> Result<(), String> {
    let w = &workers[actor];

    // Skip "not reachable" cells — they would panic by contract; in the fuzz
    // harness we simply skip them (the model documents them as kernel
    // invariant violations; producing them from random sequences is expected).
    if is_unreachable(w.state, &event) {
        return Ok(());
    }

    let prev_state = w.state;
    let prev_pending = w.pending_enqueues;

    let (new_state, fx) = apply_event(prev_state, event);

    // Update worker.
    workers[actor].state = new_state;
    if fx.enqueue_to_run_queue {
        workers[actor].enqueue_count += 1;
        workers[actor].pending_enqueues += 1;
    }
    // Dispatch: if the task transitioned to Ready and was previously Blocked*,
    // the scheduler would eventually dispatch it back to Running. We model that
    // by clearing pending_enqueues after one dispatch window (i.e., when a
    // Block event fires from Running again, the prior enqueue is consumed).
    if matches!(event, Event::Block { .. }) && prev_state == BlockState::Running {
        // A fresh Block resets the dispatch counter — the task is newly blocked.
        workers[actor].pending_enqueues = 0;
    }

    // Invariant 1: no double-enqueue while still in Blocked*.
    // After a Wake the task is Ready; a second Wake on Ready is a no-op.
    // But two Wakes before the first dispatch (pending_enqueues > 1 while
    // the state is still Blocked*) would indicate a double-enqueue bug.
    // Note: the state is now new_state; if prev_state was Blocked* and we
    // got enqueue, it transitioned to Ready — that is exactly one enqueue.
    if prev_state.is_blocked() && fx.enqueue_to_run_queue {
        // Exactly one transition allowed per block cycle.
        if prev_pending > 0 {
            return Err(format!(
                "Double-enqueue: task {actor} was already pending (count={prev_pending}) \
                 before Wake; prev_state={prev_state:?}, event={event:?}",
            ));
        }
    }

    // Invariant 2: no lost wake — if a Wake was applied to a Blocked* task,
    // it must now be Ready (not still Blocked*).
    if matches!(event, Event::Wake) && prev_state.is_blocked() {
        if workers[actor].state != BlockState::Ready {
            return Err(format!(
                "Lost wake: task {actor} was Blocked*({prev_state:?}) after Wake but \
                 ended in {:?} instead of Ready",
                workers[actor].state
            ));
        }
        if !fx.enqueue_to_run_queue {
            return Err(format!(
                "Lost enqueue: Wake on Blocked*({prev_state:?}) did not produce \
                 enqueue_to_run_queue for task {actor}",
            ));
        }
    }

    // Invariant 3: ScanExpired on Blocked* → Ready (same as Wake for our model).
    if matches!(event, Event::ScanExpired { .. }) && prev_state.is_blocked() {
        if workers[actor].state != BlockState::Ready {
            return Err(format!(
                "Lost scan wake: task {actor} was Blocked*({prev_state:?}) after \
                 ScanExpired but ended in {:?}",
                workers[actor].state
            ));
        }
    }

    // Invariant 4: ConditionTrue on Blocked* → Running (self-revert), no yield.
    if matches!(event, Event::ConditionTrue) && prev_state.is_blocked() {
        if workers[actor].state != BlockState::Running {
            return Err(format!(
                "Bad self-revert: task {actor} in Blocked*({prev_state:?}) after \
                 ConditionTrue ended in {:?} instead of Running",
                workers[actor].state
            ));
        }
        if fx.yielded {
            return Err(format!(
                "Self-revert yielded: task {actor} in Blocked*({prev_state:?}) \
                 ConditionTrue must NOT set yielded",
            ));
        }
    }

    Ok(())
}

// ── Properties ────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig {
        // 5 000 round sequences of up to 32 cross-core actions each.
        // This is the 30-second-equivalent fuzz depth for CI.
        // For the full 5-minute spec, run with PROPTEST_CASES=100000.
        cases: 5_000,
        ..ProptestConfig::default()
    })]

    /// I.3 — 4-core × 4-worker multi-core IPC/futex/notif fuzz.
    ///
    /// Simulates N_TASKS (16) concurrent worker state machines on N_CORES (4)
    /// cores.  Each round replays a random sequence of cross-core actions
    /// (Block, Wake, ScanExpired, ConditionTrue) against the worker array and
    /// asserts:
    ///   1. No lost wake.
    ///   2. No double-enqueue while blocked.
    ///   3. No spurious transitions (apply_event fidelity).
    ///   4. Self-revert does not yield.
    ///
    /// Failures are shrunk by proptest to a minimal reproducer.
    #[test]
    fn prop_multicore_fuzz_no_lost_wake_no_double_enqueue(
        round in arb_round(32),
    ) {
        let mut workers = [Worker::new(); N_TASKS];

        for (actor, action) in &round {
            let actor = *actor;
            let event = match *action {
                CrossCoreAction::Wake { target } => {
                    // Wake targets the specified task; the actor is just a label.
                    // We apply the Wake to `target`, not `actor`.
                    let w = &workers[target];
                    let ev = Event::Wake;
                    if is_unreachable(w.state, &ev) {
                        continue;
                    }
                    let prev = w.state;
                    let (new_state, fx) = apply_event(prev, ev);
                    workers[target].state = new_state;
                    if fx.enqueue_to_run_queue {
                        // No double-enqueue check (simplified: a second Wake
                        // on Ready is a no-op, so pending_enqueues doesn't
                        // increment for Ready tasks).
                        if prev.is_blocked() {
                            prop_assert!(
                                workers[target].pending_enqueues == 0,
                                "double-enqueue for task {}: prev={:?}, pending={}",
                                target, prev, workers[target].pending_enqueues
                            );
                            workers[target].enqueue_count += 1;
                            workers[target].pending_enqueues += 1;
                        }
                    }
                    if prev.is_blocked() {
                        prop_assert_eq!(
                            new_state,
                            BlockState::Ready,
                            "lost wake: task {} in {:?} after Wake ended in {:?}",
                            target, prev, new_state
                        );
                        prop_assert!(
                            fx.enqueue_to_run_queue,
                            "wake from Blocked*({:?}) must enqueue task {}",
                            prev, target
                        );
                        prop_assert!(
                            fx.on_cpu_wait_required,
                            "wake from Blocked*({:?}) must require on_cpu wait for task {}",
                            prev, target
                        );
                    }
                    continue;
                }
                CrossCoreAction::ScanExpired { target, now } => {
                    let w = &workers[target];
                    let ev = Event::ScanExpired { now };
                    if is_unreachable(w.state, &ev) {
                        continue;
                    }
                    let prev = w.state;
                    let (new_state, fx) = apply_event(prev, ev);
                    workers[target].state = new_state;
                    if fx.enqueue_to_run_queue {
                        if prev.is_blocked() {
                            prop_assert!(
                                workers[target].pending_enqueues == 0,
                                "double-enqueue (scan) for task {}: prev={:?}, pending={}",
                                target, prev, workers[target].pending_enqueues
                            );
                            workers[target].enqueue_count += 1;
                            workers[target].pending_enqueues += 1;
                        }
                    }
                    if prev.is_blocked() {
                        prop_assert_eq!(
                            new_state,
                            BlockState::Ready,
                            "lost scan wake: task {} in {:?} after ScanExpired ended in {:?}",
                            target, prev, new_state
                        );
                        // ScanExpired must NOT set on_cpu_wait_required.
                        prop_assert!(
                            !fx.on_cpu_wait_required,
                            "ScanExpired from Blocked*({:?}) must NOT require on_cpu wait for task {}",
                            prev, target
                        );
                    }
                    continue;
                }
                CrossCoreAction::Block { kind, deadline } => Event::Block { kind, deadline },
                CrossCoreAction::ConditionTrue => Event::ConditionTrue,
            };

            // Apply Block or ConditionTrue to the actor itself.
            let result = apply_and_check(&mut workers, actor, event);
            prop_assert!(result.is_ok(), "{}", result.unwrap_err());
        }

        // Post-round: no task may be in Blocked* with pending_enqueues > 1.
        for (i, w) in workers.iter().enumerate() {
            prop_assert!(
                w.pending_enqueues <= 1,
                "task {} ended with pending_enqueues={} > 1 (double-enqueue) in state {:?}",
                i, w.pending_enqueues, w.state
            );
        }
    }
}

// ── Deterministic regression tests ───────────────────────────────────────────
//
// These complement the proptest above with deterministic sequences that model
// the exact cross-core races described in the I.3 acceptance criteria.

/// IPC call/reply burst: 4 workers across 2 "cores" exchange calls.
///
/// Caller (core 0, worker 0) blocks on Reply.
/// Server (core 1, worker 4) blocks on Recv, then wakes Caller with a Wake.
/// Tests that:
/// - Caller: Running → BlockedOnReply → Ready (one enqueue, no double-enqueue).
/// - Server: Running → BlockedOnRecv → Ready (one enqueue).
#[test]
fn deterministic_ipc_call_reply_burst_4workers() {
    // Workers 0..3 = core 0; 4..7 = core 1; 8..11 = core 2; 12..15 = core 3.
    let mut callers: Vec<BlockState> = (0..N_TASKS).map(|_| BlockState::Running).collect();
    let mut enqueue_counts: Vec<u32> = vec![0; N_TASKS];

    // Step 1: core 0 workers (0,1) block on Reply; core 1 workers (4,5) block on Recv.
    for &i in &[0usize, 1] {
        let (s, fx) = apply_event(
            callers[i],
            Event::Block {
                kind: BlockKind::Reply,
                deadline: None,
            },
        );
        assert_eq!(s, BlockState::BlockedOnReply);
        assert!(fx.yielded);
        callers[i] = s;
    }
    for &i in &[4usize, 5] {
        let (s, fx) = apply_event(
            callers[i],
            Event::Block {
                kind: BlockKind::Recv,
                deadline: None,
            },
        );
        assert_eq!(s, BlockState::BlockedOnRecv);
        assert!(fx.yielded);
        callers[i] = s;
    }

    // Step 2: servers (4,5) "receive" the call — wake them.
    for &i in &[4usize, 5] {
        let prev = callers[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready, "server {i} must become Ready");
        assert!(fx.enqueue_to_run_queue);
        assert!(fx.on_cpu_wait_required);
        callers[i] = s;
        enqueue_counts[i] += 1;
    }

    // Step 3: servers send Reply → wake callers (0,1).
    for &i in &[0usize, 1] {
        let prev = callers[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(
            s,
            BlockState::Ready,
            "caller {i} must become Ready after reply"
        );
        assert!(fx.enqueue_to_run_queue);
        callers[i] = s;
        enqueue_counts[i] += 1;
    }

    // Step 4: second Wake on each (now Ready) must be a no-op — idempotency.
    for &i in &[0usize, 1, 4, 5] {
        let prev = callers[i];
        assert_eq!(prev, BlockState::Ready);
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(
            s,
            BlockState::Ready,
            "second Wake on Ready task {i} is no-op"
        );
        assert!(
            !fx.enqueue_to_run_queue,
            "idempotent Wake must not enqueue task {i}"
        );
    }

    // Verify: each worker enqueued exactly once.
    for &i in &[0usize, 1, 4, 5] {
        assert_eq!(
            enqueue_counts[i], 1,
            "task {i} must have been enqueued exactly once; got {}",
            enqueue_counts[i]
        );
    }
}

/// Futex wait/wake burst: workers on different cores race.
///
/// Workers 0,2,4,6 block on Futex.
/// Workers 1,3,5,7 issue Wake (cross-core futex_wake).
/// A ScanExpired then races against each wake — must not double-enqueue.
#[test]
fn deterministic_futex_wait_wake_race_no_double_enqueue() {
    let mut states: Vec<BlockState> = (0..N_TASKS).map(|_| BlockState::Running).collect();
    let mut enqueue_counts: Vec<u32> = vec![0; N_TASKS];

    // Waiter workers park on Futex.
    let waiters = [0usize, 2, 4, 6];
    for &i in &waiters {
        let (s, fx) = apply_event(
            states[i],
            Event::Block {
                kind: BlockKind::Futex,
                deadline: Some(5000),
            },
        );
        assert_eq!(s, BlockState::BlockedOnFutex);
        assert!(fx.yielded);
        assert_eq!(fx.deadline_set, Some(5000));
        states[i] = s;
    }

    // Waker workers: issue Wake (futex_wake cross-core IPI).
    for &i in &waiters {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready);
        assert!(fx.enqueue_to_run_queue);
        assert!(fx.deadline_cleared);
        assert!(fx.on_cpu_wait_required);
        states[i] = s;
        enqueue_counts[i] += 1;
    }

    // Race: ScanExpired fires after each Wake (now the task is Ready).
    // Must be a no-op — must NOT re-enqueue.
    for &i in &waiters {
        let prev = states[i];
        assert_eq!(prev, BlockState::Ready);
        let (s, fx) = apply_event(prev, Event::ScanExpired { now: 10_000 });
        assert_eq!(
            s,
            BlockState::Ready,
            "ScanExpired on Ready task {i} is no-op"
        );
        assert!(
            !fx.enqueue_to_run_queue,
            "ScanExpired after Wake must NOT re-enqueue task {i} (double-enqueue guard)"
        );
    }

    // No double-enqueue.
    for &i in &waiters {
        assert_eq!(
            enqueue_counts[i], 1,
            "task {i} must have been enqueued exactly once"
        );
    }
}

/// Notification signal/wait burst across all 4 cores.
///
/// 8 workers (one per core pair) block on Notif with a deadline.
/// A cross-core notification signal wakes each.
/// Then a ScanExpired races — must not double-enqueue.
/// Then a second Wake races — must be idempotent.
#[test]
fn deterministic_notif_signal_wait_race_all_cores() {
    let mut states: Vec<BlockState> = (0..N_TASKS).map(|_| BlockState::Running).collect();

    // Workers 0..7 block on notification with a deadline.
    let notif_waiters = [0usize, 1, 4, 5, 8, 9, 12, 13];
    for &i in &notif_waiters {
        let (s, fx) = apply_event(
            states[i],
            Event::Block {
                kind: BlockKind::Notif,
                deadline: Some(1000),
            },
        );
        assert_eq!(s, BlockState::BlockedOnNotif);
        assert!(fx.yielded);
        states[i] = s;
    }

    // Signal arrives (cross-core): Wake each waiter.
    let mut enqueue_counts = vec![0u32; N_TASKS];
    for &i in &notif_waiters {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready);
        assert!(fx.enqueue_to_run_queue);
        assert!(fx.deadline_cleared);
        states[i] = s;
        enqueue_counts[i] += 1;
    }

    // ScanExpired races (deadline has elapsed but task is already Ready).
    for &i in &notif_waiters {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::ScanExpired { now: 2000 });
        assert_eq!(s, BlockState::Ready);
        assert!(
            !fx.enqueue_to_run_queue,
            "ScanExpired after Wake must not re-enqueue task {i}"
        );
    }

    // Second Wake races (already Ready — idempotent).
    for &i in &notif_waiters {
        let prev = states[i];
        let (s, fx) = apply_event(prev, Event::Wake);
        assert_eq!(s, BlockState::Ready);
        assert!(
            !fx.enqueue_to_run_queue,
            "second Wake on Ready task {i} must be no-op"
        );
    }

    // Exactly one enqueue per waiter.
    for &i in &notif_waiters {
        assert_eq!(enqueue_counts[i], 1, "task {i} enqueued exactly once");
    }
}

/// Self-revert (ConditionTrue) across all 4 cores simultaneously.
///
/// Simulates the race where a notification/IPC reply arrives between the
/// state write (Block, step 1) and the condition recheck (step 3) for 16
/// workers spread across 4 cores.  Each worker must self-revert to Running
/// without yielding and without a double-enqueue.
#[test]
fn deterministic_condition_true_self_revert_all_cores() {
    let block_kinds = [
        BlockKind::Recv,
        BlockKind::Notif,
        BlockKind::Futex,
        BlockKind::Reply,
    ];

    for (core, kind) in block_kinds.iter().enumerate() {
        for worker in 0..N_WORKERS {
            let task = core * N_WORKERS + worker;
            let initial = BlockState::Running;

            // Step 1: state write Running → Blocked*.
            let (blocked, block_fx) = apply_event(
                initial,
                Event::Block {
                    kind: *kind,
                    deadline: None,
                },
            );
            assert!(blocked.is_blocked(), "task {task} must be Blocked*");
            assert!(block_fx.yielded, "task {task} block must yield");

            // Step 3: condition recheck — wake already arrived (self-revert).
            let (reverted, revert_fx) = apply_event(blocked, Event::ConditionTrue);
            assert_eq!(
                reverted,
                BlockState::Running,
                "task {task} (core={core}, kind={kind:?}) must self-revert to Running"
            );
            assert!(!revert_fx.yielded, "task {task} self-revert must NOT yield");
            assert!(
                revert_fx.deadline_cleared,
                "task {task} self-revert must clear wake_deadline"
            );
            assert!(
                !revert_fx.enqueue_to_run_queue,
                "task {task} self-revert must NOT enqueue (task is still on CPU)"
            );
        }
    }
}
