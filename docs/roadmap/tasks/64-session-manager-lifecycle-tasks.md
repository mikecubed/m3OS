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
| E | `m3ctl session-state` reports authentic `ServiceState` | A, C | Planned |
| F | Phase 57 design doc + task doc closure note | D, E | Planned |
| G | Documentation and Release | F | Planned |

> **Naming note:** `ServiceState` introduced in Track A is a *per-child* state
> (`Starting`, `Running`, `Stopping`, `Restarting`, `Failed`) and is orthogonal
> to the existing session-wide `kernel_core::session::SessionState`
> (`Booting`, `Running`, `Recovering`, `TextFallback`) defined in
> `kernel-core/src/session/startup.rs`. Both types coexist: `SessionState`
> describes the graphical session as a whole; `ServiceState` describes a
> single supervised child.

---

## Track A — Per-Service PID Map

### A.1 — Define `ServiceEntry`, `ServiceState`, `ServiceTable`

**File:** `userspace/session_manager/src/table.rs`
**Symbol:** `ServiceTable`, `ServiceEntry`, `ServiceState`
**Why it matters:** The table is the single source of truth for all lifecycle decisions; nothing else may infer service state from external signals.

**Acceptance:**
- [ ] `ServiceState` enum has variants: `Starting`, `Running`, `Stopping`, `Restarting`, `Failed`.
- [ ] `ServiceEntry` holds `pid: Option<Pid>`, `state: ServiceState`, `restart_count: u32`, `step_failures: u32`.
- [ ] `ServiceTable` provides `insert`, `update_pid`, `update_state`, `get_pid`, and `iter` methods.
- [ ] `ServiceState` is documented as per-child and **distinct** from `kernel_core::session::SessionState`; the doc comment on the enum cites both types.
- [ ] At least five unit tests cover state transition paths.

### A.2 — Wire `ServiceTable` into the event loop

**File:** `userspace/session_manager/src/main.rs`
**Symbol:** `SessionManager::run`
**Why it matters:** Every spawn must record a PID; without this the lifecycle methods have no target.

**Acceptance:**
- [ ] After each `sys_spawn` call the returned PID is stored via `ServiceTable::update_pid`.
- [ ] `session_manager` installs a SIGCHLD handler via `sys_sigaction` before the first `sys_spawn` call; the handler signals a `Notification` consumed by the event loop (no work is performed inside the handler).
- [ ] On the SIGCHLD `Notification`, the event loop drains `sys_waitpid(-1, ..., WNOHANG)` in a loop and marks each matching `ServiceEntry` exited.
- [ ] An integration test spawns a short-lived child and verifies the table transitions to `Failed` after exit.

### A.3 — Add `crash_stub` test binary for lifecycle integration tests

**Files:** `Cargo.toml`, `xtask/src/main.rs`, `kernel/src/fs/ramdisk.rs`, `userspace/crash_stub/src/main.rs` (new crate)
**Symbol:** `userspace/crash_stub`
**Why it matters:** B.1, C.1, and D.1 acceptance items require a deterministic short-lived child that ignores SIGTERM (for grace-period testing) and a variant that exits immediately (for crash-loop testing). Without this binary the integration tests cannot run inside QEMU.

**Acceptance:**
- [ ] `userspace/crash_stub` is added to the workspace `members` list in the top-level `Cargo.toml`.
- [ ] `xtask/src/main.rs` `build_userspace` includes `crash_stub` in the `bins` array with `needs_alloc = false`.
- [ ] `kernel/src/fs/ramdisk.rs` registers `crash_stub` in `BIN_ENTRIES` via `include_bytes!`.
- [ ] The binary accepts a single argv mode: `exit-immediately`, `ignore-sigterm`, or `exit-on-sigterm` and behaves accordingly.
- [ ] `cargo xtask check` passes after adding the crate.

---

## Track B — `stop()` SIGTERM + Grace + SIGKILL

### B.1 — Implement `stop_service` with two-phase signal delivery

**File:** `userspace/session_manager/src/lifecycle.rs`
**Symbol:** `stop_service`
**Why it matters:** An unconditional Ack stop cannot guarantee the child is gone before a restart begins.

**Acceptance:**
- [ ] `stop_service` is driven as a state machine across event-loop iterations (states: `SentTerm { deadline_ms }`, `SentKill { deadline_ms }`, `Reaped { exit_code }`). The event loop is **not** suspended; other IPC continues to be serviced while a stop is in flight.
- [ ] On entry, `stop_service` sends SIGTERM via `sys_kill(pid, SIGTERM)` and records `deadline_ms = now_ms() + SIGTERM_GRACE_MS`.
- [ ] Each event-loop tick (including SIGCHLD `Notification` wake-ups) re-checks `sys_waitpid(pid, ..., WNOHANG)`; on `now_ms() >= deadline_ms` it transitions to `SentKill`, sends SIGKILL, and sets a second deadline (`SIGKILL_REAP_MS = 1000`).
- [ ] In `SentKill` state, exhausting the second deadline without a reap returns `Err(StopError::KillFailed)`; otherwise the transition to `Reaped { exit_code }` resolves the in-flight `stop()` IPC reply.
- [ ] `stop()` IPC handler defers its reply (does not return `Ack` synchronously) until the state machine reaches `Reaped`.
- [ ] At least three host-side unit tests against a mock `KernelClock + SignalSink + Reaper`: normal SIGTERM exit, grace-period expiry → SIGKILL, nonexistent PID (immediate `Err`).

---

## Track C — `restart()` with Restart-Budget Enforcement

