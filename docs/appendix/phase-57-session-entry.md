# Phase 57 Track A.4 — Session-Entry Contract

**Status:** Decided
**Source Ref:** phase-57
**Scope:** A.4 (this memo) — informs F.1 (`session_manager` boot sequencer), F.2 (start-step contract), F.3 (recovery contract), F.4 (restart cap location), F.5 (m3ctl session-stop verb)
**Cross-links:**
- Phase 57 design doc — [`docs/roadmap/57-audio-and-local-session.md`](../roadmap/57-audio-and-local-session.md)
- Phase 57 audio target choice — [`docs/appendix/phase-57-audio-target-choice.md`](./phase-57-audio-target-choice.md)
- Phase 57 audio ABI — [`docs/appendix/phase-57-audio-abi.md`](./phase-57-audio-abi.md)
- Phase 57 service topology — [`docs/57-audio-and-local-session.md`](../57-audio-and-local-session.md)
- Phase 56 session integration / supervision / recovery — [`docs/56-display-and-input-architecture.md`](../56-display-and-input-architecture.md) (section "Session integration, supervision, and recovery", "Crash recovery", "Text-mode fallback")
- Phase 56 control socket precedent — [`docs/56-display-and-input-architecture.md`](../56-display-and-input-architecture.md) (control-socket section, m3ctl verbs)

---

## Decision

**Phase 57's session entry is a fixed boot sequence ordered by `session_manager`.** No console-session UID concept is introduced. The text-mode admin path is reached primarily through **`m3ctl session-stop`**, with a Phase 56 grab-hook keychord (Ctrl-Alt-F1) explicitly deferred to a later phase.

The rejected alternative — a "console session UID" model where a particular UID's login produces the graphical session and another UID drops to text mode — is documented below with the prerequisite phase that would have to deliver it.

## Entry trigger: fixed boot sequence ordered by `session_manager`

`session_manager` is a userspace daemon supervised by `init`. It runs **once at boot** and orchestrates the graphical-stack startup. There is no user-facing session login in Phase 57 — m3OS has no console-session UID concept, and adding one is out of scope per the YAGNI rules in the task list.

The trigger sequence:

1. `init` (PID 1) parses `/etc/services.d/*.conf` at boot and runs the supervised services in the order their `depends=` graphs allow.
2. `session_manager.conf` declares `depends=console`. Once `init` has reached steady state on the headless services, it forks `session_manager`.
3. `session_manager` runs the ordered start sequence below, observing each child's readiness through the existing `/run/services.status` file the supervisor maintains.
4. On end-to-end success, `session_manager` transitions to `running` and waits on its control socket.
5. On any startup-step failure that exhausts the per-service retry budget, `session_manager` transitions to `text-fallback` (see "Failure recovery contract" below) and the operator falls back to the serial / kernel framebuffer console exactly as Phase 56 F.3 documents.

The boot sequence is deterministic and reproducible. The same sequence runs every boot; there is no per-user variation, no profile selection, no session-cookie persistence. This matches Phase 57's `Session manager handles exactly one session in Phase 57` YAGNI rule.

## Ordered startup steps

`session_manager` runs these steps **in order**, blocking on each step's readiness signal before launching the next. Each step has its own start, readiness, and failure-handling contract.

| # | Step | Reads ready-state from | Failure handling |
|---|---|---|---|
| 1 | `display_server` | `/run/services.status` shows `display_server=running` AND a connect to `/run/m3os/display.sock` succeeds | Per-service retry up to **3** attempts; on exhaustion, `session_manager` escalates the whole session to `text-fallback` and exits with a named reason |
| 2 | `kbd_server` | `/run/services.status` shows `kbd_server=running` AND `display_server`'s `subscribe SurfaceCreated` (control socket) shows the keyboard endpoint registered with the dispatcher | Per-service retry up to **3** attempts; on exhaustion, escalate to `text-fallback` |
| 3 | `mouse_server` | `/run/services.status` shows `mouse_server=running` AND a probe of the mouse endpoint succeeds | Per-service retry up to **3** attempts; on exhaustion, escalate to `text-fallback` |
| 4 | `audio_server` | `/run/services.status` shows `audio_server=running` AND the `audio_server` control endpoint responds to a `version` verb | Per-service retry up to **3** attempts; on exhaustion, escalate to `text-fallback`. **Audio failure is non-fatal in spirit but escalates in Phase 57 anyway** because Phase 57's milestone explicitly demands "audible PCM output" — leaving the session running silently would silently regress the milestone evaluation gate |
| 5 | `term` | `term`'s service registry entry (`"term"`) becomes available AND the AF_UNIX listening socket at `/run/m3os/term.sock` (or whatever G.6 settles on) is bindable | Per-service retry up to **3** attempts; on exhaustion, escalate to `text-fallback` |

