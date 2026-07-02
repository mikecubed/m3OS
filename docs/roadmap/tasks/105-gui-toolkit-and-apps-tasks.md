# Phase 105 — Native GUI Toolkit & Core Desktop Apps: Task List

**Status:** In progress — **Tracks A + B + C landed and green; Track D.1 + D.2 landed and green; D.3 Sound slice landed and green.** A/B/C merged. C = `imagefmt` shared crate (extracted BMP/PNG + new baseline JPEG decode + PNG encode) + `CaptureOutput`/`CaptureReply` verbs + `screenshot` tool + `screenshot-smoke`. D.1 = `imgview` viewer (`imagefmt` + `m3ui` Toplevel) + `imgview-smoke` (PNG/BMP/JPEG decode + non-blank render, PASS multi-core). D.2 = audio `SetMasterVolume` verb + `audio_mixer` master-gain + kernel-core `audio::gain` + `audio_server` `gained_pcm` (host-tested). D.3 Sound slice = `settings` panel Toplevel (four sections; Sound's volume slider drives `SetMasterVolume` via the new `audio_client::set_master_volume`) + `settings-smoke`. E = charter recorded (in the phase doc since chartering) + **`nano` 8.7, `nnn` 5.2, and `bsdtar` 3.8.8 ports landed** (static; `tui-app-smoke` steps — bsdtar's is a gzip create→extract→`cat` round-trip); the `symphonia` player (+ `vim` alt) is the follow-on. Remaining: **D.3 Wi-Fi-stub CI arm + D.4–D.5** (settings backends + live Wi-Fi/brightness — 103/104 → Dell) / **E `symphonia` player**. Handoff: `docs/handoffs/2026-07-02-phase-105-gui-toolkit.md`.
**Source Ref:** phase-105
**Depends on:** Phase 100 (Bare-Metal GUI Session — compositor + session in init, WC framebuffer, USB-mouse cursor) ✅, Phase 99 (SMP & Scheduler Robustness) ✅ via 100. The **settings panel** is additionally sequenced after Phase 103 (power: brightness/battery) and Phase 104 (Wi-Fi AX201 + connect daemon); Tracks A/B/C and the `imgview` app are **not** gated on 103/104.
**Goal:** Ship a minimal native immediate-mode Rust widget toolkit (`m3ui`) on `desktop_client`, a compositor-brokered clipboard, a shared `imagefmt` crate (extracted BMP/PNG + new JPEG decode + PNG encode) with an output-capture screenshot tool, and the two core desktop apps (image viewer + settings/control panel) that make the GUI usable — the settings panel being the user-facing consumer of the Phase 103 power and Phase 104 Wi-Fi backends. Toolkit layout, protocol codecs, and the image codecs are host-tested; rendering/interaction are proven by QMP/PPM render probes; the live Wi-Fi/brightness arm is validated on the reference Dell per `docs/appendix/bare-metal-validation.md`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `m3ui` immediate-mode toolkit (layout solver, widgets, input/focus, theme, proportional text) — host-tested layout | 100 | **Done + green** |
| B | Clipboard / data-transfer protocol (`display_server` broker + protocol verbs + `desktop_client` helpers + `m3ui` Ctrl+C/V/X) | 100 | **Done + green** |
| C | `imagefmt` shared crate (extract BMP/PNG, add JPEG + PNG encode) + `CaptureOutput` in `display_server` + `screenshot` tool | B (capture) | **Done + green** |
| D | `imgview` image viewer + `settings` control panel (Wi-Fi/brightness/volume/battery) + audio `SetMasterVolume` | A, C; settings also 103, 104 | **D.1 + D.2 + D.3 Sound slice done + green**; D.3 Wi-Fi-stub arm + D.4–D.5 (backends/HW) pending |
| E | TUI-in-`term` parallel ports charter (`nnn`/`lf`, `nano`/`vim`, `bsdtar`, `symphonia`); browser/office deferral | — | **Charter recorded; `nano` 8.7 + `nnn` 5.2 + `bsdtar` 3.8.8 ports landed + green** (in `tui-app-smoke`); `symphonia` player (+ `vim` alt) follow-on |

