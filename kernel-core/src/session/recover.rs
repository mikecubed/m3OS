//! Phase 57 Track F.4 — Per-step recovery policy state machine.
//!
//! `session_manager` (F.2) drives a fixed graphical-session boot sequence
//! and the F.1 [`crate::session::StartupSequence`] handles the *intra-step*
//! retry loop (each step gets up to N attempts before the sequencer
//! rolls back). F.4 extends the supervision contract by tracking
//! retry counts *across* the step's lifetime so a daemon-level recovery
//! decision can be taken: when a step has consumed its budget, the
//! `session_manager` daemon must escalate to `text-fallback` mode (stop
//! every started service in reverse order, release the framebuffer back
//! to the kernel console, and surface a serial admin shell).
//!
//! [`Recovery`] is the **policy** — it counts per-step failures and
//! reports the next action. The **executor** that actually performs the
//! rollback (calling `SupervisorBackend::stop` on each service in
//! reverse order, invoking `framebuffer_release`, etc.) lives in
//! `userspace/session_manager/src/main.rs`. Splitting the two means
//! the policy is host-testable as pure logic without booting QEMU,
//! matching the engineering-discipline rule:
//!
//! > Pure logic belongs in `kernel-core`. Hardware and IPC wiring
//! > belongs in `kernel/` or `userspace/`. Tasks that straddle the
//! > boundary split their code along it so the pure part is
//! > host-testable.
//!
//! ## Resource bounds
//!
//! `Recovery` stores at most [`MAX_TRACKED_STEPS`] = 8 distinct
//! per-step counters. The current declared session has 5 steps
//! (`display_server`, `kbd_server`, `mouse_server`, `audio_server`,
//! `term`); the extra slack tolerates a future step without redesign.
//! When a 9th distinct step name is seen, [`Recovery::on_step_failure`]
//! returns [`RecoveryAction::EscalateToTextFallback`] — failing closed,
//! never silently dropping the count.
//!
//! No allocation in steady state — fixed-size array of `(name, count)`
//! pairs.

/// Maximum number of distinct step names tracked simultaneously.
///
/// Sized to comfortably exceed [`crate::session_supervisor::DECLARED_SESSION_STEP_NAMES`]
/// (currently 5) so a future step addition does not require a recompile
/// of every `Recovery` consumer. When the table is full and a new name
/// arrives, [`Recovery::on_step_failure`] fails closed by returning
/// [`RecoveryAction::EscalateToTextFallback`] — the policy never
/// silently drops a count.
pub const MAX_TRACKED_STEPS: usize = 8;

/// One entry of the per-step counter table.
///
/// `name` is `Option<&'static str>` so the empty / cleared entry is
/// representable without a separate length field. `Recovery::reset`
/// sets every entry to `(None, 0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StepEntry {
    name: Option<&'static str>,
    count: u32,
}

impl StepEntry {
    const fn empty() -> Self {
        Self {
            name: None,
            count: 0,
        }
    }
}

/// What the executor should do next after a step's `start()` failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry the step. `attempt` is 1-based: the first failure produces
    /// `attempt = 1`, meaning "you have used 1 attempt". Mirrors the
    /// shape used by [`crate::session::SessionState::Recovering`] so
    /// log lines and state transitions can use the same numbering.
    Retry { attempt: u32 },
    /// The step has exhausted the per-step retry budget. The executor
    /// must initiate the text-fallback rollback (stop every started
    /// service in reverse order, release the framebuffer, surface an
    /// admin shell on the serial console). Once any step's failure
    /// reports this action, all subsequent failures continue to report
    /// it — the recovery state is terminal until [`Recovery::reset`]
    /// is called.
    EscalateToTextFallback,
}

/// Per-step retry counter. The state machine is allocation-free: it
/// stores at most [`MAX_TRACKED_STEPS`] (`name`, `count`) pairs in a
/// fixed-size array.
///
/// The cap is captured at construction so test backends can probe edge
/// values (`Recovery::new(1)`, `Recovery::new(0)`) without altering the
/// global [`crate::session::MAX_RETRIES_PER_STEP`] constant.
pub struct Recovery {
    entries: [StepEntry; MAX_TRACKED_STEPS],
    max_retries: u32,
}

