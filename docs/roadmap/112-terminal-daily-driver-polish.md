# Phase 112 - Terminal Daily-Driver Polish (Scrollback + Selection/Clipboard)

**Status:** Planned
**Source Ref:** phase-112
**Depends on:** Phase 22 (TTY and Terminal) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 69–69d (Terminal TUI Capabilities + ncurses) ✅, Phase 105 (m3ui toolkit + compositor clipboard broker) ✅
**Builds on:** The `term` emulator (`userspace/term/`) and its already-present but **unviewable** 1000-line scrollback ring (Phase 57 G.4), the compositor clipboard broker (`kernel_core::display::clipboard::ClipboardStore`, Phase 105 Track B.4), the `display_server` focus-aware key/pointer dispatch, and the bracketed-paste framing already shipped in Phase 69 Track G.
**Primary Components:** `userspace/term` (`screen.rs`, `render.rs`, `input.rs`, `mouse.rs`, `main.rs`, `display.rs`), `kernel-core/src/display/protocol.rs`, `userspace/display_server`

## Milestone Goal

The graphical terminal becomes pleasant to **live in**: the user can scroll back through
history that has already left the screen (mouse wheel and Shift+PageUp/PageDown), and can
select terminal text with the mouse and copy it to the system clipboard / paste it back —
the two everyday interactions every real terminal has and `term` currently lacks. Nothing
new is invented at the kernel level; both features are userspace changes in `term` plus the
compositor clipboard protocol that already exists.

## Why This Phase Exists

`term` already **stores** scrollback but cannot **show** it, and it has no notion of
selection or the clipboard at all:

- The `Screen` (`userspace/term/src/screen.rs:331`) keeps `scrollback: Vec<Vec<Cell>>`
  (`screen.rs:345`), capped at `SCROLLBACK_LINES = 1000` (`lib.rs:87`), filled by
  `scroll_region_up` on eviction (`screen.rs:1013`). But the **only** accessor is
  `scrollback_len()` (`screen.rs:590`) — a count. There is **no** `view_offset`/viewport
  field on `Screen`, and no getter reads a scrollback row back out. The ring is write-only;
  the user can never see it.
- Mouse-wheel events already reach `term` but are **dropped**: `mouse_server` emits a wheel
  scroll as a `PointerEvent` with `button: PointerButton::None` and `wheel_dy = ±1`
  (`userspace/mouse_server/src/main.rs:212`), but `MouseReporter::encode`
  (`userspace/term/src/mouse.rs:250`) returns `None` for `PointerButton::None`
  (`mouse.rs:257`) and never reads `wheel_dx`/`wheel_dy` (defined at
  `kernel-core/src/input/events.rs:205`), so the scroll dies at `main.rs:397`.
- `InputHandler::translate` (`userspace/term/src/input.rs:43`) handles arrows only
  (`special_key_sequence`, `input.rs:100`) and never inspects `MOD_SHIFT`; there is no
  PageUp/PageDown/Home/End arm.
- There is **no** selection/highlight/anchor state anywhere in `term`; the future-work
  anchor is documented at `main.rs:786`. `term` does not depend on `desktop_client` and has
  no clipboard code at all — the only clipboard clients repo-wide are the Phase 105
  `clip-smoke` gate (`userspace/clip-smoke/src/main.rs`).

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
- Mouse text selection: anchor/extent tracking, cell-accurate hit-testing against a
  proportional/monospace grid, and rendering a highlight by inverting cell attributes.
- The compositor clipboard model: a broker that owns the current offer, ownership scoped to
  a client token, and the copy (offer) vs. paste (request) round trip over IPC.
- Why paste must be **bracketed** (Phase 69 `wrap_paste`) so a shell/editor can tell pasted
  bytes from typed bytes and refuse to execute them.

## Feature Scope

### Track A — Scrollback viewport

Give `Screen` a `view_offset` (rows scrolled up from the live tail, `0` = live) and a
read path into the scrollback ring, then composite. When the offset is non-zero, the
rendered frame shows the appropriate slice of `scrollback` above (and pushing down) the live
grid; when it is `0`, rendering is exactly as today. Bindings:

- **Mouse wheel** — route the currently-dropped `wheel_dy` into `view_offset` (up = older,
  down = newer), clamped to `[0, scrollback_len()]`.
- **Shift+PageUp / Shift+PageDown** — page the viewport by (rows − 1); **Shift+Home /
  Shift+End** — jump to the oldest line / snap to the live tail.
- **Snap-to-bottom** — any shell output that scrolls the live region, and any key that
  produces PTY bytes, resets `view_offset = 0` so the user never types "into" history.
- Scrollback is primary-screen only (matching the existing eviction guard at
  `screen.rs:1024`): the alternate screen (vim/htop) has no scrollback and the wheel passes
  through to the app's own mouse reporting, unchanged.

### Track B — Mouse selection + clipboard copy/paste

Add selection state to `term` and wire copy/paste to the compositor clipboard:

- **Select** — pointer press anchors, drag extends, release commits a `(start, end)` cell
  range (linear and, with a modifier, block). Render the selection by inverting the covered
  cells' fg/bg during composition.
- **Copy** — on release (and on **Ctrl+Shift+C**), serialize the selected cells to UTF-8
  (trailing-blank-trimmed per row, `\n` between rows) and offer it via a new `term`
  clipboard call: `ClientMessage::SetClipboard { mime_tag: TextPlainUtf8, len, client_token }`
  over the `"display"` handle `term` already holds, capped at `CLIPBOARD_MAX_BYTES = 3900`.
