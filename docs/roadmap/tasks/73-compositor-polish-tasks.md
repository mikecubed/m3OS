# Phase 73 — Compositor: Polish (Bar, Launcher, Notifications, Animations, Lockscreen Stub): Task List

**Status:** Planned
**Source Ref:** phase-73
**Depends on:** Phase 72 (Compositor: Multi-Toplevel, Tiling Layout, and Workspaces) ✅, Phase 71 (Greeter and Multi-User Session) ✅
**Goal:** Deliver the native client ecosystem (status bar, launcher, notification daemon, lockscreen stub, desktop background) and the visual animation and decoration layer that complete the omarchy-aesthetic desktop experience on the Phase 72 tiling compositor substrate.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Animation engine in `display_server` | Phase 72 ✅ | Planned |
| B | Decoration layer (rounded corners, shadows) | A | Planned |
| C | Status bar client (`userspace/bar/`) | Phase 72 ✅ | Planned |
| D | Launcher client (`userspace/launcher/`) | Phase 72 ✅ | Planned |
| E | Notification daemon (`userspace/notifyd/`) | Phase 72 ✅ | Planned |
| F | Lockscreen stub (`userspace/lockscreen/`) | Phase 72 ✅, Phase 71 ✅ | Planned |
| G | Desktop background client (`userspace/wallpaper/`) | Phase 72 ✅ | Planned |
| H | Build pipeline, service configs, session integration | C–G | Planned |
| I | Validation | A–H | Planned |
| J | Documentation and Release: aligned legacy learning doc, kernel version bump to 0.73.0 | I | Planned |

---

## Track A — Animation Engine

### A.1 — `AnimationEngine` struct and frame driver

**File:** `userspace/display_server/src/animation.rs`
**Symbol:** `AnimationEngine`
**Why it matters:** Smooth window-open and workspace-slide animations are the most visible part of the omarchy aesthetic; without them the compositor feels static.

**Acceptance:**
- [ ] `AnimationEngine` stores a `Vec<Animation>` and exposes `tick(frame_delta_ms: u32) -> DamageRegion`
- [ ] `tick` advances all animation timers, interpolates values, returns the union of all animated rects as dirty
- [ ] Completed animations are removed; an empty animation list produces zero damage
- [ ] `compose_frame` calls `animation_engine.tick(delta)` and merges the returned damage before the blit pass

### A.2 — Timing curves

**File:** `userspace/display_server/src/animation.rs`
**Symbol:** `Curve`
**Why it matters:** Linear animations feel mechanical; ease-out and spring curves make transitions feel intentional and polished.

**Acceptance:**
- [ ] `Curve::Linear` interpolates `start + (end - start) * t` where `t = elapsed / duration`
- [ ] `Curve::EaseOut` applies `1 - (1 - t)^2` envelope
- [ ] `Curve::Spring` applies a simple critically-damped spring approximation (no overshoot by default)
- [ ] Each curve is independently testable via a host-side unit test in `kernel-core` or the animation crate

### A.3 — Window-open, window-close, workspace-slide, and window-move animations

**File:** `userspace/display_server/src/animation.rs`
**Symbol:** `animate_window_open`, `animate_workspace_switch`, `animate_window_move`
**Why it matters:** These are the four animation events that users see constantly; they must be smooth at 60 fps on the QEMU framebuffer.

**Acceptance:**
- [ ] `animate_window_open` slides the surface from 90% scale/20% opacity to 100%/100% over 150 ms using EaseOut
- [ ] `animate_window_close` fades from 100% opacity to 0% over 100 ms and removes the surface on completion
- [ ] `animate_workspace_switch` slides the outgoing workspace off-screen and the incoming on-screen horizontally over 200 ms
- [ ] `animate_window_move` moves a tile's painted rect to its new position over 80 ms using Spring
- [ ] All four animations produce correct damage regions; no artifacts visible at 60 fps in QEMU

---

## Track B — Decoration Layer

### B.1 — Rounded corner alpha mask

**File:** `userspace/display_server/src/decoration.rs`
**Symbol:** `RoundedCornerMask`
**Why it matters:** Rounded corners are the single most visible decoration feature of the omarchy aesthetic; they differentiate the desktop from a raw tiling WM.

**Acceptance:**
- [ ] `RoundedCornerMask::new(radius)` precomputes a corner alpha ramp for a given pixel radius
- [ ] Applied in the compose pass after each surface blit: four corners are masked to transparent
- [ ] Corner radius is configurable in `/etc/compositor.conf` under `[decorations] corner_radius`
- [ ] Zero radius disables the mask pass entirely (no performance cost)

