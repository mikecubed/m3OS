//! TDD gate tests for Track F.1: IPC v2 block/wake migration.
//!
//! These tests verify the state-machine contracts that the IPC recv/send/notif
//! v2 block helpers must satisfy. They operate on the pure `sched_model`
//! (no kernel, no QEMU required) and exercise:
//!
//! 1. **Idempotent wake** — two consecutive wakes on a blocked task produce
//!    exactly one Ready transition; the second is a no-op.
//! 2. **Self-revert (ConditionTrue)** — if the condition is already true when
//!    the task reaches step 3 of the four-step recipe, it reverts to Running
//!    without yielding (no lost-wake window).
//! 3. **No lost wake** — a wake that races between step 1 (state write) and
//!    step 3 (recheck) is captured by ConditionTrue; the task does not remain
//!    Blocked*.
//! 4. **Block kind fidelity** — recv, send, and notif block kinds each produce
//!    the correct `Blocked*` variant (BlockedOnRecv, BlockedOnSend,
//!    BlockedOnNotif) on a Block event.
//!
//! These tests are the red-phase gate for the F.1 scheduler migration. All
//! assertions here pass against the existing sched_model; they specify the
//! contract that `block_current_on_recv_v2`, `block_current_on_send_v2`, and
//! `block_current_on_notif_v2` in `kernel/src/task/scheduler.rs` must uphold.
//!
//! Running:
//!   cargo test -p kernel-core -- ipc_v2_block_wake

use kernel_core::sched_model::{BlockKind, BlockState, Event, apply_event};

// ── Helper: simulate a full block→wake cycle ─────────────────────────────────

/// Simulate the four-step block/wake protocol for IPC:
///
/// 1. Block (Running → Blocked*), producing `yielded = true`.
/// 2. Wake (Blocked* → Ready), producing `enqueue_to_run_queue = true`.
/// 3. Dispatch (implicit: Ready → Running via scheduler, modeled as entering
///    Running state again).
///
/// Returns `(state_after_block, state_after_wake)`.
fn simulate_block_then_wake(kind: BlockKind) -> (BlockState, BlockState) {
    let (after_block, block_fx) = apply_event(
        BlockState::Running,
        Event::Block {
            kind,
            deadline: None,
        },
    );
    assert!(block_fx.yielded, "block must yield for kind {:?}", kind);
    assert!(
        after_block.is_blocked(),
        "block must produce a Blocked* state for kind {:?}",
        kind
    );

    let (after_wake, wake_fx) = apply_event(after_block, Event::Wake);
    assert_eq!(
        after_wake,
        BlockState::Ready,
        "wake must transition to Ready for kind {:?}",
        kind
    );
    assert!(
        wake_fx.enqueue_to_run_queue,
        "wake must enqueue to run queue for kind {:?}",
        kind
    );
    assert!(
        wake_fx.on_cpu_wait_required,
        "wake must require on_cpu spin-wait for kind {:?}",
        kind
    );
    assert!(
        wake_fx.deadline_cleared,
        "wake must clear deadline for kind {:?}",
        kind
    );

    (after_block, after_wake)
}

// ── Test 1: block kind fidelity ───────────────────────────────────────────────

/// Block(Recv) produces BlockedOnRecv (the state used by recv_msg / recv_msg_with_notif).
#[test]
fn test_recv_block_produces_blocked_on_recv() {
    let (after_block, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Recv,
            deadline: None,
        },
    );
    assert_eq!(
        after_block,
        BlockState::BlockedOnRecv,
        "recv block must produce BlockedOnRecv"
    );
}

/// Block(Send) produces BlockedOnSend (the state used by send / send_with_cap).
#[test]
fn test_send_block_produces_blocked_on_send() {
    let (after_block, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Send,
            deadline: None,
        },
    );
    assert_eq!(
        after_block,
        BlockState::BlockedOnSend,
        "send block must produce BlockedOnSend"
    );
}

/// Block(Notif) produces BlockedOnNotif (the state used by recv_msg_with_notif).
#[test]
fn test_notif_block_produces_blocked_on_notif() {
    let (after_block, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Notif,
            deadline: None,
        },
    );
    assert_eq!(
        after_block,
        BlockState::BlockedOnNotif,
        "notif block must produce BlockedOnNotif"
    );
}

// ── Test 2: idempotent wake ───────────────────────────────────────────────────

