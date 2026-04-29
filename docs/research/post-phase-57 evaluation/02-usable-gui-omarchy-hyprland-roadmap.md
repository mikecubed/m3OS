# Usable GUI Roadmap: Omarchy / Hyprland Style

**Date:** 2026-04-29
**Baseline:** `main` at `4c72e34` (`Phase 57a: scheduler block/wake protocol rewrite`, merged 2026-04-29)
**Goal:** Define what remains to turn the Phase 56/57 graphical session into a usable, keyboard-driven tiled desktop experience.

## Product target

The right target is not "run Hyprland." The right target is:

> A native m3OS compositor policy layer that gives the Omarchy / Hyprland workflow: tiled windows, workspaces, scratchpad, launcher, bar, useful global shortcuts, native terminal-first operation, and enough visual polish to feel intentional.

This matches `docs/appendix/gui/tiling-compositor-path.md` Goal A. Hyprland itself is a Wayland compositor with Linux rendering/input/runtime assumptions that m3OS does not have. Porting it would pull in Wayland, DRM/KMS, EGL/GLES, Mesa or a GPU stack, libinput, libxkbcommon, dynamic linking, and a C++ runtime before any window-management code matters.

## Omarchy reference model

Current Omarchy is an Arch Linux distribution built around Hyprland. Its manual describes a developer workstation with Neovim, Chromium, terminal, office/media apps, and a menu-driven setup flow. Its current package list includes `alacritty`, `chromium`, `nvim`, `omarchy-nvim`, `omarchy-walker`, `waybar`, `mako`, `tmux`, `btop`, `lazygit`, `lazydocker`, `ripgrep`, `fzf`, and many full desktop applications.

The relevant part for m3OS is the workflow:

- `Super+Return`: terminal
- `Super+Space`: app launcher
- `Super+Alt+Space`: system/Omarchy menu
- `Super+K`: keybinding help
- `Super+1..0`: switch workspaces
- `Super+Shift+1..0`: move active window to workspace
- `Super+Shift+Alt+1..0`: move active window silently
- `Super+Arrow`: focus neighboring window
- `Super+Shift+Arrow`: swap active window
- `Super+S`: scratchpad
- `Super+Alt+S`: move active window to scratchpad
- `Super+W`: close window
- `Super+F`: fullscreen
- `Super+T`: floating/tiled toggle
- `Super+J`: split orientation
- `Super+L`: workspace layout toggle
- `Alt+Tab`: cycle windows
- `PrintScreen`: screenshot
- `Super+Ctrl+A/B/W/T`: audio, Bluetooth, Wi-Fi, activity controls
- `Super+C/V/X`: universal copy/paste/cut

m3OS should copy the **shape and defaults**, not Hyprland's implementation or exact config language.

## Required compositor capabilities

| Capability | Current m3OS substrate | Remaining work |
|---|---|---|
| One display owner | `display_server` exists and owns framebuffer. | Stabilize crash/restart and frame pacing. |
| Surface protocol | Native protocol exists with toplevel/layer/cursor roles. | Finish zero-copy buffer transport and server-initiated events. |
| Layout policy seam | `LayoutPolicy` and `FloatingLayout` exist. | Add tiling layout implementations and policy switching. |
| Input before client dispatch | Bind table / grab hook exists. | Add default keymap, chord engine, config reload, and mode tables. |
| Control socket | `display-control` and `m3ctl` exist. | Add workspace/window/layout verbs and event push. |
| Layer clients | `Layer` role is defined. | Build actual bar, launcher, notifications, lockscreen, overlays. |
| Terminal app | `term` exists. | Make it robust enough for TUI workflows. |
| Audio | `audio_client` and `audio_server` exist; `audio_server` now has a no-hardware silent `audio.cmd` stub. | Finish real AC'97 playback and driver-level smoke coverage. |
| Session manager | `session_manager` exists. | Make it a real lifecycle owner and add keyboard recovery path. |

## Phase 57a and 57b impact

Phase 57a is now merged and materially improves the scheduler foundation for this roadmap. It removes the known v1 lost-wake class by introducing `block_current_until`, CAS-style `wake_task_v2`, `Task::on_cpu` publication safety, wait-call-site migration, watchdog/trace diagnostics, and timeout-unit fixes. It also lands user-visible secondary fixes: no-hardware `audio.cmd` registration, `serial_stdin_feeder_task` migration, and `syslogd` drain chunking.

