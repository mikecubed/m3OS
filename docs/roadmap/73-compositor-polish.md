# Phase 73 - Compositor: Polish (Bar, Launcher, Notifications, Animations, Lockscreen Stub)

**Status:** Planned
**Source Ref:** phase-73
**Depends on:** Phase 72 (Compositor: Multi-Toplevel, Tiling Layout, and Workspaces) ✅, Phase 71 (Greeter and Multi-User Session) ✅
**Builds on:** Adds the native client ecosystem and visual animation layer on top of Phase 72's tiling compositor, delivering the full omarchy/Hyprland-aesthetic desktop experience in software rendering
**Primary Components:** `userspace/bar`, `userspace/launcher`, `userspace/notifyd`, `userspace/lockscreen`, `userspace/display_server` (animation engine), `display_server` decoration layer

## Milestone Goal

m3OS gains a complete omarchy-aesthetic desktop: a persistent status bar showing workspace indicators, window title, and clock; a `SUPER+SPACE` fuzzy launcher; a notification daemon with timed pop-ups; software-rendered window-open/workspace-slide animations; optional rounded corners and drop shadows; and a lockscreen stub that blocks input until dismissed. Every component is a native m3OS client; none requires Wayland.

## Why This Phase Exists

Phase 72 delivers the structural tiling compositor — layout, workspaces, chord bindings, control socket. But a bare tiling compositor with no visible status context, no quick launch path, and no animation feels like an unfinished prototype. The omarchy aesthetic is defined as much by its smooth transitions and minimal chrome as by its tiling geometry.

This phase exists to close the gap from "working tiling WM" to "desktop you would actually want to use daily." The status bar, launcher, and notification daemon are all normal clients of the Phase 72 control socket and Layer surface role; the animation engine is an internal extension of `display_server`'s compose loop. None of this work touches the kernel.

## Learning Goals

- Understand how a status bar and launcher are compositor clients, not part of the compositor
- Learn how an animation engine ties frame scheduling to vblank timing and produces damage regions
- See how alpha masking achieves rounded corners without GPU shaders
- Understand how a notification daemon uses Layer-shell semantics to render above normal windows with an exclusive zone
- Learn the minimum viable lockscreen integration model without a PAM-equivalent

## Feature Scope

### Status bar client

`userspace/bar/` is a Layer-shell client that anchors to the top of the primary output with an exclusive zone equal to its height. It subscribes to the Phase 72 control socket for `workspace-changed` and `window-focused` events. It renders workspace number indicators (highlighting the active one), the focused window title, a wall-clock time string (from `CLOCK_REALTIME`), and an audio mute indicator (queried from the Phase 63 audio server). No toolkit; direct pixel drawing into a shared compositor surface.

### Launcher client

`userspace/launcher/` is a floating Toplevel opened by `SUPER+SPACE` (Phase 72 keybind). It scans `/usr/bin` and `/usr/local/bin` for executable files, presents a fuzzy-filtered list updated on every keystroke, and executes the selected binary via `execve` on Return. On Escape or selection it closes itself. Rendered as a centered overlay with a configurable width and maximum height.

### Notification daemon

`userspace/notifyd/` listens on AF_UNIX `/run/notifyd.sock`. Any process can open the socket, send a framed notification message `{"title": "...", "body": "...", "timeout_ms": 5000}`, and disconnect. `notifyd` renders a Layer-shell surface at `Top+Right` with a small exclusive zone; each notification is a small panel that auto-dismisses after its timeout and slides out. A `notify-send` utility binary is the reference client.

### Animation engine

An internal `AnimationEngine` struct in `display_server` owns a list of active animations and is driven once per compose frame. Each animation has a timing curve (linear, ease-out, spring-like approximation), a current time offset, a start and end value for the animated property (position, opacity, or scale), and a damage rectangle. On every frame the engine advances all animations, computes interpolated values, marks their damage regions dirty, and signals the compose loop to re-blit affected areas. Frame scheduling is tied to the vblank timer introduced in Phase 72. Animations: window-open slide+fade, window-close fade, workspace-switch horizontal slide, window-move smooth reposition. Live blur is not included (deferred to GPU phase).

### Decoration layer

The compose loop grows an optional decoration pass: rounded corners (per-edge alpha mask applied after surface blit), drop shadows (precomputed alpha overlay table, applied per-window), and configurable title bar height (default zero — omarchy aesthetic). All decoration parameters come from `/etc/compositor.conf`. Shadows are precomputed once per window size change, not per frame.

### Lockscreen stub

`userspace/lockscreen/` is a minimal Layer-shell client that covers the entire output with a black surface and displays "Locked — press Enter" text. It does not implement PAM or password verification in this phase; it exits on Enter. The full lockscreen with credential verification is deferred to a Phase 71b or post-1.0 phase. The stub is enough to allow `session_manager` to invoke it and have a real "locked" visual state. `display_server` honors the lockscreen's grab by delivering no input events to any other surface while the lockscreen Layer surface is the topmost with keyboard-interactivity.

### Background customization

A `userspace/wallpaper/` Layer-shell client at the `Bottom` anchor loads a raw RGBA image file from a configured path and blits it as the desktop background. Replaces the solid-color fill that Phase 56 used as its background. Reloads on `SIGTERM` (which `m3ctl` can send via the session manager).

## Important Components and How They Work

### `AnimationEngine`

