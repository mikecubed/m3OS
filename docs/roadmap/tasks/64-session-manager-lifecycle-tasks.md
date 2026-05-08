# Phase 64 — Session Manager Lifecycle: Task List

**Status:** Planned
**Source Ref:** phase-64
**Depends on:** Phase 57 (Audio and Local Session) ✅, Phase 19 (Signal Handling) ✅, Phase 52 (First Service Extractions) ✅
**Goal:** Replace the Phase 57 `session_manager` lifecycle stubs (`stop/restart` unconditionally return `Ack`) with real child-PID tracking, SIGTERM/SIGKILL delivery, `sys_waitpid` observation, restart-budget enforcement, and authentic `m3ctl session-state` reporting; make the typed `text-fallback` recovery contract actually drop display-server children.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Per-service PID map: `ServiceTable`, `ServiceEntry`, `ServiceState` | None | Planned |
| B | `stop()` — SIGTERM + grace + SIGKILL + `sys_waitpid` | A | Planned |
| C | `restart()` — chained stop+start with restart-budget enforcement | B | Planned |
| D | Text-fallback motion: reverse-order stop of display-server children | C | Planned |
| E | `m3ctl session-state` reports authentic `ServiceState` | A | Planned |
| F | Phase 57 design doc + task doc closure note | D, E | Planned |

---

## Track A — Per-Service PID Map

### A.1 — Define `ServiceEntry`, `ServiceState`, `ServiceTable`

**File:** `userspace/session_manager/src/table.rs`
**Symbol:** `ServiceTable`, `ServiceEntry`, `ServiceState`
**Why it matters:** The table is the single source of truth for all lifecycle decisions; nothing else may infer service state from external signals.

**Acceptance:**
- [ ] `ServiceState` enum has variants: `Starting`, `Running`, `Stopping`, `Restarting`, `Failed`.
- [ ] `ServiceEntry` holds `pid: Option<Pid>`, `state: ServiceState`, `restart_count: u32`, `step_failures: u32`.
- [ ] `ServiceTable` provides `insert`, `update_state`, `get_pid`, and `iter` methods.
- [ ] At least five unit tests cover state transition paths.

### A.2 — Wire `ServiceTable` into the event loop

**File:** `userspace/session_manager/src/main.rs`
**Symbol:** `SessionManager::run`
**Why it matters:** Every spawn must record a PID; without this the lifecycle methods have no target.

**Acceptance:**
- [ ] After each `sys_spawn` call the returned PID is stored via `ServiceTable::update_pid`.
- [ ] On SIGCHLD receipt the event loop calls `sys_waitpid(WNOHANG)` and marks the matching `ServiceEntry` exited.
- [ ] An integration test spawns a short-lived child and verifies the table transitions to `Failed` after exit.

---

## Track B — `stop()` SIGTERM + Grace + SIGKILL

### B.1 — Implement `stop_service` with two-phase signal delivery

**File:** `userspace/session_manager/src/lifecycle.rs`
**Symbol:** `stop_service`
**Why it matters:** An unconditional Ack stop cannot guarantee the child is gone before a restart begins.

**Acceptance:**
- [ ] `stop_service` sends SIGTERM via `sys_kill(pid, SIGTERM)`, then polls `sys_waitpid(WNOHANG)` for up to 5 seconds.
- [ ] If not exited after 5 seconds, sends SIGKILL and calls `sys_waitpid` with blocking semantics.
- [ ] Returns `Ok(exit_code)` after confirmed exit; returns `Err(StopError::KillFailed)` only if SIGKILL itself fails.
- [ ] `stop()` IPC handler does not reply `Ack` until `stop_service` returns.
- [ ] At least three unit tests: normal SIGTERM exit, grace-period expiry + SIGKILL, nonexistent PID.

---

## Track C — `restart()` with Restart-Budget Enforcement

### C.1 — Implement `restart_service` with budget counters

**File:** `userspace/session_manager/src/lifecycle.rs`
**Symbol:** `restart_service`
**Why it matters:** Without budget enforcement a crash-looping service can consume all system resources indefinitely.

**Acceptance:**
- [ ] `restart_service` calls `stop_service` then `start_service`; each failure increments `ServiceEntry::step_failures`.
- [ ] `ServiceEntry::restart_count` increments on each full restart attempt.
- [ ] After 3 `step_failures` or 3 `restart_count` increments, service transitions to `Failed` and `restart_service` returns `Err(BudgetExhausted)`.
- [ ] Budget exhaustion is logged at ERROR level with service name and counts.
- [ ] An integration test triggers 4 restarts of a crash-looping stub and verifies `Failed` state.

---

## Track D — Text-Fallback Motion

### D.1 — Extend `recover.rs` to call `stop_service` in reverse order

**File:** `userspace/session_manager/src/recover.rs`
**Symbol:** `run_text_fallback`
**Why it matters:** The Phase 57 fallback was logging-only; real recovery requires children to actually stop before the system regresses to text mode.

**Acceptance:**
- [ ] `run_text_fallback` iterates the service startup list in reverse (`term` → `audio_server` → `mouse_server` → `kbd_server` → `display_server`) and calls `stop_service` for each.
- [ ] If any `stop_service` call returns `Err`, the error is logged and the loop continues (best-effort teardown).
- [ ] The `text-fallback` IPC notification is emitted only after the loop completes.
- [ ] A `session-smoke` integration test externally kills `display_server` and verifies all children are gone before the fallback notification arrives.

---

## Track E — Authentic `m3ctl session-state`

### E.1 — Extend `session_manager` control socket to serve `ServiceState` queries

**File:** `userspace/session_manager/src/control.rs`
**Symbol:** `handle_state_query`
**Why it matters:** Before this phase `session-state` output was derived from IPC latency, not from the actual service state machine.

**Acceptance:**
- [ ] A new control verb `QueryState { name: Option<String> }` returns a list of `(name, ServiceState)` pairs from `ServiceTable`.
- [ ] `m3ctl session-state` (with no argument) prints all services and states; `m3ctl session-state <name>` prints one.
- [ ] Output includes the restart count for services in `Restarting` or `Failed` state.

---

## Track F — Phase 57 Documentation Closure

### F.1 — Update Phase 57 design doc with closure note

**File:** `docs/roadmap/57-audio-and-local-session.md`
**Symbol:** (document section `## Deferred Until Later`)
**Why it matters:** The audit found that Phase 57's lifecycle contract was a stub; the design doc must be corrected.

**Acceptance:**
- [ ] A `> **Phase 64 closure note:**` block is appended to `## Deferred Until Later` stating that real lifecycle methods (stop, restart, text-fallback, authentic state) were delivered by Phase 64.

### F.2 — Update Phase 57 task doc Track F acceptance items

**File:** `docs/roadmap/tasks/57-audio-and-local-session-tasks.md`
**Symbol:** Track F
**Why it matters:** Track F acceptance items for `stop`/`restart` were checked against stub behavior; they must reference the Phase 64 closure.

**Acceptance:**
- [ ] Track F items for `stop()` and `restart()` include a parenthetical "(real implementation in Phase 64)".
- [ ] No other Phase 57 acceptance items are changed.

---

## Documentation Notes

- `table.rs` and `lifecycle.rs` are new files under `userspace/session_manager/src/`; they replace inline logic previously scattered in `main.rs` and `recover.rs`.
- The 5-second SIGTERM grace period is a named constant (`SIGTERM_GRACE_MS = 5000`) defined once in `lifecycle.rs`.
- The restart budget values (3 step failures, 3 steady-state restarts) are named constants from `kernel-core::session::policy` so they appear in one place and are testable on the host.
