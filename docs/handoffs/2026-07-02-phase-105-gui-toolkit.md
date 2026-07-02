# Handoff — Phase 105: Native GUI Toolkit & Core Desktop Apps

**Date:** 2026-07-02 (living doc — update each session)
**Branch:** `feat/phase-105-settings-sound` (D.3 Sound slice; stacked on
`feat/phase-105-imgview-audio-volume` = PR #278, which is D.1+D.2 off `main`)
**State:** **Tracks A + B + C COMPLETE + green; Track D.1 + D.2 COMPLETE +
green (PR #278 open); D.3 Sound slice COMPLETE + green (this branch).**
A/B/C merged. D.3's remaining Wi-Fi-stub CI arm moves with D.4; D.4–D.5 are
the Dell-gated remainder. This branch adds the `settings` control-panel
Toplevel with the Sound section wired end to end
(slider → `audio_client::set_master_volume` → `audio_server` gain state) +
the `settings-smoke` gate. `cargo xtask check` clean.
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

## Track B — what landed (clipboard)

- Protocol (`kernel-core/src/display/protocol.rs`): `MimeTag` enum +
  `ClientMessage::{SetClipboard{tag,len,client_token}, RequestClipboard}` +
  `ServerMessage::ClipboardData{tag,len}`, opcodes 0x0019/0x001A/0x0143,
  codec + round-trip host tests.
- Store (`kernel-core/src/display/clipboard.rs`): `ClipboardStore` (64 KiB
  cap, reject-not-truncate, owner-scoped clear), host-tested.
- Compositor (`display_server`): the offer bytes ride the SAME IPC bulk
  trailing the `SetClipboard` frame (`frame.bulk[consumed..]`); handled in
  `client::dispatch` via new `DispatchOutcome.clipboard_{set,request}`
  fields; `RequestClipboard` answers SYNCHRONOUSLY by staging
  `[ClipboardData frame][bytes]` as the reply bulk (the control-socket
  reply shape), NOT the async 96-byte event queue. Offer dropped on the
  owner's Goodbye.
- `desktop_client::{set_clipboard(&str), get_clipboard()->Option<Vec<u8>>}`;
  single-IPC transport cap `CLIPBOARD_MAX_BYTES=3900` (frame+bytes < the
  4096 decode guard — multi-frame transfer for larger blobs is a follow-up).
- m3ui: `TextBuffer::apply_input` now returns `EditOutcome{text_changed,
  copy}`; `Ui::with_clipboard(get, set)` wires both; text_field does
  Ctrl+C (copy content), Ctrl+X (cut), Ctrl+V (paste first line).
- Gate: `clip-smoke` (fork: parent copies, child pastes as a distinct
  client) + `cmd_clipboard_smoke` asserting `CLIP_ROUNDTRIP_OK`
  (`M3OS_CLIPBOARD_REGRESSION`, exit 95). PASS.

## Track C — what landed (imagefmt + screenshot)

- **`userspace/lib/imagefmt`** — image codecs extracted from
  `greeter/src/image.rs` (git-mv → `imagefmt/src/lib.rs`) into a shared
  `#![cfg_attr(not(test), no_std)]` lib. `greeter` now re-exports it
  (`pub use imagefmt as image;`). Modules:
  - `lib.rs`: `ImageError`, `decode_bmp`, `decode_png`, `blit_scale_to_fit`
    (+ the original greeter tests). `MAX_IMAGE_PIXELS = 2048*2048`.
  - `png_encode.rs`: `encode_png(w, h, &[u32 BGRA]) -> Vec<u8>` — 8-bit
    RGBA, filter 0, **stored** DEFLATE blocks (no compressor), zlib
    Adler-32, per-chunk CRC-32. Round-trips through this crate's
    `decode_png`. BGRA `u32` `0xAARRGGBB` in, RGBA bytes out.
  - `jpeg.rs`: `decode_jpeg(&[u8]) -> Result<(u32,u32,Vec<u32>)>` —
    baseline SOF0 only (Huffman/DQT/DRI/restart, separable f32 IDCT with a
    const COS table, BT.601 YCbCr→RGB). SOF2 progressive → `Unsupported`.
    **no_std gotcha:** `f32::{round,floor,clamp}` are std-only — the IDCT
    rounds via `(val + 0.5) as i32` then integer `.clamp(0,255)`.
  - `tests/fixtures/tiny16.jpg` (16×16 DC-only baseline) generated by the
    committed `tools/mkjpeg.py` (a minimal standard-conformant encoder;
    no PIL/cjpeg on the host). 15 host tests; **added to
    `USERSPACE_LIB_HOST_TEST_PACKAGES`** so `xtask check` gates them.
- **`CaptureOutput` verb** (the only screenshot path — the compositor owns
  the framebuffer):
  - Protocol (`protocol.rs`): `ClientMessage::CaptureOutput{shm_id,
    max_width, max_height}` (opcode 0x001B, 12-byte body) +
    `ServerMessage::CaptureReply{width, height}` (opcode 0x0144, 8-byte
    body; `0×0` = rejected). Codec + round-trip host tests.
  - Pure-logic packer (`kernel-core/src/display/capture.rs`):
    `pack_capture_bgra(&FrameView, max_w, max_h, dst) -> (w,h)` — drops
    stride padding, swaps R/B for RGBA8888 framebuffers, clamps to what
    both `dst` and `src` hold. 6 host tests. (Grouped geometry into
    `FrameView` to dodge clippy `too_many_arguments`.)
  - Compositor (`display_server`): `client::dispatch` surfaces
    `DispatchOutcome.capture_request`; `main.rs::perform_capture` maps the
    client SHM, blits `owner.back_buffer_pixels()` (the same
    render-fingerprint source, valid in both memcpy + flip modes) via the
    packer, and stages a `CaptureReply` frame as the reply bulk. Pixels
    ride the SHM, not the reply bulk (the 4 KiB `MAX_BULK_LEN` can't hold a
    frame).
- **`screenshot` tool** (`userspace/screenshot`, four-place wired): queries
  `framebuffer_info` for dims (no FB ownership needed), `capture_output()`
  via `desktop_client`, `encode_png`, writes `/tmp/screenshot.png`, then
  re-reads + `decode_png` to prove the round-trip is lossless and counts
  non-corner pixels to catch a blank blit. Prints `SCREENSHOT_OK <w>x<h>
  nonblank=<N> bytes=<B> path=<P>` / `SCREENSHOT_FAIL reason=<why>`.
- **Gate**: `cmd_screenshot_smoke` / `screenshot_smoke_steps` (boot → login
  → `/bin/screenshot` → assert `SCREENSHOT_OK`), env
  `M3OS_SCREENSHOT_REGRESSION=1`, exit code 96. PASS on default `-smp` boot.

## Track D.1 + D.2 — what landed (imgview + master volume)

- **`imgview`** (`userspace/imgview`, four-place wired) — a `desktop_client`
  Toplevel on `m3ui` + `imagefmt`. Decodes each path arg (PNG/BMP/JPEG,
  detected by magic bytes), renders the current one scaled-to-fit (or 1:1)
  into the content region below an `m3ui` toolbar (`split_row` → filename
  label + Fit/1:1 toggle + Prev/Next `button_at`). Up-front it decodes every
  arg and prints `IMGVIEW:ok fmt=<x> ... nonblank=<N>` (or
  `IMGVIEW:blank`/`IMGVIEW:error`) per file — the serial oracle. Fixtures
  live under `xtask/assets/imgview/{sample.png,sample.bmp,sample.jpg}` (each
  regenerable by its `mk*.py`; PNG/BMP are tiny 32×24 gradients, JPEG is the
  imagefmt `tiny16.jpg`), staged to `/usr/share/imgview/` by
  `populate_ext2_files`.
- **Gate**: `cmd_imgview_smoke` / `imgview-smoke` boots the stack, runs
  `imgview` on all three fixtures, and asserts one `IMGVIEW:ok fmt=<x>` per
  format (fail on `blank`/`error`). `M3OS_IMGVIEW_REGRESSION=1`, exit 97.
  **PASS on a default multi-core boot.** Serial-only (the per-format
  non-blank count is the oracle — consistent with `screenshot-smoke`).
- **Audio `SetMasterVolume` (D.2)** — a system master volume:
  - Protocol: `AudioControlCommand::SetMasterVolume { q15_gain: u16 }`
    (opcode 0x0202) in `kernel-core/src/audio/protocol.rs`; codec +
    round-trip + proptest coverage. Q15: `0x8000` = unity, `0` mutes.
  - `audio_mixer::Mixer` gained a `set_master_volume`/`master_gain_q15` +
    applies the gain to its i64 accumulator pre-clamp (the per-client DOOM
    mixer path); 4 host tests (unity/zero/half/clamp).
  - `kernel-core::audio::gain::apply_master_gain_s16le` — the pure S16LE
    in-place scaler used by the **server** (the mixer runs in the client, so
    the *system* master applies where `audio_server` forwards PCM); 5 host
    tests.
  - `audio_server` (`irq.rs`): holds `master_gain_q15`, updates it on the
    `SetMasterVolume` verb, and runs forwarded PCM through `gained_pcm`
    (unity = zero-copy passthrough; below unity copies into a reused scratch
    so a read-only page grant is never mutated); 3 host tests.
  - No new QEMU gate for the audible level change — that arm belongs with the
    settings slider (D.3) that drives it; the host tests prove the full
    verb→state→PCM-scale path.

## Track D.3 Sound slice — what landed (this branch)

- **`audio_client::set_master_volume(q15_gain) -> Result<AudioStats>`** —
  mirrors `get_stats` (control-plane, works on a `connect()`-only client;
  the server replies with the uniform `Stats` shape). 3 new mock-socket
  host tests (control-only send, boundary gains unclamped client-side,
  UnexpectedReply).
- **`audio_server` change sentinel** — the io-loop `SetMasterVolume` arm
  now prints `AUDIO_SMOKE:master_gain q15=<N>` **only when the clamped
  gain actually changes** (a dragged slider can't spam serial). This is
  the server-side smoke oracle; the stub loop stays sentinel-free (it
  accepts the verb but holds no gain state — a gate run that lands in
  stub mode fails on the missing sentinel, correctly).
- **`userspace/settings`** (three-place wired: workspace member, xtask
  `bins`, ramdisk `BIN_ENTRIES`; **no service conf** — launched from the
  prompt like `imgview`). An `m3ui` Toplevel (m3ui-demo loop skeleton)
  rendering Network/Display/Sound/Power; Network/Display/Power are
  placeholder label rows naming their Phase 103/104 dependency. The
  volume slider is the panel's **only focusable widget** → default
  keyboard focus from frame 1, Left/Right = ±1%. `pct_to_q15` maps
  0–100% onto Q15 (`pct * 0x8000 / 100`, truncating; 100% = exactly
  unity). Volume pushes coalesce to one IPC per frame. Sentinels:
  `SETTINGS:ready`, `SETTINGS:audio=ok|unavailable`,
  `SETTINGS:volume=<pct> q15=<q> ack=ok|err|none`. Starts at 100% —
  matching the server's boot-time unity gain, so no startup write.
- **Gate `settings-smoke`** (`cmd_settings_smoke`, exit 98,
  `M3OS_SETTINGS_REGRESSION=1`, pre-push wired): toolkit-render-probe
  boot pattern (QMP+VNC, `-vga std`) **plus
  `append_ac97_audio_flags_headless`** so the ring-3 `ac97` driver claims
  the device and `audio_server` runs the real io loop (waits
  `AUDIO_SMOKE:server:READY` before launching). QMP-types
  `/bin/settings\n` at the term prompt, waits `SETTINGS:audio=ok` +
  `SETTINGS:ready`, then two `Left` presses assert the pinned pairs
  99%→q15=32440 and 98%→q15=32112 (client ack + server sentinel each)
  plus a ≥12-scanline PPM repaint diff. Uses `parse_less_render_probe_args`
  (`--timeout/--out/--keep-qemu`).
- Docs: AGENTS.md gate table row; `regression-gates.md` gained the
  missing Phase 105 gate stanzas (clipboard/screenshot/imgview/toolkit)
  + a full `settings-smoke` description; tasks doc D.3 updated.

## RESUME HERE — Track D.3 Wi-Fi-stub arm → D.4 / Track E next

The D.3 Sound slice is done + green (`settings-smoke` PASS). What's left
in Track D is **Dell-adjacent**: the D.3 **Wi-Fi stub-service CI arm**
(stub `wifi.control` service + ScanResult rows + passphrase text_field
in the Network section — moves with D.4's backend clients), **D.4** live
Wi-Fi (104) + brightness/battery (103) backends, **D.5** on-metal
validation. **Track E** (TUI-in-`term` ports) is parallel ports-infra
work that can land anytime. Merge order: PR #278 (D.1+D.2) first, then
this branch's PR (D.3 Sound).
