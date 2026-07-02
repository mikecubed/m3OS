# Handoff — Phase 105: Native GUI Toolkit & Core Desktop Apps

**Date:** 2026-07-02 (living doc — update each session)
**Branch:** `feat/phase-105-gui-toolkit-core` (off `main` at `8a49f97a`)
**State:** **Track A COMPLETE + green** — committed/PR'd. `m3ui` toolkit +
`m3ui-demo` + `toolkit-render-probe` gate PASS (widget frame composed;
keyboard Enter activates the focused button, counter repaints 35 scanlines
on the QMP/PPM dump). `cargo xtask check` clean, 41 m3ui host tests pass.
Tracks B/C/D/E are the follow-ups (below).
**Charter:** `docs/roadmap/105-gui-toolkit-and-apps.md`
**Tasks:** `docs/roadmap/tasks/105-gui-toolkit-and-apps-tasks.md`

## Context / recently merged

- PR #272 Phase 100 (bare-metal GUI software), #273 Phase 101 (ACPI
  QEMU-side), #274 Phase 107 (networked signed packages) — all on `main`.
- Phase 105's **core** (toolkit, clipboard, screenshot, image viewer) is
  explicitly NOT gated on 103/104; only the settings-panel Network/
  Display/Power sections are (they need Wi-Fi + brightness/battery
  backends that don't exist yet). So the core is fully hardware-free CI
  work and is being pulled forward.

## Scope of THIS branch (Track A only)

`userspace/lib/m3ui` — a minimal immediate-mode widget toolkit
(egui/microui-shaped, `#![no_std]` + `alloc`) on `desktop_client`:
- A.1 crate scaffold + workspace wiring
- A.2 pure-logic layout/constraint solver — the host-tested falsifiable
  core (Row/Column, fixed+flex sizing, padding, clip stack → per-widget
  `Rect`)
- A.3 input folding + focus traversal (Tab/Shift-Tab, Enter/Space,
  pointer hit-test/hover, text cursor + backspace/arrow)
- A.4 widgets: label, button, text_field, checkbox, list/selectable rows,
  slider, separator
- A.5 theme + proportional text over `kernel_core::font::atlas::Atlas`
  (8×16 `draw_text` fallback for ASCII)
- A.6 `Ui` per-frame context
- A.7 `m3ui-demo` Toplevel (4-place wiring) + `toolkit-render-probe`
  QMP/PPM gate

## Deferred to follow-up branches (documented, not done here)

- **Track B — clipboard** (`display_server` broker + protocol verbs +
  `desktop_client` set/get helpers + m3ui Ctrl+C/V/X + `clipboard-smoke`).
- **Track C — imagefmt + screenshot** (extract greeter's BMP/PNG decoders
  to `userspace/lib/imagefmt`, add baseline JPEG decode + PNG encoder,
  `CaptureOutput` verb, `screenshot` tool + gate).
- **Track D — imgview + settings** (imgview not gated on 103/104; audio
  `SetMasterVolume`; the settings Network/Display/Power sections wait on
  Phase 103/104 → Dell).
- **Track E — TUI ports** (file manager/editors/archive/audio player —
  parallel, ports-infra, can land anytime).

## Build/test facts

- Host-test the layout solver directly: `cargo test -p m3ui --target
  x86_64-unknown-linux-gnu` (the crate is `#![cfg_attr(not(test),
  no_std)]` like `pkg_app`/`kernel-core` so `cfg(test)` is std).
- New userspace binary needs the four-place wiring (workspace member,
  xtask `bins`, ramdisk `BIN_ENTRIES`, service conf + `KNOWN_CONFIGS`).
  m3ui itself is a `[lib]` (no bin wiring); only `m3ui-demo` is a binary.
- Render-probe gate pattern: QMP `screendump` → PPM parse (see
  `less-render-probe`/`compositor-stress` in `xtask/src/main.rs`,
  `xtask/src/{qmp,ppm}.rs`). Assert non-black widget regions / row
  occupancy.

## Track A — what landed (for reference when building B/C/D)

- Crate `userspace/lib/m3ui` (`default` = pure-logic core, `render`
  feature = framebuffer/font/IPC). Modules: `geom`, `layout` (the solver,
  15 tests), `input` (fold + `Focus`), `text_edit` (`TextBuffer`),
  `theme`, `paint` (`Painter` trait + `RecordingPainter` mock — the key to
  host-testing widgets), `ui` (`Ui<P: Painter>` + widgets), `render`
  (`SurfacePainter` + `decode_key`/`apply_pointer`).
- Widgets: label, button, checkbox, text_field, selectable, slider,
  separator + `split_row` (flex via the solver). All generic over
  `Painter`, so widget interaction is host-tested with the mock.
- `desktop_client` facts worth reusing: `SharedSurface` has PUBLIC fields
  + `pixels_mut() -> &'static mut [u32]`; drawing is FREE FUNCTIONS
  (`fill_rect(pixels, stride, height, x,y,w,h, color)`), BGRA8888 as
  `0xAARRGGBB` u32. `draw_text` paints an OPAQUE bg box — so `m3ui` draws
  text transparently itself via `BasicBitmapFont` glyph bits (8×16). Key
  events carry `keycode` (map via `kernel_core::input::keymap::KEY_*`) +
  `symbol` (Unicode). Toplevel: `set_toplevel_role()`, present via
  `attach_damage_commit(BufferId, shm_id, w, h)`.
- Render-probe pattern: the demo is launched by the gate typing
  `/bin/m3ui-demo\n` at the term prompt (NOT a service — like `launcher`),
  so no `services.d` conf. Gate waits `display.input-owner` +
  `TERM_SMOKE:prompt-ready`, then the demo's `M3UI_DEMO:ready`/`count=N`
  serial sentinels are the oracle alongside `changed_rows_in_band` PPM
  diffs.

## RESUME HERE — Track B (clipboard) next

Follow the charter B.1–B.4: add `SetClipboard`/`RequestClipboard`/
`ClipboardData` verbs to `kernel-core/src/display/protocol.rs`, a bounded
store + handlers in `display_server`, `desktop_client::{set_clipboard,
get_clipboard}` helpers, wire `m3ui` text-field Ctrl+C/V/X (the
`TextBuffer::apply_input` clipboard closure is already plumbed — it takes
`impl FnMut() -> Option<String>`), and a `clipboard-smoke` gate. Then
Track C (imagefmt + screenshot), then D.1 imgview + D.2 audio volume
(settings' 103/104 sections wait for the Dell).