/// Two consecutive wakes on a BlockedOnRecv task: second is a no-op.
#[test]
fn test_recv_idempotent_wake() {
    let (after_block, _after_wake) = simulate_block_then_wake(BlockKind::Recv);

    // First wake: Blocked* → Ready (verified inside simulate_block_then_wake).
    let (first_after_wake, _) = apply_event(after_block, Event::Wake);
    assert_eq!(first_after_wake, BlockState::Ready);

    // Second wake: Ready → Ready (no-op; CAS fails because state is no longer Blocked*).
    let (second_after_wake, second_fx) = apply_event(first_after_wake, Event::Wake);
    assert_eq!(
        second_after_wake,
        BlockState::Ready,
        "second wake on Ready task must be a no-op"
    );
    assert!(
        !second_fx.enqueue_to_run_queue,
        "second wake must not re-enqueue"
    );
    assert!(
        !second_fx.on_cpu_wait_required,
        "second wake must not spin-wait"
    );
}

/// Two consecutive wakes on a BlockedOnSend task: second is a no-op.
#[test]
fn test_send_idempotent_wake() {
    let (after_block, _after_wake) = simulate_block_then_wake(BlockKind::Send);

    let (first_after_wake, _) = apply_event(after_block, Event::Wake);
    assert_eq!(first_after_wake, BlockState::Ready);

    let (second_after_wake, second_fx) = apply_event(first_after_wake, Event::Wake);
    assert_eq!(
        second_after_wake,
        BlockState::Ready,
        "second wake on Ready task (send) must be a no-op"
    );
    assert!(!second_fx.enqueue_to_run_queue);
}

/// Two consecutive wakes on a BlockedOnNotif task: second is a no-op.
#[test]
fn test_notif_idempotent_wake() {
    let (after_block, _after_wake) = simulate_block_then_wake(BlockKind::Notif);

    let (first_after_wake, _) = apply_event(after_block, Event::Wake);
    assert_eq!(first_after_wake, BlockState::Ready);

    let (second_after_wake, second_fx) = apply_event(first_after_wake, Event::Wake);
    assert_eq!(
        second_after_wake,
        BlockState::Ready,
        "second wake on Ready task (notif) must be a no-op"
    );
    assert!(!second_fx.enqueue_to_run_queue);
}

// ── Test 3: self-revert (ConditionTrue fast path) ────────────────────────────

/// If the message arrives between the state write and the yield (step 3
/// condition recheck), the task self-reverts to Running without yielding.
/// This is the ConditionTrue path in block_current_until.
#[test]
fn test_recv_self_revert_condition_true() {
    // Block: Running → BlockedOnRecv (state write, step 1).
    let (after_block, block_fx) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Recv,
            deadline: None,
        },
    );
    assert_eq!(after_block, BlockState::BlockedOnRecv);
    assert!(block_fx.yielded, "first block must yield (step 4 path)");

    // ConditionTrue (step 3 recheck): BlockedOnRecv → Running, no yield.
    let (after_revert, revert_fx) = apply_event(after_block, Event::ConditionTrue);
    assert_eq!(
        after_revert,
        BlockState::Running,
        "ConditionTrue must self-revert to Running"
    );
    assert!(
        !revert_fx.yielded,
        "ConditionTrue self-revert must NOT yield"
    );
    assert!(
        revert_fx.deadline_cleared,
        "ConditionTrue self-revert must clear deadline"
    );
    assert!(
        !revert_fx.enqueue_to_run_queue,
        "ConditionTrue self-revert must not enqueue to run queue"
    );
}

/// Same self-revert test for BlockedOnSend (send path).
#[test]
fn test_send_self_revert_condition_true() {
    let (after_block, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Send,
            deadline: None,
        },
    );
    assert_eq!(after_block, BlockState::BlockedOnSend);

    let (after_revert, revert_fx) = apply_event(after_block, Event::ConditionTrue);
    assert_eq!(after_revert, BlockState::Running);
    assert!(!revert_fx.yielded);
    assert!(revert_fx.deadline_cleared);
}

/// Same self-revert test for BlockedOnNotif (notif path).
#[test]
fn test_notif_self_revert_condition_true() {
    let (after_block, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Notif,
            deadline: None,
        },
    );
    assert_eq!(after_block, BlockState::BlockedOnNotif);

    let (after_revert, revert_fx) = apply_event(after_block, Event::ConditionTrue);
    assert_eq!(after_revert, BlockState::Running);
    assert!(!revert_fx.yielded);
    assert!(revert_fx.deadline_cleared);
}

// ── Test 4: no lost wake ──────────────────────────────────────────────────────

