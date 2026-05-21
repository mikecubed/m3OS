# Compositor: Multi-Toplevel, Tiling Layout, and Workspaces (Phase 72)

**Aligned Roadmap Phase:** Phase 72
**Status:** Complete
**Source Ref:** phase-72
**Supersedes Legacy Doc:** new

## Overview

Phase 56 delivered a single-Toplevel compositor: one userspace process
(`display_server`) owns the framebuffer, clients submit surfaces via a
typed protocol, and a focus-aware dispatcher routes keyboard and
pointer events. What Phase 56 did not deliver was any policy on top of
that substrate. Only one app could realistically use the display at a
time; there was no concept of workspaces; the keybind table had no
modifier chords; gaps and borders were not in scope.

Phase 72 closes the gap. The compositor becomes a real tiling window
manager that can run four or more GUI applications simultaneously,
arrange them under configurable layout policies (master/stack,
dwindle, spiral, grid, tabbed, fullscreen), separate them across up
to nine numbered workspaces per output, and respond to `SUPER+`-chord
keybinds. Every byte of new code is userspace policy on top of the
Phase 56 substrate — no new kernel primitives.

The architectural pivot is the `LayoutPolicy` trait introduced at the
end of Phase 56. The Phase 72 work moves the trait + its concrete
implementations into a new crate (`userspace/lib/layout/`), where they
are independently testable and swappable at runtime. The compositor
core no longer cares which policy is active; it asks the active
workspace for a list of `(SurfaceId, Rect)` pairs and blits each
surface at its assigned rectangle. That is the Open/Closed boundary
of the entire phase.

## What This Doc Covers

- The Phase 72 layout crate (`userspace/lib/layout/`) with
  `TiledLayoutPolicy`, six concrete policies, and the
  `tile_contract_suite` every policy must pass.
- The per-output `WorkspaceManager` state machine, including
  switch-workspace, move-window-to-workspace, and per-workspace
  layout selection.
- The `BindStack` chord engine: default chords, resize-mode overlay,
  config-driven reload.
- Per-window borders + outer/inner gaps painted after the compose
  pass.
- The AF_UNIX-equivalent control socket (over the Phase 56 IPC
  endpoint) and the new `m3ctl` subcommands `layout`,
  `workspace switch`, `move-to-workspace`, `tile fullscreen`,
  `tile set-master-ratio`, `reload`, `query windows`, and
  `query workspaces`.
- `/etc/compositor.conf` and its hot-reload path.
- The `cargo xtask tiling-smoke` regression gate.

## Core Implementation

### The `LayoutPolicy` boundary

The trait that closes the design lives in `userspace/lib/layout/src/lib.rs`:

```rust
pub trait TiledLayoutPolicy {
    fn tile(
        &mut self,
        windows: &[TiledWindow],
        output: Rect,
        gaps: GapConfig,
    ) -> Vec<(SurfaceId, Rect)>;

    fn adjust_focused(
        &mut self,
        focused: SurfaceId,
        direction: ResizeDirection,
        step: i16,
    ) -> Result<(), LayoutError> {
        Err(LayoutError::Unsupported)
    }

    fn on_window_added(&mut self, _window: TiledWindow) {}
    fn on_window_removed(&mut self, _id: SurfaceId) {}
    fn on_focus_changed(&mut self, _id: Option<SurfaceId>) {}
}
```

Six implementations ship in the crate:

| Policy | Behaviour |
|---|---|
| `MasterStackLayout` | One master tile on the left; remaining tiles stack vertically on the right. `master_ratio` is adjustable at runtime via `m3ctl tile set-master-ratio <f>` or resize-mode `H`/`L`. |
| `DwindleLayout` | Hyprland-style binary tree. Each new window halves the most-recently-added tile, alternating horizontal then vertical splits. Persistent split ratios survive re-tile calls. |
| `SpiralLayout` | Rotating variant of dwindle. Each split rotates in the same direction so new windows spiral around the centre. |
| `GridLayout` | Even `ceil(sqrt(N)) × floor(sqrt(N))` grid. Distributes integer remainders so the total tiled area matches the output exactly. |
| `TabbedLayout` | Focused window covers the full output; unfocused windows receive a zero-size rect. Tab strip metadata lives on `TabbedLayout::focused`. |
| `FullscreenLayout` | Focused window covers the full output; everything else gets a zero-size rect. `m3ctl tile fullscreen` toggles between this and the previous layout. |

