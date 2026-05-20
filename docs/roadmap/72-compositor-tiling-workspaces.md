# Phase 72 - Compositor: Multi-Toplevel, Tiling Layout, and Workspaces

**Status:** Complete
**Source Ref:** phase-72
**Depends on:** Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅, Phase 68 (Phase 56 Completion and Closeout) ✅
**Builds on:** Extends the Phase 56 single-Toplevel compositor into a real tiling window manager by adding multi-client rendering, swappable layout policies, numbered workspaces, a keybind chord engine, and a `hyprctl`-equivalent control socket
**Primary Components:** `userspace/display_server`, `userspace/lib/layout`, `userspace/m3ctl`, `kernel/src/ipc`

## Milestone Goal

`display_server` becomes a tiling compositor that can run four or more GUI applications simultaneously, arrange them under configurable layout policies (master/stack, dwindle, grid, tabbed, fullscreen), separate them across up to nine numbered workspaces per output, and respond to `SUPER+`-chord keybinds without any kernel changes beyond the Phase 56 substrate.

The `LayoutPolicy` trait is the OCP boundary: new layouts plug in without modifying compositor core, which is precisely the Goal A architecture described in `docs/appendix/gui/tiling-compositor-path.md`. Applying SRP, layout math, workspace state, keybind dispatch, and the compose loop each live in their own module with no cross-cutting globals. Applying DRY, gaps and borders are computed once per relayout pass — not per frame — and the result is cached until the next relayout event. Layout policies are pure logic with no I/O, making them straightforwardly host-testable via `cargo test -p layout`; workspace state is a small state machine verified by unit tests before QEMU integration.

## Why This Phase Exists

Phase 56 delivered the architectural substrate — one userspace process owns the framebuffer, clients submit surfaces via the native IPC protocol, focus-aware input routing works. But it is a single-Toplevel system: only one app can realistically occupy the display at a time, there is no concept of workspaces, and the keybind system does not support modifier chords.

The tiling compositor experience — the omarchy/Hyprland aesthetic — is entirely policy code on top of that substrate. No new kernel primitives are required. This phase adds the policy: a layout engine, a workspace state machine, chord-based keybinds, per-window borders and gaps, and the control socket that makes scripting and status-bar integration possible. Everything here is pure userspace work in Rust.

## Learning Goals

- Understand how a tiling window manager is a layout policy on top of a compositor, not a separate program
- Learn how binary-tree and fixed-grid layout algorithms partition screen real estate
- See how workspace state machines decouple logical desktops from physical outputs
- Understand how a modifier-chord input grab works in front of normal client focus delivery
- Learn how a control socket (hyprctl-equivalent) enables external scripting without tight coupling

## Feature Scope

### Multi-toplevel client rendering

Phase 56's `SurfaceRegistry` already stores multiple Toplevel entries; the compose loop, damage tracking, and focus dispatcher must all be extended to handle more than one simultaneously. Tab-order focus, click-to-focus, and cursor-enters-window focus all need to be correct across N toplevels. Validated by running two `term` instances side-by-side.

### Layout policies

A single `userspace/lib/layout/` crate provides implementations of the `LayoutPolicy` trait introduced in Phase 56 (Track A.7 / E.1). Policies ship in one crate: Master/Stack, Dwindle (binary tree, alternating split direction per insertion), Spiral (dwindle variant with fixed rotation), Grid (even N×M partition), Tabbed (one tile visible at a time with a tab strip), and Fullscreen-toggle (a per-window overlay that hides chrome and covers the entire output). Floating override is a per-window attribute: windows in the floating list render above the tiled tree at their last position and size. The trait also exposes a `adjust_focused(focused, direction, step)` hook so the resize-mode keybinds (Track D) can dispatch into the active policy — master/stack and dwindle/spiral implement meaningful behavior; grid/tabbed/fullscreen return `LayoutError::Unsupported`.

### Workspace state machine