However, the Phase 57a validation gate records that the real-hardware GUI startup test still fails. The diagnosis changed: it is no longer the old lost-wake protocol, but cooperative-scheduling starvation. Timer IRQs and reschedule IPIs set the per-core reschedule flag, yet IRQ return resumes the interrupted user code; the scheduler only runs when the task voluntarily yields, blocks, or enters a yielding syscall path. That can starve `display_server`, `term`, storage, logging, or input work queued on the same core.

The follow-up listed in `docs/appendix/preemptive-multitasking.md` is therefore part of the GUI reliability floor:

- 57b: preemption foundation, full register-save state, `preempt_count`, and lock discipline, with no behavior change yet.
- 57c: user-mode timer/IPI preemption so CPU-bound user tasks cannot monopolize a core.
- 57d: full kernel preemption after spin-wait and lock audits.

Tiling/keybinding work can still be prototyped now, but it should not be presented as a usable desktop until the post-57a starvation path is closed. Stage 0 should treat scheduler acceptance as: watchdog-clean GUI boot, terminal input, compositor restart recovery, and an idle session that stays live even while another user process burns CPU.

## Native tiling policy layer

Add a compositor policy module, tentatively `m3wm`, inside or alongside `display_server`.

### State model

The compositor needs these first-class objects:

- `Output`: resolution, scale, reserved layer zones, current workspace.
- `Workspace`: id/name, layout mode, root layout tree, focused surface, history.
- `Window`: surface id, role, title/app id when available, floating/tiled/fullscreen/group flags.
- `LayoutTree`: master-stack, dwindle/BSP, grid/columns, tabbed/group containers.
- `Scratchpad`: special workspace visible on demand across outputs.
- `BindMap`: global keybindings, mode bindings, and reloadable config source.

### Layouts

Start with three:

1. Master-stack: easiest to inspect and debug.
2. Dwindle/BSP: Hyprland-like default path; Hyprland documents dwindle as a binary-tree layout.
3. Floating override: needed for launchers, dialogs, screenshots, and manual exceptions.

Add later:

- Tabbed/group containers.
- Per-workspace layout choice.
- Manual preselect split direction.
- Multi-monitor workspace behavior.

### Control verbs

The control socket should grow a stable `m3ctl` surface:

| Verb | Purpose |
|---|---|
| `workspace <id>` | Switch workspace. |
| `move-to-workspace <id>` | Move active window. |
| `move-to-workspace-silent <id>` | Move without following. |
| `focus {left,right,up,down,next,prev}` | Focus movement. |
| `swap {left,right,up,down}` | Swap tiled windows. |
| `toggle-floating` | Float or tile the active window. |
| `fullscreen` | Toggle fullscreen. |
| `toggle-scratchpad` | Show/hide scratchpad. |
| `move-to-scratchpad` | Send active window to scratchpad. |
| `layout <name>` | Select layout for current workspace. |
| `kill-active` | Request close/kill for active window. |
| `list-windows` | Machine-readable window list. |
| `subscribe` | Stream workspace/focus/window/layout events. |

This is the native `hyprctl` equivalent.

## Default keybinding set

Ship a default file, for example `/etc/m3os/gui/bindings.conf`, that maps directly to control verbs and launch commands:

| Key | Action |
|---|---|
| `Super+Return` | launch `/bin/term` |
| `Super+Space` | launch native launcher |
| `Super+Alt+Space` | launch system menu |
| `Super+K` | keybinding overlay |
| `Super+1..0` | switch workspace |
| `Super+Shift+1..0` | move active window to workspace |
| `Super+Shift+Alt+1..0` | move active window silently |
| `Super+Arrow` | focus direction |
| `Super+Shift+Arrow` | swap direction |
| `Super+Tab` | next populated workspace |
| `Super+Shift+Tab` | previous populated workspace |
| `Alt+Tab` | next window on workspace |
| `Super+W` | close active window |
| `Super+F` | fullscreen |
| `Super+T` | toggle floating |
| `Super+J` | toggle split |
| `Super+L` | cycle layout |
| `Super+S` | scratchpad |
| `Super+Alt+S` | move active window to scratchpad |
| `Super+Ctrl+L` | lock |
| `Super+Ctrl+A` | audio control client |
| `Super+Esc` | session/system menu |
| `Ctrl+Alt+F1` | text-fallback/session-stop, after the grab-hook regression exists |

Use exact modifier matching. Do not forward swallowed global chords to clients.

## Native client baseline

These clients make the system feel like a desktop rather than a compositor demo:

