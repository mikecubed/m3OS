# Compositor: Polish (Bar, Launcher, Notifications, Animations, Lockscreen Stub) (Phase 73)

**Aligned Roadmap Phase:** Phase 73
**Status:** Complete
**Source Ref:** phase-73
**Supersedes Legacy Doc:** new

## Overview

Phase 72 delivered the structural tiling compositor: a Phase 56
`display_server` extended with workspaces, layout policies, chord
bindings, and a control socket. What Phase 72 did *not* deliver is the
visible chrome that turns a tiling WM into a desktop you can use daily.
A user logging in saw bare tiles on a teal background, with no clock,
no quick-launch path, and no notifications.

Phase 73 closes the gap. It adds five small native clients —
`wallpaper`, `bar`, `launcher`, `notifyd`, `lockscreen` — plus a
shared boilerplate crate (`desktop_client`), an internal
`AnimationEngine` and a `decoration` module inside `display_server`.
Critically, every visible piece lives *outside* the compositor: each
client is a regular `display_server` consumer that uses the existing
Layer-shell and Toplevel surface roles. The compositor itself gains
nothing more than two new internal modules and one keybind handler.

That separation is the architectural point of the phase. SRP means a
crash in `bar` does not bring down focus dispatch; a hang in `notifyd`
does not freeze the framebuffer; a developer iterating on the launcher
does not have to rebuild the compositor. The trade-off is the per-client
boilerplate, which is why we extracted `desktop_client` to absorb it.

## What This Doc Covers

- The Phase 73 `desktop_client` crate
  (`userspace/lib/desktop_client/`) — the handshake, SHM allocation,
  bitmap-text drawing helpers consumed by all four new clients.
- The `AnimationEngine` module
  (`userspace/display_server/src/animation.rs`) — `Curve` types,
  per-animation timers, and the `tick(delta) → DamageRegion` contract.
- The decoration layer
  (`userspace/display_server/src/decoration.rs`) — `RoundedCornerMask`
  and `DropShadow`.
- The status bar client (`userspace/bar/`).
- The fuzzy-filter launcher (`userspace/launcher/`) and the
  `SUPER+SPACE` chord binding in `keybind.rs`.
- The notification daemon (`userspace/notifyd/`) and its companion
  `notify-send` CLI binary.
- The lockscreen stub (`userspace/lockscreen/`) and the
  `m3ctl lock` verb.
- The desktop background client (`userspace/wallpaper/`).
- The build-pipeline wiring (`xtask`, ramdisk, ext2 disk, init
  `KNOWN_CONFIGS`).

## Core Implementation

### Native clients use Phase 56 protocol, not a toolkit

There is no widget library. Each client allocates a shared-memory
buffer, calls `Hello` + `CreateSurface` + `SetSurfaceRole`, then blits
BGRA8888 pixels directly into the buffer and sends `AttachSharedBuffer`
+ `DamageSurface` + `CommitSurface`. The compositor composes the
result into the framebuffer.

`desktop_client::DisplayConnection` wraps the connect-and-handshake
walk. `desktop_client::SharedSurface::allocate(w, h)` creates the SHM
backing. `desktop_client::draw_text` walks the
`kernel_core::session::font::BasicBitmapFont` 8×16 glyphs and writes
each pixel into the buffer.

### Animation pipeline

`AnimationEngine::tick(frame_delta_ms)` advances every in-flight
animation's `elapsed_ms`, returns the union of every animation's
damage rectangle, and removes completed entries. The four animations
called out in the spec — `WindowOpen`, `WindowClose`, `WorkspaceSwitch`,
`WindowMove` — each have a default curve and duration. `Curve` is an
enum with `Linear`, `EaseOut`, and `Spring` variants; each evaluates a
normalized time `t ∈ [0, 1]` to a curve-eased value.

The engine is pure logic. It owns no framebuffer state and issues no
syscalls. The composer is the integration point: after the existing
`compose_frame` runs, the engine reports its current `DamageRegion`,
which gets unioned with the existing damage tracker so the next blit
covers both surface deltas and animation steps.

### Decoration pass

`RoundedCornerMask::new(radius)` precomputes an alpha-ramp table over a
`radius × radius` quadrant. `apply(pixels, surface_rect, background)`
walks the four corners, mirroring the ramp through the centre and
blending each corner pixel toward `background`. `DropShadow::compute(w,
h, blur_radius, color)` precomputes a (`w + 2*blur_radius`,
`h + 2*blur_radius`) alpha buffer with squared-distance falloff. Both
are configurable via `[decorations]` in `/etc/compositor.conf`. Zero
radius / zero blur disables the pass entirely.