impl Recovery {
    /// Construct a recovery state machine with the supplied per-step
    /// retry cap. The cap is the total number of attempts a step may
    /// consume before [`Recovery::on_step_failure`] returns
    /// [`RecoveryAction::EscalateToTextFallback`]. A cap of 0 escalates
    /// on the very first failure; a cap of 1 retries 0 times before
    /// escalating; etc.
    pub const fn new(max_retries: u32) -> Self {
        Self {
            entries: [StepEntry::empty(); MAX_TRACKED_STEPS],
            max_retries,
        }
    }

    /// Record a `start()` failure for `step_name` and return the next
    /// action. The counter is post-incremented: the first call for a
    /// step records 1 and reports `Retry { attempt: 1 }` (still under
    /// the cap of 3 by default); the third call reports
    /// `EscalateToTextFallback`. Subsequent calls keep reporting the
    /// escalation — the state is sticky until [`Recovery::reset`].
    ///
    /// Returns [`RecoveryAction::EscalateToTextFallback`] when the table
    /// is full and `step_name` is not already tracked: failing closed
    /// is the conservative behavior under the resource-bound rule.
    pub fn on_step_failure(&mut self, step_name: &'static str) -> RecoveryAction {
        let idx = match self.find_or_allocate(step_name) {
            Some(i) => i,
            None => {
                // Table is full and the name is new — escalate rather
                // than silently dropping the count. The 8-slot table
                // exceeds the declared 5-step session by 60% so this
                // path is unreachable in production; covering it with
                // a sane default keeps the contract total.
                return RecoveryAction::EscalateToTextFallback;
            }
        };
        // Increment with saturating math so a pathological caller that
        // somehow drives the counter past `u32::MAX` does not wrap.
        self.entries[idx].count = self.entries[idx].count.saturating_add(1);
        let used = self.entries[idx].count;
        if used >= self.max_retries {
            RecoveryAction::EscalateToTextFallback
        } else {
            RecoveryAction::Retry { attempt: used }
        }
    }

    /// Clear every per-step counter so the recovery looks fresh again.
    /// Used by `session_manager`'s control verbs (F.5
    /// `session-restart`) to drop the failure history before
    /// re-driving the boot sequence.
    pub fn reset(&mut self) {
        self.entries = [StepEntry::empty(); MAX_TRACKED_STEPS];
    }

    /// Return the index of the entry for `name`, allocating an empty
    /// slot if the name is not yet tracked. Returns `None` if every
    /// slot is occupied by a different name (table-full case).
    fn find_or_allocate(&mut self, name: &'static str) -> Option<usize> {
        // Existing entry with the same name takes priority.
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.name == Some(name) {
                return Some(i);
            }
        }
        // First empty slot.
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.name.is_none() {
                self.entries[i].name = Some(name);
                return Some(i);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Inline tests — the integration test suite at
// `kernel-core/tests/phase57_f4_recovery.rs` covers the full contract;
// these unit tests guard the table-full edge case that the integration
// tests cannot directly observe (since they exercise only the public
// API).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_full_escalates_for_new_name() {
        // Fill every slot with a distinct name.
        let mut rec = Recovery::new(100);
        let names: [&'static str; MAX_TRACKED_STEPS] =
            ["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7"];
        for name in names {
            // Using a high cap so each step stays in `Retry` for now.
            assert_eq!(
                rec.on_step_failure(name),
                RecoveryAction::Retry { attempt: 1 }
            );
        }
        // A 9th distinct name has nowhere to land — fail closed.
        let action = rec.on_step_failure("overflow");
        assert_eq!(action, RecoveryAction::EscalateToTextFallback);
    }

    #[test]
    fn count_saturates_rather_than_wrapping() {
        // Construct a recovery, hand-fill the entry to (u32::MAX - 1),
        // and verify one more failure saturates rather than wrapping.
        let mut rec = Recovery::new(u32::MAX);
        rec.entries[0].name = Some("display_server");
        rec.entries[0].count = u32::MAX - 1;
        let _ = rec.on_step_failure("display_server");
        assert_eq!(rec.entries[0].count, u32::MAX);
        let _ = rec.on_step_failure("display_server");
        // saturating_add prevents wrap.
        assert_eq!(rec.entries[0].count, u32::MAX);
    }
}