---

## Track A — `m3ui` Immediate-Mode Toolkit

### A.1 — Crate scaffold + workspace wiring

**Files:**
- `userspace/lib/m3ui/Cargo.toml`, `userspace/lib/m3ui/src/lib.rs` (new)
- `Cargo.toml` (workspace `members`)

**Symbol:** the crate root + `pub mod {layout, widget, input, theme}`
**Why it matters:** `m3ui` is the central missing layer; it must be a `#![no_std]` + `alloc` library depending on `desktop_client`, `kernel-core` (font), and `syscall-lib`, so every later widget/app builds on one foundation rather than re-hand-rolling pixels.

**Acceptance:**
- [ ] `cargo xtask check` builds `m3ui`; it is a `no_std` lib (no binary wiring needed for the lib itself).
- [ ] `Cargo.toml` deps are exactly `desktop_client`, `kernel-core` (default-features off), `syscall-lib` — no `std`/libc.

### A.2 — Pure-logic layout/constraint solver (host-tested)

**File:** `userspace/lib/m3ui/src/layout.rs` (new)
**Symbol:** `LayoutTree`, `Constraint::{Fixed, Flex}`, `solve(region: Rect) -> Vec<Rect>`
**Why it matters:** The solver is the falsifiable core of the toolkit and the only part testable without a framebuffer; getting `Row`/`Column` + fixed/flex + padding/spacing + a clip stack right is what makes every widget land in the correct rect.

**Acceptance:**
- [ ] Host test: a `Column` of three `Fixed(40)` rows + one `Flex(1)` spacer in a `200×400` region yields four rects with the exact y-offsets/heights (flex absorbs the `400 − 3×40` remainder).
- [ ] Host test: `Row` flex split distributes leftover width deterministically across multiple `Flex` weights; padding + spacing shrink children correctly.
- [ ] Host test: nested container clip bounds a child rect to its parent (no overflow past the parent region).
- [ ] `cargo test -p m3ui --target x86_64-unknown-linux-gnu` passes.

### A.3 — Input folding + focus traversal

**File:** `userspace/lib/m3ui/src/input.rs` (new)
**Symbol:** `InputState::from_events(&[ServerMessage]) -> InputState`, `FocusRing::advance`
**Why it matters:** The toolkit must turn the `display_server` event stream (`ServerMessage::{Key, Pointer, FocusIn, FocusOut, SurfaceResized}` from `DisplayConnection::pull_event`) into a per-frame pointer/key/modifier state and own focus order, or no widget can be activated by keyboard or pointer.

**Acceptance:**
- [ ] `InputState` exposes pointer x/y + button state, a per-frame key queue, and a modifier mask folded from the `KeyEvent` stream.
- [ ] Host test: Tab advances focus across the frame's focusable widgets in declaration order and Shift-Tab reverses; Enter/Space mark the focused widget activated.
- [ ] Pointer hit-test against a widget `Rect` is exercised by a host test (point-in-rect, including edges).

### A.4 — Widgets: label, button, text_field, checkbox, list, slider

**File:** `userspace/lib/m3ui/src/widget.rs` (new)
**Symbol:** `Ui::{label, button, text_field, checkbox, list, slider, separator}`
**Why it matters:** These are the concrete building blocks the apps need; each is an immediate-mode call that claims a layout rect, draws chrome via `desktop_client::{fill_rect, stroke_rect}` + proportional text, hit-tests, and returns its interaction result.

