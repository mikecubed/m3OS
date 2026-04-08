//! Service model: host-testable pure logic for dependency graphs,
//! state transitions, restart policy, and exit classification.
//!
//! This module extracts the algorithmic core of the init service manager
//! so it can be tested on the host without syscalls or I/O.

use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAX_SERVICES: usize = 16;
pub const MAX_DEPS: usize = 4;
pub const MAX_NAME: usize = 32;

// ---------------------------------------------------------------------------
// ServiceState — pure data type mirroring init's ServiceStatus
// ---------------------------------------------------------------------------

/// Service lifecycle state.
///
/// Valid transitions:
/// ```text
///   NeverStarted ──→ Starting
///   Starting     ──→ Running
///   Starting     ──→ Stopped (exec failed)
///   Running      ──→ Stopping
///   Running      ──→ Stopped (unexpected exit)
///   Stopping     ──→ Stopped
///   Stopped      ──→ Starting (restart)
///   Stopped      ──→ PermanentlyStopped (max restarts exceeded)
///   *            ──→ PermanentlyStopped (unresolvable deps, etc.)
///   PermanentlyStopped ──→ (nothing — terminal state)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    NeverStarted,
    Starting,
    Running,
    Stopping,
    Stopped { exit_code: i32 },
    PermanentlyStopped,
}

/// Error returned when a state transition is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: ServiceState,
    pub to: ServiceState,
}

