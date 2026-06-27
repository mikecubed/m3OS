# Phase 105 - Native GUI Toolkit & Core Desktop Apps

**Status:** Planned
**Source Ref:** phase-105
**Depends on:** Phase 100 (Bare-Metal GUI Session — `display_server`/`mouse_server`/`session_manager`/`greeter` in init's builtin configs, the WC user framebuffer, a USB-mouse cursor) ✅, Phase 99 (SMP & Scheduler Robustness) ✅ transitively via 100. The **settings/control panel** is additionally sequenced **after** Phase 103 (Laptop Power Management — brightness/battery backend) and Phase 104 (Intel AX201 Wi-Fi + connect daemon); the rest of the phase (toolkit, clipboard, screenshot, image viewer) is **not** gated on 103/104.
**Builds on:** The `desktop_client` immediate-mode primitives (`SharedSurface` over `sys_shm`, `fill`/`fill_rect`/`stroke_rect`/`draw_text`/`draw_text_scaled`, and `DisplayConnection::pull_event` returning `ServerMessage::{Key,Pointer,FocusIn,FocusOut,SurfaceResized,CloseRequest}`), the `kernel_core::font` TTF rasterizer/atlas (`font::atlas::Atlas`, `font::raster`), and the from-scratch PNG/BMP decoders currently buried in `userspace/greeter/src/image.rs`. It adds the missing **WIDGET / LAYOUT / event-routing** layer that every current GUI client (`greeter`, `bar`, `launcher`, `lockscreen`, `notifyd`) hand-rolls pixel-by-pixel.
**Primary Components:** `userspace/lib/m3ui` (new — the immediate-mode toolkit crate), `kernel-core/src/display/protocol.rs` + `userspace/display_server/src/main.rs` (new clipboard + output-capture verbs), `userspace/lib/imagefmt` (new — extracted BMP/PNG decoders + JPEG + a PNG encoder), `userspace/screenshot` (new — screenshot tool), `userspace/imgview` (new — image viewer Toplevel), `userspace/settings` (new — settings/control panel Toplevel), with backend reuse of `wifi-core::control`, the Phase 103 brightness/battery surface, and `kernel-core/src/audio` + `userspace/lib/audio_mixer` for volume.

## Milestone Goal

m3OS already has a working graphical **stack** — a compositor that owns the framebuffer, layer-shell and Toplevel surface roles, focus-aware keyboard/pointer routing, damage tracking, and SHM surfaces — but it has **no GUI widget toolkit**. Every graphical client today reaches into a raw BGRA8888 buffer with `fill_rect` + `draw_text` and re-implements its own button-hit-testing and text-entry from scratch. This phase closes that central gap: it ships a minimal **native immediate-mode Rust toolkit** (buttons, labels, text fields, lists, checkboxes, a layout pass, and theming) on top of `desktop_client`, plus the **core desktop apps that make the GUI usable to a person** — a clipboard so text round-trips between windows, a screenshot tool that writes a real PNG, an image viewer that opens PNG/BMP/JPEG, and a **settings/control panel** that is the natural user-facing consumer of the Phase 103 power and Phase 104 Wi-Fi backends (join a network, set brightness, set volume, read the battery).

## Why This Phase Exists

The GUI-workstation arc charted in Phase 98 reaches a usable compositor in Phase 100, a real pointer in Phase 102, power in Phase 103, and Wi-Fi in Phase 104 — but a usable *workstation* needs usable *apps*, and there is no abstraction to build them on. Three concrete problems force this phase:

- **There is no widget/layout/event abstraction.** `desktop_client` is explicit that it "is *not* a toolkit. The compose loop renders BGRA8888 pixels directly into a shared-memory surface; there is no widget tree, no layout engine." So `greeter` hand-codes its login form's text-field cursor, focus ring, and button hit-test (`userspace/greeter/src/render.rs` + `main.rs`), and `launcher`/`bar` each re-derive their own. Any new app pays that whole cost again. The missing layer is small, idiomatic (every GUI client is already Rust-on-`desktop_client`), needs no `std`/libc, and reuses the existing SHM surface + TTF atlas + input primitives — only the widget/layout/event glue is absent.
- **The Phase 103/104 backends have no user-facing consumer.** After 103/104 land, a user can set brightness or join a Wi-Fi network only from a shell (`m3ctl wifi status` is read-only today; there is no connect command and no brightness command at all). That undermines the "usable GUI workstation" goal of the whole arc. The settings/control panel is the deliberate consumer that turns those backends into something a person can drive with a pointer.
- **Core data-flow primitives a desktop assumes are simply missing.** There is **no clipboard** in the tree (a tree-wide grep for `clipboard`/`paste`/selection in `display_server` + the display protocol finds nothing), so text cannot move between two windows. There is **no output capture / screenshot path** (clients cannot read the kernel framebuffer — `display_server` owns it via `display::fb_owner` — so only the compositor can produce a screenshot), and no PNG *encoder* anywhere (the greeter decoders are read-only). These are table-stakes for any graphical session and have no current home.

## Learning Goals

- Understand the **immediate-mode GUI** model (egui / Dear ImGui / microui shape): the UI is re-declared every frame, widget identity and hit-testing are derived from a per-frame layout pass rather than a retained object tree, and event routing collapses to "did the pointer/keyboard land on this frame's widget rect." Contrast with retained-mode toolkits (GTK/Qt) and why immediate mode fits a `no_std` SHM-surface client far better.
- See how a **clipboard / selection protocol** works as a compositor-brokered data transfer (the Wayland `wl_data_device` model in miniature): an offer is published by the copy source, the compositor stores it, and a paste consumer requests it by MIME tag — no shared writable memory between clients, consistent with the m3OS IPC rules.
- Learn how a small OS gets **proportional text** out of an existing bitmap/TTF rasterizer: the toolkit layers glyph metrics + an `Atlas` cache over the kernel-core font code rather than the fixed 8×16 cell `draw_text` path.
- Understand why a **graphical browser engine and office suite are infeasible** on this substrate (no GPU acceleration, no toolkit deep enough, multi-hundred-MB engines that assume a full POSIX + GL stack) and how a teaching OS substitutes text-mode equivalents instead of faking the real thing.

## Feature Scope

### Track A — `m3ui`: a minimal immediate-mode widget toolkit

A new library crate `userspace/lib/m3ui` (egui/microui-shaped, `#![no_std]` + `alloc`) layered on `desktop_client`. Per frame the app builds a `Ui` against a `SharedSurface`, declares widgets (`label`, `button`, `text_field`, `checkbox`, `list`/selectable rows, `slider`, `separator`), and reads back interaction results (clicked / changed / focused). A **layout pass** (`Row`/`Column` containers with fixed and flex sizing, padding, and a clip stack) computes each widget's `Rect`; the **event router** maps the `ServerMessage::{Key,Pointer,FocusIn,FocusOut,SurfaceResized}` stream `desktop_client` already delivers onto widget focus + hit-testing (Tab/Shift-Tab focus traversal, Enter/Space activation, pointer click/hover, text cursor + backspace/arrow editing). A **theme** struct centralizes colors/metrics. Rendering uses the existing `fill_rect`/`stroke_rect` for chrome and a new proportional-text path backed by `kernel_core::font::atlas::Atlas` (falling back to the 8×16 `draw_text` for ASCII). The **layout/constraint solver is pure logic and host-tested** — the falsifiable core of the toolkit — independent of any framebuffer or IPC. (Distinct from the existing `userspace/lib/layout` crate, which is the compositor's window-*tiling* geometry; `m3ui`'s layout is intra-window widget layout.)

### Track B — Clipboard / data-transfer protocol

`display_server` becomes the clipboard broker (no clipboard exists today). New protocol verbs in `kernel-core/src/display/protocol.rs`: a client-to-server `SetClipboard { mime_tag, len }` offer (the bytes follow on the bulk channel) and a `RequestClipboard { mime_tag }` paste request, answered by a server-to-client `ClipboardData { mime_tag, len }` with bytes on the bulk channel (mirroring the existing `pull_event` → `ipc_take_pending_bulk` path). The compositor holds the last offer in a bounded store (`text/plain;charset=utf-8` first; size-capped). `desktop_client` gains `set_clipboard(&str)` / `get_clipboard() -> Option<Vec<u8>>` helpers, and `m3ui` text fields wire Ctrl+C/Ctrl+V/Ctrl+X to them. No client ever shares writable memory — the transfer is copy-through-the-compositor.

### Track C — `imagefmt` shared decoders + output capture + screenshot

- **Extract** the BMP/PNG decoders and the scale-to-fit blitter from `userspace/greeter/src/image.rs` into a new `userspace/lib/imagefmt` crate (`decode_bmp`, `decode_png`, `blit_scale_to_fit`, `ImageError`), and repoint `greeter` at it so the decoders stop being duplicated per app.
- **Add JPEG** baseline decode (`decode_jpeg`, modeled on `jpeg-decoder` — re-expressed for `no_std`+`alloc`: SOI/APP0/DQT/SOF0/DHT/SOS parse, baseline Huffman + dequant + 8×8 IDCT + YCbCr→BGRA, no progressive/arithmetic/CMYK) so the image viewer covers the three common formats.
- **Add a PNG encoder** (`encode_png` — RGBA8/RGB8, filter 0, a stored-or-fixed-Huffman deflate stream + CRC/Adler), the first encoder in the tree, needed by the screenshot tool.
- **Output capture** in `display_server`: a new `CaptureOutput { shm_id }` verb where a client pre-allocates an output-sized SHM, the compositor blits the current composited framebuffer into it, and replies with the dimensions — the only way to screenshot, since `display::fb_owner` makes the compositor the sole framebuffer reader.
- **`screenshot` tool** (`userspace/screenshot`): allocates an `output_size()` SHM, calls `CaptureOutput`, `encode_png`s the result, and writes `/tmp/screenshot-N.png` (or a `-o` path).

### Track D — Image viewer + settings/control panel Toplevels

- **`imgview`** (`userspace/imgview`): a Toplevel app that opens a PNG/BMP/JPEG file via `imagefmt`, renders it with `blit_scale_to_fit` into its surface, and uses `m3ui` for the title bar / file name / a "fit vs 1:1" toggle and left/right navigation across files in a directory.
- **`settings`** (`userspace/settings`): a Toplevel control panel built entirely from `m3ui`, with sections:
  - **Network** — a Wi-Fi picker driving the Phase 104 connect daemon over `wifi-core::control` (`WIFI_SCAN_REQ` → a `ScanResult` list; selecting a row + entering a passphrase in an `m3ui` text field sends `WIFI_CONNECT_REQ`; `WIFI_STATUS` shows the associated SSID + RSSI + IPv4).
  - **Display** — a brightness slider calling the Phase 103 backlight backend.
  - **Sound** — a master volume slider driving `audio_server` (this phase adds a `SetMasterVolume` control verb to `kernel-core/src/audio` + master-gain in `audio_mixer`, since the Phase 57 audio control surface is `GetStats`-only today).
  - **Power** — battery percentage + AC state read from the Phase 103 surface.

The CI-testable surface (rendering, volume, the *issuing* of the right IPC verbs against a stub service) is gated in QEMU; the **live** Wi-Fi scan/connect and real backlight change ride real hardware (AX201/backlight have no QEMU model) and land under the Phase 98 bare-metal validation convention.

### Track E — TUI-in-`term` parallel ports (charter note, toolkit-independent)

These are **not blocked on the toolkit** — they run in the existing `term` emulator on the ncurses/termios stack and the Phase 85 ports infrastructure, and can land in parallel. Charter (not necessarily all in this phase's PR): an `nnn`/`lf` file manager, `nano`/`vim` editors, `bsdtar` for archives, and a `symphonia`-based (Rust) terminal audio player feeding `audio_server`. They give the workstation real file/edit/archive/media tooling cheaply while the graphical apps mature. **Explicitly deferred as infeasible:** a graphical browser engine and an office suite (no GPU accel, no toolkit deep enough, multi-hundred-MB engines) — the substitutes are text-mode `w3m`/`lynx`.

## Important Components and How They Work

### `userspace/lib/m3ui` — the toolkit

The crate is organized as a pure-logic core plus a thin device-bound shell:

- `m3ui::layout` (**host-tested, pure logic**) — the constraint solver. A `LayoutTree` of `Row`/`Column`/`Fixed`/`Flex` nodes with padding/spacing resolves to a flat list of `Rect`s given a root region; flex children split leftover space; a clip stack bounds children to their parent. No framebuffer, no IPC — a unit test asserts "a Column of three `Fixed(40)` buttons + one `Flex(1)` spacer in a 200×400 region yields the expected four rects" and that flex distributes remainder deterministically.
- `m3ui::widget` — `label`, `button`, `text_field`, `checkbox`, `list`, `slider`, `separator`. Each is an immediate-mode call: it claims a rect from the layout pass, draws its chrome via `desktop_client::{fill_rect,stroke_rect}` + the proportional-text helper, hit-tests against the per-frame pointer/focus state, and returns a small result (`bool` clicked, `&mut String` edited, etc.).
- `m3ui::input` — folds the `ServerMessage` event stream into a per-frame `InputState` (pointer position/buttons, key queue, modifier mask) and owns focus traversal (Tab/Shift-Tab over the frame's focusable widgets, Enter/Space activate).
- `m3ui::theme` — a `Theme` of colors + metrics (fg/bg/accent/border, padding, font size), so apps share one look.
- `m3ui::Ui` — the per-frame context: `Ui::begin(&SharedSurface, &InputState)` → widget calls → `Ui::end()` returns the damage rect to commit. Proportional text uses a small owned `kernel_core::font::atlas::Atlas` keyed on the bundled TTF, with the 8×16 `BasicBitmapFont` as the ASCII fallback.

### Clipboard in `display_server`

The compositor gains a `Clipboard` store (last offer's MIME tag + bytes, bounded to a cap, dropped on `Goodbye`). `SetClipboard`/`RequestClipboard`/`ClipboardData` are added to the protocol enums and codec in `kernel-core/src/display/protocol.rs` (host-tested encode/decode), and handled in `display_server`'s verb dispatch. Bytes ride the same bulk channel the event pull already uses. This is the Wayland selection model reduced to one fixed selection and one MIME type, deliberately small.

### `userspace/lib/imagefmt` + output capture

`imagefmt` is the greeter decoders moved out verbatim (so the existing host tests move with them) plus `decode_jpeg` and `encode_png`. `display_server`'s `CaptureOutput` blits the composited output (the same buffer `display::compose` produces) into a client-provided SHM; the `screenshot` binary owns the PNG encode + file write so the compositor stays a pure pixel source.

### `settings` backends

The panel is a pure `m3ui` client; each section is an IPC client of an existing service: `wifi.control` (`wifi-core::control` verbs), the Phase 103 brightness/battery surface, and `audio_server` (via the new `SetMasterVolume`). No new policy lives in the compositor — the panel is the consumer, not an owner.

## How This Builds on Earlier Phases

- **Extends Phase 73's `desktop_client`** from "shared handshake + raw pixel helpers" to a real toolkit foundation — `m3ui` consumes `SharedSurface`, the draw helpers, and `DisplayConnection::pull_event` unchanged, and adds the widget/layout/event layer the crate's own doc comment says it deliberately is not.
- **Reuses the Phase 56/69 display protocol + `display_server`** input/focus/resize plumbing (`ServerMessage::{Key,Pointer,FocusIn,FocusOut,SurfaceResized,CloseRequest}`) as the toolkit's event source, and extends the protocol with clipboard + capture verbs rather than adding a new IPC surface.
- **Reuses `kernel_core::font`** (the TTF rasterizer + `Atlas` from the font phase) for proportional widget text instead of the fixed-cell bitmap path.
- **Lifts the Phase 71 greeter image decoders** out of `greeter/src/image.rs` into a shared crate and adds the JPEG decode + PNG encode they never had.
- **Is the user-facing consumer of Phase 103 (power) and Phase 104 (Wi-Fi)** — the settings panel is the deliberate UI for backends those phases ship headless, which is why the panel is sequenced after them while the rest of Phase 105 is not.
- **Reuses the Phase 98 bare-metal validation strategy** (`docs/appendix/bare-metal-validation.md`) for the settings panel's live Wi-Fi/brightness arms, since AX201 and the laptop backlight have no QEMU model.

## Implementation Outline

1. **Track A** — scaffold `userspace/lib/m3ui` (workspace member + lib); implement the pure-logic `layout` solver with host tests; build the `widget`/`input`/`theme`/`Ui` layers over `desktop_client`; add a proportional-text helper over `kernel_core::font::atlas::Atlas`; ship a tiny `m3ui-demo` Toplevel for the render-probe gate.
2. **Track B** — add `SetClipboard`/`RequestClipboard`/`ClipboardData` to `kernel-core/src/display/protocol.rs` (+ codec host tests); implement the bounded `Clipboard` store + verb handlers in `display_server`; add `set_clipboard`/`get_clipboard` to `desktop_client`; wire Ctrl+C/V/X in `m3ui` text fields.
3. **Track C** — create `userspace/lib/imagefmt`, move the BMP/PNG decoders + blitter (and their tests) out of `greeter`, repoint `greeter`; add `decode_jpeg` + `encode_png` with host tests; add `CaptureOutput` to the protocol + `display_server`; build the `screenshot` binary (four-place new-binary wiring).
4. **Track D** — build `imgview` (open + render PNG/BMP/JPEG via `imagefmt`, `m3ui` chrome); build `settings` (the four sections); add `SetMasterVolume` to the audio protocol + `audio_mixer` master-gain; wire the Wi-Fi/brightness/battery sections to their backends; gate the CI surface, defer the live HW arms to the bare-metal protocol.
5. **Track E** — record the TUI-port charter (`nnn`/`lf`, `nano`/`vim`, `bsdtar`, `symphonia`) as parallel ports and the browser/office deferral; stand up Portfiles where in-scope for this PR.
6. **Validation** — host tests (layout solver, protocol codecs, JPEG decode + PNG encode round-trip); QMP/PPM render probes (`toolkit-render-probe`, `screenshot-smoke`); a two-client `clipboard-smoke`; the `imgview` render probe; the `settings` CI arm against a stub `wifi.control`; the live `settings` Wi-Fi/brightness arm under `Validated-on-HW`.

## Acceptance Criteria

- **Toolkit renders + is interactive:** an `m3ui-demo` Toplevel renders a button, a checkbox, a text field, and a list; `toolkit-render-probe` (QMP/PPM) asserts ≥ a threshold of changed scanlines vs an empty-surface baseline, then injects Tab → Tab → Enter and asserts the on-screen focus ring moved and an Enter-driven counter incremented (a blank/non-interactive surface fails). Keyboard **and** pointer focus both drive the demo.
- **Toolkit layout is host-tested:** `cargo test -p m3ui` passes, including a Column/Row solver test asserting exact `Rect`s for fixed+flex children, padding/spacing, and clip bounding; a focus-traversal test asserting Tab/Shift-Tab order over focusable widgets.
- **Clipboard round-trips:** `clipboard-smoke` launches two `m3ui`/`desktop_client` clients; client A `set_clipboard("M3OS_CLIP_OK")`, client B `get_clipboard()` returns exactly those bytes — asserted by a serial sentinel `CLIP_ROUNDTRIP_OK`; the protocol codec round-trip is host-tested.
- **Screenshot writes a valid PNG:** `screenshot-smoke` runs `screenshot`, then decodes the written file back with `imagefmt::decode_png` and asserts the dimensions equal `output_size()` and the pixel buffer is non-uniform (not all one color) — `SHOT_PNG_OK`; JPEG decode + PNG encode/decode round-trips are host-tested.
- **Image viewer opens all three formats:** `imgview` opens a bundled PNG, BMP, and JPEG; a render probe asserts each produces a non-black surface and the EXIF-free decode dimensions match the file headers.
- **Settings panel — CI arm:** the panel renders all four sections; the volume slider drives `audio_server` (`SetMasterVolume` reflected in `audio_mixer` master-gain, host-tested; a non-silent audio gate confirms a level change); the Wi-Fi section issues `WIFI_SCAN_REQ`/`WIFI_CONNECT_REQ` against a stub `wifi.control` service and renders the returned `ScanResult` rows.
- **Settings panel — live HW arm:** on the reference Dell (Tiger Lake) the panel lists real AX201 scan results, connects to a network with a passphrase (`WIFI_STATUS` shows the SSID + a leased IPv4), and the brightness slider changes the physical backlight — recorded per `docs/appendix/bare-metal-validation.md` with Status `Validated-on-HW (run N, date)`, the captured sentinel quoted (`SETTINGS_WIFI_ASSOCIATED <ssid>`), not a bare "Complete."
- **No regressions:** `cargo xtask check` is clean; the existing `greeter` still renders its background after the decoder extraction (the `tiling-smoke`/session render probes stay green).

## Companion Task List

- [Phase 105 Task List](./tasks/105-gui-toolkit-and-apps-tasks.md)

## How Real OS Implementations Differ

- **Immediate vs retained mode:** production desktops use retained-mode toolkits (GTK, Qt, AppKit) with a persistent widget object tree, an accessibility tree, CSS-class theming, and signal/slot event wiring. `m3ui` is immediate-mode (egui / Dear ImGui / microui) — far smaller and a natural fit for a re-rendered SHM surface, but with no retained tree, no a11y, and no rich theming. egui itself is ~tens of thousands of lines with GPU tessellation; `m3ui` is a teaching subset that blits CPU rectangles + cached glyphs.
- **Clipboard:** Wayland's `wl_data_device`/`wl_data_source`/`wl_data_offer` supports drag-and-drop, multiple simultaneous MIME types, incremental transfer, and primary selection; X11 adds `CLIPBOARD`/`PRIMARY`/`SECONDARY` atoms and ICCCM ownership negotiation. `m3ui`/`display_server` implement one selection, one MIME type (`text/plain`), copy-through-the-compositor — the minimal correct core.
- **Screenshot:** wlroots exposes `wlr-screencopy` with DMA-BUF zero-copy and `grim`/`slurp` region capture; m3OS does a full-output SHM blit + a CPU PNG encode. No GPU, no region select, no cursor compositing options.
- **Text:** real toolkits shape text with HarfBuzz (ligatures, BiDi, complex scripts) over FreeType + fontconfig. `m3ui` does LTR ASCII/Latin glyph runs from one bundled TTF via the kernel-core atlas — no shaping, no fallback chain.
- **The browser/office deferral:** Servo/WebKit/Blink and LibreOffice are multi-hundred-MB engines assuming GPU compositing, a JIT, full CSS/layout/JS, and a deep toolkit — infeasible here. Mature OSes ship them; a teaching OS substitutes text-mode `w3m`/`lynx` and the Track E TUI tools, which is an honest scope choice rather than a fake.

## Deferred Until Later

- **GPU-accelerated rendering** (the toolkit and compositor stay CPU-blit) — deferred to any future GPU phase.
- **Text shaping / BiDi / complex scripts / a font fallback chain** — `m3ui` is LTR ASCII+Latin from one bundled face.
- **Retained-mode / accessibility tree / screen-reader support, drag-and-drop, and theming-from-file** — out of scope for the immediate-mode core.
- **Rich clipboard** (multiple MIME types, `image/png` paste, files, primary selection, incremental transfer) — only `text/plain` in this phase.
- **A deep widget set** (tree views, tabbed notebooks, scrollbars beyond a simple list, modal dialogs, menus beyond the launcher) and in-toolkit animation — incremental follow-ons.
- **Image-viewer editing**, JPEG progressive/arithmetic/CMYK, and additional formats (GIF/WebP/TIFF) — decode-only baseline JPEG + PNG + BMP here.
- **A graphical browser engine and an office suite** — consciously infeasible; pursue text-mode `w3m`/`lynx` and the Track E TUI ports instead.
- **The Track E ports not built in this PR** (`nano`/`vim`, `bsdtar`, `symphonia` player) carry their own Portfiles in follow-on work; they are toolkit-independent.