### `SUPER+SPACE` launcher

The chord lives in `userspace/display_server/src/keybind.rs` as
`KeybindAction::LaunchLauncher`, registered against `SUPER+SPACE` in
`register_default_chords`. The action handler in `main.rs` forks an
unprivileged child that `execve`s `/bin/launcher`. The launcher itself
opens a 600×400 floating Toplevel centred on the primary output, scans
`/usr/bin` + `/usr/local/bin` + `/bin` via `getdents64`, presents the
filtered list re-scored on every keystroke, and `execve`s the selected
binary on Return.

### Notification daemon

`notifyd` listens on `AF_UNIX` `/run/notifyd.sock` (`SOCK_STREAM`).
Clients send a 4-byte little-endian length prefix followed by a UTF-8
JSON body with `title`, `body`, and `timeout_ms` fields. Each
notification renders as a panel anchored at `Top+Right` with a soft
title color and wraps long bodies at 40 characters per row. Panels
auto-dismiss after their `timeout_ms`.

`notify-send` is a tiny CLI that connects, writes one frame, and
exits. Used by the smoke test and any script that wants to surface a
pop-up.

### Lockscreen stub

`m3ctl lock` forks `/bin/lockscreen`, which requests a full-output
Layer surface with `KeyboardInteractivity::Exclusive`. The compositor
already honours the grant: every keystroke goes to the lockscreen
until it exits. The stub draws a centred "Locked — press Enter to
unlock" message and exits when Enter is pressed. Real credential
verification is deferred to a Phase 71b follow-up.

### Wallpaper

The wallpaper client maps a Background-layer surface anchored to all
four edges and fills it from a configured RGBA image (12-byte magic
header + raw pixels). Missing path or decode failure falls back to a
configurable solid colour. `SIGHUP` triggers a re-read of the config
file; `SIGTERM` exits.

## Key Files

| File | What lives there |
|---|---|
| `userspace/display_server/src/animation.rs` | `AnimationEngine`, `Curve`, `Animation`, `DamageRegion` |
| `userspace/display_server/src/decoration.rs` | `RoundedCornerMask`, `DropShadow`, `DecorationConfig` |
| `userspace/display_server/src/keybind.rs` | `KeybindAction::LaunchLauncher` + `SUPER+SPACE` chord |
| `userspace/display_server/src/config.rs` | `[decorations]` + `[wallpaper]` parsers |
| `userspace/lib/desktop_client/src/lib.rs` | `DisplayConnection`, `SharedSurface`, text-drawing helpers |
| `userspace/bar/src/main.rs` | Status bar render loop + clock |
| `userspace/launcher/src/main.rs` | Directory scan + fuzzy filter + Toplevel UI |
| `userspace/notifyd/src/main.rs` | AF_UNIX listener + Layer-shell panels |
| `userspace/notifyd/src/bin/notify_send.rs` | CLI client |
| `userspace/lockscreen/src/main.rs` | Layer-shell + exclusive-keyboard grab |
| `userspace/wallpaper/src/main.rs` | Background-layer client + RGBA loader |

## How This Builds on Earlier Phases

- Phase 56 surface protocol (`Hello`, `CreateSurface`,
  `SetSurfaceRole`, `AttachSharedBuffer`) — every Phase 73 client
  speaks it unchanged.
- Phase 56 Layer-shell role with anchor masks, exclusive zones, and
  keyboard-interactivity — bar and lockscreen exercise the full range.
- Phase 56 bitmap font (`kernel_core::session::font::BasicBitmapFont`)
  — text rendering in all four text clients reuses the same 8×16 glyph
  tables `term` and `greeter` use.
- Phase 72 chord engine — `LaunchLauncher` is a new variant slotted
  alongside the existing `SwitchWorkspace`, `SpawnTerm`, etc.
- Phase 72 `/etc/compositor.conf` parser — `[decorations]` and
  `[wallpaper]` sections piggyback on the same `[section]`/`key=value`
  shape.

## Related Roadmap Docs

- [Phase 73 design](./roadmap/73-compositor-polish.md)
- [Phase 73 task list](./roadmap/tasks/73-compositor-polish-tasks.md)