**Acceptance:**
- [ ] `button` returns `true` on the frame it is clicked/activated; `checkbox(&mut bool)` toggles; `text_field(&mut String)` inserts/backspaces/cursor-moves from the key queue; `list` returns the selected index; `slider(&mut f32, range)` updates on drag.
- [ ] Each widget draws a visible focus ring when focused (asserted indirectly by A.7's render probe).
- [ ] A host test exercises `text_field` editing logic (insert at cursor, backspace, left/right) against a synthetic key queue without a framebuffer.

### A.5 — Theme + proportional text

**Files:**
- `userspace/lib/m3ui/src/theme.rs` (new)
- `userspace/lib/m3ui/src/text.rs` (new)

**Symbol:** `Theme` (colors/metrics), `TextLayer` over `kernel_core::font::atlas::Atlas`
**Why it matters:** A shared `Theme` gives every app one look, and proportional text (TTF glyphs via the kernel-core `Atlas`, ASCII falling back to `desktop_client::draw_text`) lifts widget labels above the fixed 8×16 cell grid.

**Acceptance:**
- [ ] `Theme::default()` defines fg/bg/accent/border colors + padding/spacing/font-size; widgets read all colors/metrics from it.
- [ ] `TextLayer` measures and blits a proportional string via `kernel_core::font::atlas::Atlas::resolve`, falling back to the bitmap path for codepoints the atlas lacks.
- [ ] A host test asserts `TextLayer` advance-width for a known ASCII string is monotonic and non-zero.

### A.6 — `Ui` per-frame context

**File:** `userspace/lib/m3ui/src/lib.rs`
**Symbol:** `Ui::begin(&SharedSurface, &InputState)`, `Ui::end() -> Rect`
**Why it matters:** The frame context ties layout + input + widgets together and returns the damage rect, so an app's loop is `pull events → Ui::begin → declare widgets → Ui::end → attach_damage_commit`.

**Acceptance:**
- [ ] `Ui::begin`/`end` bracket a frame; `end` returns the union damage rect of touched widgets (or full-surface).
- [ ] A documented example loop in the crate docs shows the `pull_event` → `Ui` → `desktop_client::DisplayConnection::attach_damage_commit` round.

### A.7 — `m3ui-demo` Toplevel + `toolkit-render-probe` gate

**Files:**
- `userspace/m3ui-demo/{Cargo.toml,src/main.rs}` (new — four-place new-binary wiring: workspace member, xtask `bins`, `kernel/src/fs/ramdisk.rs` `include_bytes!`+`BIN_ENTRIES`)
- `xtask/src/main.rs` (`cmd_toolkit_render_probe`, new)

**Symbol:** `main` (demo), `cmd_toolkit_render_probe`
**Why it matters:** A serial `Wait` cannot see rendered widgets; the QMP/PPM render-probe (mirroring `cmd_less_render_probe`) is the falsifiable proof the toolkit actually draws and responds to input.

**Acceptance:**
- [x] `m3ui-demo` renders a button + checkbox + text field + a list in a Toplevel; `needs_alloc = true`, defines `#[global_allocator]` (`syscall_lib::heap::BrkAllocator`).
- [x] `toolkit-render-probe` screendumps a baseline, then asserts the rendered frame changed ≥ a threshold of scanlines vs an empty surface.
- [x] The probe injects Enter (default-focused `+1` button) and asserts the counter incremented on serial (`M3UI_DEMO:count=1`) AND the composited frame repainted ≥12 scanlines; a `Tab` press then moves the focus ring (frame changes again). *(Uses `QmpClient::press_key`, the actual API name.)*
- [x] The pointer-activation logic is host-tested (`m3ui::ui::tests::button_click_by_pointer` injects a pointer click and asserts the button fires); driving QEMU's absolute pointer through the guest input stack is input-plumbing owned by `usb-smoke`, not the toolkit, so it is deliberately not re-tested in this render gate.

---

## Track B — Clipboard / Data-Transfer Protocol

### B.1 — Clipboard protocol verbs + codec (host-tested)

**File:** `kernel-core/src/display/protocol.rs`
**Symbol:** `ClientMessage::{SetClipboard, RequestClipboard}`, `ServerMessage::ClipboardData`
**Why it matters:** No clipboard exists in the tree today; the transfer must be a compositor-brokered offer/request with bytes on the bulk channel (the `pull_event`/`ipc_take_pending_bulk` pattern), never shared writable memory.

**Acceptance:**
- [x] `SetClipboard { mime_tag, len }`, `RequestClipboard { mime_tag }`, and `ClipboardData { mime_tag, len }` added to the protocol enums with `encode`/`decode`.
- [x] Host tests round-trip all three new variants through `encode`→`decode` (matching the existing protocol codec tests).
- [x] `mime_tag` enumerates at least `TextPlainUtf8`; the wire format is documented inline.

### B.2 — Compositor clipboard store + verb handlers

**File:** `userspace/display_server/src/main.rs` (or a new `userspace/display_server/src/clipboard.rs`)
**Symbol:** `Clipboard` (bounded store), the `SetClipboard`/`RequestClipboard` dispatch arms
**Why it matters:** `display_server` is the only process that can broker a clipboard (clients share no memory); it must store the last offer and answer paste requests, dropping the offer on client `Goodbye`.

**Acceptance:**
- [x] An offer's bytes are stored up to a documented cap (e.g. 64 KiB); a larger offer is rejected, not truncated silently.
- [x] A `RequestClipboard` returns the stored bytes via `ClipboardData` + the bulk channel; an empty clipboard returns a zero-length `ClipboardData`.
- [x] The store is bounded and freed; a client `Goodbye` that owned the offer does not leave a dangling buffer (host-tested store logic where extractable).

### B.3 — `desktop_client` clipboard helpers + `m3ui` editing keys

**Files:**
- `userspace/lib/desktop_client/src/lib.rs`
- `userspace/lib/m3ui/src/widget.rs`

**Symbol:** `DisplayConnection::{set_clipboard, get_clipboard}`, `text_field` Ctrl+C/V/X handling
**Why it matters:** Apps need an ergonomic copy/paste call, and the toolkit text field is where a user expects Ctrl+C/V/X to work.

**Acceptance:**
- [x] `set_clipboard(&str)` sends `SetClipboard` + the bytes; `get_clipboard() -> Option<Vec<u8>>` issues `RequestClipboard` and reads `ClipboardData`.
- [x] `m3ui::text_field` copies its selection/content on Ctrl+C, cuts on Ctrl+X, and pastes `get_clipboard()` text at the cursor on Ctrl+V.

### B.4 — `clipboard-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_clipboard_smoke` (new)
**Why it matters:** Proves the end-to-end round-trip between two independent clients — the phase's clipboard acceptance — rather than just the codec.

**Acceptance:**
- [x] Two `desktop_client` clients run; client A `set_clipboard("M3OS_CLIP_OK")`, client B `get_clipboard()` returns exactly those bytes → serial sentinel `CLIP_ROUNDTRIP_OK`.
- [x] The gate fails (no `CLIP_ROUNDTRIP_OK`) if the bytes differ or the request returns empty.

---

## Track C — `imagefmt` + Output Capture + Screenshot

### C.1 — Extract BMP/PNG decoders into `imagefmt`

**Files:**
- `userspace/lib/imagefmt/{Cargo.toml,src/lib.rs}` (new)
- `userspace/greeter/src/image.rs` (becomes a thin re-export / is removed), `userspace/greeter/src/lib.rs`
- `Cargo.toml` (workspace `members`)

**Symbol:** `decode_bmp`, `decode_png`, `blit_scale_to_fit`, `ImageError` (moved verbatim, tests included)
**Why it matters:** The greeter decoders are the only image codecs in the tree and are buried in one app; extracting them (with their host tests) into a shared crate lets `imgview` and `screenshot` reuse them and stops per-app duplication.

**Acceptance:**
- [x] `decode_bmp`/`decode_png`/`blit_scale_to_fit`/`ImageError` live in `imagefmt` (git-mv of `greeter/src/image.rs` → `imagefmt/src/lib.rs`); the existing `image.rs` host tests move with them and pass (`cargo test -p imagefmt`). Added to `USERSPACE_LIB_HOST_TEST_PACKAGES` so `xtask check` gates them.
- [x] `greeter` depends on `imagefmt` (`pub use imagefmt as image;`) and still decodes + renders its background; the session/`tiling-smoke` render path is unchanged.

### C.2 — Baseline JPEG decoder

**File:** `userspace/lib/imagefmt/src/jpeg.rs` (new)
**Symbol:** `decode_jpeg(&[u8]) -> Result<(u32, u32, Vec<u32>), ImageError>`
**Why it matters:** The image viewer must cover the three common formats; JPEG is the only one not already present, and a `no_std` baseline decoder rounds out `imagefmt`.

**Acceptance:**
- [x] Decodes baseline (SOF0) Huffman JPEG: SOI/APP0/DQT/DHT/SOF0/SOS parse, dequant + 8×8 IDCT + YCbCr→BGRA; returns `ImageError::Unsupported` for progressive (SOF2)/arithmetic/CMYK.
- [x] Host test: a bundled 16×16 baseline JPEG (`tests/fixtures/tiny16.jpg`, generated by the committed `tools/mkjpeg.py`) decodes to the expected dimensions and a sane (non-uniform, in-gamut, opaque) pixel buffer; a truncation-fuzz test asserts no panic.
- [x] Re-expressed for `no_std`+`alloc`; provenance noted in the module header. **no_std gotcha:** `f32::{round,floor,clamp}` are std-only, so the IDCT rounds via `(val + 0.5) as i32` + integer `.clamp(0,255)`.

### C.3 — PNG encoder

**File:** `userspace/lib/imagefmt/src/png_encode.rs` (new)
**Symbol:** `encode_png(width, height, &[u32]) -> Vec<u8>`
**Why it matters:** The first encoder in the tree; the screenshot tool needs to write a real PNG, and the encode→decode round-trip is a clean falsifiable test.

**Acceptance:**
- [x] Emits a valid PNG (signature, IHDR RGBA8, IDAT with a **stored** deflate stream + Adler-32, IEND with per-chunk CRC-32). A >64 KiB raw image spans multiple stored blocks (host-tested).
- [x] Host test: `decode_png(encode_png(w, h, px))` returns the same `(w, h)` and pixel values (round-trip); an Adler-32 known-answer test pins the checksum.

### C.4 — `CaptureOutput` verb in `display_server`

**Files:**
- `kernel-core/src/display/protocol.rs`
- `userspace/display_server/src/main.rs`

**Symbol:** `ClientMessage::CaptureOutput { shm_id, max_width, max_height }` (opcode 0x001B) + `ServerMessage::CaptureReply { width, height }` (opcode 0x0144); the dispatch arm blitting `owner.back_buffer_pixels()`. Pure packer: `kernel_core::display::capture::pack_capture_bgra`.
**Why it matters:** Clients cannot read the kernel framebuffer (`display::fb_owner` makes the compositor the sole reader), so the only way to screenshot is a compositor-side blit of the composited output into a client-provided SHM.

**Acceptance:**
- [x] `CaptureOutput` validates the client SHM is ≥ `width*height*4` bytes (via `shm_size`) and blits the most-recently-composed frame into it as packed BGRA8888 (stride padding dropped, R/B swapped for RGBA8888 framebuffers), replying `CaptureReply { width, height }`. Pixels ride the SHM (the 4 KiB reply bulk can't hold a frame).
- [x] Codec round-trip host-tested (both verbs, incl. the `0×0` reject); the pure packer has 6 host tests (padding-drop, RGBA swap, max-dim clamp, undersized-`dst` row clamp, sub-row reject, zero-area). An undersized/unmappable `shm_id` yields a `0×0` reply, not a partial write.

### C.5 — `screenshot` tool + `screenshot-smoke` gate

**Files:**
- `userspace/screenshot/{Cargo.toml,src/main.rs}` (new — four-place new-binary wiring)
- `xtask/src/main.rs` (`cmd_screenshot_smoke`, new)

**Symbol:** `main` (`framebuffer_info` → `desktop_client::capture_output` (allocate SHM → `CaptureOutput`) → `encode_png` → write file → re-read + `decode_png` self-check), `cmd_screenshot_smoke`
**Why it matters:** Closes the screenshot path end-to-end and gives the phase its "a screenshot writes a valid PNG of the current output" acceptance.

**Acceptance:**
- [x] `screenshot [PATH]` writes `/tmp/screenshot.png` (or the arg path); exits non-zero on capture/encode/write failure (`SCREENSHOT_FAIL reason=<why>`).
- [x] `screenshot-smoke` runs it; the tool itself re-reads + `imagefmt::decode_png`s the written file, asserts dimensions == the captured `(w,h)` and pixels equal the capture (lossless round-trip), and counts non-corner pixels to reject a blank blit → `SCREENSHOT_OK <w>x<h> nonblank=<N> bytes=<B> path=<P>`. Gate `M3OS_SCREENSHOT_REGRESSION=1`, exit 96. **PASS on a default multi-core boot.**

---

## Track D — Image Viewer + Settings/Control Panel

### D.1 — `imgview` image-viewer Toplevel

**Files:**
- `userspace/imgview/{Cargo.toml,src/main.rs}` (new — four-place new-binary wiring)

**Symbol:** `main` (Toplevel loop: open file → `imagefmt` decode → `blit_scale_to_fit` → `m3ui` chrome)
**Why it matters:** The first content app on the new toolkit; proves `imagefmt` + `m3ui` + a Toplevel compose together for a real user task.

**Acceptance:**
- [x] Opens a PNG, a BMP, and a JPEG (by path arg, format auto-detected by magic bytes) and renders each scaled-to-fit; an `m3ui` toolbar (`split_row`) shows the filename + a Fit/1:1 toggle + Prev/Next buttons. Fixtures: `xtask/assets/imgview/{sample.png,sample.bmp,sample.jpg}` staged to `/usr/share/imgview/`.
- [x] `imgview-smoke` (serial, `M3OS_IMGVIEW_REGRESSION=1`, exit 97) asserts each format decodes and scale-to-fit-renders to non-blank content (`IMGVIEW:ok fmt=<x> nonblank=<N>`, one per format); a decode failure shows an error label + `IMGVIEW:error`/`IMGVIEW:blank` rather than crashing. **PASS on a default multi-core boot.** (Serial non-blank self-check stands in for a PPM probe, mirroring `screenshot-smoke`.)

### D.2 — Audio `SetMasterVolume` control verb + mixer master-gain

**Files:**
- `kernel-core/src/audio/protocol.rs`
- `userspace/lib/audio_mixer/src/lib.rs`
- `userspace/audio_server/src/irq.rs` (dispatch)

**Symbol:** `AudioControlCommand::SetMasterVolume { q15_gain }`, `audio_mixer` master-gain apply
**Why it matters:** The Phase 57 audio control surface is `GetStats`-only; the settings panel's volume slider needs a real volume verb, applied as a master gain in the mixer.

**Acceptance:**
- [x] `SetMasterVolume { q15_gain: u16 }` added to `AudioControlCommand` (opcode 0x0202) with codec; per-value round-trip test + proptest strategy updated. Q15: `0x8000` = unity, `0` = mute.
- [x] `audio_mixer::Mixer` applies the master gain to the mixed S16LE output (`set_master_volume`/`master_gain_q15`, applied to the i64 accumulator pre-clamp); host tests: gain 0 → silence, unity → unchanged, half → scaled, above-unity → clamped.
- [x] `audio_server` handles the verb on the control surface: it holds `master_gain_q15` and runs forwarded PCM through `gained_pcm` (kernel-core `audio::gain::apply_master_gain_s16le`; unity = zero-copy, below-unity copies into a reused scratch so a read-only page grant is never mutated). Host-tested at the mixer, gain-helper (5), and `gained_pcm` (3) layers. **Note:** the live *audible* level-change gate is deferred to D.3 (the settings slider that drives the verb); the host tests prove the full verb→state→PCM-scale path meanwhile.

### D.3 — `settings` control panel Toplevel

**Files:**
- `userspace/settings/{Cargo.toml,src/main.rs}` (new — workspace member + xtask `bins` + ramdisk `BIN_ENTRIES`; no service conf — launched from the prompt like `imgview`)
- `userspace/lib/audio_client/src/lib.rs` (`set_master_volume` verb, new)
- `userspace/audio_server/src/irq.rs` (`AUDIO_SMOKE:master_gain` change sentinel)
- `xtask/src/main.rs` (`cmd_settings_smoke`, new)

**Symbol:** `main` + `build_ui` (one `Ui` pass declaring the four sections), `pct_to_q15`
**Why it matters:** The deliberate user-facing consumer of the Phase 103/104 backends — the reason the GUI workstation is usable without a shell.

> **Landed in the hardware-free Sound slice:** the panel + Sound wiring +
> gate. The Wi-Fi stub-service CI arm moves with D.4's backend wiring.

**Acceptance:**
- [x] Renders four `m3ui` sections (Network, Display, Sound, Power) with working focus — the volume slider holds default keyboard focus; Network/Display/Power render placeholder rows naming their Phase 103/104 dependency until D.4 wires them. *(No scrolling: the panel fits its content; revisit if D.4's section content outgrows the window.)*
- [x] **Sound:** the volume slider drives `SetMasterVolume` (D.2) via the new `audio_client::set_master_volume` (control-plane verb, host-tested against the mock socket); the server confirms the gain-state update via the change-only `AUDIO_SMOKE:master_gain q15=<N>` sentinel in the io loop. Gate: `settings-smoke` (`M3OS_SETTINGS_REGRESSION=1`, exit 98) — QMP/VNC boot with the AC'97 device attached, keyboard `Left` drives 100%→99%→98% asserting client-ack + server-state sentinels and a ≥12-scanline repaint. **PASS on a default multi-core boot.**
- [ ] **CI arm:** the Wi-Fi section issues `wifi_core::control::WIFI_SCAN_REQ`/`WIFI_CONNECT_REQ` against a stub `wifi.control` service and renders the returned `ScanResult` rows + a passphrase `text_field`; gated headlessly. *(Deferred to the D.4 slice — lands with the backend clients.)*

### D.4 — Settings backends: Wi-Fi (104), brightness/battery (103)

**File:** `userspace/settings/src/main.rs` (backend clients)
**Symbol:** `wifi_connect(ssid, passphrase)` over `wifi-core::control`, brightness/battery calls into the Phase 103 surface
**Why it matters:** Wires the panel to the real headless backends; this is the integration point that makes 103/104 user-visible.

**Acceptance:**
- [ ] Wi-Fi: selecting a scan row + entering a passphrase sends `WIFI_CONNECT_REQ`; `WIFI_STATUS` (`wifi_core::control::WifiStatus`) drives the associated-SSID + RSSI + IPv4 display.
- [ ] Display: the brightness slider calls the Phase 103 backlight setter; Power: battery % + AC state read from the Phase 103 surface and rendered.

### D.5 — Live HW validation (settings over real AX201 + backlight)

**File:** `docs/appendix/bare-metal-validation.md` (results appendix) + `scripts/` runbook entry
**Symbol:** the recorded reference-Dell run
**Why it matters:** AX201 and the laptop backlight have no QEMU model; the panel's headline capability (join a real network, change the real backlight) can only be proven on metal, per the Phase 98 convention.

> **Status: operator-owned / HW-only.** Validated on the reference Dell (Tiger Lake) per `docs/appendix/bare-metal-validation.md`; CI carries the stub-service + render arms (D.3/D.4).

**Acceptance:**
- [ ] On the Dell: the panel lists real AX201 scan results, connects with a passphrase, and `WIFI_STATUS` shows the SSID + a leased IPv4 — captured sentinel `SETTINGS_WIFI_ASSOCIATED <ssid>` quoted in the runbook.
- [ ] The brightness slider visibly changes the physical backlight (on-device render assertion or dated photo per the validation appendix).
- [ ] The task-doc / README Status for this arm reads `Validated-on-HW (run N, date) — Dell Precision 5560 / Tiger Lake; evidence: <pointer>`, not a bare "Complete."

---

## Track E — TUI-in-`term` Parallel Ports (Charter)

### E.1 — TUI port charter + browser/office deferral note

**Files:**
- `docs/roadmap/105-gui-toolkit-and-apps.md` (Feature Scope Track E, Deferred Until Later)
- (in-scope-this-PR) `ports/util/<name>/Portfile` stubs where built

**Symbol:** the charter entries (`nnn`/`lf`, `nano`/`vim`, `bsdtar`, `symphonia` player); the `w3m`/`lynx` substitute note
**Why it matters:** These are toolkit-independent (they run in the existing `term` on the ncurses/termios stack + the Phase 85 ports infra) and give the workstation real file/edit/archive/media tooling cheaply, while recording the honest infeasibility of a graphical browser/office suite.

**Acceptance:**
- [x] The design doc records the four TUI ports as parallel (not toolkit-blocked) work and the browser/office deferral with the text-mode `w3m`/`lynx` substitute. *(Charter §"Track E" + §"Deferred Until Later" — present since the phase was chartered.)*
- [x] Any TUI port actually built in this PR follows the "Adding a New Cross-Compiled Port" rule (Portfile + `PORTS` + `port_build.rs` dispatch + `tui-app-smoke` step) and runs in `term`. **Landed: `nano` 8.7** (autotools; htop's `-idirafter` Linux-UAPI injection for `<sys/vt.h>`; wide-curses pinned via `NCURSESW_CFLAGS/LIBS`) **and `nnn` 5.2** (plain Makefile; `O_NORL/O_NOX11/O_NOFIFO`; `patches/0001-inotify-optional.patch` since m3OS has no inotify — degrades to no-directory-watching). Both static against the in-tree ncursesw/tinfow, sealed into the pkgcache, staged by `populate_phase_69d_ports`, and asserted by new `tui-app-smoke` steps (nano: title bar + seeded buffer line render, ^X exit; nnn: two seeded entries render, `q` exit). **Also landed: `bsdtar` 3.8.8** (libarchive; static bsdtar only, zlib the sole codec backend, acl/xattr off — no such syscalls; smoke = gzip create → extract → `cat` payload round-trip with the fence discipline). Remaining charter ports (`symphonia` player, `vim` alternate) are follow-on per the charter's "not necessarily all in this phase's PR."

---

## Documentation Notes

- `m3ui` is the central missing layer the whole GUI-workstation arc needs — record that `desktop_client`'s own doc comment ("this crate is *not* a toolkit") is the gap this phase closes, and that `m3ui`'s `layout` is intra-window widget layout, distinct from the compositor's window-*tiling* `userspace/lib/layout`.
- The clipboard, `CaptureOutput`, and `SetMasterVolume` are **new protocol verbs** — keep their codecs host-tested alongside the existing display/audio protocol tests, and note the Wayland-selection / `GetStats`-only lineage they extend.
- `imagefmt` is the greeter decoders **moved** (not copied) — once extracted, the only image codecs in the tree live there; the JPEG decoder and PNG encoder are net-new. Keep the `jpeg-decoder` provenance note in `jpeg.rs`.
- The settings panel is the only Track gated on Phases 103/104; Tracks A/B/C and `imgview` land independent of the HW arc, so the CI surface (render probes, host tests, `clipboard-smoke`, `screenshot-smoke`) is green without any laptop hardware.
- The live Wi-Fi/brightness arm uses the Phase 98 `Validated-on-HW (run N, date)` convention — do not mark D.5 "Complete" on an uncaptured run.
- Prefer exact files/symbols over directories as these land; update the checkboxes and the Track Layout status column per track as work completes.