/// Sequence: Block(Recv) → Wake races in before ConditionTrue
/// (wake arrived between step 1 and step 3):
///
///  block side:  Running → BlockedOnRecv  (step 1: state write)
///  wake side:   BlockedOnRecv → Ready    (step 2: CAS succeeds)
///  block side:  (step 3: recheck — ConditionTrue already true, self-revert)
///               Ready → ??? — but the block side self-reverts from BlockedOnRecv,
///               not from Ready. The model captures the block-side view:
///               after step 1, ConditionTrue fires (Racing wake read flag = true).
///               The task does NOT yield; it sees Running.
///
/// The key: after the sequence (Block, ConditionTrue), the task is Running,
/// not in any Blocked* state. No lost wake.
#[test]
fn test_recv_no_lost_wake_racing_condition_true() {
    // Block (step 1 state write).
    let (blocked, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Recv,
            deadline: None,
        },
    );
    assert_eq!(blocked, BlockState::BlockedOnRecv);

    // ConditionTrue (racing wake set flag before step 3 recheck).
    let (reverted, _) = apply_event(blocked, Event::ConditionTrue);
    assert_eq!(
        reverted,
        BlockState::Running,
        "racing wake via ConditionTrue must leave task Running, not Blocked*"
    );
    assert!(
        !reverted.is_blocked(),
        "task must not remain in any Blocked* state"
    );
}

/// Sequence: Block(Send) → Wake (external wake on BlockedOnSend) → not blocked.
#[test]
fn test_send_no_lost_wake() {
    let (blocked, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Send,
            deadline: None,
        },
    );
    let (woken, _) = apply_event(blocked, Event::Wake);
    assert!(!woken.is_blocked(), "woken send task must not be Blocked*");
    assert_eq!(woken, BlockState::Ready);
}

/// Sequence: Block(Notif) → Wake → not blocked.
#[test]
fn test_notif_no_lost_wake() {
    let (blocked, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Notif,
            deadline: None,
        },
    );
    let (woken, _) = apply_event(blocked, Event::Wake);
    assert!(!woken.is_blocked(), "woken notif task must not be Blocked*");
    assert_eq!(woken, BlockState::Ready);
}

// ── Test 5: full IPC round-trip model ────────────────────────────────────────

/// Model a complete recv-block cycle: the receiver blocks, the sender delivers
/// and wakes, the receiver resumes.
///
/// recv_msg sequence (no sender queued initially):
///   1. Receiver: Running → BlockedOnRecv  (block_current_on_recv_v2)
///   2. Sender: BlockedOnRecv → Ready      (wake_task_v2 from send/reply)
///   3. Receiver dispatched: Ready → Running (scheduler dispatch)
///   4. take_message() succeeds.
///
/// This validates the happy-path IPC model that F.1's implementation must
/// uphold: after the wake, the task is no longer blocked.
#[test]
fn test_full_recv_round_trip() {
    // Step 1: Receiver blocks.
    let (recv_blocked, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Recv,
            deadline: None,
        },
    );
    assert_eq!(recv_blocked, BlockState::BlockedOnRecv);

    // Step 2: Sender wakes receiver (wake_task_v2).
    let (recv_ready, wake_fx) = apply_event(recv_blocked, Event::Wake);
    assert_eq!(recv_ready, BlockState::Ready);
    assert!(wake_fx.enqueue_to_run_queue);
    assert!(wake_fx.on_cpu_wait_required);

    // Step 3: Dispatcher picks receiver; it re-enters Running.
    // (Modeled as: Ready is the observable post-wake state; Running would be
    // the state after dispatch. The model stops at Ready — kernel dispatch
    // is outside the pure state machine.)
    assert!(
        !recv_ready.is_blocked(),
        "receiver must not be blocked after wake"
    );
}

/// Model a complete send-block cycle: sender blocks, receiver picks up and wakes.
///
/// send() sequence (no receiver queued initially):
///   1. Sender: Running → BlockedOnSend  (block_current_on_send_v2)
///   2. Receiver recv: BlockedOnSend → Ready (wake_task_v2 after deliver_message)
///   3. Sender dispatched: Ready → Running.
#[test]
fn test_full_send_round_trip() {
    // Step 1: Sender blocks.
    let (send_blocked, _) = apply_event(
        BlockState::Running,
        Event::Block {
            kind: BlockKind::Send,
            deadline: None,
        },
    );
    assert_eq!(send_blocked, BlockState::BlockedOnSend);

    // Step 2: Receiver wakes sender (wake_task_v2 from recv_msg).
    let (send_ready, wake_fx) = apply_event(send_blocked, Event::Wake);
    assert_eq!(send_ready, BlockState::Ready);
    assert!(wake_fx.enqueue_to_run_queue);

    assert!(!send_ready.is_blocked());
}
