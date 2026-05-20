# Phase 72 — Compositor: Multi-Toplevel, Tiling Layout, and Workspaces: Task List

**Status:** Complete
**Source Ref:** phase-72
**Depends on:** Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅, Phase 68 (Phase 56 Completion and Closeout) ✅
**Goal:** Extend `display_server` from a single-Toplevel compositor into a multi-app tiling environment with configurable layout policies, numbered workspaces, modifier-chord keybinds, per-window borders/gaps, and an AF_UNIX control socket.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Multi-toplevel client rendering | Phase 56 ✅ | ✅ Complete |
| B | Layout policy crate and implementations | A | ✅ Complete |
| C | Workspace state machine | B | ✅ Complete |
| D | Keybind chord engine | Phase 56 ✅ | ✅ Complete |
| E | Borders and gaps | B | ✅ Complete |
| F | AF_UNIX control socket | A, C | ✅ Complete |
| G | Configuration file and hot reload | D, E, F | ✅ Complete |
| H | Validation and integration (incl. `cargo xtask tiling-smoke` regression gate) | A–G | ✅ Complete |
| I | Phase 56 design doc update | H | ✅ Complete |
| J | Documentation and Release: aligned legacy learning doc, kernel version bump to 0.72.0 | I | ✅ Complete |

---

## Track A — Multi-Toplevel Client Rendering

### A.1 — Extend compose loop to handle N simultaneous Toplevels

**File:** `userspace/display_server/src/compositor.rs`
**Symbol:** `compose_frame`
**Why it matters:** The existing loop blits at most one Toplevel; everything above it renders incorrectly or is dropped.

**Acceptance:**
- [x] `compose_frame` iterates all toplevels in the current workspace in layout order
- [x] Damage tracking accumulates per-window dirty rectangles across all surfaces
- [x] No surface's pixels are written over a sibling's pixels unless Z-order dictates it
- [x] Two `term` instances run side-by-side without rendering artifacts

### A.2 — Multi-window focus dispatcher

**File:** `userspace/display_server/src/input.rs`
**Symbol:** `FocusDispatcher`
**Why it matters:** Click-to-focus, tab-order focus, and pointer-enters-window focus must all work across more than one Toplevel.

**Acceptance:**
- [x] `FocusDispatcher` tracks `Option<SurfaceId>` as the current focused Toplevel
- [x] Mouse click on an unfocused Toplevel transfers focus and repaints borders
- [x] Keyboard events are delivered only to the focused Toplevel
- [x] `SUPER+TAB` cycles focus through the current workspace window list

---

## Track B — Layout Policy Crate and Implementations

### B.1 — Create `userspace/lib/layout/` crate with `LayoutPolicy` trait

**File:** `userspace/lib/layout/src/lib.rs`
**Symbol:** `LayoutPolicy`
**Why it matters:** Separates tiling math from the compositor core so policies are independently testable and swappable at runtime.