Every implementation passes the same `tile_contract_suite` (length
parity, in-output bounds, non-overlap, determinism). Host tests run
under `cargo test -p layout --target x86_64-unknown-linux-gnu` (24
tests passing).

### The workspace state machine

`userspace/display_server/src/workspace.rs` introduces a
`WorkspaceManager` with a fixed `[Workspace; 9]` array and a `current`
index. Each `Workspace` holds:

- A `Vec<SurfaceId>` of window ids in insertion order.
- A `PolicyKind` tag selecting which built-in policy is active.
- A `PolicySet` bundling per-kind state so toggling between layouts
  preserves the user's master ratio / dwindle split ratios.

The state machine exposes `switch_workspace(n)`, `move_to_workspace(id, n, follow)`,
and `set_current_layout(kind)`. Switching workspaces flips the
`current` index and the compose loop's filter callback masks Toplevel
surfaces belonging to other workspaces; Layer / Background / Overlay
surfaces remain visible across all workspaces.

### The chord engine

`userspace/display_server/src/keybind.rs` adds a `BindStack` of
`BindModeTable`s. The bottom of the stack is the default-mode table,
pre-populated with the canonical chord set:

- `SUPER+1..9` → switch to workspace N.
- `SUPER+SHIFT+1..9` → move the focused window to workspace N.
- `SUPER+TAB` → cycle focus through the active workspace.
- `SUPER+RETURN` → spawn `/bin/term`.
- `SUPER+Q` → request close on the focused surface (placeholder).
- `SUPER+R` → enter resize mode.

`EnterResize` pushes a transient resize-mode table that maps `H`/`J`/`K`/`L`
to `ResizeFocused { direction, step }` and `Escape` / `SUPER+R` to
`ExitResize`. The dispatcher consults `BindStack::active_table()` on
every key event; matched chords are consumed before delivery to the
focused client. `LayoutError::Unsupported` on `adjust_focused`
silently no-ops for grid / tabbed / fullscreen layouts per spec.

### Borders and gaps

`userspace/display_server/src/borders.rs` paints active / inactive
borders after the compose pass. The compose loop's
`run_compose_filtered` entry accepts an optional `BorderConfig` and a
`focused_id` and walks the same Toplevel set the surface blit used,
painting `border_cfg.active_color` around the focused tile and
`border_cfg.inactive_color` around everything else. Outer gaps shrink
the output rect before layout; inner gaps are applied symmetrically
between adjacent tiles via per-policy `shrink_horizontal` /
`shrink_vertical` helpers in the layout crate.

### Control socket and m3ctl

`kernel-core::display::protocol` grows eight new `ControlCommand`
variants — `SetLayout`, `SwitchWorkspace`, `MoveToWorkspace`,
`Reload`, `QueryWindows`, `QueryWorkspaces`, `SetMasterRatio`,
`TileFullscreen` — plus three new `ControlEvent` variants
(`WindowListReply`, `WorkspaceListReply`, `WorkspaceChanged`).
`display_server`'s `serve_one_control_request` intercepts each verb
and routes to a dedicated handler. `m3ctl`'s `parse_verb` adds the
matching CLI surface (`layout <name>`, `workspace switch <n>`,
`move-to-workspace <n> [--follow]`, `reload`, `query windows`,
`query workspaces`, `tile fullscreen`, `tile set-master-ratio <r>`).

### Configuration and hot reload

