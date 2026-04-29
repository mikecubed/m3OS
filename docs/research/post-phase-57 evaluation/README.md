# Post-Phase 57 Evaluation

**Date:** 2026-04-29
**Baseline:** `main` at `4c72e34` (`Phase 57a: scheduler block/wake protocol rewrite`, merged 2026-04-29)
**Worktree:** `/home/mikecubed/projects/ostest-post-phase-57-evaluation`
**Scope:** Evaluate Phase 57 progress, what remains for an Omarchy / Hyprland-style usable GUI, and what it would take to run real graphical and TUI applications.

## Bottom line

Phase 57 gives m3OS the right *shape* for a local graphical system: a userspace display server, typed input path, session manager, graphical terminal, audio service surface, and control-plane hooks. That is a real milestone, but it is not yet a usable desktop.

The current `main` branch is best described as **architecture-complete, scheduler-rewritten, and smoke-connected**, not fully functional. Phase 57a materially improves the foundation by removing the old lost-wake protocol, but the real-hardware GUI gate still fails because m3OS does not yet pre-empt running user code on timer IRQ return. Several Phase 57 components still contain production stubs or validation shortcuts:

- `audio_server` now has a no-hardware `audio.cmd` stub so sessions no longer fall back to text solely because AC'97 is absent, but the production `Ac97Backend` still does accounting-only submission and reports no IRQ events.
- `audio-smoke` verifies that `audio_server.conf` loads, not that `audio-demo` produces PCM that the device consumes.
- `session_manager` observes service registration and owns a control surface, but direct start/stop/restart integration with `init` is still passive or logged as future F.4 work.
- The Phase 57a v2 block/wake protocol is now in-tree and eliminates the known v1 lost-wake class, but `docs/handoffs/57a-validation-gate.md` records that the user-hardware graphical gate still fails due to cooperative-scheduling starvation.

Phase 57a changes the diagnosis, not the product verdict. The merged branch adds transition tables and host models, `TaskBlockState` / per-task `pi_lock`, `block_current_until`, CAS-style `wake_task_v2`, `Task::on_cpu` publication safety, call-site migration away from v1 blocking, timeout-unit cleanup, scheduler watchdog/trace work, `serial_stdin_feeder` migration, `syslogd` drain chunking, and the no-hardware `audio.cmd` fallback.

The remaining scheduler gap is now Phase 57b: pre-emptive multitasking. The new `docs/appendix/preemptive-multitasking.md` notes that timer IRQs set the per-core reschedule flag, but IRQ return goes back to the interrupted user code; the scheduler only runs at voluntary yield/block/syscall points. That means a CPU-bound task or kernel busy-wait can monopolize its core and starve display, terminal, storage, or logging work queued behind it. Phase 57b/57c are the foundation needed before this can be called a robust desktop baseline.

57a does not deliver tiling, session lifecycle ownership, real AC'97 playback, a launcher/bar, application compatibility, browser support, or pre-emption. Those remain separate GUI/product work after the scheduler foundation is stable.

For a usable Omarchy-like GUI, the realistic path is still the one documented in `docs/appendix/gui/tiling-compositor-path.md`: build a **native m3OS tiling compositor policy layer** on top of the Phase 56 display server, rather than porting Hyprland itself.

## Documents

1. [Phase 57 Progress Ledger](./01-phase-57-progress.md)
2. [Usable GUI Roadmap](./02-usable-gui-omarchy-hyprland-roadmap.md)
3. [Real Applications and Browser Roadmap](./03-real-applications-browser-roadmap.md)
4. [TUI and Neovim Roadmap](./04-tui-and-neovim-roadmap.md)

## Decision summary

| Question | Short answer |
|---|---|
| Is Phase 57 done enough to build on? | Yes for architecture, protocols, and the v2 block/wake substrate; no for product-level reliability. Close the preemption/starvation, audio, session, and validation gaps first. |
| Should m3OS port Hyprland? | Not now. Hyprland implies the Linux Wayland, DRM/KMS, EGL/GLES, Mesa, libinput, and C++ runtime stack. A native tiling compositor gives most visible value at far lower cost. |
| What is the next GUI milestone? | Stabilize the post-57a runtime, address Phase 57b preemption or equivalent starvation fixes, then add a native tiling workspace/layout/keybind policy layer, default Omarchy-like keybindings, and native bar/launcher/notification/lock clients. |
| What is the first real app strategy? | Native m3OS apps and TUI apps first; simple software Wayland clients later via a `wl_shm` adapter; full browsers much later. |
| Can Neovim happen before a browser? | Yes. It is still non-trivial, but it is a much smaller target than Chromium or Firefox and aligns with the existing PTY/terminal work. |

## External references used

- Omarchy manual: <https://learn.omacom.io/2/the-omarchy-manual>
- Omarchy default package list: <https://raw.githubusercontent.com/basecamp/omarchy/dev/install/omarchy-base.packages>
- Omarchy Hyprland defaults: <https://raw.githubusercontent.com/basecamp/omarchy/dev/config/hypr/hyprland.conf>
- Omarchy tiling bindings: <https://raw.githubusercontent.com/basecamp/omarchy/dev/default/hypr/bindings/tiling-v2.conf>
- Omarchy utility bindings: <https://raw.githubusercontent.com/basecamp/omarchy/dev/default/hypr/bindings/utilities.conf>
- Hyprland dispatchers: <https://wiki.hypr.land/Configuring/Dispatchers/>
- Hyprland dwindle layout: <https://wiki.hypr.land/Configuring/Layouts/Dwindle-Layout/>
- Hyprland animations: <https://wiki.hypr.land/Configuring/Advanced-and-Cool/Animations/>
- Chromium Linux build requirements: <https://chromium.googlesource.com/chromium/src/+/main/docs/linux/build_instructions.md>
- Firefox Linux build requirements: <https://firefox-source-docs.mozilla.org/setup/linux_build.html>
- Neovim build docs: <https://neovim.io/doc/build/>
