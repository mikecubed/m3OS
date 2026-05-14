//! Phase 68 Track D.2 + E.1 — Service supervisor primitives.
//!
//! [`ServiceState`] is the per-service lifecycle enum exposed to the
//! supervisor; [`start_services_ordered`] computes the
//! dependency-ordered start sequence the production init loop should
//! follow. [`handle_budget_exhaustion`] is the typed-action dispatcher
//! for the `on-restart=` directive added in Phase 68 Track E.1.
//!
//! Distinct from `kernel_core::session::startup::SessionState` /
//! `userspace/session_manager/src/table.rs::ServiceState`: init owns
//! boot ordering and restart budget; `session_manager` owns the
//! graphical session.

extern crate alloc;

use alloc::vec::Vec;

use super::manifest::{OnRestartAction, ServiceManifest};

/// Per-service lifecycle state tracked by [`start_services_ordered`].
///
/// `Pending` → `Starting` → `Running` is the happy path. Failure of
/// the exec call moves the slot directly to `Failed`; the supervisor
/// then decides whether to restart (subject to `max_restart`) or
/// escalate via [`handle_budget_exhaustion`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ServiceState {
    /// Manifest known; not yet started (dependencies unfulfilled).
    #[default]
    Pending,
    /// Exec issued; awaiting a "ready" signal (or simply the service
    /// to start drawing on its endpoint).
    Starting,
    /// Service is up and considered healthy.
    Running,
    /// Exec failed or the service exited unexpectedly and the
    /// restart budget is exhausted.
    Failed,
}

/// Result of one pass over the manifest list. Carries the
/// dependency-ordered indices to start *now* (every dependency in
/// `Running`) and the list of indices that remain blocked (some
/// dependency is not `Running` yet — try again next pass).
#[derive(Clone, Debug, Default)]
pub struct StartPlan {
    /// Indices into the manifest slice that are ready to start this
    /// pass. Ordered: every entry's dependencies are already
    /// `Running`.
    pub ready_to_start: Vec<usize>,
    /// Indices still blocked on at least one dependency.
    pub blocked: Vec<usize>,
}

/// Phase 68 Track D.2 — compute which services are ready to start in
/// the current pass.
///
/// Given parallel slices `manifests` and `states`, return a
/// [`StartPlan`]: every index whose state is `Pending` and whose
/// dependencies (looked up by name in `manifests`) are all `Running`
/// goes into `ready_to_start`; everything else still
/// `Pending`/`Starting` ends up in `blocked`.
///
/// The supervisor calls this in its main loop: invoke
/// `start_services_ordered`, exec everything in `ready_to_start`
/// (transitioning their state to `Starting` and then `Running`
/// according to the supervisor's readiness probe), then call the
/// function again. The loop terminates when `ready_to_start` is empty
/// and `blocked` is empty (everything started) or when `blocked`
/// stabilises and `ready_to_start` is empty (dependency cycle or
/// dead service blocking forward progress).
pub fn start_services_ordered(manifests: &[ServiceManifest], states: &[ServiceState]) -> StartPlan {
    assert_eq!(
        manifests.len(),
        states.len(),
        "start_services_ordered: parallel slices must have equal length",
    );
    let mut plan = StartPlan::default();
    for (i, manifest) in manifests.iter().enumerate() {
        match states[i] {
            ServiceState::Pending => {
                if deps_satisfied(manifest, manifests, states) {
                    plan.ready_to_start.push(i);
                } else {
                    plan.blocked.push(i);
                }
            }
            ServiceState::Starting => {
                // Still coming up; nothing to do this pass.
                plan.blocked.push(i);
            }
            ServiceState::Running | ServiceState::Failed => {
                // No work — either healthy or terminally bad.
            }
        }
    }
    plan
}

fn deps_satisfied(
    manifest: &ServiceManifest,
    manifests: &[ServiceManifest],
    states: &[ServiceState],
) -> bool {
    for dep_name in &manifest.depends {
        let idx = match manifests.iter().position(|m| &m.name == dep_name) {
            Some(i) => i,
            None => {
                // Unresolvable — `detect_cycles` should have flagged
                // this manifest; conservative behaviour is "do not
                // start" so the supervisor never starts a service
                // whose dependency cannot be satisfied.
                return false;
            }
        };
        if states[idx] != ServiceState::Running {
            return false;
        }
    }
    true
}

