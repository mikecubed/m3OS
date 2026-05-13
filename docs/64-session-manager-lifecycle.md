# Session Manager Lifecycle (Phase 64)

**Aligned Roadmap Phase:** Phase 64
**Status:** Complete
**Source Ref:** phase-64
**Supersedes Legacy Doc:** new

## Overview

Phase 64 replaces the Phase 57 `session_manager` lifecycle stubs (which returned `Ack` unconditionally for `start` / `stop` / `restart` and derived `session-state` from IPC round-trip latency) with the real supervisor primitives a modern init system needs: a per-service `ServiceTable` that records each child's PID and per-child `ServiceState`; a two-phase `stop_service` that delivers SIGTERM, waits 5 seconds, escalates to SIGKILL, and observes the child's disappearance via a non-blocking `kill(pid, 0)` liveness probe — expressed as a pure-logic state machine in `lifecycle.rs` and driven to completion by a synchronous `nanosleep` poll loop in `runtime.rs` (the deviation from `sys_waitpid` is deliberate: `session_manager` is not the parent of its supervised children — init is, via its existing manifest-driven boot — and adopting `waitpid` would require either moving every session-service `fork`+`execve` from init into `session_manager` or introducing init→session_manager exit-notification IPC; the kill-probe is functionally equivalent for Phase 64's stop and restart-budget contracts); a `restart_service` that chains `stop` + `start` and enforces both `MAX_RETRIES_PER_STEP` (per-attempt step failures) and `MAX_RESTART_COUNT` (steady-state full-restart count) budgets, escalating exhaustion on a `DISPLAY_CRITICAL_SERVICES` entry (`display_server`, `kbd_server`, `mouse_server`) to the text-fallback motion; a new typed `SessionStateDetailed` verb + `ServiceStates` reply variant that carries per-service `(name, ServiceState, restart_count, step_failures)` quads sourced from the table (the legacy CLI `m3ctl session-state` still requests the session-wide `ControlVerb::SessionState` and prints the single-line state; a future CLI flag — out of scope for Phase 64 — will request `SessionStateDetailed` and use the new per-service printer); and a `recover.rs` that actually drops display-server children in reverse start order (`term` → `audio_server` → `mouse_server` → `kbd_server` → `display_server`) before emitting the fallback notification.

## What This Doc Covers

- The shape of the new `ServiceTable` and the per-child `ServiceState` enum (and why it is orthogonal to the session-wide `kernel_core::session::SessionState`).
- The two-phase stop protocol: SIGTERM → 5 s grace → SIGKILL → kill-probe disappearance check, structured as a `StopMachine` whose tick is exposed for a future event-loop hoist and currently driven by a synchronous `nanosleep` poll loop in `runtime.rs`.
- The pure-logic seams (`KernelClock`, `SignalSink`, `Reaper`) that let the stop state machine and the budget enforcement be host-tested without QEMU.
- The budget contract: when each of `MAX_RETRIES_PER_STEP` and `MAX_RESTART_COUNT` fires, and why `DISPLAY_CRITICAL_SERVICES` is the gate that escalates budget exhaustion to text-fallback.
- The new `SessionStateDetailed` verb + `ServiceStates` reply variant — the per-service quads it carries and how the control-socket dispatcher sources them from the table. (The legacy CLI `m3ctl session-state` still issues the session-wide `ControlVerb::SessionState`.)

## Key Files

| File | Role |
|---|---|
| `userspace/session_manager/src/table.rs` | `ServiceTable`, `ServiceEntry`, `ServiceState`, `Pid` — the per-service PID and lifecycle-state map. Single source of truth for `session-state` and the text-fallback motion. |
| `userspace/session_manager/src/lifecycle.rs` | `StopMachine`, `begin_stop`, `tick`, `record_restart_attempt`, `is_display_critical`, plus the `KernelClock` / `SignalSink` / `Reaper` traits. Pure-logic stop + restart state machines. |
| `userspace/session_manager/src/recover.rs` | `run_text_fallback` — iterates the declared services in reverse and calls `stop_service` for each, then triggers the framebuffer restore. Phase 57's logging-only F.4 wrapper is replaced here with the real motion. |
| `userspace/session_manager/src/control.rs` | `poll_control_once` — extended in Phase 64 so the new `SessionStateDetailed` verb returns the per-service `ServiceStates` reply (forwarded via `SupervisorBackend::services_snapshot`), and the existing `session-stop` / `session-restart` text-fallback rollback uses the real `lifecycle::stop_service` motion (via `InitSupervisorBackend::stop`) instead of Phase 57's logging-only stub. |
| `kernel-core/src/session/mod.rs` | `MAX_RETRIES_PER_STEP`, `MAX_RESTART_COUNT` — both Phase 64 budget constants live at one host-testable path. |

## Why Two Different `*State` Types

The phase introduces a per-child `ServiceState` (`Starting`, `Running`, `Stopping`, `Restarting`, `Failed`) that records the lifecycle of one supervised process. The pre-existing `kernel_core::session::SessionState` (`Booting`, `Running`, `Recovering`, `TextFallback`) describes the *graphical session as a whole*. The two are orthogonal: a `display_server` in `ServiceState::Failed` does not by itself imply the session is in `SessionState::TextFallback`. Only `restart_service`'s budget-exhaustion path, gated by `DISPLAY_CRITICAL_SERVICES`, escalates one child's failure into the session-wide regression.

Keeping the two types distinct lets `m3ctl session-state` answer two different questions on the same control surface: "what is the graphical session doing?" (the `SessionState`) and "what is each supervised child doing?" (the per-service `ServiceState` triples).

## The Two-Phase Stop Protocol

A `stop_service` request:

1. Sends SIGTERM to the recorded PID via `sys_kill`.
2. Records a deadline `now_ms() + SIGTERM_GRACE_MS` (5 seconds).
3. On each `tick`, the production `Reaper` (`runtime::KillProbeReaper`) issues a non-blocking `kill(pid, 0)` liveness probe — a `-ESRCH` from the kernel means the PID is gone and the machine transitions to `Reaped`.
4. If `now_ms() >= deadline_ms` and the child is still alive, sends SIGKILL and arms a `SIGKILL_REAP_MS` (1 second) deadline.
5. A SIGKILL deadline that elapses without observing the PID disappear surfaces as `StopError::ReapFailed`.

The stop machine in `lifecycle.rs` is pure-logic and structured for an eventual event-loop hoist; the binary today drives it via `runtime::stop_service_blocking`, a synchronous `nanosleep` poll loop. That synchronous shape means one in-flight stop briefly stalls other IPC for the duration of the grace + reap windows; the deferred-reply hoist is documented as a Phase 64 follow-up in the PR description.

## Restart Budget Enforcement

Two counters guard against crash-looping services:

- `step_failures` increments on each individual `stop` or `start` failure within one restart attempt. Reaching `MAX_RETRIES_PER_STEP` (3) transitions the service to `Failed`.
- `restart_count` increments on each full successful restart. Reaching `MAX_RESTART_COUNT` (3) also transitions to `Failed`.

A successful restart clears `step_failures` but preserves `restart_count` — that is a steady-state budget per the design doc. When the failed service is in `DISPLAY_CRITICAL_SERVICES` (`display_server`, `kbd_server`, `mouse_server`), `restart_service` invokes `recover::run_text_fallback`; `audio_server` and `term` are intentionally absent from that set because an audio failure or a `term` failure does not warrant regressing the whole session to text mode.

## Related Roadmap Docs

- [Phase 64 — Session Manager Lifecycle](./roadmap/64-session-manager-lifecycle.md) — the roadmap design doc.
- [Phase 64 Task List](./roadmap/tasks/64-session-manager-lifecycle-tasks.md) — the task-level breakdown.
- [Phase 57 — Audio and Local Session](./roadmap/57-audio-and-local-session.md) — predecessor phase that introduced the `session_manager` skeleton and the `text-fallback` recovery contract Phase 64 makes real.
- [Phase 19 — Signal Handling](./roadmap/19-signal-handling.md) — provides the `sys_kill` / `sys_waitpid` / `sys_sigaction` primitives Phase 64 builds on.

## How Real OS Implementations Differ

- systemd tracks unit lifecycle through cgroups, which survive `execve` and contain double-fork patterns; m3OS's direct PID table is sufficient because supervised children do not daemonize.
- Real init systems implement socket activation and a readiness protocol (`sd_notify`); m3OS uses a simpler IPC registration handshake inherited from Phase 52.
- systemd's restart budget is time-windowed (`N restarts in T seconds`); m3OS uses a simpler cumulative-count-per-boot cap.