Per-output workspace sets of N numbered slots (default 9). The compositor maintains a `current_workspace` per output and a per-workspace layout selection. Operations: switch-workspace (move the viewport to workspace N), move-window-to-workspace (detach from the current workspace's layout tree and attach to another's), and follow/no-follow semantics on move (whether the compositor switches to the target workspace after the move). State lives inside `display_server`'s `SurfaceRegistry`.

### Keybind and chord engine

The Phase 56 bind table is extended to support full modifier+key chords (`SUPER`, `SUPER+SHIFT`, `SUPER+CTRL`). Chords are intercepted before clients see the keystroke. An optional leader-key mode allows multi-step sequences. Per-mode binding tables (resize mode, presentation mode) allow transient rebinds. The engine reloads from `/etc/compositor.conf` at runtime via `m3ctl reload`. The grab path consults the bind table before delivering events to the focused client.

### Per-window borders and gaps

Outer gaps (distance between the tiled region and the screen edge) and inner gaps (distance between adjacent tiles) are layout parameters read from `/etc/compositor.conf`. Borders are drawn by the composer as 1–4 px colored rectangles; active and inactive window borders use distinct colors. Title bars are off by default (omarchy aesthetic). Gap and border values are hot-reloadable via `m3ctl reload`.

### Control socket

`display_server` opens an AF_UNIX socket at `/run/compositor.sock`. The protocol is a small binary framing format (4-byte length + JSON body). Commands: `tile`, `layout <name>`, `workspace switch <n>`, `move-to-workspace <n>`, `reload`, `query windows`, `query workspaces`. Events pushed to subscribers: `workspace-changed`, `window-focused`, `window-opened`, `window-closed`. The `m3ctl` binary is the CLI client for this socket.

### Configuration

`/etc/compositor.conf` is a TOML file. Sections: `[gaps]` (outer, inner pixel counts), `[borders]` (width, active color, inactive color), `[keybinds]` (chord → action mapping), `[workspaces]` (default layout per slot). Parsed at startup and on `m3ctl reload`. Syntax errors are logged and the previous config retained.

## Important Components and How They Work

### `userspace/lib/layout/`

A new Rust crate providing one `LayoutPolicy` trait and six implementations. Each policy receives a list of `TiledWindow` handles with their minimum/preferred sizes and the output rectangle (minus gaps), and returns a flat list of `(handle, Rect)` assignments. The compositor apply loop takes that list and generates damage regions. Dwindle maintains an internal binary tree that grows with window insertions and shrinks on closures. Grid re-partitions the available area evenly on every layout change.

### `display_server` — compose loop extension

The existing compose loop iterates surfaces once. This phase extends it to: (1) pull the current workspace's window list in layout order, (2) call `active_policy.layout(windows, output_rect)`, (3) apply gap math, (4) blit each surface at its assigned rectangle, and (5) paint border rectangles over tile edges. Floating windows are blitted after tiled windows in Z-order.

### `display_server` — workspace state machine

A `WorkspaceManager` struct owns a `Vec<Workspace>` (one per output). Each `Workspace` contains a window list and a selected `LayoutPolicy` index. Switching workspaces saves the current damage state, activates the target workspace's window list, and triggers a full damage redraw. Moving a window across workspaces emits `window-focused` to any control-socket subscribers if the move follows.

### Keybind chord engine

A `BindTable` struct maps `(ModifierSet, KeySym)` to `Action` variants. On every key event, the input dispatcher checks the bind table before forwarding to the focused client. If the chord matches, the action is dispatched and the key event is consumed. Per-mode tables are pushed/popped on a stack so mode transitions are reversible.

### `m3ctl` binary

Extends the existing `m3ctl` tool with tile/workspace subcommands. Connects to `/run/compositor.sock`, sends a framed JSON command, and prints the response. Used by both humans and the status bar.

## How This Builds on Earlier Phases

