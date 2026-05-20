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

- Four simultaneous GUI applications (two `term` instances, `edit`, and DOOM from Phase 70) tile correctly under the dwindle layout without visual corruption or stale damage. Phase 72 initial scope letterboxed DOOM; Phase 72b closeout (Track K) wires DOOM to observe `SurfaceResized` for diagnostic purposes, but doomgeneric keeps a fixed 320×200 backing buffer — the compositor handles the geometry mismatch by scaling/letterboxing the DOOM surface into the assigned tile rather than reflowing the playfield resolution.
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

## Phase 72b — Closeout (in this PR)

Initial Phase 72 shipped the full structural feature set (multi-toplevel rendering, layout policies, workspaces, chord engine, borders/gaps, control socket, configuration, smoke gate) and was marked Complete. End-to-end smoke testing on real graphical boot then surfaced several integration gaps that the original tiling-smoke gate did not exercise — including a singleton-service collision that prevented `SUPER+RETURN` from ever spawning a second `term`, a missing compositor-side `SurfaceResized` emitter that caused term surfaces to extend past the tile borders, and four `publish_*` event-push stubs that the F.2 acceptance row had marked complete without verifying delivery on the wire. The user's review explicitly rejected deferring these to a separate Phase 72b phase, so the closeout work lives under Phase 72 as **Track K** in the task list.

The closeout scope is:

- **Term redesign — term is no longer a boot-readiness signal.** session_manager treats `display_server` (default boot) and `greeter` (graphical-only boot) as the desktop-ready signals; term moves out of `DECLARED_SESSION_STEP_NAMES` and out of `term.conf`-as-supervised-service. `term::SERVICE_NAME` and its `ipc_register_service` call are removed; term becomes a freely N-instantiable user-facing app launched via the new `[autostart]` config or `SUPER+RETURN`. **Greeter also stops `execve`-ing `/bin/term` post-auth** — the authenticated user lands at an empty compositor and presses `SUPER+RETURN` to spawn a terminal, matching the Hyprland / sway / i3 idiom where login leaves you at a bare tiling desktop rather than a pre-spawned shell.
- **`[autostart]` section in `/etc/compositor.conf`.** Mirroring Hyprland's `exec-once`, the compositor runs each declared `exec = /path/to/binary` once after first compose. `cargo xtask run-gui --skip-login` (no greeter) stages `exec = /bin/term` so a terminal appears at boot; greeter mode and smoke modes omit the autostart line so the user can opt in by editing the file.
- **Compositor emits `ServerMessage::SurfaceResized`.** display_server tracks the last-known tile dimensions per Toplevel and emits `SurfaceResized { width, height }` when the dims change so clients can reflow within their assigned tile. Closes the visible-extends-past-border symptom.
- **DOOM consumes `SurfaceResized`.** dg_m3os.c's `DC_EVENT_SURFACE_RESIZED` arm logs the event and acknowledges receipt rather than just silently dropping it. **DOOM does not reflow its playfield**: doomgeneric exposes a fixed `DOOMGENERIC_RESX × DOOMGENERIC_RESY` screen buffer baked into the engine, and runtime resolution changes would require an upstream doomgeneric refactor that is out of scope for m3OS. The compositor's **aspect-preserving scale + letterbox** path remains responsible for the visual mismatch: when a Toplevel's intrinsic buffer dimensions don't match its assigned tile, the compositor synthesises a nearest-neighbour scaled snapshot that fills the tile along the constrained axis and adds letterbox bars on the unconstrained axis. This is a deliberate change from the pure-letterbox-only initial scope — the visual contract is now "fit the tile, preserve aspect ratio, no stretching." K.4's value is end-to-end protocol observability (the SurfaceResized message now visibly arrives at every Toplevel client, term *and* DOOM) rather than a true playfield resize. The per-frame scaled-snapshot allocation is bounded to surf!=tile clients (DOOM steady-state, term during its resize transient); a cached scaled buffer is a documented Phase 73 follow-up.
- **`adjust_focused` UX for non-resizable layouts.** Grid/tabbed/fullscreen still return `LayoutError::Unsupported` (no meaningful semantics), but resize-mode now logs a clear `resize not supported under <policy>` message and auto-exits the mode so the user is not silently stuck pressing H/J/K/L.
- **`SUPER+Q` close protocol.** New `ServerMessage::CloseRequest { surface_id }` notification. SUPER+Q's `KillFocused` action emits it to the focused surface's owning client; term, greeter, and DOOM each handle it by initiating a graceful shutdown.
- **Per-client surface ownership + Goodbye teardown.** Phase 70 worked around the multi-client `Goodbye` regression by preserving the entire registry on disconnect, leaking surfaces. Closeout introduces per-surface `ClientId` ownership in `SurfaceRegistry` and, on Goodbye or transport close, destroys only that client's surfaces.
- **Control-socket event push — `publish_*` actually delivers.** The four publish helpers transmit framed events to subscribed control clients via the existing async-IPC primitives. Subscribers that close their connection are reaped without panic. Closes audit blocker C5 / Red Flag #15 — the F.2 acceptance row was incorrectly marked complete in initial Phase 72.

## Deferred Until Later

Scheduled (target phase named):

- Animation engine, window-open/close animations, workspace slide transitions (Phase 73)
- Status bar, launcher, notification daemon as separate client processes (Phase 73)

Scheduled Backlog (no firm phase, recorded so they live in a durable place):

- Multi-monitor independent workspace sets beyond the per-output design documented here — gated on multi-output framebuffer driver work; revisit once a real multi-output target is in scope
- Leader-key / which-key visual overlay — UX polish; low priority but cheap to add anytime
- Named workspaces and Hyprland-style "groups" (window sets independent of workspaces) — UX feature; not blocking core function
- Touchpad gestures for workspace switching — depends on touchpad driver evolution

Out of scope (permanent):

- `wl_shm` Wayland compatibility shim — m3OS is explicitly not Wayland; see `wayland-gap-analysis.md`
