//! Phase 57 Track F.4 — Recovery state-machine pure-logic tests.
//!
//! These tests pin the contract `session_manager`'s `recover.rs`
//! consumer must implement to satisfy the F.4 acceptance:
//!
//! > On a step's `start` failure: the recovery state machine retries up
//! > to the documented per-service cap (3 by default per the
//! > resource-bounds rule); exceeding the cap escalates to
//! > `text-fallback`.
//!
//! The state machine is a **policy** struct: it counts attempts per
//! step name and reports either `Retry { attempt }` (still under the cap)
//! or `EscalateToTextFallback` (cap reached). The actual rollback
//! execution (stopping services in reverse order, releasing the
//! framebuffer, surfacing a serial admin shell) is the **executor's**
//! responsibility and lives in `userspace/session_manager/src/main.rs`.
//! Splitting them keeps the policy host-testable without booting QEMU.
//!
//! Per Phase 57 task list F.4:
//!
//! - Failing tests commit first against a fake-supervisor double.
//! - The cap is per-service, not per-session: each step's failure
//!   counter is independent. A previous step that succeeded does not
//!   consume the next step's retry budget.
//! - The cap defaults to [`MAX_RETRIES_PER_STEP`] = 3 (per the
//!   resource-bounds rule) but `Recovery::new` accepts an override so
//!   tests can probe edge values without changing the global.

use kernel_core::session::recover::{Recovery, RecoveryAction};
use kernel_core::session::MAX_RETRIES_PER_STEP;

// ---------------------------------------------------------------------------
// New recovery: every step has the full retry budget.
// ---------------------------------------------------------------------------

#[test]
fn first_failure_returns_retry_attempt_one() {
    // First failure on a fresh recovery should ask for retry attempt 1
    // (one attempt has been consumed). The state machine encodes
    // attempts as 1-based: callers see "this is your Nth retry of the
    // step", which matches the F.1 `Recovering { retry_count }` shape.
    let mut rec = Recovery::new(MAX_RETRIES_PER_STEP);
    let action = rec.on_step_failure("display_server");
    assert_eq!(action, RecoveryAction::Retry { attempt: 1 });
}

#[test]
fn second_failure_returns_retry_attempt_two() {
    let mut rec = Recovery::new(MAX_RETRIES_PER_STEP);
    let _ = rec.on_step_failure("display_server");
    let action = rec.on_step_failure("display_server");
    assert_eq!(action, RecoveryAction::Retry { attempt: 2 });
}

#[test]
fn third_failure_at_cap_escalates_to_text_fallback() {
    // With cap=3, the third failure means we have used the budget — the
    // state machine returns `EscalateToTextFallback`. The third attempt
    // is the last one that may be retried; the failure that closes it
    // is the escalation trigger.
    let mut rec = Recovery::new(MAX_RETRIES_PER_STEP);
    let _ = rec.on_step_failure("display_server");
    let _ = rec.on_step_failure("display_server");
    let action = rec.on_step_failure("display_server");
    assert_eq!(action, RecoveryAction::EscalateToTextFallback);
}

#[test]
fn fourth_failure_remains_at_text_fallback() {
    // After escalation, further failures are a no-op: the executor has
    // already taken over and there is no recovery from text-fallback.
    let mut rec = Recovery::new(MAX_RETRIES_PER_STEP);
    let _ = rec.on_step_failure("display_server");
    let _ = rec.on_step_failure("display_server");
    let _ = rec.on_step_failure("display_server");
    let action = rec.on_step_failure("display_server");
    assert_eq!(action, RecoveryAction::EscalateToTextFallback);
}

// ---------------------------------------------------------------------------
// Per-step independence — failures on one step don't affect another.
// ---------------------------------------------------------------------------

#[test]
fn each_step_has_independent_retry_budget() {
    // Both display_server and audio_server fail twice each; neither
    // hits the cap of 3, so both still have retry attempts available.
    let mut rec = Recovery::new(MAX_RETRIES_PER_STEP);
    let _ = rec.on_step_failure("display_server");
    let _ = rec.on_step_failure("audio_server");
    let _ = rec.on_step_failure("display_server");
    let _ = rec.on_step_failure("audio_server");
    let action_d = rec.on_step_failure("display_server");
    assert_eq!(
        action_d,
        RecoveryAction::EscalateToTextFallback,
        "display_server's third failure escalates"
    );
    // audio_server has only seen 2 failures so a third triggers
    // escalation independently — the budget is per-step, so audio
    // benefits from its own count even though display has already
    // escalated.
    let action_a = rec.on_step_failure("audio_server");
    assert_eq!(
        action_a,
        RecoveryAction::EscalateToTextFallback,
        "audio_server escalates on its own third failure"
    );
}

// ---------------------------------------------------------------------------
// Reset clears every per-step counter.
// ---------------------------------------------------------------------------

#[test]
fn reset_restores_full_retry_budget() {
    let mut rec = Recovery::new(MAX_RETRIES_PER_STEP);
    let _ = rec.on_step_failure("display_server");
    let _ = rec.on_step_failure("display_server");
    rec.reset();
    let action = rec.on_step_failure("display_server");
    assert_eq!(
        action,
        RecoveryAction::Retry { attempt: 1 },
        "after reset, the first failure looks fresh again"
    );
}

#[test]
fn reset_clears_all_steps_not_just_one() {
    let mut rec = Recovery::new(MAX_RETRIES_PER_STEP);
    let _ = rec.on_step_failure("display_server");
    let _ = rec.on_step_failure("kbd_server");
    let _ = rec.on_step_failure("audio_server");
    rec.reset();
    assert_eq!(
        rec.on_step_failure("display_server"),
        RecoveryAction::Retry { attempt: 1 }
    );
    assert_eq!(
        rec.on_step_failure("kbd_server"),
        RecoveryAction::Retry { attempt: 1 }
    );
    assert_eq!(
        rec.on_step_failure("audio_server"),
        RecoveryAction::Retry { attempt: 1 }
    );
}

// ---------------------------------------------------------------------------
// Configurable cap — `Recovery::new(1)` escalates immediately.
// ---------------------------------------------------------------------------

#[test]
fn cap_one_escalates_on_first_failure() {
    // When the per-step cap is 1, the first failure exhausts the budget.
    let mut rec = Recovery::new(1);
    let action = rec.on_step_failure("display_server");
    assert_eq!(action, RecoveryAction::EscalateToTextFallback);
}

#[test]
fn cap_zero_escalates_immediately() {
    // Pathological cap of 0 means no retries are permitted; any
    // failure escalates. The state machine accepts this without panicking
    // — the policy reports the executor's correct action.
    let mut rec = Recovery::new(0);
    let action = rec.on_step_failure("display_server");
    assert_eq!(action, RecoveryAction::EscalateToTextFallback);
}

// ---------------------------------------------------------------------------
// Documented per-service cap is 3 — sanity check against the constant.
// ---------------------------------------------------------------------------

#[test]
fn default_cap_matches_documented_three() {
    // Pin the relationship: the F.4 task list says "the documented
    // per-service cap (3 by default per the resource-bounds rule)".
    // The cap lives in `kernel_core::session::MAX_RETRIES_PER_STEP`.
    assert_eq!(MAX_RETRIES_PER_STEP, 3);
}
