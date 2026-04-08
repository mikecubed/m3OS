//! Property and unit tests for the service model in kernel-core.
//!
//! Covers: dependency graph validation, state transitions, restart policy,
//! and exit classification.

use kernel_core::service::*;

// ---------------------------------------------------------------------------
// Dependency graph tests (G.2)
// ---------------------------------------------------------------------------

#[test]
fn test_cycle_detected() {
    let mut g = DepGraphCore::new();
    let a = g.add_service(b"A").unwrap();
    let b = g.add_service(b"B").unwrap();
    let c = g.add_service(b"C").unwrap();

    // A → B → C → A (cycle)
    g.add_dependency(a, b"B").unwrap();
    g.add_dependency(b, b"C").unwrap();
    g.add_dependency(c, b"A").unwrap();

    let result = g.build();
    assert!(result.has_cycle, "cycle A→B→C→A must be detected");
    assert!(
        result.topo_order.is_empty(),
        "topo_order should be empty when cycle exists"
    );
}

#[test]
fn test_missing_dep_produces_warning() {
    let mut g = DepGraphCore::new();
    let a = g.add_service(b"A").unwrap();
    g.add_dependency(a, b"nonexistent").unwrap();

    let result = g.build();
    assert!(!result.has_cycle);
    assert_eq!(result.warnings.len(), 1);
    match &result.warnings[0] {
        DepWarning::UnresolvableDep { service, dep_name } => {
            assert_eq!(*service, a);
            assert_eq!(&dep_name[..11], b"nonexistent");
        }
    }
}

#[test]
fn test_valid_graph_topo_order() {
    let mut g = DepGraphCore::new();
    let syslogd = g.add_service(b"syslogd").unwrap();
    let crond = g.add_service(b"crond").unwrap();

    // crond depends on syslogd
    g.add_dependency(crond, b"syslogd").unwrap();

    let result = g.build();
    assert!(!result.has_cycle);
    assert!(result.warnings.is_empty());
    assert_eq!(result.topo_order.len(), 2);

    // syslogd must come before crond in topo order
    let syslogd_pos = result
        .topo_order
        .iter()
        .position(|&x| x == syslogd)
        .unwrap();
    let crond_pos = result.topo_order.iter().position(|&x| x == crond).unwrap();
    assert!(
        syslogd_pos < crond_pos,
        "syslogd (pos {}) must come before crond (pos {})",
        syslogd_pos,
        crond_pos
    );
}

#[test]
fn test_empty_graph() {
    let mut g = DepGraphCore::new();
    let result = g.build();
    assert!(!result.has_cycle);
    assert!(result.warnings.is_empty());
    assert!(result.topo_order.is_empty());
}

// ---------------------------------------------------------------------------
// State transition tests (G.2)
// ---------------------------------------------------------------------------

#[test]
fn test_permanently_stopped_rejects_start() {
    let state = ServiceState::PermanentlyStopped;
    let result = state.try_transition(ServiceState::Starting);
    assert!(
        result.is_err(),
        "PermanentlyStopped → Starting must be rejected"
    );
    let err = result.unwrap_err();
    assert_eq!(err.from, ServiceState::PermanentlyStopped);
    assert_eq!(err.to, ServiceState::Starting);
}

#[test]
fn test_stopped_to_starting_valid() {
    let state = ServiceState::Stopped { exit_code: 0 };
    let result = state.try_transition(ServiceState::Starting);
    assert!(result.is_ok(), "Stopped → Starting must succeed");
    assert_eq!(result.unwrap(), ServiceState::Starting);
}

#[test]
fn test_running_to_starting_invalid() {
    let state = ServiceState::Running;
    let result = state.try_transition(ServiceState::Starting);
    assert!(
        result.is_err(),
        "Running → Starting must be rejected (must stop first)"
    );
}

// ---------------------------------------------------------------------------
// Additional transition coverage
// ---------------------------------------------------------------------------

#[test]
fn test_never_started_to_starting_valid() {
    let state = ServiceState::NeverStarted;
    assert!(state.try_transition(ServiceState::Starting).is_ok());
}

#[test]
fn test_starting_to_running_valid() {
    let state = ServiceState::Starting;
    assert!(state.try_transition(ServiceState::Running).is_ok());
}

#[test]
fn test_running_to_stopping_valid() {
    let state = ServiceState::Running;
    assert!(state.try_transition(ServiceState::Stopping).is_ok());
}

#[test]
fn test_stopping_to_stopped_valid() {
    let state = ServiceState::Stopping;
    assert!(
        state
            .try_transition(ServiceState::Stopped { exit_code: 0 })
            .is_ok()
    );
}

// ---------------------------------------------------------------------------
// Restart policy tests
// ---------------------------------------------------------------------------

#[test]
fn test_should_restart_always() {
    assert!(should_restart(
        RestartPolicy::Always,
        &ExitClassification::CleanExit
    ));
    assert!(should_restart(
        RestartPolicy::Always,
        &ExitClassification::ErrorExit(1)
    ));
}

#[test]
fn test_should_restart_on_failure() {
    assert!(!should_restart(
        RestartPolicy::OnFailure,
        &ExitClassification::CleanExit
    ));
    assert!(should_restart(
        RestartPolicy::OnFailure,
        &ExitClassification::ErrorExit(1)
    ));
    assert!(should_restart(
        RestartPolicy::OnFailure,
        &ExitClassification::SignalDeath(9)
    ));
}

#[test]
fn test_should_restart_never() {
    assert!(!should_restart(
        RestartPolicy::Never,
        &ExitClassification::ErrorExit(1)
    ));
}

// ---------------------------------------------------------------------------
// Exit classification tests
// ---------------------------------------------------------------------------

#[test]
fn test_classify_clean_exit() {
    assert_eq!(classify_exit(0), ExitClassification::CleanExit);
}

#[test]
fn test_classify_error_exit() {
    // Exit code 1 in bits 8-15: 1 << 8 = 256
    assert_eq!(classify_exit(256), ExitClassification::ErrorExit(1));
}

#[test]
fn test_classify_signal_death() {
    // Bit 7 set + signal 9 in bits 0-6: 0x80 | 9 = 137
    assert_eq!(classify_exit(0x80 | 9), ExitClassification::SignalDeath(9));
}

// ---------------------------------------------------------------------------
// Restart delay tests
// ---------------------------------------------------------------------------

#[test]
fn test_restart_delay_backoff() {
    assert_eq!(restart_delay(0), 1);
    assert_eq!(restart_delay(1), 2);
    assert_eq!(restart_delay(2), 5);
    assert_eq!(restart_delay(100), 5); // capped
}