Lives in `userspace/display_server/src/animation.rs`. Called once per frame from `compose_frame`. Each `Animation` entry stores: `target: AnimationTarget` (a surface position, an opacity value, or a geometry rect), `curve: Curve` (enum: Linear, EaseOut, Spring), `start: f32`, `end: f32`, `elapsed_ms: u32`, `duration_ms: u32`. The engine steps each animation's `elapsed_ms` by the frame delta, interpolates, applies the result to the associated surface's compose parameters, and records the union of all animated surface rects as the dirty region for the frame. Animations that complete are removed.

### Layer surface rendering order

The Phase 56 compositor already has `Layer` surface roles with `Top`/`Bottom` anchor and exclusive-zone semantics. This phase wires the `bar` (Top), `launcher` (floating Toplevel above tiled), `notifyd` (Top+Right), `lockscreen` (Top, fullscreen), and `wallpaper` (Bottom) into that existing order. No new compositor protocol changes are needed.

### Fuzzy finder in `launcher`

A pure-Rust substring scorer over a `Vec<String>` collected by reading directory entries from `/usr/bin` and `/usr/local/bin` at launch time. The scorer ranks by: (1) prefix match, (2) subsequence match score (longer common subsequence wins), (3) alphabetical tiebreak. The list is re-filtered on every keystroke with O(N) pass over the pre-collected entries. N ≈ a few hundred in practice.

### `notify-send` utility

A minimal binary in `userspace/notifyd/` (or a separate `userspace/notify-send/`) that opens `/run/notifyd.sock`, sends one framed JSON notification, and exits. Used by scripts and by the smoke test.

## How This Builds on Earlier Phases

- Extends Phase 72's Layer surface role to host `bar`, `notifyd`, `lockscreen`, and `wallpaper` clients
- Extends Phase 72's control socket subscription model with the `window-focused` and `workspace-changed` events consumed by `bar`
- Extends Phase 72's compose loop with the animation engine and decoration pass
- Reuses Phase 57's PTY and ANSI infrastructure inside `term`; `bar` does not use a PTY
- Reuses Phase 63's audio server query path for the mute indicator in `bar`
- Phase 71's auth flow is the eventual target for the real lockscreen; this phase provides a visual stub only

## Implementation Outline

1. Implement `AnimationEngine` and wire it into the Phase 72 compose loop with vblank-aligned frame scheduling
2. Add rounded-corner alpha mask generation and drop-shadow precomputation to the decoration pass
3. Write `userspace/bar/`: Layer-shell surface, control-socket subscriber, workspace/title/clock/audio rendering
4. Write `userspace/launcher/`: floating Toplevel, directory scan, fuzzy filter, `execve` on selection
5. Write `userspace/notifyd/`: AF_UNIX listener, Layer-shell client, timed notification panels, `notify-send` binary
6. Write `userspace/lockscreen/`: Layer-shell fullscreen surface, input grab, placeholder text
7. Write `userspace/wallpaper/`: Layer-shell Bottom surface, RGBA image load and blit
8. Add all five new binaries to `xtask` build pipeline, ramdisk embedding, and ext2 service configs
9. Wire `session_manager` to start `bar`, `notifyd`, and `wallpaper` in the boot sequence
10. Validate the full scenario: bar visible, `SUPER+SPACE` launcher, `notify-send` pop-up, animated window open

## Acceptance Criteria

- Status bar is visible at the top of the screen with workspace indicators, focused window title, and a live clock; the active workspace indicator updates immediately on `SUPER+1..9`
- `SUPER+SPACE` opens the launcher; typing three characters filters the list; pressing Return launches the selected binary; `Escape` closes the launcher
- `notify-send "Test" "Hello world"` produces a pop-up at the top-right that dismisses after its default timeout (5 s)
- A window-open animation (slide+fade) is visible at 60 fps on the QEMU framebuffer with no tearing artifacts
- Rounded corners and drop shadows are configurable in `/etc/compositor.conf` and take effect after `m3ctl reload`
- `session_manager` starts `lockscreen` on `m3ctl lock`; while the lockscreen is active, no keystrokes reach other surfaces; pressing Enter dismisses it

## Companion Task List

- [Phase 73 Task List](./tasks/73-compositor-polish-tasks.md)

## How Real OS Implementations Differ

- Waybar, fuzzel, and mako are independent projects with their own dependencies (gtk3/4, pango, cairo, wayland-protocols); m3OS's equivalents are purpose-built with direct pixel blitting and no toolkit
- Hyprland's animation engine uses GPU-side interpolation and composites in GLES2 shader passes; m3OS interpolates on the CPU and marks damage regions for CPU blit
- Real lockscreens (swaylock, hyprlock) integrate with PAM for password verification, support multiple authentication modules, and handle multi-monitor correctly; the stub here does none of that
- Production notification daemons implement the full D-Bus org.freedesktop.Notifications interface; m3OS uses a simpler AF_UNIX framed JSON protocol

## Deferred Until Later

- Live background blur behind the launcher and lockscreen (requires GPU; see `tiling-compositor-path.md`)
- Full lockscreen with PAM-equivalent credential verification (Phase 71b)
- Clipboard manager (cross-app copy/paste infrastructure not yet in scope)
- Screenshot utility (`m3ctl screenshot` as a control-socket subcommand — straightforward addition post-Phase 73)
- Multi-monitor bar instances and per-monitor workload indicators
- Touch / gesture support for swipe-to-switch-workspace
- Real-time shader effects and color grading (GPU phase prerequisite)