- **Paste** — **Ctrl+Shift+V** (and middle-click, optional) sends `RequestClipboard`, reads
  the `ServerMessage::ClipboardData` reply bulk, and injects it through the existing
  `wrap_paste` (`input.rs:152`) so it arrives bracketed.

`term` links neither `desktop_client` today; the clipboard verbs are added to `term`'s own
`DisplayClient` (`userspace/term/src/display.rs:248`) rather than pulling in a second client
library, keeping `term`'s single-connection model intact.

## Important Components and How They Work

### `Screen` viewport and the render seam (Track A)

The renderer is **command-driven, not a row-iterating frame drawer**: `Screen::feed`
(`screen.rs:635`) emits typed `RenderCommand`s that `Renderer::apply`/`compose`
(`render.rs:223`/`:291`) drain to the framebuffer. The two places that already re-emit an
entire grid — `switch_to_primary` (`screen.rs:488`) and `resize` (`screen.rs:550`) — iterate
the live buffer `0..rows` only. Track A adds a `view_offset` field to `Screen` and a
`compose_view(out)` path that, for a non-zero offset, emits `PutGlyph`s sourced from
`scrollback[len − offset ..]` for the top rows and the live `buf` for the remainder. Because
the seam lives in `screen.rs` (where `PutGlyph`s are generated), `render.rs` is untouched
beyond a full-repaint trigger on offset change.

### Selection state and highlight (Track B)

A small `Selection { anchor: (u16,u16), extent: (u16,u16), mode: Linear|Block, active: bool }`
lives alongside `Screen`. Pointer events — today decoded at `main.rs:395` and handed to
`MouseReporter::encode` — gain a pre-pass: when the app has **not** enabled mouse reporting
(DEC private modes tracked by `update_mouse_mode`, `main.rs:859`), a press/drag/release drives
the selection instead of being reported to the PTY; when the app **has** grabbed the mouse,
selection is bypassed (Shift-drag can force-select, the standard xterm override). Highlight is
a compositional attribute applied in the `PutGlyph` emit path, so no separate draw pass is
needed.

### Clipboard verbs on `term`'s `DisplayClient` (Track B)

`term` connects only to the `"display"` service (`display.rs:283`) and receives keys/pointers
through `display_server`'s focus-aware dispatcher — it never talks to `kbd_server`/`mouse_server`
directly. The clipboard rides the **same** handle: `set_clipboard(text)` frames
`SetClipboard` + raw bytes and `ipc_call`s the display handle; `get_clipboard()` sends
`RequestClipboard` and decodes the `ClipboardData` reply bulk — mirroring
`desktop_client::{set_clipboard, get_clipboard}` (`userspace/lib/desktop_client/src/lib.rs:195`/`:224`)
but implemented inline so `term` keeps one connection and one client library.

## How This Builds on Earlier Phases

- **Extends Phase 57/69** by making the long-stored scrollback ring actually viewable and by
  consuming the wheel events the Phase 56 input path already delivers.
- **Reuses the Phase 105 clipboard broker** (`ClipboardStore` + the `SetClipboard`/
  `RequestClipboard`/`ClipboardData` protocol) unchanged — `term` becomes its second real
  client after `clip-smoke`.
- **Reuses the Phase 69 `wrap_paste`** bracketed-paste framing for the paste direction.
- **Changes nothing in the kernel** — both tracks are userspace-only, honoring the
  userspace-first rule.

## Implementation Outline

1. **Track A:** add `view_offset` + a scrollback read accessor to `Screen`; add
   `compose_view`; route `wheel_dy` (`main.rs:395`) and new Shift+PageUp/PageDown/Home/End
   keysyms (`input.rs:100`, reading `MOD_SHIFT`) into the offset; implement snap-to-bottom on
   output/keystroke; gate to the primary screen.
2. **Track B:** add `Selection` state + pointer pre-pass in `main.rs`; render highlight in the
   `PutGlyph` emit path; add `set_clipboard`/`get_clipboard` to `term`'s `DisplayClient`; bind
   Ctrl+Shift+C (copy-on-release) and Ctrl+Shift+V (paste via `wrap_paste`).
3. Add the QMP/PPM render probe + clipboard round-trip gates (below).

## Acceptance Criteria

- **Track A:** a headless QMP/PPM probe (the `htop-render-probe` pattern) fills the screen
  past one page, scrolls up via injected wheel/Shift+PageUp, and asserts that **evicted**
  content (rows no longer in the live grid) is now visible; a subsequent keystroke/output
  snaps back to the live tail (the pre-scroll frame re-appears). The alternate screen (an
  htop/less launch) shows the wheel passing through to the app, not the viewport.
- **Track B:** a clipboard round-trip through `term` — drive a selection over known text,
  Ctrl+Shift+C, and assert the compositor `ClipboardStore` now holds exactly those bytes
  (read back by a second client, extending `clipboard-smoke`); then Ctrl+Shift+V and assert
  the bracketed bytes (`ESC[200~`…`ESC[201~`) arrive on the PTY. The highlight is visible on
  the QMP/PPM dump (inverted cells).
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

## Deferred Until Later

- Scrollback reflow on resize and a configurable/unbounded history size.
- OSC 52 clipboard (remote-program clipboard control) and primary-selection (middle-click)
  as a separate buffer from the clipboard.
- Word/line (double/triple-click) selection and URL/hyperlink detection (OSC 8).
- Search-in-scrollback (`/`-style incremental find).
- Clipboard MIME targets beyond UTF-8 text (images from `imgview`, rich text).