`userspace/display_server/src/config.rs` parses
`/etc/compositor.conf` (a TOML subset: sections, key/value lines,
`#` comments). Sections: `[gaps]`, `[borders]`, `[keybinds]`,
`[workspaces]`. The minimal working config is staged on every disk
image by `xtask::populate_ext2_files`. Syntax errors return a typed
`ConfigError` and the compositor falls back to the previous /
default config. `m3ctl reload` re-parses the file and applies gaps,
borders, keybinds, and per-workspace defaults without restarting the
compositor.

### Validation

`cargo xtask tiling-smoke` (gated behind `M3OS_TILING_REGRESSION=1`
in `.githooks/pre-push`) boots m3OS, exchanges ten `m3ctl` verbs
covering `version`, `query workspaces`, `workspace switch`,
`layout`, `tile fullscreen`, and `reload`, and asserts each reply
sentinel. The exit code `SMOKE_EXIT_TILING_SMOKE_FAILED = 72`
distinguishes a tiling-specific failure for CI routing.

## Key Files

| Path | Purpose |
|---|---|
| `userspace/lib/layout/src/lib.rs` | `TiledLayoutPolicy` trait, `GapConfig`, `ResizeDirection`, `PolicyKind`, `tile_contract_suite`. |
| `userspace/lib/layout/src/master_stack.rs` | `MasterStackLayout`. |
| `userspace/lib/layout/src/dwindle.rs` | `DwindleLayout` (incl. `SpiralLayout` via `DwindleLayout::spiral()`). |
| `userspace/lib/layout/src/{grid,tabbed,fullscreen}.rs` | Grid, tabbed, fullscreen policies. |
| `userspace/display_server/src/workspace.rs` | `WorkspaceManager`, `Workspace`, `PolicySet`, `WorkspaceLayoutAdapter`. |
| `userspace/display_server/src/keybind.rs` | `BindStack`, `BindModeTable`, `KeybindAction`, default + resize chord registrations. |
| `userspace/display_server/src/borders.rs` | Border painting. |
| `userspace/display_server/src/config.rs` | TOML parser + types. |
| `userspace/display_server/src/main.rs` | Phase 72 verb handlers + chord-action dispatcher. |
| `userspace/m3ctl/src/lib.rs` | New `parse_verb` arms for the Phase 72 verbs. |
| `kernel-core/src/display/protocol.rs` | New `ControlCommand` + `ControlEvent` variants and opcodes. |
| `xtask/src/main.rs` | `cargo xtask tiling-smoke` gate + `compositor.conf` disk staging. |

## How Real OS Implementations Differ

- Hyprland and sway implement tiling as part of the compositor binary
  rather than a separable library crate; m3OS's `layout` crate is a
  deliberate design choice that keeps policy testable in isolation.
- Real compositors use DRM/KMS atomic commits for page flips, gaining
  precise vblank timing; m3OS blits into a linear framebuffer and
  synthesizes a vblank signal from a timer.
- Wayland-based compositors can host multiple client protocols
  simultaneously (xdg-shell, layer-shell, xwayland); m3OS uses a
  single native IPC protocol throughout.
- Production tiling WMs (i3, Hyprland) store layout trees in
  persistent configuration, allow per-app layout rules, and support
  scratchpad / special workspaces; these are deferred to later
  phases.

## Related Roadmap Docs

- [Phase 72 design doc](./roadmap/72-compositor-tiling-workspaces.md)
- [Phase 72 task list](./roadmap/tasks/72-compositor-tiling-workspaces-tasks.md)
- [Phase 56 design doc](./roadmap/56-display-and-input-architecture.md) — the substrate the Phase 72 policy code builds on
- [Phase 68 design doc](./roadmap/68-display-server-closeout.md) — closed the subscription-push and damage-tracking gaps Phase 72 depends on
- [Tiling compositor path](./appendix/gui/tiling-compositor-path.md) — the goal-A architecture this phase finally lands
