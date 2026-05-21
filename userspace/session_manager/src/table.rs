//! Phase 64 Track A — per-service PID and state table.
//!
//! `ServiceTable` is the single source of truth for `session_manager`'s
//! lifecycle decisions. Every spawned child's PID is recorded here on
//! creation, every state transition is written here, and every external
//! query (`m3ctl session-state`, the text-fallback motion in
//! `recover.rs`) reads from here.  Nothing else may infer service state
//! from external signals such as IPC latency.
//!
//! ## `ServiceState` vs `kernel_core::session::SessionState`
//!
//! [`ServiceState`] is a **per-child** state describing one supervised
//! process — `Starting`, `Running`, `Stopping`, `Restarting`, `Failed`.
//! It is orthogonal to the session-wide
//! [`kernel_core::session::SessionState`] (`Booting`, `Running`,
//! `Recovering`, `TextFallback`) defined in
//! `kernel-core/src/session/startup.rs`, which describes the graphical
//! session as a whole. Both types coexist; implementers must not
//! collapse them. A `display_server` in `ServiceState::Failed` does
//! not by itself imply the session is in `SessionState::TextFallback`
//! — only the supervisor's budget-exhaustion path makes that escalation.
//!
//! ## Allocation
//!
//! The table uses a small heap-backed `Vec<ServiceEntry>` indexed by
//! service name. The set of supervised services is fixed at boot (the
//! five graphical-session services declared by
//! [`kernel_core::session_supervisor::declared_session_step_names`]),
//! so the vector never grows beyond five entries after the initial
//! [`ServiceTable::insert`] calls.

use alloc::string::String;
use alloc::vec::Vec;

/// Per-child supervised-service state.
///
/// Distinct from [`kernel_core::session::SessionState`] (session-wide).
/// See the module-level doc comment for the orthogonality contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// `start()` has been issued and the child has been spawned, but
    /// the readiness probe has not yet succeeded.
    Starting,
    /// The child is registered and serving its IPC contract.
    Running,
    /// `stop()` has been issued; SIGTERM has been sent and the grace
    /// period is in flight. May escalate to SIGKILL before reaching
    /// the reaped state (which transitions the entry out of `Stopping`).
    Stopping,
    /// `restart()` is in flight — the entry has been stopped and is in
    /// the process of being started again. Distinct from `Starting`
    /// because the restart-budget counter has already been bumped.
    Restarting,
    /// The restart budget is exhausted or a non-recoverable error
    /// terminated the service. No further automatic restart will occur.
    Failed,
}

/// One supervised service's PID, lifecycle state, and restart counters.
///
/// `restart_count` increments on each full `restart()` attempt;
/// `step_failures` increments on each individual `stop()` or `start()`
/// step failure inside one restart attempt. Both counters together
/// define the Phase 64 budget contract (see `lifecycle.rs`).
#[derive(Debug, Clone)]
pub struct ServiceEntry {
    /// Service name as declared by
    /// [`kernel_core::session_supervisor::declared_session_step_names`].
    pub name: String,
    /// PID of the currently supervised child, or `None` when no child
    /// is running for this service (between a successful stop and the
    /// next start, or after `Failed`).
    pub pid: Option<Pid>,
    /// Current lifecycle state.
    pub state: ServiceState,
    /// Number of full `restart()` attempts performed since boot.
    pub restart_count: u32,
    /// Number of individual `stop()` / `start()` step failures inside
    /// the current restart attempt. Reset by [`ServiceTable::clear_step_failures`]
    /// on a successful restart and read by the budget check in
    /// `lifecycle::restart_service`.
    pub step_failures: u32,
}

/// Process identifier as returned by `fork()` and consumed by
/// `kill()` / `waitpid()`. Kept as a typed wrapper rather than a bare
/// `i32` so a future signed/unsigned audit cannot accidentally swap
/// the PID for a signal number or exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pid(pub i32);

/// Map from service name → [`ServiceEntry`].
///
/// Single-threaded; access discipline is enforced by `session_manager`'s
/// event-loop ownership (the daemon is single-threaded by design).
///
/// The table is constructed empty at daemon startup and populated by
/// one [`Self::insert`] call per declared service before the first
/// `start()` is issued.
#[derive(Debug, Default)]
pub struct ServiceTable {
    entries: Vec<ServiceEntry>,
}