### B.2 — Drop shadow precomputation

**File:** `userspace/display_server/src/decoration.rs`
**Symbol:** `DropShadow`
**Why it matters:** Precomputed shadows give depth without requiring per-frame Gaussian blur.

**Acceptance:**
- [ ] `DropShadow::compute(width, height, blur_radius, color)` returns a pixel buffer with alpha falloff
- [ ] Shadow is recomputed only when the associated window changes size
- [ ] Shadow is blitted behind the window surface in Z-order with configurable x/y offset and color
- [ ] Shadow size, blur radius, offset, and color are all configurable in `[decorations]`

---

## Track C — Status Bar Client

### C.1 — `bar` binary skeleton and Layer-shell attachment

**File:** `userspace/bar/src/main.rs`
**Symbol:** `main`, `BarSurface`
**Why it matters:** The status bar is the primary persistent UI chrome; it must be always-visible and correctly excluded from the tiled window area.

**Acceptance:**
- [ ] `bar` connects to `display_server` and requests a Layer surface at `Top` anchor with height 24 px
- [ ] The exclusive zone is set to 24 px so the Phase 72 layout engine does not tile windows under the bar
- [ ] `bar` survives `display_server` restart (supervised process model from Phase 51/57)

### C.2 — Workspace indicators, window title, clock, and audio mute

**File:** `userspace/bar/src/render.rs`
**Symbol:** `render_bar`
**Why it matters:** These four pieces of information are what users need at a glance; nothing else on the bar is in scope.

**Acceptance:**
- [ ] Nine workspace number boxes render left-aligned; the active workspace box uses a distinct highlight color
- [ ] Focused window title renders center-aligned, truncated to fit if too long
- [ ] A `HH:MM` clock renders right-aligned, updated once per second via `CLOCK_REALTIME`
- [ ] An audio mute indicator (speaker icon or "MUTE" text) appears right of the clock when the Phase 63 audio server reports mute state
- [ ] All four elements update correctly when the relevant event or timer fires

### C.3 — Control socket subscriber for bar

**File:** `userspace/bar/src/events.rs`
**Symbol:** `subscribe_compositor_events`
**Why it matters:** Workspace and focus information must come from the compositor in real time, not by polling.

**Acceptance:**
- [ ] `bar` connects to `/run/compositor.sock` and sends `{"cmd": "subscribe"}`
- [ ] On `workspace-changed` event, the workspace indicator repaints within one frame
- [ ] On `window-focused` event, the title field repaints within one frame
- [ ] Lost connection to compositor socket is handled gracefully (bar shows "—" rather than crashing)

---

## Track D — Launcher Client

### D.1 — `launcher` binary with fuzzy-filter list

**File:** `userspace/launcher/src/main.rs`
**Symbol:** `Launcher`
**Why it matters:** A fast keyboard-driven launcher is the primary app-open path in the omarchy workflow; without it users must drop to a shell.

**Acceptance:**
- [ ] `launcher` opens a floating Toplevel centered on the primary output (configurable width: 600 px default)
- [ ] On startup it scans `/usr/bin` and `/usr/local/bin` for executable files and stores names in a sorted `Vec<String>`
- [ ] Keystrokes update a filter string; the displayed list re-filters on every keystroke using a substring + subsequence scorer
- [ ] Return key executes the top-listed entry via `execve`; Escape closes the launcher without executing

### D.2 — Keybind wiring for `SUPER+SPACE`

**File:** `userspace/display_server/src/keybind.rs`
**Symbol:** `BindTable`
**Why it matters:** The launcher is useless if it cannot be opened from the keyboard without a shell command.

**Acceptance:**
- [ ] `SUPER+SPACE` chord is registered in the default bind table as `Action::LaunchProgram("/usr/bin/launcher")`
- [ ] `LaunchProgram` action forks and execs the named binary as a supervised child of `session_manager`
- [ ] If a `launcher` instance is already running, the keybind focuses it rather than opening a second

---

## Track E — Notification Daemon

### E.1 — `notifyd` daemon and AF_UNIX listener

**File:** `userspace/notifyd/src/main.rs`
**Symbol:** `NotifyDaemon`
**Why it matters:** A notification daemon is a fundamental desktop integration point used by coreutils, scripts, and future GUI apps.

**Acceptance:**
- [ ] `notifyd` creates `/run/notifyd.sock` at startup; clients connect, send one framed JSON notification, and disconnect
- [ ] Protocol: 4-byte LE length + UTF-8 JSON body with fields `title`, `body`, `timeout_ms`
- [ ] Malformed messages are discarded; the daemon does not crash
- [ ] `notifyd` is supervised by `session_manager` and restarts on crash