The ordering is not arbitrary:

- **Display before input.** `kbd_server` and `mouse_server` publish their typed events to `display_server`'s input endpoint (Phase 56 A.2 capability map). They cannot finish their bind step until `display_server`'s endpoint exists.
- **Display + input before audio.** Audio doesn't strictly depend on display, but failing audio after display is up gives a clean session-state transition (the user sees the framebuffer, then the session manager logs the audio failure, then the operator can inspect the framebuffer to see the error). Failing audio before display is up costs the operator the visual surface for the failure log.
- **Audio before term.** `term`'s bell path consumes `audio_client` (Phase 57 service-topology constraint). If `term` starts before `audio_server` is ready, the bell silently no-ops — that's not a fatal regression but it confuses the smoke harness; ordering audio first removes the ambiguity.
- **`term` last.** `term` is a regular client of every service above it. Starting it last keeps the dependency graph explicit and matches the Phase 56 manifest convention (`gfx-demo.conf` declares `depends=display`).

The "ready" semantic for each step is **the same shape as Phase 56**: the supervisor's `/run/services.status` file reports the child's `running` state, and `session_manager` does an additional protocol-level probe (a verb on the control socket, or a connect on the listening socket) before declaring the step done. This catches the case where a process is up but its endpoint isn't yet bound — a known race that Phase 56's F.2 regression captured.

## Failure-recovery contract

The cap location and escalation policy:

| Failure shape | Where the cap lives | Escalation |
|---|---|---|
| **Transient bind / socket error** during a startup step (e.g., `EADDRINUSE` on the control socket because the previous instance is still draining) | A retry counter inside `session_manager`'s start-step loop, declared once in `kernel-core::session::startup` (per the task list's "Session-step ordering ... lives once in kernel-core::session::startup" rule) | Retry the **same** step once after a 100 ms sleep. If the second attempt also fails, count it against the per-service cap below |
| **Per-service start failure** (the supervised process crashes during init, or its readiness probe never succeeds within the timeout) | **Per-service in `kernel-core::session::startup`** — declared once as a typed `RetryBudget { max_attempts: 3 }` and consumed by the start-step loop. Not in `init`, because the cap is about the session bring-up step, not about steady-state crash recovery | Up to **3 attempts** per service. On exhaustion, `session_manager` transitions the session state to `text-fallback`, emits a structured `session.boot.escalate { service, attempts, reason }` log line, and exits |
| **Steady-state crash** of any of the five services after the session reaches `running` | **`init`'s existing supervisor restart cap (`max_restart=N`)**, per the Phase 56 F.1 manifest convention. Phase 56's `display_server.conf` already uses `max_restart=5`; Phase 57's manifests reuse the same shape. **This cap is separate from the boot retry cap** | The supervisor restarts the service per its manifest. If the supervisor restart cap is also exhausted, the supervisor flips the service to `permanently-stopped` and `session_manager` observes the state change and transitions the session to `text-fallback` |
| **`session_manager` itself crashes** | `init` supervises `session_manager` with `restart=on-failure max_restart=3` (the same cap shape) | On exhaustion, `init` flips `session_manager` to `permanently-stopped`. The graphical stack stops being orchestrated; existing `display_server` etc. keep running; operator falls back to the text-mode admin path (m3ctl) |

The two-cap design is intentional: the **boot retry cap** governs "how hard does session_manager try to bring the session up?" and the **steady-state restart cap** governs "how often can a service crash before we give up?" They have different semantics and different correct values. The Phase 56 control plane already manages the steady-state cap (per `etc/services.d/*.conf` `max_restart=` values); Phase 57 only adds the boot-retry cap, declared once in `kernel-core::session::startup`.

### Mapping to specific failure modes