impl ServiceTable {
    /// An empty table. Callers populate it via [`Self::insert`].
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Insert a new service entry in `Starting` state with no PID and
    /// zero counters. Idempotent on duplicate names — a second insert
    /// for the same name is a no-op (returns `false`); the first insert
    /// returns `true`.
    pub fn insert(&mut self, name: &str) -> bool {
        if self.entries.iter().any(|e| e.name == name) {
            return false;
        }
        self.entries.push(ServiceEntry {
            name: String::from(name),
            pid: None,
            state: ServiceState::Starting,
            restart_count: 0,
            step_failures: 0,
        });
        true
    }

    /// Record the PID of the just-spawned child for `name`. Returns
    /// `true` if the entry exists; `false` if `name` was not previously
    /// inserted (callers treat that as a programming error and log it).
    pub fn update_pid(&mut self, name: &str, pid: Option<Pid>) -> bool {
        match self.entries.iter_mut().find(|e| e.name == name) {
            Some(entry) => {
                entry.pid = pid;
                true
            }
            None => false,
        }
    }

    /// Transition the named entry to a new lifecycle state. Returns
    /// `true` on success; `false` if the entry does not exist.
    ///
    /// The table does not enforce a state-transition graph — the
    /// caller (`lifecycle.rs`) owns that policy. Keeping the table
    /// transition-agnostic means the state machine can be tested in
    /// `lifecycle.rs` without weaving in a separate invariant in the
    /// table itself (SRP).
    pub fn update_state(&mut self, name: &str, state: ServiceState) -> bool {
        match self.entries.iter_mut().find(|e| e.name == name) {
            Some(entry) => {
                entry.state = state;
                true
            }
            None => false,
        }
    }