/// Output of [`handle_budget_exhaustion`]. The caller (the production
/// init binary) executes the chosen action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BudgetExhaustionOutcome {
    /// Log an ERROR and leave the service `Failed`.
    LogAndContinue,
    /// Send the typed text-fallback verb to `session_manager`.
    EscalateTextFallback,
    /// Treat as a fatal init-stage failure.
    Panic,
}

/// Phase 68 Track E.1 — given the manifest's `on_restart` field, pick
/// the [`BudgetExhaustionOutcome`] the supervisor should execute. The
/// helper is pure logic (returns a value rather than performing the
/// side effect) so the supervisor decides *where* the text-fallback
/// IPC happens.
pub fn handle_budget_exhaustion(action: OnRestartAction) -> BudgetExhaustionOutcome {
    match action {
        OnRestartAction::LogAndContinue => BudgetExhaustionOutcome::LogAndContinue,
        OnRestartAction::TextFallback => BudgetExhaustionOutcome::EscalateTextFallback,
        OnRestartAction::Panic => BudgetExhaustionOutcome::Panic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn manifest(name: &str, deps: &[&str]) -> ServiceManifest {
        let mut m = ServiceManifest::empty();
        m.name = name.to_string();
        m.command = "/x".to_string();
        m.depends = deps.iter().map(|s| s.to_string()).collect();
        m
    }

    #[test]
    fn service_with_no_deps_starts_immediately() {
        let ms = [manifest("a", &[])];
        let states = [ServiceState::Pending];
        let plan = start_services_ordered(&ms, &states);
        assert_eq!(plan.ready_to_start, [0]);
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn service_waits_until_dependency_is_running() {
        let ms = [
            manifest("kbd_server", &[]),
            manifest("mouse_server", &["kbd_server"]),
        ];
        let states = [ServiceState::Pending, ServiceState::Pending];
        let plan = start_services_ordered(&ms, &states);
        assert_eq!(plan.ready_to_start, [0]);
        assert_eq!(plan.blocked, [1]);
    }

    // Phase 68 Track D.3 acceptance — `mouse_server` does not start
    // until `kbd_server` is `Running`. Mirrors the manifest_depends
    // test the spec calls out.
    #[test]
    fn mouse_server_starts_after_kbd_server_is_running() {
        let ms = [
            manifest("kbd_server", &[]),
            manifest("mouse_server", &["kbd_server"]),
        ];
        // First pass: only kbd_server is ready.
        let mut states = [ServiceState::Pending, ServiceState::Pending];
        let plan = start_services_ordered(&ms, &states);
        assert_eq!(plan.ready_to_start, [0]);

        // Simulate exec → starting.
        states[0] = ServiceState::Starting;
        let plan = start_services_ordered(&ms, &states);
        assert!(plan.ready_to_start.is_empty());
        assert_eq!(plan.blocked, vec![0, 1]);

        // Service is now running. Mouse should be ready.
        states[0] = ServiceState::Running;
        let plan = start_services_ordered(&ms, &states);
        assert_eq!(plan.ready_to_start, [1]);
    }

    #[test]
    fn unresolvable_dependency_blocks_indefinitely() {
        let ms = [manifest("a", &["missing"])];
        let states = [ServiceState::Pending];
        let plan = start_services_ordered(&ms, &states);
        assert!(plan.ready_to_start.is_empty());
        assert_eq!(plan.blocked, [0]);
    }

    #[test]
    fn failed_services_are_excluded() {
        let ms = [manifest("a", &[])];
        let states = [ServiceState::Failed];
        let plan = start_services_ordered(&ms, &states);
        assert!(plan.ready_to_start.is_empty());
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn handle_budget_exhaustion_log_and_continue() {
        assert_eq!(
            handle_budget_exhaustion(OnRestartAction::LogAndContinue),
            BudgetExhaustionOutcome::LogAndContinue
        );
    }

    #[test]
    fn handle_budget_exhaustion_text_fallback() {
        assert_eq!(
            handle_budget_exhaustion(OnRestartAction::TextFallback),
            BudgetExhaustionOutcome::EscalateTextFallback
        );
    }

    #[test]
    fn handle_budget_exhaustion_panic() {
        assert_eq!(
            handle_budget_exhaustion(OnRestartAction::Panic),
            BudgetExhaustionOutcome::Panic
        );
    }
}