| Client | Surface role | First useful behavior |
|---|---|---|
| Bar | `Layer` top with exclusive zone | workspace indicators, focused title, clock, battery/network placeholders, audio state. |
| Launcher | `Layer` overlay or top | fuzzy app command launcher; starts `/bin/term`, `/bin/edit`, `/bin/gfx-demo`, future apps. |
| Keybinding help | `Layer` overlay | generated from the live bind map. |
| Notification daemon | `Layer` top/overlay | structured message display from services and apps. |
| Lock screen | `Layer` overlay with exclusive keyboard | password auth via Phase 27/48 path. |
| Screenshot tool | control verb plus file writer | full-screen capture first, region/window later. |
| Settings menu | launcher/menu client | theme, display, input, session actions. |

Build these as normal clients using `Layer`, control-socket subscriptions, and the native protocol. Avoid baking them into `display_server`.

## Visual polish without GPU

Do now:

- Gaps and borders as layout/composer math.
- Active/inactive border colors.
- Rounded corners via CPU masks if cheap enough.
- Simple fade/slide animations on small damage regions.
- Workspace slide by blitting offset buffers.
- Snapshot blur only for launcher/lock screen if the CPU cost is acceptable.

Defer:

- Live blur.
- Shader effects.
- 4K high-refresh animation guarantees.
- GPU-accelerated composition.

Hyprland's visible identity is not only blur. Workspaces, keyboard flow, scratchpad, gaps/borders, launcher, and bar carry most of the user experience.

## Staged plan

### Stage 0: Stabilize Phase 57

- Finish real audio; keep the Phase 57a no-hardware `audio.cmd` stub as the hardware-absent policy.
- Make session manager lifecycle control real.
- Replace shallow smokes with end-to-end smokes.
- Close post-57a scheduler starvation via Phase 57b/57c or equivalent targeted fixes.
- Finish display event push and keybinding payload correctness.

### Stage 1: Tiling core

- Add workspace state machine.
- Add master-stack and dwindle layouts.
- Add focus/swap/move/fullscreen/floating operations.
- Add `m3ctl` verbs and event subscriptions.
- Acceptance: two `term` windows tile, focus, swap, move to workspace, and survive compositor restart.

### Stage 2: Keybinding workflow

- Default binding file.
- Config parser and reload.
- Bind help overlay.
- Leader/mode tables later.
- Acceptance: all default keys in the table above work in `run-gui`.

### Stage 3: Native shell clients

- Bar, launcher, notification daemon, lockscreen, screenshot.
- Acceptance: boot lands in a tiled terminal workspace with bar and launcher. No serial shell needed for ordinary use.

### Stage 4: Terminal-first productivity

- `term` compatibility hardening.
- Ports for common TUI tools.
- Neovim bring-up.
- Acceptance: edit code in `nvim`, run shell commands, use at least one multiplexer or fuzzy finder.

### Stage 5: Compatibility

- Optional `wl_shm` Wayland backend for specific software clients.
- Native toolkit for m3OS apps.
- Browser research phase only after runtime/graphics prerequisites are real.

## Regression suite

Add QEMU-level GUI regressions for:

- Boot reaches tiled `term`.
- `Super+Return` opens a second terminal and tiles it.
- `Super+1`, `Super+2`, `Super+Shift+2` switch and move windows.
- `Super+S` toggles scratchpad.
- `Super+Space` launches the launcher overlay.
- `Super+K` shows keybindings from live config.
- `Ctrl+Alt+F1` reaches text fallback once the keychord path is added.
- Compositor crash restarts without losing the serial admin path.
- 30-minute idle GUI session has no scheduler stuck-task warnings.

## Sources

- Omarchy manual: <https://learn.omacom.io/2/the-omarchy-manual>
- Omarchy package list: <https://raw.githubusercontent.com/basecamp/omarchy/dev/install/omarchy-base.packages>
- Omarchy Hyprland config: <https://raw.githubusercontent.com/basecamp/omarchy/dev/config/hypr/hyprland.conf>
- Omarchy tiling bindings: <https://raw.githubusercontent.com/basecamp/omarchy/dev/default/hypr/bindings/tiling-v2.conf>
- Omarchy utility bindings: <https://raw.githubusercontent.com/basecamp/omarchy/dev/default/hypr/bindings/utilities.conf>
- Hyprland dispatchers: <https://wiki.hypr.land/Configuring/Dispatchers/>
- Hyprland dwindle layout: <https://wiki.hypr.land/Configuring/Layouts/Dwindle-Layout/>
- Hyprland animations: <https://wiki.hypr.land/Configuring/Advanced-and-Cool/Animations/>
