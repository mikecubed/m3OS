# Post-Phase 57 Evaluation

**Date:** 2026-04-29
**Baseline:** `main` at `449fc05165868a22e756038b50ccc55981291fcd`
**Worktree:** `/home/mikecubed/projects/ostest-post-phase-57-evaluation`
**Scope:** Evaluate Phase 57 progress, what remains for an Omarchy / Hyprland-style usable GUI, and what it would take to run real graphical and TUI applications.

## Bottom line

Phase 57 gives m3OS the right *shape* for a local graphical system: a userspace display server, typed input path, session manager, graphical terminal, audio service surface, and control-plane hooks. That is a real milestone, but it is not yet a usable desktop.

The current `main` branch is best described as **architecture-complete and smoke-connected**, not fully functional. Several Phase 57 components still contain production stubs or validation shortcuts:

- `audio_server` has the protocol and pure AC'97 helper logic, but the production `Ac97Backend` still does accounting-only submission and reports no IRQ events.
- `audio-smoke` verifies that `audio_server.conf` loads, not that `audio-demo` produces PCM that the device consumes.
- `session_manager` observes service registration and owns a control surface, but direct start/stop/restart integration with `init` is still passive or logged as future F.4 work.
- The GUI stack still depends on the planned Phase 57a scheduler rewrite to eliminate known lost-wake freezes in graphical startup.

For a usable Omarchy-like GUI, the realistic path is still the one documented in `docs/appendix/gui/tiling-compositor-path.md`: build a **native m3OS tiling compositor policy layer** on top of the Phase 56 display server, rather than porting Hyprland itself.

## Documents

1. [Phase 57 Progress Ledger](./01-phase-57-progress.md)
2. [Usable GUI Roadmap](./02-usable-gui-omarchy-hyprland-roadmap.md)
3. [Real Applications and Browser Roadmap](./03-real-applications-browser-roadmap.md)
4. [TUI and Neovim Roadmap](./04-tui-and-neovim-roadmap.md)

## Decision summary

| Question | Short answer |
|---|---|
| Is Phase 57 done enough to build on? | Yes for architecture and protocols; no for product-level reliability. Close the stubs and validation gaps first. |
| Should m3OS port Hyprland? | Not now. Hyprland implies the Linux Wayland, DRM/KMS, EGL/GLES, Mesa, libinput, and C++ runtime stack. A native tiling compositor gives most visible value at far lower cost. |
| What is the next GUI milestone? | Stabilize Phase 57 + Phase 57a, then add a native tiling workspace/layout/keybind policy layer, default Omarchy-like keybindings, and native bar/launcher/notification/lock clients. |
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