| Failure mode | Reaches `text-fallback`? |
|---|---|
| `display_server` crashes once during boot | No — retry up to 3, succeed on attempt 2 or 3 |
| `display_server` crashes 3 times during boot | **Yes** — boot retry cap exhausted |
| `display_server` crashes once after `running` | No — supervisor restarts; session keeps running |
| `display_server` crashes 5 times after `running` (Phase 56 cap) | **Yes** — supervisor flips it to `permanently-stopped`, `session_manager` observes and escalates |
| `audio_server` crashes once during boot | No — retry up to 3 |
| `audio_server` crashes 3 times during boot | **Yes** — milestone demands audible PCM, so the boot considers it fatal |
| `term` is killed by an operator after the session is `running` | No — supervisor restarts `term`. The session state stays `running`; only `term` cycles |
| Any service fails its IOMMU bar-coverage assertion at claim time | **Yes** — this is a hard architectural failure, not transient. `session_manager` does not retry; it escalates immediately on the first claim failure |

### Where `text-fallback` itself is implemented

`session_manager` does **not** implement text-fallback. Phase 56 F.3 already implements text-mode fallback at the `init` + kernel-framebuffer level: when no userspace process holds the framebuffer, the kernel's `CONSOLE_YIELDED` flag stays false and `kernel/src/fb/mod.rs::write_str` resumes producing characters. Phase 57's `text-fallback` transition is therefore a **state transition emitted by `session_manager` that drops its framebuffer-using children** — once `display_server` exits, the kernel's existing fallback path takes over without further action.

The transition steps:

1. `session_manager` emits `session.boot.escalate { service, attempts, reason }` (or `session.recover.escalate { service, reason }` for the steady-state path).
2. `session_manager` sends a graceful `stop` verb to each service via its existing control socket, in **reverse** order (`term` → `audio_server` → `mouse_server` → `kbd_server` → `display_server`).
3. After a 1-second timeout per service, services that haven't exited get `SIGTERM`; after another 1-second timeout, `SIGKILL`.
4. `session_manager` exits with code `2` (text-fallback escalation) so `init` can record the named reason.
5. The kernel framebuffer console resumes (Phase 56 F.3 path) automatically once `display_server` is gone.
6. Serial `login` remains available throughout (Phase 56 F.3 invariant).

## Text-mode admin path (primary: `m3ctl session-stop`)

Phase 56 already shipped a `m3ctl` binary that speaks to `display_server`'s control socket. Phase 57 extends `m3ctl` with a single new verb — `m3ctl session-stop` — that talks to **`session_manager`'s control socket** (a separate AF_UNIX path: `/run/m3os/session.sock`) and drives the same graceful shutdown path described above (steps 2–6).

`m3ctl session-stop` is the chosen primary path because:

- It reuses the existing `m3ctl` client and the existing AF_UNIX-control-socket framing (Phase 56 control-socket precedent). No new client binary, no new framing.
- It is invocable from a serial shell (`ssh` or the kernel framebuffer console don't need to be touched). The operator types `m3ctl session-stop` and the graphical stack drains cleanly.
- It cooperates with the supervisor: `m3ctl session-stop` does not skip the cap accounting — `session_manager` exits with code 2 just like the boot-failure escalation, so init's logs reflect the same shape regardless of how text-fallback was entered.

The verb is added to `kernel-core::session::control::ControlCommand` (one declaration site) and dispatched in `userspace/session_manager/src/control.rs`. `m3ctl` (already a Phase 56 binary) gains the `session-stop` subcommand in its argument parser. No new socket, no new binary.

### Rejected alternative for the text-mode path: Phase 56 grab-hook keychord (Ctrl-Alt-F1)

A keyboard-driven entry — Ctrl-Alt-F1, captured by Phase 56's grab hook — was considered. The grab hook (`m3ctl register-bind` Ctrl-Alt-F1) can already be installed against `display_server`'s control socket; it would emit a `BindTriggered` event that `session_manager` could subscribe to and treat as a `session-stop` request.

Reasons for deferring this to a later phase:

1. **Two paths is one too many for Phase 57.** The minimal admin-recovery story is one verb. Adding a keychord *and* a verb means two start-up registration steps, two test surfaces, and two regressions to maintain. Phase 57 picks the verb because it's the cheaper of the two.
2. **The keychord implies an input-grab discipline that Phase 57 doesn't yet enforce.** A grabbed Ctrl-Alt-F1 must reach `session_manager` even when a focused fullscreen client (e.g., a future game) holds the keyboard. That ordering is plausible — Phase 56's grab hook predates focus dispatch on purpose — but proving the corner case (grab-hook fires even when input is otherwise grabbed by a misbehaving client) needs a regression that Phase 57 does not yet have.
3. **The serial path is enough.** The same operator who would press Ctrl-Alt-F1 in a graphical terminal can `m3ctl session-stop` from any serial shell, including SSH or the kernel framebuffer console (under Phase 56 F.3 fallback). Phase 57's recovery rule is "an operator can always reach text-mode admin," not "an operator can always reach text-mode admin **without leaving the keyboard**."

If a later phase adds the keychord, it lands as a small additive change: `session_manager` subscribes to `BindTriggered` on `display_server`'s control socket, wires a `Ctrl-Alt-F1` registration at startup, and dispatches the same `ControlCommand::SessionStop` it already accepts on its own socket. No protocol changes; no new binaries.

## Rejected alternative for entry trigger: console-session UID

The alternative entry trigger considered was: **"the graphical session starts when a designated console-session UID logs in (e.g., on `tty1`), and falls back to text mode for any other login."** This is the model Linux desktops use: `gdm` / `sddm` / `lightdm` start the graphical session for an authenticated user; a different UID can log into a text VT independently.

Reasons this was rejected for Phase 57:

1. **m3OS has no "console session UID" concept yet.** The Phase 27 user-account work introduced UIDs and `/etc/passwd`-style account records, but there is no notion of a *console session* (the abstract idea that a particular UID owns the framebuffer + input + audio for the duration of a login) and no equivalent of `pam_systemd` / `logind` to manage it. Adding the concept is a phase-sized chunk of work, not an A.4 deliverable.
2. **The prerequisite would be a "session model" phase.** Such a phase would introduce: a session record type, a per-session UID + cgroup-equivalent + capability table, a bridge between authentication (Phase 27 `login`) and the start of the graphical stack, and a per-session control socket. None of this exists in Phase 56's substrate.
3. **The Phase 57 milestone gate is satisfied without it.** Phase 57's evaluation gate is "the supported target can produce audible PCM output through the documented audio contract" + "there is a documented and working path into a local graphical session" + "session shutdown, crash recovery, and fallback to administration are documented and tested." The fixed-boot-sequence trigger satisfies all three; the console-session UID model would be net additive but would not change the truth value of any gate row.

The console-session UID model is captured here so a later phase ("Phase 5x: Console Sessions and Multi-User Graphical Login" — name TBD) can pick it up without re-doing the rejection analysis. The forward-compatible piece is that `session_manager`'s lifecycle is already factored as a separate process from `init` and from `display_server`, so transplanting it into a per-session model is "spawn one `session_manager` per login session" rather than "rewrite the orchestrator."

## Acceptance trace

This memo discharges A.4's acceptance:

- [x] Names the entry trigger (fixed boot sequence ordered by `session_manager`) and documents the rejected alternative (console-session UID), with the prerequisite phase that would have to deliver it ("Phase 5x: Console Sessions and Multi-User Graphical Login," not yet scheduled).
- [x] Names the explicit ordered startup steps `session_manager` runs (`display_server` → `kbd_server` → `mouse_server` → `audio_server` → `term`) and the failure handling for each (per-service retry cap of 3, then escalate to `text-fallback`).
- [x] Names the failure-recovery contract: which failures escalate to `text-fallback` (boot-retry cap exhaustion, supervisor steady-state cap exhaustion, IOMMU bar-coverage assertion failure), which to a single restart attempt (transient bind/socket errors), and where the restart cap lives (boot retry cap in `kernel-core::session::startup` per F.4; steady-state cap in `etc/services.d/*.conf` per Phase 56 manifest convention).
- [x] Records how a developer reaches text-mode admin: **primary is `m3ctl session-stop`** speaking to `session_manager`'s control socket on `/run/m3os/session.sock`. Records the rejected alternative (Phase 56 grab-hook keychord Ctrl-Alt-F1) and why it's deferred to a later phase.