- Extends Phase 56's `LayoutPolicy` trait with concrete implementations rather than the single `FloatingLayout` stub
- Replaces Phase 56's single-Toplevel compose path with a multi-window, workspace-aware compose loop
- Extends Phase 56's bind table to support modifier chords via a `BindTable` struct
- Reuses Phase 56's Layer surface role and exclusive-zone logic unchanged for the control socket subscribers
- Reuses Phase 55b IPC primitives for control-socket event push

## Implementation Outline

1. Extend compose loop and focus dispatcher for N simultaneous Toplevels; validate with two `term` instances
2. Create `userspace/lib/layout/` crate with `LayoutPolicy` trait and `FloatingLayout` migration
3. Implement Master/Stack, Dwindle, Spiral, Grid, Tabbed, Fullscreen policies in the new crate
4. Implement `WorkspaceManager` in `display_server`; wire switch-workspace and move-window-to-workspace
5. Extend `BindTable` to support modifier chords and per-mode tables; wire reload path
6. Add gap and border rendering to the compose loop
7. Implement AF_UNIX control socket in `display_server` with framed JSON protocol
8. Extend `m3ctl` with tile/workspace/layout/reload subcommands
9. Write `/etc/compositor.conf` TOML parser and hot-reload path
10. Update Phase 56 design doc to record that multi-toplevel and tiling are now delivered here

## Acceptance Criteria

- Four simultaneous GUI applications (two `term` instances, `edit`, and DOOM from Phase 70) tile correctly under the dwindle layout without visual corruption or stale damage; DOOM's fixed-size SHM surface is letterboxed centred within its assigned tile (DOOM does not yet consume `SurfaceResized`)
- `SUPER+1..9` switches between nine workspaces; each workspace retains its window list independently
- `SUPER+SHIFT+1` moves the focused window to workspace 1; the window no longer appears on the source workspace
- `m3ctl layout grid` switches the active workspace's layout to grid; windows re-tile within one frame
- `m3ctl reload` picks up changes to `/etc/compositor.conf` without restarting `display_server`
- Outer and inner gap values from config are visually correct; active window border color differs from inactive
- An attempt to open a tenth workspace on a nine-slot system is rejected gracefully (no crash)

## Companion Task List

- [Phase 72 Task List](./tasks/72-compositor-tiling-workspaces-tasks.md)

## How Real OS Implementations Differ

- Hyprland and sway implement tiling as part of the compositor binary rather than a separable library crate; m3OS's `layout` crate is a deliberate design choice that keeps policy separate from the compositor core
- Real compositors use DRM/KMS atomic commits for page flips, gaining precise vblank timing; m3OS blits into a linear framebuffer and synthesizes a vblank signal from a timer
- Wayland-based compositors can host multiple client protocols simultaneously (xdg-shell, layer-shell, xwayland); m3OS uses a single native IPC protocol throughout
- Production tiling WMs (i3, Hyprland) store layout trees in persistent configuration, allow per-app layout rules, and support scratchpad/special workspaces; these are deferred

## Deferred Until Later

- Animation engine, window-open/close animations, workspace slide transitions (Phase 73)
- Status bar, launcher, notification daemon as separate client processes (Phase 73)
- Multi-monitor independent workspace sets beyond the per-output design documented here
- Leader-key / which-key visual overlay (described as optional in the chord engine)
- Named workspaces and Hyprland-style "groups" (window sets independent of workspaces)
- `wl_shm` Wayland compatibility shim (explicitly not Wayland; see `wayland-gap-analysis.md`)
- Touchpad gestures for workspace switching
- Teaching the Phase 70 DOOM port to consume `ServerMessage::SurfaceResized` (Phase 69 already plumbed the message; under Phase 72 DOOM is letterboxed within its assigned tile)
- `LayoutPolicy::adjust_focused` implementations for grid / tabbed / fullscreen layouts (return `LayoutError::Unsupported` in Phase 72; meaningful semantics deferred)
