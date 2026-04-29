# Phase 57 Progress Ledger

**Date:** 2026-04-29
**Baseline:** `main` at `4c72e34` (`Phase 57a: scheduler block/wake protocol rewrite`, merged 2026-04-29)
**Question:** What has Phase 57 actually delivered, and what remains before it can be treated as a stable local GUI baseline?

## Executive assessment

Phase 57 has landed the main process and protocol boundaries:

- `display_server`, `kbd_server`, `mouse_server`, `m3ctl`, and `gfx-demo` from Phase 56 are present.
- `session_manager`, `audio_server`, `audio_client`, `audio-demo`, and `term` are workspace members.
- `kernel-core` has host-testable display, input, audio, and session model modules.
- `xtask` builds and stages the new userspace binaries, and `run-gui` now has AC'97 QEMU flags.
- The docs describe the intended local-session topology and the deferred desktop boundary clearly.
- Phase 57a has now rewritten the scheduler block/wake protocol around `block_current_until`, per-task `pi_lock`, CAS-style `wake_task_v2`, `Task::on_cpu`, watchdog/trace diagnostics, and timeout-unit cleanup.

The problem is that several "complete" labels are ahead of the runtime. The Phase 57/57a code on `main` still has production stubs, shallow smoke checks, and one major scheduler-latency gap: the kernel is still cooperative at IRQ return, so CPU-bound user code or kernel busy-waits can monopolize a core until a voluntary yield/block/syscall boundary. Treat it as a platform checkpoint, not as a finished GUI/audio/session product.

## Delivered surfaces

| Area | What exists on `main` | Notes |
|---|---|---|
| Display ownership | `userspace/display_server` claims the framebuffer, registers `display` and `display-control`, accepts client protocol messages, composes surfaces, drains input, and tracks frame stats. | Good substrate for a native compositor. |
| Input routing | `kbd_server`, `mouse_server`, `kernel-core::input`, bind table, focus dispatch, and event queues exist. | Usable enough for the terminal path, but richer keybind/chord behavior is still later work. |
| Session manager | `userspace/session_manager` exists, registers `session-manager`, owns a control context, runs the ordered startup sequence, and reports `running` or `text-fallback`. | Its `init` integration is still mostly observe-and-log rather than direct lifecycle ownership. |
| Terminal | `userspace/term` exists with PTY, ANSI screen state, renderer, input handler, and bell path. | First useful graphical client shape is present. It still needs terminal compatibility hardening before Neovim/tmux-class apps are credible. |
| Audio protocol | `kernel-core::audio`, `userspace/lib/audio_client`, `audio-demo`, and `audio_server` protocol/stream/client registries exist. | The production AC'97 backend is not yet a real device driver. |
| Scheduler block/wake | Phase 57a adds `TaskBlockState`, per-task `pi_lock`, `block_current_until`, `wake_task_v2`, `Task::on_cpu`, v2 transition tests, watchdog/trace hooks, timeout-unit fixes, and removes the old v1 lost-wake machinery from live code. | This closes the v1 lost-wake class, but not CPU starvation from cooperative scheduling. |
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

Phase 57a did improve the boot story here: `audio_server` now registers `audio.cmd` even when AC'97 is absent and falls back to a silent stub loop. That prevents `session_manager` from taking text fallback solely because no audio controller exists. It does not make real PCM playback work.

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

### 4. Phase 57a fixed lost-wake, but not pre-emption

`docs/roadmap/tasks/57a-scheduler-rewrite-tasks.md` now marks Phase 57a complete in-tree. The meaningful runtime changes have landed:

- `block_current_until` is the unified condition-recheck blocking primitive.
- `wake_task_v2` performs CAS-style blocked-to-ready transitions under per-task block state.
- `Task::on_cpu` replaces the RSP-publication part of the old `PENDING_SWITCH_OUT` scheme.
- IPC, notifications, futexes, I/O multiplexing, sleeps, wait queues, and internal waiters migrated to the v2 protocol.
- The old `switching_out` / `wake_after_switch` / `PENDING_SWITCH_OUT` lost-wake machinery is absent from live code.
- Poll/select/epoll timeout units now match the 1 kHz scheduler tick.
- `serial_stdin_feeder_task`, `audio_server` no-hardware registration, and `syslogd` drain chunking received secondary fixes.

That is a real foundation improvement. It changes the scheduler risk from "known lost-wake protocol bug" to "known cooperative-scheduling starvation bug."

`docs/handoffs/57a-validation-gate.md` records the important result: the real-hardware graphical stack gate still fails. The root cause is not the old v1 lost-wake class; it is that timer IRQs and reschedule IPIs set `reschedule`, but IRQ return goes back to the interrupted user code. The scheduler only runs when the task voluntarily yields, blocks, or reaches a syscall path that yields. A CPU-bound user task or kernel busy-wait can therefore monopolize its core.

The follow-up is tracked in `docs/appendix/preemptive-multitasking.md` as Phase 57b/57c/57d:

- 57b: full register-save/preemption state and `preempt_count` foundation, no behavior change.
- 57c: user-mode timer/IPI preemption so CPU-bound user code cannot starve its core.
- 57d: full kernel preemption after spin-wait and lock discipline audits.

**What 57a does not change:** it does not create tiling workflows, launchers, bars, app compatibility, browser runtime support, real AC'97 playback, full `session_manager` lifecycle ownership, or pre-emptive multitasking. Those remain separate GUI/product work after the scheduler foundation is stable.

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
| Runtime completeness | Medium-low | Several production paths still stub out the load-bearing behavior, and preemption is not implemented. |
| Validation strength | Low-medium | Host tests and scheduler models exist, but the smoke gates do not yet prove the full user-facing contract; the 57a real-hardware GUI gate currently fails. |
| Usable GUI readiness | Low | No tiling policy, launcher, bar, clipboard, notifications, lockscreen, app ecosystem, or stable key workflow yet. |
| Best next action | Close correctness gaps before adding polish | Resolve post-57a scheduler starvation via 57b/57c or targeted busy-wait fixes, finish real audio, complete session lifecycle control, then build the tiling policy layer. |

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

4. Resolve the post-57a scheduler starvation gap:
   - Implement the Phase 57b preemption foundation from `docs/appendix/preemptive-multitasking.md` or close the specific busy-wait/syscall monopolies that block GUI boot.
   - Add a user-mode preemption gate in the 57c shape before claiming desktop reliability.
   - Keep `block_current_until` / `wake_task_v2` as the only scheduler wait/wake path for migrated callers.
   - No known display/input starvation or stuck-task warning should remain in `run-gui`.
   - Poll/select/epoll timeouts should continue matching wall clock.

5. Update Phase 57 docs with a reality-status section:
   - Keep the design docs as the intended architecture.
   - Add an explicit implementation-status table to avoid future readers mistaking scaffolded paths for closed runtime behavior.
