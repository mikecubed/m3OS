# Phase 64 - Session Manager Lifecycle

**Status:** Planned
**Source Ref:** phase-64
**Depends on:** Phase 57 (Audio and Local Session) ✅, Phase 19 (Signal Handling) ✅, Phase 52 (First Service Extractions) ✅
**Builds on:** Replaces the Phase 57 `session_manager` stub lifecycle methods (unconditional `Ack`) with real child-PID tracking, SIGTERM/SIGKILL delivery, SIGCHLD observation via `sys_waitpid`, and restart-budget enforcement
**Primary Components:** userspace/session_manager, kernel signal path, sys_waitpid, m3ctl

## Milestone Goal

`session_manager` tracks the PIDs of every supervised child, delivers SIGTERM with a grace period followed by SIGKILL, observes termination via `sys_waitpid`, enforces restart budgets, and actually drops display-server children when the typed `text-fallback` recovery contract fires. `m3ctl session-state` reports the real service state from the per-service map, not a value derived from IPC round-trip latency.

## Why This Phase Exists

Phase 57 declared its `text-fallback` recovery contract as a milestone deliverable, but the underlying `stop()`, `restart()`, and state-reporting mechanisms return `Ack` unconditionally. A `stop()` that replies immediately without sending a signal cannot form the foundation of a recovery contract. If the display server crashes, the recovery path that is supposed to drop its children and fall back to text mode instead logs a message and pretends it succeeded.

This phase exists to make `session_manager`'s supervisory behavior real, not simulated.

## Learning Goals

- Understand how a supervisor tracks child processes via PID tables and SIGCHLD.
- Learn the two-phase stop protocol (SIGTERM → grace → SIGKILL) used by real init systems.
- See how restart budgets prevent infinite restart loops from masking persistent failures.
- Understand the difference between reporting derived state (latency-based inference) and authentic state (per-service map).

## Feature Scope

### Child PID tracking

`session_manager` records the PID of every child it spawns in a per-service `ServiceEntry` struct. PIDs are updated on spawn and cleared on confirmed exit. The map is authoritative; no other mechanism may infer service state.

### `stop()` — SIGTERM with grace period then SIGKILL

`stop()` sends SIGTERM to the child PID, sets a 5-second timer, and then calls `sys_waitpid` in a loop. If the child has not exited by the timer expiry, `stop()` sends SIGKILL and waits again. The call does not reply `Ack` until `sys_waitpid` confirms the exit.

### `restart()` — chained stop + start with restart-budget enforcement

`restart()` calls `stop()`, then `start()`. Each service has a restart budget of 3 retries per step (stop failure, start failure) and 3 steady-state restarts. Exceeding either budget transitions the service to `Failed` state and triggers the `text-fallback` recovery path if the failed service is display-critical.

### Text-fallback motion actually drops display-server children

When the `text-fallback` recovery path fires, `session_manager` calls `stop()` on each display-server child in reverse startup order (`term` → `audio_server` → `mouse_server` → `kbd_server` → `display_server`). Only after all stops confirm does it emit the text-fallback IPC notification. Before this phase this path was logging-only.

### `m3ctl session-state` reports authentic state

The `session-state` subcommand queries `session_manager` for the per-service `ServiceState` enum value (Starting, Running, Stopping, Restarting, Failed). Before this phase the state was derived from IPC response latency.

## Important Components and How They Work

### `userspace/session_manager/src/table.rs` (new)

A `ServiceTable` mapping service names to `ServiceEntry { pid: Option<Pid>, state: ServiceState, restart_count: u32 }`. Updated atomically within the single-threaded `session_manager` event loop. The table is the single source of truth for `m3ctl session-state`.

### `userspace/session_manager/src/lifecycle.rs` (new)

Contains `stop_service`, `start_service`, and `restart_service`. `stop_service` implements the SIGTERM → grace → SIGKILL protocol using `sys_kill` and `sys_waitpid`. `restart_service` calls `stop_service` then `start_service` and enforces budget limits.

### `userspace/session_manager/src/recover.rs`

Extended to perform actual stop calls in reverse order. Before this phase: emitted a structured log event and returned `Ok`. After this phase: iterates the service startup order in reverse, calls `stop_service` for each, and returns `Err` if any stop fails within budget.

### `m3ctl session-state`

Queries the `session_manager` control socket and prints the `ServiceState` for each tracked service. After this phase: state is the actual `ServiceState` value from the `ServiceTable`, not a derived label.

## How This Builds on Earlier Phases

- Uses Phase 19 signal delivery (`sys_kill`, `SIGTERM`, `SIGKILL`) without modification.
- Uses `sys_waitpid` established in Phase 19 / Phase 27 for synchronous child reaping.
- Extends Phase 57's `session_manager` service layout — `boot.rs` startup ordering and the `text-fallback` recovery hook are unchanged; `recover.rs` and the control socket are extended.
- Aligns with Phase 52's service model: restart policy format (`restart=on-failure max_restart=3`) is preserved; the enforcement that was absent is now implemented.

## Implementation Outline

1. Define `ServiceEntry`, `ServiceState`, and `ServiceTable` in a new `table.rs` module.
2. Wire `ServiceTable` into the `session_manager` event loop so every spawn records a PID.
3. Implement `stop_service` in `lifecycle.rs` with SIGTERM, 5-second grace, SIGKILL, and `sys_waitpid`.
4. Implement `restart_service` with budget counters; transition to `Failed` on budget exhaustion.
5. Extend `recover.rs` to call `stop_service` for each child in reverse order.
6. Extend the `session_manager` control socket to serve `ServiceState` queries.
7. Update `m3ctl session-state` to display real state.
8. Update Phase 57 design doc with a closure note referencing this phase.

## Acceptance Criteria

- `cargo xtask session-smoke` terminates a supervised child externally (SIGKILL) and observes `session_manager` restart it within the retry budget.
- Stopping a service via `m3ctl session-stop <name>` causes the child to receive SIGTERM and exit within 6 seconds; `sys_waitpid` is called before the Ack is returned.
- Exceeding the restart budget transitions the service to `Failed` state visible in `m3ctl session-state`.
- The `text-fallback` path stops all display-server children before emitting the fallback notification; confirmed by watching child PID list in the `session-smoke` integration test.
- Phase 57 design doc carries a closure note referencing Phase 64 for real lifecycle implementation.

## Companion Task List

- [Phase 64 Task List](./tasks/64-session-manager-lifecycle-tasks.md)

## How Real OS Implementations Differ

- systemd tracks unit lifecycle through cgroups, which survive execve and guarantee child containment regardless of double-fork patterns; m3OS uses a direct PID table which is sufficient for supervised non-daemonizing children.
- Real init systems implement socket activation and readiness protocols (sd_notify); m3OS uses a simpler IPC registration handshake inherited from Phase 52.
- Restart budgets in systemd are time-windowed (N restarts in T seconds); m3OS uses a simpler cumulative count per boot.

## Deferred Until Later

- Socket activation and readiness notification
- Time-windowed restart budgets
- Multiple concurrent sessions
- Per-session cgroup or namespace isolation
- Graceful shutdown sequencing for the full system (not just the graphical session)