impl ServiceState {
    /// Attempt a state transition. Returns the new state on success,
    /// or an `InvalidTransition` error if the transition is not allowed.
    pub fn try_transition(self, target: ServiceState) -> Result<ServiceState, InvalidTransition> {
        let valid = match (self, target) {
            // Terminal state — cannot leave PermanentlyStopped.
            (ServiceState::PermanentlyStopped, _) => false,

            // NeverStarted can only go to Starting or PermanentlyStopped.
            (ServiceState::NeverStarted, ServiceState::Starting) => true,
            (ServiceState::NeverStarted, ServiceState::PermanentlyStopped) => true,
            (ServiceState::NeverStarted, _) => false,

            // Starting can go to Running, Stopped (exec failure), or PermanentlyStopped.
            (ServiceState::Starting, ServiceState::Running) => true,
            (ServiceState::Starting, ServiceState::Stopped { .. }) => true,
            (ServiceState::Starting, ServiceState::PermanentlyStopped) => true,
            (ServiceState::Starting, _) => false,

            // Running can go to Stopping, Stopped (unexpected exit), or PermanentlyStopped.
            (ServiceState::Running, ServiceState::Stopping) => true,
            (ServiceState::Running, ServiceState::Stopped { .. }) => true,
            (ServiceState::Running, ServiceState::PermanentlyStopped) => true,
            (ServiceState::Running, _) => false,

            // Stopping can go to Stopped or PermanentlyStopped.
            (ServiceState::Stopping, ServiceState::Stopped { .. }) => true,
            (ServiceState::Stopping, ServiceState::PermanentlyStopped) => true,
            (ServiceState::Stopping, _) => false,

            // Stopped can go to Starting (restart) or PermanentlyStopped.
            (ServiceState::Stopped { .. }, ServiceState::Starting) => true,
            (ServiceState::Stopped { .. }, ServiceState::PermanentlyStopped) => true,
            (ServiceState::Stopped { .. }, _) => false,
        };

        if valid {
            Ok(target)
        } else {
            Err(InvalidTransition {
                from: self,
                to: target,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// RestartPolicy + ExitClassification
// ---------------------------------------------------------------------------

/// Restart policy for a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    Always,
    OnFailure,
    Never,
}

/// Classified exit status of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClassification {
    /// Process exited normally with code 0.
    CleanExit,
    /// Process exited with a nonzero code.
    ErrorExit(i32),
    /// Process was killed by a signal.
    SignalDeath(i32),
}

/// Classify a raw wait status into an `ExitClassification`.
///
/// Convention (matching m3OS wait status layout):
/// - Bits 8-15: exit code (if not signaled)
/// - Bit 7: signal flag (WIFSIGNALED equivalent)
/// - Bits 0-6: signal number (if signaled)
pub fn classify_exit(raw_status: i32) -> ExitClassification {
    if raw_status & 0x80 != 0 {
        // Signal death — lower 7 bits are signal number.
        let sig = raw_status & 0x7f;
        ExitClassification::SignalDeath(sig)
    } else {
        let code = (raw_status >> 8) & 0xff;
        if code == 0 {
            ExitClassification::CleanExit
        } else {
            ExitClassification::ErrorExit(code)
        }
    }
}

/// Determine whether a service should be restarted given its policy and exit.
pub fn should_restart(policy: RestartPolicy, exit: &ExitClassification) -> bool {
    match policy {
        RestartPolicy::Always => true,
        RestartPolicy::OnFailure => !matches!(exit, ExitClassification::CleanExit),
        RestartPolicy::Never => false,
    }
}

/// Compute restart delay in seconds with exponential backoff, capped at 5s.
///
/// - 0 previous restarts → 1s
/// - 1 previous restart  → 2s
/// - 2+ previous restarts → 5s
pub fn restart_delay(consecutive_restarts: u32) -> u32 {
    match consecutive_restarts {
        0 => 1,
        1 => 2,
        _ => 5,
    }
}

// ---------------------------------------------------------------------------
// DepGraphCore — host-testable dependency graph
// ---------------------------------------------------------------------------

/// Warning produced during dependency graph build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepWarning {
    /// A service lists a dependency that does not exist.
    UnresolvableDep {
        service: usize,
        dep_name: [u8; MAX_NAME],
    },
}

/// Result of building the dependency graph.
#[derive(Debug)]
pub struct BuildResult {
    pub warnings: Vec<DepWarning>,
    pub has_cycle: bool,
    pub topo_order: Vec<usize>,
}

/// A generic, host-testable dependency graph for services.
pub struct DepGraphCore {
    /// Service names (fixed-size byte arrays).
    names: [[u8; MAX_NAME]; MAX_SERVICES],
    name_lens: [usize; MAX_SERVICES],
    /// Number of registered services.
    count: usize,
    /// Dependency edges: for each service, names of its dependencies.
    dep_names: [[[u8; MAX_NAME]; MAX_DEPS]; MAX_SERVICES],
    dep_name_lens: [[usize; MAX_DEPS]; MAX_SERVICES],
    dep_counts: [usize; MAX_SERVICES],
    /// Resolved forward edges (populated by build).
    edges: [[usize; MAX_DEPS]; MAX_SERVICES],
    edge_counts: [usize; MAX_SERVICES],
}

impl Default for DepGraphCore {
    fn default() -> Self {
        Self::new()
    }
}

impl DepGraphCore {
    /// Create a new empty dependency graph.
    pub fn new() -> Self {
        Self {
            names: [[0u8; MAX_NAME]; MAX_SERVICES],
            name_lens: [0; MAX_SERVICES],
            count: 0,
            dep_names: [[[0u8; MAX_NAME]; MAX_DEPS]; MAX_SERVICES],
            dep_name_lens: [[0; MAX_DEPS]; MAX_SERVICES],
            dep_counts: [0; MAX_SERVICES],
            edges: [[0; MAX_DEPS]; MAX_SERVICES],
            edge_counts: [0; MAX_SERVICES],
        }
    }

    /// Add a service by name. Returns its index, or `None` if full.
    pub fn add_service(&mut self, name: &[u8]) -> Option<usize> {
        if self.count >= MAX_SERVICES {
            return None;
        }
        let idx = self.count;
        let copy_len = name.len().min(MAX_NAME);
        self.names[idx][..copy_len].copy_from_slice(&name[..copy_len]);
        self.name_lens[idx] = copy_len;
        self.count += 1;
        Some(idx)
    }

    /// Add a dependency to a service by dep name.
    pub fn add_dependency(&mut self, service_idx: usize, dep_name: &[u8]) -> Result<(), DepError> {
        if service_idx >= self.count {
            return Err(DepError::InvalidServiceIndex);
        }
        let dc = self.dep_counts[service_idx];
        if dc >= MAX_DEPS {
            return Err(DepError::TooManyDeps);
        }
        let copy_len = dep_name.len().min(MAX_NAME);
        self.dep_names[service_idx][dc][..copy_len].copy_from_slice(&dep_name[..copy_len]);
        self.dep_name_lens[service_idx][dc] = copy_len;
        self.dep_counts[service_idx] += 1;
        Ok(())
    }

    /// Build the graph: resolve dep names to indices, detect cycles,
    /// produce topological order.
    pub fn build(&mut self) -> BuildResult {
        let mut warnings = Vec::new();

        // Resolve dep names to indices.
        for i in 0..self.count {
            self.edge_counts[i] = 0;
            for d in 0..self.dep_counts[i] {
                let dep_len = self.dep_name_lens[i][d];
                let dep_name = &self.dep_names[i][d][..dep_len];

                let mut found = false;
                for j in 0..self.count {
                    if j == i {
                        continue;
                    }
                    let name_len = self.name_lens[j];
                    if dep_len == name_len && dep_name == &self.names[j][..name_len] {
                        if self.edge_counts[i] < MAX_DEPS {
                            self.edges[i][self.edge_counts[i]] = j;
                            self.edge_counts[i] += 1;
                        }
                        found = true;
                        break;
                    }
                }

                if !found {
                    let mut arr = [0u8; MAX_NAME];
                    arr[..dep_len].copy_from_slice(dep_name);
                    warnings.push(DepWarning::UnresolvableDep {
                        service: i,
                        dep_name: arr,
                    });
                }
            }
        }

        // Cycle detection via DFS.
        let has_cycle = self.detect_cycle();

        // Topological sort (only meaningful if no cycle).
        let topo_order = if has_cycle {
            Vec::new()
        } else {
            self.topo_sort()
        };

        BuildResult {
            warnings,
            has_cycle,
            topo_order,
        }
    }

    /// Return the name of a service by index.
    pub fn service_name(&self, idx: usize) -> &[u8] {
        &self.names[idx][..self.name_lens[idx]]
    }

    /// Return the number of registered services.
    pub fn count(&self) -> usize {
        self.count
    }

    // -- internal helpers --

    fn detect_cycle(&self) -> bool {
        // 0=unvisited, 1=in-progress, 2=done
        let mut state = [0u8; MAX_SERVICES];
        for i in 0..self.count {
            if state[i] == 0 && self.dfs_cycle(i, &mut state) {
                return true;
            }
        }
        false
    }

    fn dfs_cycle(&self, node: usize, state: &mut [u8; MAX_SERVICES]) -> bool {
        state[node] = 1;
        for d in 0..self.edge_counts[node] {
            let dep = self.edges[node][d];
            if state[dep] == 1 {
                return true;
            }
            if state[dep] == 0 && self.dfs_cycle(dep, state) {
                return true;
            }
        }
        state[node] = 2;
        false
    }

    fn topo_sort(&self) -> Vec<usize> {
        let mut order = Vec::new();
        let mut visited = [false; MAX_SERVICES];
        for i in 0..self.count {
            if !visited[i] {
                self.topo_visit(i, &mut visited, &mut order);
            }
        }
        order
    }

    fn topo_visit(&self, node: usize, visited: &mut [bool; MAX_SERVICES], order: &mut Vec<usize>) {
        if visited[node] {
            return;
        }
        visited[node] = true;
        // Visit dependencies first.
        for d in 0..self.edge_counts[node] {
            let dep = self.edges[node][d];
            self.topo_visit(dep, visited, order);
        }
        order.push(node);
    }
}

/// Errors from `add_dependency`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepError {
    InvalidServiceIndex,
    TooManyDeps,
}
