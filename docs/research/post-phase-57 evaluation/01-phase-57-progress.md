# Phase 57 Progress Ledger

**Date:** 2026-04-29
**Baseline:** `main` at `449fc05165868a22e756038b50ccc55981291fcd`
**Question:** What has Phase 57 actually delivered, and what remains before it can be treated as a stable local GUI baseline?

## Executive assessment

Phase 57 has landed the main process and protocol boundaries:

- `display_server`, `kbd_server`, `mouse_server`, `m3ctl`, and `gfx-demo` from Phase 56 are present.
- `session_manager`, `audio_server`, `audio_client`, `audio-demo`, and `term` are workspace members.
- `kernel-core` has host-testable display, input, audio, and session model modules.
- `xtask` builds and stages the new userspace binaries, and `run-gui` now has AC'97 QEMU flags.
- The docs describe the intended local-session topology and the deferred desktop boundary clearly.

The problem is that several "complete" labels are ahead of the runtime. The Phase 57 code on `main` still has production stubs and shallow smoke checks. Treat it as a platform checkpoint, not as a finished GUI/audio/session product.

## Delivered surfaces

| Area | What exists on `main` | Notes |
|---|---|---|
| Display ownership | `userspace/display_server` claims the framebuffer, registers `display` and `display-control`, accepts client protocol messages, composes surfaces, drains input, and tracks frame stats. | Good substrate for a native compositor. |
| Input routing | `kbd_server`, `mouse_server`, `kernel-core::input`, bind table, focus dispatch, and event queues exist. | Usable enough for the terminal path, but richer keybind/chord behavior is still later work. |
| Session manager | `userspace/session_manager` exists, registers `session-manager`, owns a control context, runs the ordered startup sequence, and reports `running` or `text-fallback`. | Its `init` integration is still mostly observe-and-log rather than direct lifecycle ownership. |
| Terminal | `userspace/term` exists with PTY, ANSI screen state, renderer, input handler, and bell path. | First useful graphical client shape is present. It still needs terminal compatibility hardening before Neovim/tmux-class apps are credible. |
| Audio protocol | `kernel-core::audio`, `userspace/lib/audio_client`, `audio-demo`, and `audio_server` protocol/stream/client registries exist. | The production AC'97 backend is not yet a real device driver. |
| Build and image wiring | `Cargo.toml`, `xtask`, ramdisk entries, and service configs know about the Phase 57 crates. | Four-place userspace binary convention is mostly followed. |
| Documentation | Phase 57 design, task, audio target, audio ABI, session entry, and learning docs exist. | These docs are useful, but some acceptance claims now need a reality-status addendum. |

## Important gaps

### 1. AC'97 output is still a production stub

`userspace/audio_server/src/device.rs` has real pure helpers for AC'97 register layout and BDL state, but the production `Ac97Backend` still says the real path is D.2 work:

- `Ac97Backend::init` records the device handle and marks itself initialized; it does not allocate/program the BDL or PCM ring.
- `submit_frames` accepts bytes for accounting only.
- `drain` returns immediately.
- `handle_irq` returns `IrqEvent::None`.

`userspace/audio_server/src/irq.rs` also decodes `SubmitFrames { len }` without passing the trailing PCM payload into `StreamRegistry::submit`. That means the current `audio_client` can send frame bytes, but the server path does not yet feed them to hardware.

**Consequence:** audible PCM output is not a closed acceptance item on `main`, despite the Phase 57 docs marking the phase complete.

### 2. Audio smoke does not prove audio

`xtask/src/main.rs::audio_smoke_steps` waits for:

- kernel boot banner
- `init: loaded service 'audio_server'`

It explicitly does not run `audio-demo` and does not assert `frames_consumed` progress. This is useful as build/image wiring coverage, but it is not an audio smoke test.

**Required replacement:** `cargo xtask audio-smoke` should boot with AC'97, run `audio-demo`, observe `AUDIO_DEMO:PASS`, and query audio stats showing non-zero submitted and consumed frame counts. For headless CI, `-audiodev none,id=snd0` is fine; the assertion should be driver progress, not host audibility.

### 3. `session_manager` is not yet a full lifecycle owner

`userspace/session_manager/src/main.rs` registers and runs the startup sequence, but its production backend still has transitional behavior:

- `start(name)` returns `Ack` unconditionally and relies on `init`'s manifest walker to have spawned services.
- `await_ready` only polls the IPC service registry.
- `stop(service)` logs that F.4 will issue the `init.cmd` write later.
- `restart(service)` returns `Ack` without doing restart work.

**Consequence:** the session manager can observe a happy path and expose a control surface, but it does not yet own the complete start/stop/restart contract described by the Phase 57 docs.