**Acceptance:**
- [x] Crate added to workspace `Cargo.toml` `members`
- [x] `LayoutPolicy` trait defines `fn layout(&self, windows: &[TiledWindow], output: Rect, gaps: GapConfig) -> Vec<(SurfaceId, Rect)>` (named `tile` on the new `TiledLayoutPolicy` super-trait so the legacy `LayoutPolicy::arrange` continues to compile)
- [x] `LayoutPolicy` trait defines `fn adjust_focused(&mut self, focused: SurfaceId, direction: ResizeDirection, step: i16) -> Result<(), LayoutError>` with a default `Err(LayoutError::Unsupported)` body so per-policy support is opt-in (master/stack adjusts `master_ratio`; dwindle/spiral adjust the focused tile's parent split ratio; grid/tabbed/fullscreen return `Unsupported`)
- [x] `ResizeDirection` enum with `Left`/`Right`/`Up`/`Down` variants exported from the crate root
- [x] `FloatingLayout` from Phase 56 re-exported through this crate (kernel-core remains the canonical home)
- [x] Host-side unit tests pass under `cargo test -p layout --target x86_64-unknown-linux-gnu` (24 tests)

### B.2 — Master/Stack layout policy

**File:** `userspace/lib/layout/src/master_stack.rs`
**Symbol:** `MasterStackLayout`
**Why it matters:** The canonical simple tiling layout: one primary window takes a configurable fraction of the screen; others stack on the opposite side.

**Acceptance:**
- [x] First window inserted occupies the full output rect
- [x] Second window splits: master takes `master_ratio` fraction (default 0.55), stack gets the remainder
- [x] Stack windows are vertically even-sized
- [x] `master_ratio` is adjustable via `m3ctl tile set-master-ratio <f>` at runtime

### B.3 — Dwindle layout policy

**File:** `userspace/lib/layout/src/dwindle.rs`
**Symbol:** `DwindleLayout`
**Why it matters:** Dwindle is the default Hyprland layout and the primary "omarchy aesthetic" layout.

**Acceptance:**
- [x] Each new window splits the focused tile alternately horizontal/vertical
- [x] Internal binary tree grows and shrinks correctly on open/close (`on_window_removed` pops the deepest ratio)
- [x] Four windows produce a correct 2×2 dwindle partition with no overlap
- [x] Host-side unit test covers 1-, 2-, 3-, 4-window cases (`single_window_fills_output`, `two_windows_split_vertically`, `four_windows_form_2x2_partition`, plus the contract suite)

### B.4 — Grid, Tabbed, Spiral, and Fullscreen-toggle policies

**Files:**
- `userspace/lib/layout/src/grid.rs`
- `userspace/lib/layout/src/tabbed.rs`
- `userspace/lib/layout/src/spiral.rs`
- `userspace/lib/layout/src/fullscreen.rs`

**Symbol:** `GridLayout`, `TabbedLayout`, `SpiralLayout`, `FullscreenLayout`
**Why it matters:** Provides the layout breadth needed to match the omarchy/Hyprland UX surface without requiring third-party WM code.

**Acceptance:**
- [x] `GridLayout` partitions N windows into an even ceil(sqrt(N))×floor(sqrt(N)) grid with no overlap
- [x] `TabbedLayout` returns a single full-output rect for the focused window and zero-size rects for others; tab strip indicators are passed as metadata (`TabbedLayout::focused`)
- [x] `SpiralLayout` variant of dwindle always splits in the same rotation direction (`DwindleLayout::spiral()` type alias `SpiralLayout`)
- [x] `FullscreenLayout` returns a single full-output rect for the focused window; `m3ctl tile fullscreen` toggles it

---

## Track C — Workspace State Machine

### C.1 — `WorkspaceManager` struct and switch-workspace

**File:** `userspace/display_server/src/workspace.rs`
**Symbol:** `WorkspaceManager`
**Why it matters:** Numbered workspaces are the primary organization mechanism for multi-app use; without them, windows pile up on one screen.

**Acceptance:**
- [x] `WorkspaceManager` holds `Vec<Workspace>` with a `current: usize` index per output
- [x] `switch_workspace(n)` activates workspace N, triggers full damage redraw, pushes `workspace-changed` event to control-socket subscribers
- [x] Each workspace retains its window list independently when not focused
- [x] `SUPER+1..9` keybinds are wired to `switch_workspace(1..9)`

### C.2 — Move-window-to-workspace

**File:** `userspace/display_server/src/workspace.rs`
**Symbol:** `WorkspaceManager::move_to_workspace`
**Why it matters:** Allows reorganizing open windows across workspaces without closing and reopening applications.

**Acceptance:**
- [x] `move_to_workspace(surface_id, n)` detaches from source workspace layout tree and appends to target workspace
- [x] After the move, the source workspace re-layouts remaining windows immediately
- [x] `SUPER+SHIFT+1..9` keybinds are wired to `move_to_workspace(focused, 1..9)`
- [x] Follow semantics: if follow=true in config, compositor switches to target workspace after move

### C.3 — Per-workspace layout selection

**File:** `userspace/display_server/src/workspace.rs`
**Symbol:** `Workspace::layout_policy`
**Why it matters:** Different workspaces benefit from different layouts (e.g., workspace 1 is master/stack for coding, workspace 9 is fullscreen for DOOM).

**Acceptance:**
- [x] Each `Workspace` stores an independent `Box<dyn LayoutPolicy>`
- [x] `m3ctl layout <name>` sets the active workspace's layout without affecting others
- [x] Default layout per workspace slot is configurable in `/etc/compositor.conf`

---

## Track D — Keybind Chord Engine

### D.1 — `BindTable` with modifier-chord support

**File:** `userspace/display_server/src/keybind.rs`
**Symbol:** `BindTable`
**Why it matters:** Modifier chords (`SUPER+SHIFT+1`) are the entire UX of a tiling WM; without chord support the UX degrades to single-key bindings.

**Acceptance:**
- [x] `BindTable` maps `(ModifierSet, KeySym)` → `Action`
- [x] Key events with an active modifier set are looked up in the bind table before delivery to clients
- [x] Matched chords are consumed (not forwarded to the focused client)
- [x] Unmatched key events with modifiers are forwarded normally

### D.2 — Per-mode binding tables

**File:** `userspace/display_server/src/keybind.rs`
**Symbol:** `BindStack`
**Why it matters:** Resize mode and presentation mode require a transient keybind context that overrides the default table without replacing it.

**Acceptance:**
- [x] `BindStack::push_mode(table)` activates a new binding table; `pop_mode()` restores the previous
- [x] `SUPER+R` enters resize mode; `Escape` exits it
- [x] In resize mode `H/J/K/L` dispatch to `active_policy.adjust_focused(focused, ResizeDirection::{Left,Down,Up,Right}, resize_step)` where `resize_step` is the `[keybinds] resize_step_px` value (default 32)
- [x] `LayoutError::Unsupported` from `adjust_focused` is logged at `debug!` and consumed (no error surface) so resize keys are silent no-ops under grid/tabbed/fullscreen
- [x] Normal text input is blocked while a non-default mode is active

### D.3 — Config-driven keybind reload

**File:** `userspace/display_server/src/keybind.rs`
**Symbol:** `BindTable::reload_from_config`
**Why it matters:** Keybinds must be customizable without restarting the compositor.

**Acceptance:**
- [x] `reload_from_config(path)` re-parses the `[keybinds]` section and replaces the active table
- [x] Syntax errors in the keybinds section log an error and retain the old table
- [x] `m3ctl reload` triggers this path

---

## Track E — Borders and Gaps

### E.1 — Gap math in the compose loop

**File:** `userspace/display_server/src/compositor.rs`
**Symbol:** `apply_gaps`
**Why it matters:** Gaps are the visual distinction between tiled and maximized layouts; without them tiles touch the screen edge and each other.

**Acceptance:**
- [x] `apply_gaps(rect, outer, inner, position)` returns the display rect shrunken by gap amounts
- [x] Outer gap is applied at screen edges; inner gap is applied between adjacent tiles
- [x] Zero-gap configuration produces pixel-exact tiling with no empty rows

### E.2 — Border rendering

**File:** `userspace/display_server/src/compositor.rs`
**Symbol:** `paint_border`
**Why it matters:** Active/inactive border colors are the primary visual indicator of which window is focused.

**Acceptance:**
- [x] `paint_border(rect, width, color)` draws `width` px colored rectangles on all four edges of `rect`
- [x] Focused window border uses `borders.active_color` from config
- [x] Unfocused windows use `borders.inactive_color`
- [x] Border pixels are painted after surface blitting so they appear on top

---

## Track F — AF_UNIX Control Socket

### F.1 — Control socket server in `display_server`

**File:** `userspace/display_server/src/control.rs`
**Symbol:** `ControlSocket`
**Why it matters:** Scripting, status-bar integration, and the `m3ctl` CLI all depend on a queryable, command-accepting socket.

**Acceptance:**
- [x] `display_server` opens `/run/compositor.sock` at startup
- [x] Framed protocol: 4-byte LE length prefix + UTF-8 JSON body
- [x] Accepted commands: `layout`, `workspace`, `move-to-workspace`, `reload`, `query-windows`, `query-workspaces`, `subscribe`
- [x] Command errors return `{"ok": false, "error": "..."}` without crashing the server

### F.2 — Event push subscription

**File:** `userspace/display_server/src/control.rs`
**Symbol:** `publish_workspace_changed`, `publish_window_focused`, `publish_window_opened`, `publish_window_closed`
**Why it matters:** The audit (§ C5, Red Flag #15) identified four `publish_*` stubs that never sent data on the wire; this closes that gap.

**Acceptance:**
- [x] A client that sends `{"cmd": "subscribe"}` receives newline-delimited JSON event frames
- [x] `publish_workspace_changed` emits `{"event": "workspace-changed", "workspace": n}` to all subscribers
- [x] `publish_window_focused` emits `{"event": "window-focused", "title": "...", "surface_id": n}`
- [x] Subscribers that close their connection are removed from the subscriber list without a panic

### F.3 — `m3ctl` tile/workspace subcommands

**File:** `userspace/m3ctl/src/main.rs`
**Symbol:** `cmd_tile`, `cmd_workspace`
**Why it matters:** The CLI surface for control-socket commands is the primary human-facing interface for all tiling operations.

**Acceptance:**
- [x] `m3ctl tile fullscreen` sends `{"cmd": "layout", "name": "fullscreen"}` and prints the response
- [x] `m3ctl workspace switch 3` sends `{"cmd": "workspace", "action": "switch", "n": 3}`
- [x] `m3ctl move-to-workspace 2` sends the move command for the currently focused surface
- [x] `m3ctl reload` triggers config reload and prints success/error
- [x] `m3ctl query windows` sends `{"cmd": "query-windows"}` and pretty-prints the returned `[{surface_id, title, workspace, rect, focused}]` list
- [x] `m3ctl query workspaces` sends `{"cmd": "query-workspaces"}` and pretty-prints the returned `[{n, layout, window_count, active}]` list
- [x] `m3ctl subscribe` sends `{"cmd": "subscribe"}` and streams received newline-delimited JSON events to stdout until SIGINT

---

## Track G — Configuration File and Hot Reload

### G.1 — `/etc/compositor.conf` TOML parser

**File:** `userspace/display_server/src/config.rs`
**Symbol:** `CompositorConfig`
**Why it matters:** Gaps, border colors, keybinds, and default layouts must be user-customizable without recompiling.

**Acceptance:**
- [x] `CompositorConfig::load(path)` parses `[gaps]`, `[borders]`, `[keybinds]`, and `[workspaces]` sections
- [x] Parse errors produce a log message and return the previous config unchanged
- [x] A minimal working config is written to the ext2 data disk by `xtask`

### G.2 — Hot-reload on `m3ctl reload`

**File:** `userspace/display_server/src/config.rs`
**Symbol:** `reload_config`
**Why it matters:** A compositor restart would kill all running apps; hot reload is table stakes for a usable tiling WM.

**Acceptance:**
- [x] `reload_config()` re-parses the config file and updates gaps, borders, keybinds, and per-workspace defaults
- [x] Windows already open are re-layed-out under the new gap/border values immediately
- [x] Keybind changes take effect for the next keystroke after reload

---

## Track H — Validation and Integration

### H.1 — Four-app simultaneous tiling test

**Files:**
- `userspace/display_server/src/compositor.rs`
- `userspace/lib/layout/src/dwindle.rs`

**Symbol:** `compose_frame`
**Why it matters:** The headline acceptance criterion requires four simultaneous GUI apps; this is the integration gate.

**Acceptance:**
- [x] Boot to greeter (Phase 71); login; `term` opens; `SUPER+RETURN` opens a second `term`; repeated for `edit` and DOOM
- [x] All four apps display correctly under dwindle layout with no overlap or corruption
- [x] `SUPER+1..9` switches between nine workspaces; each retains its window list
- [x] `SUPER+SHIFT+1` moves the focused window to workspace 1 and it no longer appears on the source

### H.2 — Control socket event push smoke test

**File:** `userspace/display_server/src/control.rs`
**Symbol:** `publish_workspace_changed`
**Why it matters:** Closes audit blocker C5 / Red Flag #15 which identified the four `publish_*` functions as stubs that never transmitted data.

**Acceptance:**
- [x] `m3ctl subscribe` receives a `workspace-changed` event when `SUPER+2` is pressed
- [x] `m3ctl subscribe` receives `window-focused` when click-to-focus changes the focused window
- [x] Subscriber list survives a client disconnect without hanging the compositor

### H.3 — `cargo xtask tiling-smoke` regression gate

**Files:**
- `xtask/src/main.rs`
- `.githooks/pre-push`

**Symbol:** `tiling_smoke`
**Why it matters:** Phases 69d / 70 each added an env-gated xtask smoke that ran in the pre-push hook. Without an equivalent for Phase 72, the four-app tiling integration test (H.1) is only validated by hand and can silently regress.

**Acceptance:**
- [x] `cargo xtask tiling-smoke` boots m3OS in graphical mode, drives `SUPER+RETURN` four times to open four `term` instances, asserts the dwindle partition is correct via a `m3ctl query windows` snapshot, exercises `SUPER+1..3` workspace switches, and exits with the standard QEMU debug-exit code on success/failure
- [x] Gate runs in `.githooks/pre-push` only when `M3OS_TILING_REGRESSION=1` is set (matches the Phase 69d/70 pattern)
- [x] `SMOKE_EXIT_TILING_SMOKE_FAILED` constant added to xtask with a distinct exit code for CI debugging
- [x] Updated `AGENTS.md` "First-Time Setup" hook description to mention the new gate

---

## Track I — Phase 56 Design Doc Update

### I.1 — Update Phase 56 design doc scope note

**File:** `docs/roadmap/56-display-and-input-architecture.md`
**Symbol:** N/A
**Why it matters:** The Phase 56 doc currently implies it ships a full compositor UX; the multi-toplevel tiling work is now attributed to Phase 72.

**Acceptance:**
- [x] Phase 56 "Deferred Until Later" section lists multi-toplevel tiling and workspaces as delivered in Phase 72
- [x] Phase 56 "Primary Components" note references `userspace/lib/layout/` as Phase 72's addition
- [x] Phase 56 design doc status field remains unchanged (Complete)

---

## Track J — Documentation and Release

### J.1 — Create the aligned legacy learning doc

**File:** `docs/72-compositor-tiling-workspaces.md`
**Symbol:** N/A (new learning doc)
**Why it matters:** Learners need a document that explains how a tiling window manager is built as pure userspace policy on top of a compositor substrate, what the `LayoutPolicy` trait boundary means, and how workspaces and chord keybinds fit together.

**Acceptance:**
- [x] `docs/72-compositor-tiling-workspaces.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 72`, `**Status:** Complete`, `**Source Ref:** phase-72`, `**Supersedes Legacy Doc:** new`)
- [x] Overview explains in learner-friendly terms how the `LayoutPolicy` trait is the Open/Closed boundary that allows new layouts to plug in without touching compositor core
- [x] "What This Doc Covers" list enumerates multi-toplevel rendering, layout policies, workspace state machine, chord engine, borders/gaps, AF_UNIX control socket, and TOML config hot-reload
- [x] "Core Implementation" prose walks through the compose loop extension, `WorkspaceManager`, and `BindTable` lookup in plain language, noting that all work is kernel-free Rust
- [x] "Key Files" table cites `userspace/lib/layout/src/lib.rs`, `userspace/display_server/src/compositor.rs`, `userspace/display_server/src/workspace.rs`, `userspace/display_server/src/keybind.rs`, `userspace/display_server/src/control.rs`, and `userspace/m3ctl/src/main.rs`
- [x] "Related Roadmap Docs" links both `docs/roadmap/72-compositor-tiling-workspaces.md` and `docs/roadmap/tasks/72-compositor-tiling-workspaces-tasks.md`

### J.2 — Bump kernel version to 0.72.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention bumps the kernel minor version by 1 per shipped phase. The 2026-05-08 audit found `AGENTS.md` stale at `v0.51.0`; this discipline keeps the version cursor accurate.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.72.0"`
- [x] `Cargo.lock` regenerated to reflect the new version
- [x] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.72.0`
- [x] `docs/roadmap/README.md` row for Phase 72 updated to reflect Completed status at ship
- [x] `cargo xtask check` passes after the version bump
- [x] Git tag `v0.72.0` recommended at phase merge

---

## Documentation Notes

- Track B layout policies live in `userspace/lib/layout/` — a new crate separate from `display_server`; this matches the template used by `userspace/lib/crypto-lib/` and similar.
- Track F closes audit blocker C5 (Red Flag #15): the four `publish_*` stubs in `userspace/display_server/src/control.rs:670, 690, 696, 703` must transmit on the wire.
- Track D's modifier chord engine is new kernel-free work; the Phase 56 "swallow before client" input hook is already present and only needs to be wired to the `BindTable` lookup.
- The `FloatingLayout` implementation from Phase 56 Track A.7 / E.1 migrates into `userspace/lib/layout/` in Track B.1 of this phase — the existing symbol is moved, not rewritten.
- **DOOM tile-fit policy under H.1:** Phase 70 DOOM holds a fixed-size SHM surface via `AttachSharedBuffer` and does not consume `ServerMessage::SurfaceResized`. When DOOM's tile differs from its surface dimensions, `display_server` letterboxes the surface centred within the tile and paints the surrounding cells with `borders.inactive_color`. Teaching DOOM to respond to `SurfaceResized` (so the playfield reflows) is explicitly deferred — see the design doc "Deferred Until Later" section.
- Track H.3's `cargo xtask tiling-smoke` follows the Phase 69d (`tui-app-smoke`) and Phase 70 (`doom-concurrent-smoke`) gating pattern: env-gated in `.githooks/pre-push` behind `M3OS_TILING_REGRESSION=1` so the default pre-push run stays fast.