### C.1 — Implement `restart_service` with budget counters

**File:** `userspace/session_manager/src/lifecycle.rs`
**Symbol:** `restart_service`
**Why it matters:** Without budget enforcement a crash-looping service can consume all system resources indefinitely.

**Acceptance:**
- [ ] `restart_service` calls `stop_service` then `start_service`; each failure increments `ServiceEntry::step_failures`.
- [ ] `ServiceEntry::restart_count` increments on each full restart attempt.
- [ ] After `MAX_RETRIES_PER_STEP` (3) `step_failures` or `MAX_RESTART_COUNT` (3) `restart_count` increments, service transitions to `Failed` and `restart_service` returns `Err(BudgetExhausted)`.
- [ ] On `BudgetExhausted` for any display-critical service (`display_server`, `kbd_server`, or `mouse_server`), `restart_service` invokes `recover::run_text_fallback`; the display-critical set is a named constant `DISPLAY_CRITICAL_SERVICES` in `lifecycle.rs`.
- [ ] Budget exhaustion is logged at ERROR level with service name, `restart_count`, and `step_failures`.
- [ ] An integration test launches `crash_stub` (A.3, mode `exit-immediately`) under the supervisor, triggers 4 restart attempts, and verifies `ServiceState::Failed`.
- [ ] An integration test launches `crash_stub` (A.3, mode `exit-immediately`) as `display_server`, exhausts the budget, and verifies `run_text_fallback` was invoked.

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

**Notes for the implementer:** The `session-state`, `session-stop`, and `session-restart` verbs already exist as Phase 57 stubs in `userspace/session_manager/src/control.rs`; only the **payload** returned by `session-state` and the **dispatching backend** for `session-stop` / `session-restart` change in this phase. Do not re-register verbs.

**Acceptance:**
- [ ] The existing `session-state` verb (Phase 57) is extended so the reply carries a list of `(name, ServiceState, restart_count)` triples sourced from `ServiceTable`.
- [ ] `m3ctl session-state` (with no argument) prints all services and states; `m3ctl session-state <name>` prints one.
- [ ] Output includes the `restart_count` and `step_failures` for services in `Restarting` or `Failed` state.
- [ ] The existing `session-stop` and `session-restart` verbs route to `lifecycle::stop_service` / `lifecycle::restart_service` (Track B/C) rather than the Phase 57 stub backend.

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

## Track G — Documentation and Release

### G.1 — Create the aligned legacy learning doc

**File:** `docs/64-session-manager-lifecycle.md` (legacy learning doc — **distinct from** `docs/roadmap/64-session-manager-lifecycle.md`, which is the Phase 64 roadmap design doc; do not overwrite the roadmap doc)
**Symbol:** (new document)
**Why it matters:** Learners need a concise reference explaining what real supervisor lifecycle management looks like — PID tracking, two-phase stop, restart budgets — without conflating it with Phase 57's stub behavior or future socket-activation work.

**Acceptance:**
- [ ] `docs/64-session-manager-lifecycle.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 64`, `**Status:** Planned`, `**Source Ref:** phase-64`, `**Supersedes Legacy Doc:** new`).
- [ ] Overview is one learner-friendly paragraph explaining the move from unconditional Ack stubs to real SIGTERM/SIGKILL lifecycle.
- [ ] Key Files table cites `userspace/session_manager/src/table.rs`, `userspace/session_manager/src/lifecycle.rs`, `userspace/session_manager/src/recover.rs`, and `userspace/session_manager/src/control.rs`.
- [ ] Related Roadmap Docs links `docs/roadmap/64-session-manager-lifecycle.md` and `docs/roadmap/tasks/64-session-manager-lifecycle-tasks.md`.

### G.2 — Bump kernel version to 0.64.0

**Files:** `kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md`, `docs/roadmap/README.md`
**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-bump per shipped phase; keeping the version cursor accurate ensures `AGENTS.md` and the README reflect the real state of the kernel at any given phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.64.0"`
- [ ] `Cargo.lock` regenerated (run `cargo check` or `cargo xtask check` to trigger)
- [ ] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.64.0`
- [ ] `AGENTS.md` project-overview paragraph appended with a one-line summary of Phase 64 (real `session_manager` lifecycle: PID tracking, two-phase stop, restart budgets, authentic `session-state`).
- [ ] `AGENTS.md` `cargo xtask check` description updated to add `session_manager` to the host-test list (the new `table.rs` and `lifecycle.rs` modules are host-testable).
- [ ] `cargo xtask check` passes after the bump
- [ ] Git tag `v0.64.0` recommended at phase merge

---

## Documentation Notes

- `table.rs` and `lifecycle.rs` are new files under `userspace/session_manager/src/`; they replace inline logic previously scattered in `main.rs` and `recover.rs`.
- The 5-second SIGTERM grace period is a named constant (`SIGTERM_GRACE_MS = 5000`) defined once in `lifecycle.rs`. A complementary `SIGKILL_REAP_MS = 1000` bounds the post-SIGKILL reap window.
- The restart budget values reuse the existing `kernel_core::session::MAX_RETRIES_PER_STEP` constant (`= 3`) and introduce a new `kernel_core::session::MAX_RESTART_COUNT` constant (`= 3`) in the same module. There is no `kernel_core::session::policy` submodule — both constants live at `kernel_core::session::` so they are host-testable from `kernel-core` unit tests.
- `ServiceState` (per-child) and `kernel_core::session::SessionState` (session-wide) are distinct types. Implementers must not collapse them.