### 4. GUI reliability depends on active Phase 57a work

`docs/roadmap/57a-scheduler-rewrite.md` exists because display startup can hang under lost-wake races. The active `feat/57a-scheduler-rewrite` work changes the risk profile: scheduler modeling, transition-table work, per-task `pi_lock` infrastructure, watchdog/trace diagnostics, and timeout-unit cleanup are already being addressed off `main`.

Those foundations matter, but the runtime-changing work is still the decisive part for this evaluation. The planned 57a closure needs to:

- Add the v2 block primitive, `block_current_until`, with the condition recheck protocol.
- Rewrite `wake_task` around per-task block state and `Task::on_cpu` publication safety.
- Migrate IPC, notifications, futexes, poll/select/epoll, sleeps, wait queues, and kernel-internal callers away from v1 blocking.
- Delete the old `switching_out`, `wake_after_switch`, and `PENDING_SWITCH_OUT` machinery once migration is complete.
- Fix secondary GUI blockers, including `serial_stdin_feeder_task`, no-hardware `audio.cmd` registration, and the `syslogd` CPU-hog investigation.
- Pass the validation gate: GUI boot, real-hardware runs, scheduler soak, watchdog-clean idle sessions, and relevant host/property tests.

**Expected impact if it lands:** the evaluation should stop treating lost-wake scheduler behavior as a known GUI correctness blocker. Instead, it becomes a validation requirement for the desktop stack: prove graphical startup, terminal input, session recovery, and TUI-style blocking workloads stay live under the new protocol.

**What it does not change:** 57a does not create tiling workflows, launchers, bars, app compatibility, browser runtime support, real AC'97 playback, or full `session_manager` lifecycle ownership. Those remain separate GUI/product work after the scheduler foundation is stable.

### 5. Display server still has Phase 56 follow-throughs

The display server is a strong start, but the current docs and code still leave desktop-relevant gaps:

- Subscription events are recorded but not pushed to subscribers.
- True zero-copy page-grant buffer transport is deferred.
- Layer surfaces exist in the protocol, but active exclusive layer handling is not fully wired through the input pass.
- Bind-triggered control events still carry placeholder `(mask=0, keycode=id)` payloads in one path.
- `mouse_server` dependency direction remains a documented follow-up.

These are acceptable for a milestone compositor, but not for an Omarchy-class workflow.

## Current phase grade

| Dimension | Grade | Rationale |
|---|---|---|
| Architecture | High | The service split, protocols, capability boundaries, and documentation are directionally right. |
| Runtime completeness | Medium-low | Several production paths still stub out the load-bearing behavior. |
| Validation strength | Low-medium | Host tests exist, but the smoke gates do not yet prove the full user-facing contract. |
| Usable GUI readiness | Low | No tiling policy, launcher, bar, clipboard, notifications, lockscreen, app ecosystem, or stable key workflow yet. |
| Best next action | Close correctness gaps before adding polish | Finish real audio or a no-op audio fallback, complete session lifecycle control, land and validate Phase 57a scheduler fixes, then build the tiling policy layer. |

## Immediate closure checklist

1. Make `audio_server` production-backed:
   - Allocate and map BDL + PCM DMA ring through the Phase 55b/55a device-host path.
   - Write PCM payloads into the ring.
   - Program BDBAR/LVI/CR.
   - Advance consumed counters on IRQ.
   - Expose stats through the control surface.

2. Replace shallow smoke gates:
   - `audio-smoke`: `audio-demo` round trip plus stats progress.
   - `session-smoke`: state running plus visible `term` readiness plus an injected key reaching the PTY.
   - `session-recover-smoke`: crash or stop `display_server`, verify reverse stop order, framebuffer fallback, serial/admin reachability.

3. Finish `session_manager` lifecycle ownership:
   - Implement `init.cmd` start/stop/restart writes or a cleaner supervisor IPC.
   - Poll `/run/services.status` in addition to IPC registry.
   - Make `session-stop` and `session-restart` observable from `m3ctl`.

4. Land and validate the Phase 57a scheduler rewrite or an equivalent fix:
   - `block_current_until` and the rewritten `wake_task` should be the only block/wake path for migrated callers.
   - `switching_out`, `wake_after_switch`, and `PENDING_SWITCH_OUT` should be gone or provably unreachable.
   - No known display/input lost-wake hang should remain in `run-gui`.
   - Poll/select/epoll timeouts should match wall clock.
   - `audio_server` should not force text fallback when AC'97 is absent unless the chosen product policy requires audio.

5. Update Phase 57 docs with a reality-status section:
   - Keep the design docs as the intended architecture.
   - Add an explicit implementation-status table to avoid future readers mistaking scaffolded paths for closed runtime behavior.