    /// Get the PID currently associated with `name`, or `None` if the
    /// entry has no live child or the name was never inserted.
    pub fn get_pid(&self, name: &str) -> Option<Pid> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| e.pid)
    }

    /// Get the lifecycle state of `name`, or `None` if the entry
    /// doesn't exist.
    pub fn get_state(&self, name: &str) -> Option<ServiceState> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.state)
    }

    /// Borrow the entry for `name` immutably. Used by the control
    /// socket to read `restart_count` + `step_failures` alongside the
    /// state.
    pub fn get(&self, name: &str) -> Option<&ServiceEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Iterate over all entries in insertion order. Used by the
    /// `session-state` payload encoder and the reverse-order
    /// text-fallback motion.
    pub fn iter(&self) -> impl Iterator<Item = &ServiceEntry> {
        self.entries.iter()
    }

    /// Increment the full-restart counter for `name`. Returns the new
    /// `restart_count`, or `None` if the entry does not exist.
    pub fn bump_restart_count(&mut self, name: &str) -> Option<u32> {
        let entry = self.entries.iter_mut().find(|e| e.name == name)?;
        entry.restart_count = entry.restart_count.saturating_add(1);
        Some(entry.restart_count)
    }

    /// Increment the in-restart step-failure counter for `name`.
    /// Returns the new `step_failures`, or `None` if the entry does
    /// not exist.
    pub fn bump_step_failures(&mut self, name: &str) -> Option<u32> {
        let entry = self.entries.iter_mut().find(|e| e.name == name)?;
        entry.step_failures = entry.step_failures.saturating_add(1);
        Some(entry.step_failures)
    }

    /// Reset the in-restart step-failure counter on a successful
    /// restart attempt. The full-restart `restart_count` is preserved
    /// — that is a steady-state budget per the Phase 64 design doc.
    pub fn clear_step_failures(&mut self, name: &str) -> bool {
        match self.entries.iter_mut().find(|e| e.name == name) {
            Some(entry) => {
                entry.step_failures = 0;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn fresh() -> ServiceTable {
        let mut t = ServiceTable::new();
        assert!(t.insert("display_server"));
        t
    }

    #[test]
    fn insert_starts_in_starting_state_with_no_pid() {
        let t = fresh();
        let e = t.get("display_server").expect("entry exists");
        assert_eq!(e.state, ServiceState::Starting);
        assert_eq!(e.pid, None);
        assert_eq!(e.restart_count, 0);
        assert_eq!(e.step_failures, 0);
    }

    #[test]
    fn duplicate_insert_is_noop() {
        let mut t = fresh();
        assert!(!t.insert("display_server"));
        let count = t.iter().filter(|e| e.name == "display_server").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn update_pid_and_get_pid_round_trip() {
        let mut t = fresh();
        assert!(t.update_pid("display_server", Some(Pid(42))));
        assert_eq!(t.get_pid("display_server"), Some(Pid(42)));
        assert!(t.update_pid("display_server", None));
        assert_eq!(t.get_pid("display_server"), None);
    }

    #[test]
    fn update_unknown_returns_false() {
        let mut t = fresh();
        assert!(!t.update_pid("nonexistent", Some(Pid(1))));
        assert!(!t.update_state("nonexistent", ServiceState::Failed));
        assert_eq!(t.get_pid("nonexistent"), None);
        assert_eq!(t.get_state("nonexistent"), None);
    }

    /// Full Phase 64 happy-path transition: a service starts, becomes
    /// running, is stopped, and the entry is left without a PID.
    #[test]
    fn happy_path_state_transitions() {
        let mut t = fresh();
        t.update_pid("display_server", Some(Pid(42)));
        assert!(t.update_state("display_server", ServiceState::Running));
        assert_eq!(t.get_state("display_server"), Some(ServiceState::Running));
        assert!(t.update_state("display_server", ServiceState::Stopping));
        assert_eq!(t.get_state("display_server"), Some(ServiceState::Stopping));
        // Reaped: PID cleared, state goes back to Starting before the
        // next start, or to Failed if the budget is exhausted. The table
        // doesn't enforce the choice; the caller does.
        t.update_pid("display_server", None);
        assert_eq!(t.get_pid("display_server"), None);
    }

    /// Restart transition: `Running` → `Restarting` → `Starting` after
    /// the restart budget bumps. The table records the counts; the
    /// caller decides when to escalate to `Failed`.
    #[test]
    fn restart_transition_bumps_counters() {
        let mut t = fresh();
        t.update_state("display_server", ServiceState::Running);
        assert_eq!(t.bump_restart_count("display_server"), Some(1));
        t.update_state("display_server", ServiceState::Restarting);
        assert_eq!(t.bump_step_failures("display_server"), Some(1));
        assert_eq!(t.bump_step_failures("display_server"), Some(2));
        let e = t.get("display_server").unwrap();
        assert_eq!(e.restart_count, 1);
        assert_eq!(e.step_failures, 2);
        // On the next successful restart, step_failures clears but
        // restart_count is preserved.
        assert!(t.clear_step_failures("display_server"));
        let e = t.get("display_server").unwrap();
        assert_eq!(e.restart_count, 1);
        assert_eq!(e.step_failures, 0);
    }

    /// Failed → no further automatic transitions, but the table still
    /// permits an explicit `update_state` for the case where the
    /// operator re-enables the service via `m3ctl session-restart`.
    /// The transition-graph policy lives in `lifecycle.rs`; the table
    /// only records the recorded state.
    #[test]
    fn failed_state_can_be_revisited_explicitly() {
        let mut t = fresh();
        assert!(t.update_state("display_server", ServiceState::Failed));
        assert_eq!(t.get_state("display_server"), Some(ServiceState::Failed));
        assert!(t.update_state("display_server", ServiceState::Starting));
        assert_eq!(t.get_state("display_server"), Some(ServiceState::Starting));
    }

    #[test]
    fn iter_preserves_insertion_order() {
        let mut t = ServiceTable::new();
        t.insert("display_server");
        t.insert("kbd_server");
        t.insert("mouse_server");
        t.insert("audio_server");
        t.insert("greeter");
        let names: Vec<_> = t.iter().map(|e| e.name.clone()).collect();
        assert_eq!(
            names,
            [
                "display_server".to_string(),
                "kbd_server".to_string(),
                "mouse_server".to_string(),
                "audio_server".to_string(),
                "greeter".to_string(),
            ]
        );
    }
}