### E.2 — Layer-shell notification panels

**File:** `userspace/notifyd/src/surface.rs`
**Symbol:** `NotificationPanel`
**Why it matters:** Notifications must appear visually on screen, not just log to serial.

**Acceptance:**
- [ ] Each notification renders as a small panel at `Top+Right` with configurable padding
- [ ] Panels stack vertically if multiple arrive before the first dismisses
- [ ] Each panel displays `title` (bold-style, larger pixel font) and `body` (smaller font)
- [ ] Panel auto-dismisses after `timeout_ms`; if `timeout_ms` is zero it stays until clicked

### E.3 — `notify-send` utility binary

**File:** `userspace/notifyd/src/bin/notify_send.rs`
**Symbol:** `main`
**Why it matters:** Provides a testable command-line interface that scripts and the smoke test can invoke.

**Acceptance:**
- [ ] `notify-send "Title" "Body"` connects to `/run/notifyd.sock`, sends the notification, and exits 0
- [ ] `notify-send --timeout 10000 "Title" "Body"` sets `timeout_ms` to 10000
- [ ] Exit code 1 with error message if `/run/notifyd.sock` does not exist

---

## Track F — Lockscreen Stub

### F.1 — `lockscreen` stub binary

**File:** `userspace/lockscreen/src/main.rs`
**Symbol:** `Lockscreen`
**Why it matters:** A visible lockscreen state is required for the session manager to offer a "lock" command even before full credential verification is implemented.

**Acceptance:**
- [ ] `lockscreen` requests a Layer-shell surface at full-output size with `Top` anchor and `keyboard-interactivity: exclusive`
- [ ] Surface is solid black with centered "Locked — press Enter to unlock" text
- [ ] Pressing Enter closes the `lockscreen` process and returns input focus to the previous surface
- [ ] While `lockscreen` is running, no keystroke (except Enter) is delivered to any other surface

### F.2 — `m3ctl lock` command

**File:** `userspace/m3ctl/src/main.rs`
**Symbol:** `cmd_lock`
**Why it matters:** Users need a single command to trigger the lockscreen; `session_manager` also needs this path for idle-lock in the future.

**Acceptance:**
- [ ] `m3ctl lock` sends a `{"cmd": "lock"}` message to `/run/compositor.sock`
- [ ] `display_server` responds by forking and supervising `lockscreen` as a Layer client
- [ ] A second `m3ctl lock` while lockscreen is running is a no-op

---

## Track G — Desktop Background Client

### G.1 — `wallpaper` Layer-shell Bottom client

**File:** `userspace/wallpaper/src/main.rs`
**Symbol:** `WallpaperClient`
**Why it matters:** A configurable desktop background completes the visual identity of the desktop; the Phase 56 solid-color fill is a placeholder.

**Acceptance:**
- [ ] `wallpaper` reads a raw RGBA image path from `/etc/compositor.conf` under `[wallpaper] path`
- [ ] It attaches a Layer-shell surface at `Bottom` anchor (rendered behind all tiled windows)
- [ ] On `SIGHUP` (sent by `m3ctl reload`) it reloads the configured path and repaints
- [ ] If the path does not exist it renders a solid color from `[wallpaper] fallback_color`

---

## Track H — Build Pipeline, Service Configs, Session Integration

### H.1 — Add all five new binaries to xtask and ramdisk

**Files:**
- `xtask/src/main.rs`
- `kernel/src/fs/ramdisk.rs`

**Symbol:** `build_userspace` (xtask), `BIN_ENTRIES` (ramdisk)
**Why it matters:** Binaries not listed in both places are silently absent at runtime (`execve` returns ENOENT).

**Acceptance:**
- [ ] `bar`, `launcher`, `notifyd`, `lockscreen`, `wallpaper`, `notify-send` are all in the `bins` array in `build_userspace`
- [ ] Each binary has a corresponding `include_bytes!` entry in `BIN_ENTRIES` in `ramdisk.rs`
- [ ] `cargo xtask run` succeeds without linker or embedding errors for all six binaries

### H.2 — Service configs and `session_manager` boot sequence

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files`)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`)
- `userspace/session_manager/src/main.rs`

**Symbol:** `populate_ext2_files`, `KNOWN_CONFIGS`, `start_session_services`
**Why it matters:** Daemons not listed in the service config and `KNOWN_CONFIGS` fallback will not be started on boot.

