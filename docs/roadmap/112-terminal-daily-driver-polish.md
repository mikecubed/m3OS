# Phase 112 - Terminal Daily-Driver Polish (Scrollback + Selection/Clipboard)

**Status:** ✅ Complete
**Source Ref:** phase-112
**Depends on:** Phase 22 (TTY and Terminal) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 69–69d (Terminal TUI Capabilities + ncurses) ✅, Phase 92b (USB HID Report Protocol) ✅, Phase 105 (m3ui toolkit + compositor clipboard broker) ✅
**Builds on:** The `term` emulator (`userspace/term/`) and its already-present but **unviewable** 1000-line scrollback ring (Phase 57 G.4), the compositor clipboard broker (`kernel_core::display::clipboard::ClipboardStore`, Phase 105 Track B.4), the `display_server` focus-aware key/pointer dispatch, the Phase 92b `usb-hid` Report-protocol pointer decode (the tree's only wheel producer), and the bracketed-paste framing already shipped in Phase 69 Track G.
**Primary Components:** `userspace/term` (`screen.rs`, `render.rs`, `input.rs`, `mouse.rs`, `main.rs`, `display.rs`), `kernel-core/src/display/protocol.rs`, `xtask/src/qmp.rs`

## Milestone Goal

The graphical terminal becomes pleasant to **live in**: the user can scroll back through
history that has already left the screen (Shift+PageUp/PageDown on every lane, mouse wheel
where a Report-protocol pointer is present), and can select terminal text with the mouse and
copy it to the system clipboard / paste it back — the two everyday interactions every real
terminal has and `term` currently lacks. Nothing new is invented at the kernel level; both
features are userspace changes in `term` plus the compositor clipboard protocol that already
exists.

## Why This Phase Exists

`term` already **stores** scrollback but cannot **show** it, and it has no notion of
selection or the clipboard at all:

- The `Screen` (`userspace/term/src/screen.rs`, `pub struct Screen`) keeps
  `scrollback: Vec<Vec<Cell>>`, capped at `SCROLLBACK_LINES = 1000` (`lib.rs:87`), filled by
  `scroll_region_up` on eviction. But the **only** accessor is `Screen::scrollback_len()` — a
  count. There is **no** `view_offset`/viewport field on `Screen`, and no getter reads a
  scrollback row back out. The ring is write-only; the user can never see it. (`Screen::cell`
  and `Screen::cell_primary` do exist and read the *live* grid — they are the ready-made
  accessors Track B's hit-test and copy-serialization should reuse.)
- **Mouse-wheel events do not currently reach `term` on the default lane** — and, contrary to
  a first reading of `mouse_server`, they are not merely dropped by `term`. Two producers
  exist and only one is live:
  - **PS/2 (default lane) — no wheel at all.** `mouse_server` computes
    `wheel_dy` from `packet.wheel` (`userspace/mouse_server/src/main.rs:212`), but
    `MouseDecoder` only populates that field in IntelliMouse 4-byte mode. The kernel never
    enables it: `init_mouse()` (`kernel/src/arch/x86_64/ps2.rs`) Step 4 deliberately stays in
    3-byte framing ("keeps cursor movement reliable and simply ignores any optional wheel byte
    as a resync"), and `try_intellimouse_handshake()` is `#[allow(dead_code)]` with **no call
    site**. `packet.wheel` is therefore structurally always `0`.
  - **USB HID Report protocol — real wheel.** The Phase 92b `usb-hid` driver decodes a wheel
    via `decode_pointer_report` and injects a `PointerEvent { button: None, wheel_dy }` into
    `mouse_server` over `MOUSE_EVENT_INJECT` (`userspace/drivers/usb-hid/src/main.rs:631`).
    This path requires a **Report-protocol** pointer: `classify_role` sends any Boot-subclass
    mouse (QEMU's `usb-mouse`) down `DeviceRole::BootMouse`, whose 3-byte decoder explicitly
    discards the trailing wheel byte (`kernel-core/src/usb/hid.rs:479`). QEMU's `usb-tablet`
    (subclass 0) is the device that yields wheel — the same one `usb-report-smoke` attaches.

  So `term`'s `MouseReporter::encode` (`userspace/term/src/mouse.rs:250`) returning `None` for
  `PointerButton::None` (`mouse.rs:257`) and never reading `wheel_dx`/`wheel_dy` (defined at
  `kernel-core/src/input/events.rs:205`) is a **second** blocker on top of the missing
  producer, not the only one. Track A fixes the `term` side and scopes the wheel to the
  Report-protocol lane; PS/2 IntelliMouse is explicitly deferred (see below).
- `InputHandler::translate` (`userspace/term/src/input.rs:43`) handles arrows only
  (`special_key_sequence`, `input.rs:100`) and never inspects `MOD_SHIFT` — `input.rs` imports
  only `MOD_CTRL`. There is no PageUp/PageDown/Home/End arm **at all**, so even *unshifted*
  paging is dead in `term` today: `less` and `htop` cannot page with the keyboard. The
  canonical VT sequence table already exists at `kernel-core/src/input/hid_poll.rs:147`;
  Track A fills `term`'s in from it while it is rewriting that function.
- There is **no** selection/highlight/anchor state anywhere in `term`; the future-work
  anchor is documented in `pull_one_event`'s doc comment (`main.rs`, "A future track that
  adds e.g. mouse-aware shell selection would thread `Pointer` into the input handler here").
  `term` does not depend on `desktop_client` (see its `Cargo.toml`) and has no clipboard code
  at all — the only clipboard client repo-wide is the Phase 105 `clip-smoke` gate
  (`userspace/clip-smoke/src/main.rs`).

Meanwhile the substrate for copy/paste is already built and validated: the compositor
brokers a clipboard through `ClientMessage::SetClipboard` / `RequestClipboard` and
`ServerMessage::ClipboardData` (`kernel-core/src/display/protocol.rs:521`/`:529`/`:601`),
backed by `ClipboardStore` (`kernel-core/src/display/clipboard.rs:21`) and round-tripped by
`clipboard-smoke`. `term` already holds the `"display"` IPC handle these verbs travel over.
This phase is the last user-facing mile on top of infrastructure that already exists.

## Learning Goals

- The difference between a terminal's **screen buffer** and its **scrollback**, and how a
  viewport (view offset) composites the two into one rendered frame without disturbing the
  live grid the shell writes to.
- "Snap-to-bottom" semantics — why a terminal must return to the live tail on new output or
  keystrokes, and how alternate-screen apps (vim, htop) opt out of scrollback.
- Why an input translator that only *writes bytes* cannot express "I consumed this key
  locally" — and how adding a return channel to `InputHandler::translate` is the minimal
  change that lets one function serve both PTY output and viewport control.
- Mouse text selection: anchor/extent tracking, cell-accurate hit-testing against a
  monospace grid, and rendering a highlight by inverting cell attributes.
- The compositor clipboard model: a broker that owns the current offer, ownership scoped to
  a client token, and the copy (offer) vs. paste (request) round trip over IPC.
- Why paste must be **bracketed** (Phase 69 `wrap_paste`) so a shell/editor can tell pasted
  bytes from typed bytes and refuse to execute them.
- How HID **Boot** vs. **Report** protocol changes which fields survive decode — the reason
  a wheel exists on `usb-tablet` but not on `usb-mouse`.

## Feature Scope

### Track A — Scrollback viewport

Give `Screen` a `view_offset` (rows scrolled up from the live tail, `0` = live) and a
read path into the scrollback ring, then composite. When the offset is non-zero, the
rendered frame shows the appropriate slice of `scrollback` above (and pushing down) the live
grid; when it is `0`, rendering is exactly as today. Bindings:

- **Shift+PageUp / Shift+PageDown** — page the viewport by (rows − 1); **Shift+Home /
  Shift+End** — jump to the oldest line / snap to the live tail. This is the **primary,
  always-available** binding on every lane, including PS/2-only boots.
- **Unshifted PageUp/PageDown/Home/End** — currently emit nothing; Track A fills in the
  standard VT sequences (`ESC[5~`, `ESC[6~`, `ESC[H`, `ESC[F`) from the existing
  `hid_poll.rs` table so `less`/`htop` can page. Adjacent fix, same function.
- **Mouse wheel** — route the currently-ignored `wheel_dy` into `view_offset` (up = older,
  down = newer), clamped to `[0, scrollback_len()]`. Live **only where a Report-protocol
  pointer is attached** (`usb-tablet` on an xHCI lane); inert on PS/2-only boots, where the
  key bindings above carry the feature.
- **Snap-to-bottom** — any shell output that scrolls the live region, and any key that
  produces PTY bytes, resets `view_offset = 0` so the user never types "into" history.
- Scrollback is primary-screen only (matching the existing eviction guard in
  `scroll_region_up`: `if primary && full_screen`): the alternate screen (vim/htop) has no
  scrollback and the wheel passes through to the app's own mouse reporting, unchanged.

### Track B — Mouse selection + clipboard copy/paste

Add selection state to `term` and wire copy/paste to the compositor clipboard:

- **Select** — pointer press anchors, drag extends, release commits a `(start, end)` cell
  range (linear and, with a modifier, block). Render the selection by inverting the covered
  cells' fg/bg during composition.
- **Copy** — on release (and on **Ctrl+Shift+C**), serialize the selected cells to UTF-8 and
  offer it via a new `term` clipboard call: `ClientMessage::SetClipboard { mime_tag:
  TextPlainUtf8, len, client_token }` over the `"display"` handle `term` already holds, capped
  at `CLIPBOARD_MAX_BYTES` (3900). Serialization rules are explicit because `Cell` is not a
  plain char: skip cells with `wide_continuation == true` (the trailing half of a
  double-width glyph, whose codepoint lives in the leading cell — copying both double-emits
  CJK text), trim trailing cells whose `codepoint == 0x20` per row, and join rows with `\n`.
- **Paste** — **Ctrl+Shift+V** (and middle-click, optional) sends `RequestClipboard`, reads
  the `ServerMessage::ClipboardData` reply bulk, and injects it through the existing
  `wrap_paste` (`input.rs:152`) so it arrives bracketed.

`term` does not link `desktop_client` today and will not start; the clipboard verbs are added
to `term`'s own `DisplayClient` (`userspace/term/src/display.rs`) rather than pulling in a
second client library, keeping `term`'s single-connection model intact. `CLIPBOARD_MAX_BYTES`
therefore moves from `desktop_client` to `kernel-core/src/display/protocol.rs` (next to the
`MAX_FRAME_BODY_LEN = 4096` guard its value is derived from) and is re-exported from
`desktop_client` so existing callers are unaffected.

## Important Components and How They Work

### `Screen` viewport and the render seam (Track A)

The renderer is **command-driven, not a row-iterating frame drawer**: `Screen::feed`
emits typed `RenderCommand`s that `Renderer::apply`/`compose` (`render.rs:223`/`:291`) drain
to the framebuffer. The two places that already re-emit an entire grid —
`Screen::switch_to_primary` and `Screen::resize` — iterate the live buffer `0..rows` only.
Track A adds a `view_offset` field to `Screen` and a `compose_view(out)` path that, for a
non-zero offset, emits `PutGlyph`s sourced from `scrollback[len − offset ..]` for the top rows
and the live `buf` for the remainder. Because the seam lives in `screen.rs` (where `PutGlyph`s
are generated), `render.rs` is untouched beyond a full-repaint trigger on offset change.

### The `InputHandler::translate` return channel (Track A)

`InputHandler` is a stateless unit struct and `translate<W: PtyWriter>(&mut self, event,
writer)` returns `()`. That signature cannot express either half of Track A's key work: it
cannot say "this was Shift+PageUp, I consumed it, emit no PTY bytes and scroll the viewport",
and it cannot tell `main.rs` that a key *did* produce bytes (the snap-to-bottom trigger).
`InputHandler` also holds no reference to `Screen`, and must not — it is host-tested in
isolation. Track A therefore changes `translate` to **return a typed outcome** (e.g.
`enum KeyOutcome { WroteBytes, Consumed, ViewScroll(ViewCmd), None }`) and lets `main.rs` —
which owns both the `Screen` and the PTY fd — apply it. `special_key_sequence` likewise gains
a modifiers parameter, and `input.rs` starts importing `MOD_SHIFT` alongside `MOD_CTRL`.

### Selection state and highlight (Track B)

A small `Selection { anchor: (u16,u16), extent: (u16,u16), mode: Linear|Block, active: bool }`
lives alongside `Screen`. Pointer events — today decoded in the `PulledEvent::Pointer` arm of
the main loop and handed straight to `MouseReporter::encode` — gain a pre-pass: when the app
has **not** enabled mouse reporting (DEC private modes tracked by `update_mouse_mode`), a
press/drag/release drives the selection instead of being reported to the PTY; when the app
**has** grabbed the mouse, selection is bypassed (Shift-drag can force-select, the standard
xterm override). Highlight is a compositional attribute applied in the `PutGlyph` emit path,
so no separate draw pass is needed.

### Clipboard verbs on `term`'s `DisplayClient` (Track B)

`term` connects only to the `"display"` service and receives keys/pointers through
`display_server`'s focus-aware dispatcher — it never talks to `kbd_server`/`mouse_server`
directly. The clipboard rides the **same** handle: `set_clipboard(text)` frames
`SetClipboard` + raw bytes and `ipc_call`s the display handle; `get_clipboard()` sends
`RequestClipboard` and decodes the `ClipboardData` reply bulk — mirroring
`desktop_client::{set_clipboard, get_clipboard}` (`userspace/lib/desktop_client/src/lib.rs:195`/`:224`)
but implemented inline so `term` keeps one connection and one client library.

### QMP input injection for the gate (Track C)

`xtask/src/qmp.rs` today exposes `press_key`, `press_chord`, `send_pointer_rel`,
`send_pointer_abs`, and `screendump` — it has **no button press/release and no wheel**. Both
Track C arms need what is missing: the scrollback arm needs wheel injection, and the selection
arm needs press → motion → release. Both are `input-send-event` `btn` events (`wheel-up` /
`wheel-down`, `left`), so Track C adds the wrappers before the smoke itself can be written.

## How This Builds on Earlier Phases

- **Extends Phase 57/69** by making the long-stored scrollback ring actually viewable, and
  fills in the unshifted page-key sequences `term` has been missing since Phase 57 G.5.
- **Consumes the Phase 92b `usb-hid` Report-protocol wheel** — the tree's only live wheel
  producer — rather than assuming the Phase 56 PS/2 path delivers one (it does not).
- **Reuses the Phase 105 clipboard broker** (`ClipboardStore` + the `SetClipboard`/
  `RequestClipboard`/`ClipboardData` protocol) unchanged — `term` becomes its second real
  client after `clip-smoke`.
- **Reuses the Phase 69 `wrap_paste`** bracketed-paste framing for the paste direction.
- **Changes nothing in the kernel.** Both tracks are userspace-only, honoring the
  userspace-first rule. This holds *because* the PS/2 IntelliMouse work is deferred — see
  "Deferred Until Later". The only shared-crate change is relocating the
  `CLIPBOARD_MAX_BYTES` constant into `protocol.rs`.

## Implementation Outline

1. **Track A:** add `view_offset` + a scrollback read accessor to `Screen`; add
   `compose_view`; change `InputHandler::translate` to return a typed outcome and
   `special_key_sequence` to take modifiers; add the unshifted VT page sequences and the
   Shift+PageUp/PageDown/Home/End viewport binds; route `wheel_dy` from the
   `PulledEvent::Pointer` arm into the offset; implement snap-to-bottom on output/keystroke;
   gate to the primary screen.
2. **Track B:** add `Selection` state + pointer pre-pass in `main.rs`; render highlight in the
   `PutGlyph` emit path; move `CLIPBOARD_MAX_BYTES` to `protocol.rs`; add
   `set_clipboard`/`get_clipboard` to `term`'s `DisplayClient`; bind Ctrl+Shift+C
   (copy-on-release) and Ctrl+Shift+V (paste via `wrap_paste`).
3. **Track C:** add QMP `btn`/wheel injection to `xtask/src/qmp.rs`, then the render probe +
   clipboard round-trip gate (below).

Every pure-logic piece above is host-testable — `term` ships a `[lib]` target precisely for
this and `cargo xtask check` already runs its host tests. Viewport clamping, `compose_view`
row sourcing, the key-outcome table, and selection serialization all get host tests **in the
same task that adds them**, not deferred to the QEMU gate.

## Acceptance Criteria

- **Track A:** a headless QMP/PPM probe (the `htop-render-probe` pattern) fills the screen
  past one page, scrolls up via injected Shift+PageUp, and asserts that **evicted** content
  (rows no longer in the live grid) is now visible; a subsequent keystroke/output snaps back
  to the live tail (the pre-scroll frame re-appears). On the Report-protocol lane
  (`-device qemu-xhci -device usb-tablet`, as in `usb-report-smoke`) the same assertion is
  driven by injected wheel-up. The alternate screen (an htop/less launch) shows the wheel
  passing through to the app, not the viewport.
- **Track B:** a clipboard round-trip through `term` — drive a selection over known text,
  Ctrl+Shift+C, and assert the compositor `ClipboardStore` now holds exactly those bytes
  (read back by a second client); then Ctrl+Shift+V and assert the bracketed bytes
  (`ESC[200~`…`ESC[201~`) arrive on the PTY. The highlight is visible on the QMP/PPM dump
  (inverted cells).
- Host tests cover viewport clamping, `compose_view` (including that `view_offset == 0`
  output is unchanged from today), the key-outcome table for shifted/unshifted page keys, and
  selection→UTF-8 serialization (wide-continuation skip, trailing-blank trim, `\n` join).
- A new gate `term-daily-driver-smoke` (`M3OS_TERM_POLISH_REGRESSION=1`) runs both arms; the
  default production build is unchanged.

## Companion Task List

- [Phase 112 Task List](./tasks/112-terminal-daily-driver-polish-tasks.md)

## How Real OS Implementations Differ

- **xterm/VTE/kitty/foot** keep scrollback in a ring or memory-mapped file far larger than
  1000 lines (often unbounded/reflowing on resize); m3OS keeps the fixed 1000-line ring and
  does not reflow scrollback across a resize.
- Mature terminals implement **rectangular block select, word/line double/triple-click,
  URL detection, and OSC 52 clipboard** (a terminal escape that lets the *remote* program set
  the clipboard); m3OS ships linear + optional block select and compositor-brokered copy only
  (OSC 52 is deferred).
- Selection interaction with mouse-reporting apps is a well-worn xterm convention (Shift
  overrides app grab); m3OS follows that convention rather than inventing a new one.
- Real clipboards carry **multiple MIME targets** (UTF-8, UTF-16, HTML, image); the m3OS
  broker offers `text/plain;utf-8` only (`MimeTag::TextPlainUtf8`).
- Real systems get a wheel from the **PS/2 IntelliMouse extension** as a matter of course;
  m3OS's PS/2 driver deliberately stays in 3-byte framing for cursor reliability, so the
  wheel arrives only over USB HID Report protocol.

## Deferred Until Later

- **PS/2 IntelliMouse (4-byte) wheel support.** `kernel/src/arch/x86_64/ps2.rs` has the
  handshake written but unreferenced (`try_intellimouse_handshake`, `#[allow(dead_code)]`),
  and `init_mouse()` Step 4 documents why it stays in 3-byte framing: some QEMU/front-end
  combinations ACK the IntelliMouse probe yet still surface basic PS/2 motion, so 4-byte
  framing risks cursor desync. Enabling it (with a device-ID `0x03` check and a fallback to
  3-byte framing) is the follow-up that would bring the wheel to PS/2-only lanes. Until then
  Shift+PageUp/PageDown is the supported scrollback control there.
- **Unscaled USB-tablet absolute coordinates (Phase 92b defect, found here).**
  `usb-hid`'s `poll_report_pointer` injects the decoded logical position straight into
  `PointerEvent::abs_position` (`abs_position = (x as i32, y as i32)`) with no mapping from the
  report's logical range onto the framebuffer. QEMU's `usb-tablet` declares a 0..0x7FFF logical
  range, so a pointer parked in the middle of that range lands at ~(16384, 16384) — far outside
  a 1920×1080 screen — and `hit_test` finds no surface, so the compositor drops *every* tablet
  pointer event. The Phase 112 gate works around it by injecting screen-pixel coordinates (QEMU
  passes `input-send-event` abs values into the tablet's logical range 1:1, so device units
  coincidentally equal pixels), which is why that lane is scoped to pixel coordinates and says
  so. A real fix needs `ReportField` to carry logical min/max — it carries neither today — plus
  a framebuffer-size query in the driver, so it belongs to the USB/HID subsystem, not here.
- **PS/2 IntelliMouse (4-byte) wheel support** — see above; the wheel is USB-only until then.
- Scrollback reflow on resize and a configurable/unbounded history size.
- OSC 52 clipboard (remote-program clipboard control) and primary-selection (middle-click)
  as a separate buffer from the clipboard.
- Word/line (double/triple-click) selection and URL/hyperlink detection (OSC 8).
- Search-in-scrollback (`/`-style incremental find).
- Clipboard MIME targets beyond UTF-8 text (images from `imgview`, rich text).
- Multi-frame clipboard transfer for offers larger than `CLIPBOARD_MAX_BYTES`.