**Acceptance:**
- [ ] `.conf` files for `bar`, `notifyd`, and `wallpaper` are written to the ext2 data disk by `populate_ext2_files`
- [ ] `KNOWN_CONFIGS` in `init/src/main.rs` lists all three daemon configs
- [ ] `session_manager` starts `wallpaper`, `bar`, and `notifyd` (in that order) after `display_server` and input services are up
- [ ] `cargo xtask clean && cargo xtask run` recreates the disk with all configs present

---

## Track I — Validation

### I.1 — Full omarchy-aesthetic desktop smoke test

**Files:**
- `userspace/bar/src/main.rs`
- `userspace/launcher/src/main.rs`
- `userspace/notifyd/src/main.rs`

**Symbol:** N/A (integration test)
**Why it matters:** The acceptance criteria for Phase 73 are the user-visible outcomes; this is the gating scenario.

**Acceptance:**
- [ ] Boot sequence completes; bar is visible at top with workspace indicators and a live clock
- [ ] `SUPER+SPACE` opens launcher; typing "ter" surfaces `term` at the top of the filtered list; Return launches it
- [ ] `notify-send "Hello" "World"` produces a pop-up at top-right that disappears after 5 s
- [ ] Opening a `term` window shows the slide+fade animation at 60 fps with no visible tearing
- [ ] `m3ctl lock` covers the screen with the lockscreen stub; Enter dismisses it; other keys do not reach `term`
- [ ] Rounded corners and drop shadows are visible on all windows when enabled in config

---

## Track J — Documentation and Release

### J.1 — Create the aligned legacy learning doc

**File:** `docs/73-compositor-polish.md`
**Symbol:** N/A (new learning doc)
**Why it matters:** Learners need a document explaining how the omarchy-aesthetic desktop is assembled from independent Layer-shell and Toplevel clients, how the animation engine produces smooth frame transitions without GPU access, and how each client relates to the Phase 72 compositor substrate.

**Acceptance:**
- [ ] `docs/73-compositor-polish.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 73`, `**Status:** Planned`, `**Source Ref:** phase-73`, `**Supersedes Legacy Doc:** new`)
- [ ] Overview explains in learner-friendly terms how `bar`, `launcher`, `notifyd`, `lockscreen`, and `wallpaper` are normal compositor clients using Layer-shell roles — not part of the compositor itself — and why this SRP split matters
- [ ] "What This Doc Covers" list enumerates the animation engine, decoration layer, status bar, launcher, notification daemon, lockscreen stub, and desktop background client
- [ ] "Core Implementation" prose walks through the `AnimationEngine::tick` → `compose_frame` → damage-region pipeline and the Layer surface rendering order for each client
- [ ] "Key Files" table cites `userspace/display_server/src/animation.rs`, `userspace/display_server/src/decoration.rs`, `userspace/bar/src/main.rs`, `userspace/launcher/src/main.rs`, `userspace/notifyd/src/main.rs`, `userspace/lockscreen/src/main.rs`, and `userspace/wallpaper/src/main.rs`
- [ ] "Related Roadmap Docs" links both `docs/roadmap/73-compositor-polish.md` and `docs/roadmap/tasks/73-compositor-polish-tasks.md`

### J.2 — Bump kernel version to 0.73.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention bumps the kernel minor version by 1 per shipped phase. The 2026-05-08 audit found `AGENTS.md` stale at `v0.51.0`; this discipline keeps the version cursor accurate.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.73.0"`
- [ ] `Cargo.lock` regenerated to reflect the new version
- [ ] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.73.0`
- [ ] `docs/roadmap/README.md` row for Phase 73 updated to reflect Completed status at ship
- [ ] `cargo xtask check` passes after the version bump
- [ ] Git tag `v0.73.0` recommended at phase merge

---

## Documentation Notes

- Track A's `AnimationEngine` is internal to `display_server` — it is not a separate crate or a client-facing API.
- Track C `bar` uses the Phase 72 control-socket subscription protocol; Track E's `notify-send` uses a separate `/run/notifyd.sock`. They are independent sockets.
- Track F (lockscreen) explicitly does NOT implement credential verification — that is Phase 71b. The acceptance criteria reflect this; "press Enter to unlock" is the only interaction in scope.
- All five new binaries must follow the four-place registration rule: workspace member, xtask `bins`, ramdisk `BIN_ENTRIES`, and service config (for daemons). Missing any one causes a silent runtime failure.
- The Phase 56 `Layer` surface role's `keyboard-interactivity: exclusive` flag is what gives the lockscreen input capture; this must be tested explicitly (Track F.1 third bullet).
