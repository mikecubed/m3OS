# Phase 112 - Terminal Daily-Driver Polish (Scrollback + Selection/Clipboard)

**Status:** ✅ Complete — four acceptance items were re-scoped rather than delivered (one B.3 host test, three C.2 gate assertions). They are left unticked in the task list and named in "Deferred Until Later" below, not quietly folded in.
**Source Ref:** phase-112
**Depends on:** Phase 22 (TTY and Terminal) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 69–69d (Terminal TUI Capabilities + ncurses) ✅, Phase 92b (USB HID Report Protocol) ✅, Phase 105 (m3ui toolkit + compositor clipboard broker) ✅
**Builds on:** The `term` emulator (`userspace/term/`) and its already-present but **unviewable** 1000-line scrollback ring (Phase 57 G.4), the compositor clipboard broker (`kernel_core::display::clipboard::ClipboardStore`, Phase 105 Track B.4), the `display_server` focus-aware key/pointer dispatch, the Phase 92b `usb-hid` Report-protocol pointer decode (the tree's only wheel producer), and the bracketed-paste framing already shipped in Phase 69 Track G.
**Primary Components:** `userspace/term` (`lib.rs`, `screen.rs`, `render.rs`, `input.rs`, `mouse.rs`, `main.rs`, `display.rs`), `kernel-core/src/display/protocol.rs`, `kernel-core/src/input/dispatch.rs`, `userspace/display_server` (`input.rs`, `main.rs`), `userspace/tui-smoke`, `xtask/terminfo/m3os-term.ti`, `xtask/src/qmp.rs`, `xtask/src/main.rs`

## Milestone Goal

The graphical terminal becomes pleasant to **live in**: the user can scroll back through
history that has already left the screen (Shift+PageUp/PageDown on every lane, mouse wheel
where a Report-protocol pointer is present), and can select terminal text with the mouse and
copy it to the system clipboard / paste it back — the two everyday interactions every real
terminal has and `term` currently lacks. Both features are userspace changes in `term` plus the
compositor clipboard protocol that already exists.

One ring-0 fix proved unavoidable, and it is the most user-visible bug the phase closes: a PTY
`write()` that could not fit in the 4 KiB ring returned `0` instead of `-EAGAIN`, which Rust's
`std` reports as `WriteZero` and `println!` turns into a panic — so **the shell inside `term`
died whenever a program printed faster than `term` drained the PTY**. Running `dmesg` killed the
terminal essentially every time. See "The PTY write-zero fix" below.

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
- Why a display server owes its clients **surface-local** coordinates rather than screen ones,
  and why the compositor must keep the screen-absolute value for itself (cursor blit,
  hit-test) — two coordinate spaces that look identical only for a window at the origin.
- Why the **compositor** is the only process that can put keyboard modifiers on a pointer
  event: the mouse driver never sees the keyboard, so a modifier field filled in by the
  producer is structurally always empty.

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
  scrollback, and once an application has enabled a tracking mode the wheel is reported **to
  the application** rather than driving the viewport. That pass-through had to be *built*, not
  merely preserved — see below.

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
- **Paste** — **Ctrl+Shift+V** sends `RequestClipboard`, reads
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

### The wheel's two destinations (Track A)

One injected notch has two possible destinations, and which one it reaches is decided by
`MouseReporter::classify` (`userspace/term/src/mouse.rs`):

- **No tracking mode enabled** — `classify` returns `PointerAction::ScrollView(wheel_dy *
  wheel_rows)` and `term` scrolls its own scrollback viewport. Positive `wheel_dy` (wheel-up)
  moves toward older history. Nothing is written to the PTY.
- **A tracking mode enabled** (`?9` / `?1000` / `?1002` / `?1003`) — `MouseReporter::encode`
  turns the notch into an xterm **wheel pseudo-button** and it goes to the application, so
  `less`/`htop`/`tmux` scroll their own panes instead of `term` scrolling behind them.

The pseudo-button convention is xterm's: the wheel rides the same button field as the real
buttons, distinguished by bit 6, so **wheel-up is button 64 and wheel-down 65** (66/67 are the
horizontal pair, which nothing in this tree produces — no source populates `wheel_dx`). A notch
has **no release edge**, so it is always encoded as a press: SGR reports terminate in upper-case
`M` (`ESC[<64;Px;PyM`) and there is no lower-case-`m` counterpart to look for, and the
press-only X10 mode (`?9h`) receives the notch for free rather than having it dropped by the
release guard. In the legacy 6-byte form the indices land on `Cb` bytes 96/97
(`ESC[M` + `` ` `` / `a`).

This is the correction to the phase's original framing, which said the wheel "passes through to
the app's own mouse reporting, unchanged." It did not. `encode` returned `None` for every
`PointerButton::None` event, so on the alternate screen a tracking application saw **nothing** —
the notch fell through `classify` to `Ignore` and was discarded. The button index widened from
`u8` to `u16` to make room for 64/65 above the real-button range, and both encoder clamps (the
legacy 2-bit button field's `min(2)`, SGR's `min(31)`) are bypassed for indices ≥ 64 so the
pseudo-buttons are not folded back onto a real button. Note the scope of the fix: an application
that has *not* enabled a tracking mode still receives no wheel byte, which is correct xterm
behavior — there the notch drives `term`'s own scrollback instead.

### Pointer coordinate space and live modifiers (cross-cutting)

The most architecturally significant change in the phase is not in `term` at all. Making mouse
selection work required fixing what `display_server` sends every client, and that is a
wire-visible behavior change for the whole graphical stack.

**Surface-local coordinates.** `ServerMessage::Pointer(ev).abs_position` used to be
**output-local** — the same screen-absolute value the compositor hit-tested with. A client had
no way to convert it, because the compositor never told a client where its own surface sat, so
every client that divided the coordinate by a cell or compared it against a widget rect laid out
from `(0, 0)` was silently wrong by its window's origin. For `term` that is at minimum the bar's
48 px exclusive zone plus the outer gap in *y*, plus the whole tile width in *x* for any window
not in the leftmost column.

The fix threads the origin out of the router rather than having each client guess:
`kernel_core::input::dispatch::PointerRouteDecision` gained
`deliver_origin: Option<(i32, i32)>` — the hit surface's geometry-rect top-left in output-local
coordinates, set in the same statement pair as `deliver_to` from the live geometry slice, so it
is `Some` exactly when `deliver_to` is and always reflects the surface's *current* position
rather than a hover-time cache. `display_server`'s outbound branch subtracts it
(`saturating_sub`) immediately before the `InputEffect::Outbound` push. Three consequences worth
naming:

- `term`'s cell derivation (`term::pointer_cell`, `abs_position / CELL_WIDTH`) is correct as
  written, and VT mouse reports and selection hit-testing become correct at the same moment.
- `m3ui`'s widget hit-test (`apply_pointer` → `InputState::set_pointer`, used by `settings` and
  `imgview`) compares against rects laid out from `(0, 0)` and is likewise fixed — a defect that
  had been latent since Phase 105 and was never on record.
- The compositor's *own* `pointer_position` is no longer read back out of a client-bound
  message. `main.rs` had a feedback loop that took the position from the outgoing
  `ServerMessage::Pointer`; with the message now surface-local that would drag the cursor toward
  the top-left by the surface origin on every event, so the loop is deleted.
  `InputEffect::CursorMoved` — which fires unconditionally, including over empty desktop — is
  the sole authority, and it stays **screen-absolute** along with the cursor blit and the next
  pass's hit-test. The two spaces are now distinct and must not be confused.

**Live keyboard modifiers.** Every pointer producer in the tree (`mouse_server`, `usb-hid`)
hardcodes `ModifierState::empty()`, because none of them read the keyboard. The compositor is
the only process that sees both streams, so `InputDispatcher` now snapshots the modifier state
at the top of `route_key_event` — before the `match` on event kind, so it updates on Down,
Repeat **and** Up, and whether the key was delivered, grabbed or dropped — and exposes it via
`modifiers()`. `display_server` stamps that over the producer's placeholder before routing.
The snapshot is masked to `MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_SUPER`, so the CAPS/NUM lock
latches that `ModifierTracker::state()` ORs in never reach a pointer event. Without this the
xterm Shift-drag override and Alt block-select in Track B are *unreachable* — the modifier field
on a pointer event was always empty. A companion `forget_modifiers()` exists for stream
interruptions (session lock, VT switch, `kbd_server` reconnect) but has no call site yet; see
"Deferred Until Later".

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

### The PTY write-zero fix (ring 0, found by the Track C gate)

The scrollback arm was the first thing in the tree to type `dmesg` into the graphical `term`, and
it failed every time — not because the viewport was wrong, but because **the shell had died**
(`[signal] [p21] killed by signal 6`, then `term: shell exited`, leaving bare compositor
background). The bug is in the PTY write path and predates this phase: it has been there since
the original Phase 29 PTY implementation (`1f9cc7d90`).

`PtyRingBuffer::write` (`kernel-core/src/pty.rs`) returns `0` when the ring (`PTY_BUF_SIZE =
4096`) is full. Both `sys_write` PTY arms (`kernel/src/arch/x86_64/syscall/mod.rs`,
`FdBackend::PtySlave` and `FdBackend::PtyMaster`) propagated that straight out as the syscall's
return value — so a full ring looked to userspace exactly like a successful zero-byte write.
Rust's `std` maps a `write()` of `Ok(0)` to `ErrorKind::WriteZero`, `println!`/`eprintln!` panic
on a failed write, and `ion` is built `panic = "abort"`. The chain is therefore:

    dmesg bursts the log -> s2m ring fills -> write() returns 0 -> WriteZero
      -> ion's next prompt panics -> abort() -> SIGABRT -> term's shell exits

The neighbouring `FdBackend::UnixSocket` arm already did this correctly, `poll()` already
advertised `POLLOUT` only when `!s2m.is_full()`, and the wake side already existed and was
already commented for exactly this purpose ("Reading from s2m frees space for slave writers" ->
`wake_slave`). Nothing had ever slept on it. The fix wraps each copy loop in a retry: a partial
write returns the count (POSIX-legal on a tty), a non-blocking fd gets `-EAGAIN`, a pending
signal gets `-EINTR`, and otherwise the writer parks on the wait queue until space frees, with
the master/slave-hangup check re-evaluated each pass so a dying `term` releases a blocked writer
with `-EIO` rather than hanging it.

The readiness predicates live in `kernel-core/src/pty.rs` (`slave_write_ready`,
`master_write_ready`) so they are host-testable. `slave_write_ready` takes the number of bytes
required rather than asking `!is_full()`: an OPOST+ONLCR newline expands to CR+LF and must land
atomically, so with exactly one free slot "not full" is true while the retry still cannot place
the byte — a tight kernel busy-spin. `master_write_ready` re-reads the line-discipline mode at
wake time (canonical checks `edit_buf`, raw checks `m2s`) so a concurrent `tcsetattr` cannot
leave a sleeper waiting on the wrong buffer.

This is why "the terminal becomes pleasant to live in" needed a kernel change: no amount of
scrollback polish matters if the shell dies when a command prints quickly.

## How This Builds on Earlier Phases

- **Extends Phase 57/69** by making the long-stored scrollback ring actually viewable, and
  fills in the unshifted page-key sequences `term` has been missing since Phase 57 G.5.
- **Consumes the Phase 92b `usb-hid` Report-protocol wheel** — the tree's only live wheel
  producer — rather than assuming the Phase 56 PS/2 path delivers one (it does not).
- **Reuses the Phase 105 clipboard broker** (`ClipboardStore` + the `SetClipboard`/
  `RequestClipboard`/`ClipboardData` protocol) unchanged — `term` becomes its second real
  client after `clip-smoke`.
- **Reuses the Phase 69 `wrap_paste`** bracketed-paste framing for the paste direction.
- **Touches ring 0 in exactly one place**, and only to fix a POSIX violation the phase's own gate
  exposed: the PTY `sys_write` arms (see "The PTY write-zero fix"). Every *feature* track is ring-3,
  honoring the userspace-first rule. No new policy moved into the kernel.
  The PS/2 IntelliMouse work stays deferred — see "Deferred Until Later".
  Two shared-crate (`kernel-core`) changes ride along, both pure logic and both host-tested:
  relocating `CLIPBOARD_MAX_BYTES` into `display/protocol.rs` beside the
  `MAX_FRAME_BODY_LEN` guard its value derives from, and the `input/dispatch.rs` work described
  under "Pointer coordinate space and live modifiers" (`PointerRouteDecision::deliver_origin`
  and `InputDispatcher::modifiers()`). Neither is kernel code — `kernel-core` is the shared
  host-testable logic crate.

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
   *Discovered while wiring the pre-pass:* the modifier field on a delivered pointer event is
   always empty and the coordinates are output-local, so neither the Shift override nor the
   cell hit-test can work as specified. Add `PointerRouteDecision::deliver_origin` and
   `InputDispatcher::modifiers()` to `kernel_core::input::dispatch`, consume both in
   `display_server`'s outbound path, and delete the compositor's
   `pointer_position`-from-outbound-message feedback loop.
   *Also discovered:* `MouseReporter::encode` never reported the wheel to a tracking
   application, so the alternate-screen pass-through the scope assumed did not exist. Widen the
   button index and add the xterm 64/65 pseudo-buttons.
3. **Track C:** add QMP `btn`/wheel injection plus `send_key_state` / `drag_abs_with_mods`
   (a modifier held *across* a drag, which `press_key`'s synthesised down+up cannot express) to
   `xtask/src/qmp.rs`; add the in-guest `tui-smoke mouse-live` probe that gives the
   alternate-screen arm a deterministic serial oracle; then the six-arm gate.
   *Discovered while bringing arm 6 up:* a modifier-held drag fired edge-to-edge does **not**
   reach the guest as a held modifier, and QEMU is not at fault — it delivers every edge as
   asked. The keyboard and the pointer are two independent interrupt-IN endpoints drained by
   one `usb-hid` poll loop, one report per device per tick; `usb-tablet` hands out its queued
   motion/button reports one poll at a time while `usb-kbd` drains its whole scancode queue in
   a couple of polls, so the keyboard timeline runs *ahead* of the pointer timeline and the
   guest reads the Shift **break** — sent last — between the first `MoveAbs` and the button
   press. The compositor then stamps the whole gesture with no modifiers and the arm reads a
   false negative (both drags change zero scanlines, exactly as if the capability were
   missing). Fix: `qmp::GESTURE_STEP_PACING`, a per-edge gap that must exceed the guest
   poller's worst-case sleep (`hid_poll::HID_POLL_MAX_IDLE_NS`, 100 ms); applied uniformly so
   the arm's unshifted and Shift-held drags differ *only* in the modifier.
4. **Terminfo:** add the input-side capabilities for the navigation cluster `term` now emits
   (`kdch1`, `khome`, `kend`, `kpp`, `knp`) to `xtask/terminfo/m3os-term.ti`. `kich1` stays
   absent — `term` sends nothing for Insert, and advertising a sequence the terminal never
   sends is the failure mode that file's own comment block warns about. The entry is compiled
   by `tic` at image-build time, so `cargo xtask clean` is required for it to reach the disk.

Every pure-logic piece above is host-testable — `term` ships a `[lib]` target precisely for
this and `cargo xtask check` already runs its host tests. Viewport clamping, `compose_view`
row sourcing, the key-outcome table, and selection serialization all get host tests **in the
same task that adds them**, not deferred to the QEMU gate.

## Acceptance Criteria

These are the criteria **as met**. Where the original wording promised something the delivered
gate does not assert, the honest version is stated here and the shortfall is carried into
"Deferred Until Later"; the per-item record with reasons is in the companion task list.

- **Track A — scrollback:** a headless QMP/PPM probe (the `htop-render-probe` pattern) fills
  the screen past one page with `dmesg`, scrolls up via injected Shift+PageUp, and asserts that
  **evicted** content (rows no longer in the live grid) is now visible (≥20 changed scanlines
  against the settled live-tail frame); a subsequent printable keystroke both moves the frame
  *and* lands within 96 scanlines of the live-tail frame — "it moved" is not "it went home".
  On the Report-protocol lane (`-device qemu-xhci -device usb-tablet`, as in
  `usb-report-smoke`, with the Boot-subclass `usb-mouse` removed so exactly one pointer is on
  the bus) the same assertion is driven by injected wheel-up.
- **Track A — the wheel's other destination:** with an application holding the mouse on the
  alternate screen, the same injected notch reaches the *application* instead. The
  purpose-built `tui-smoke mouse-live` probe enables `?1000h` + `?1006h`, decodes `term`'s SGR
  reports, and gives two independent oracles: `TUI_SMOKE:mouse-live:cb=64` / `cb=65` on serial
  (the application decoded it) and a per-notch hue-and-region check on the PPM (≥20 000
  saturated-red pixels in the top half for wheel-up, blue in the bottom half for wheel-down,
  each dominating the opposite half, with the previous band gone). Its exit tally must read
  exactly `up=1` / `down=1`.
- **Track B — clipboard round trip:** drive a selection over known text with a QMP drag; the
  highlight is visible on the QMP/PPM dump (≥20 changed scanlines, an fg/bg swap across the
  band), and copy-on-release lands the text in the compositor `ClipboardStore`, read back by an
  **independent** client (`clip-smoke --paste`) and asserted as a whole trimmed line equal to
  the marker. Ctrl+Shift+V then delivers the offer to the PTY: the frame changes by ≥20
  scanlines *and* `term` is still on screen afterwards (black-pixel ratio ≥0.15).
  **Not asserted on this lane:** the `ESC[200~`…`ESC[201~` framing itself, because no program in
  the image enables `?2004h` and PTY bytes have no sink but the framebuffer — it is covered by
  the `term::input::wrap_paste` host tests. Copy is driven by release, not by Ctrl+Shift+C.
- **Track B — the xterm Shift override:** with a mouse-tracking application holding the pointer,
  an unshifted drag must leave the frame quiet (≤96 scanlines — the application owns it) while
  the identical Shift-held drag must paint a selection **and** replace the compositor's standing
  clipboard offer. This is the only end-to-end proof of the compositor's live-modifier stamping.
- Host tests cover viewport clamping and `scrollback_row` bounds, `compose_view` (including
  that `view_offset == 0` output is byte-identical to the existing full re-emit, and that a
  non-zero offset is inert on the alternate screen), the key-outcome table for shifted and
  unshifted page keys plus a **range sweep** proving `term`'s unshifted sequences match
  `hid_poll` across the whole `0xE000..=0xE0FF` keysym block in both directions,
  selection→UTF-8 serialization (wide-continuation skip, trailing-blank trim, `\n` join),
  selection invalidation across all eleven grid mutators, the pixel→cell hit-test at the grid
  edges (`term::pointer_cell`), the clipboard cap predicate, the wheel pseudo-button encodings,
  and — in `kernel-core` — that `deliver_origin` is `Some` exactly when `deliver_to` is, tracks
  a moved surface's current geometry, that `modifiers()` is masked to
  SHIFT/CTRL/ALT/SUPER and updates on Down, Repeat and Up, and that a whole drag's worth of
  pointer events routed after a Shift key-down leaves that snapshot intact (the compositor
  stamps *from* it, so a `route_pointer_event` that cleared it would silently downgrade every
  Shift-drag). `xtask`'s own tests pin the gesture wire shape and step order, and assert
  `GESTURE_STEP_PACING` outlasts the guest HID poller's idle backoff. `cargo test -p term`
  reports 186 passing.
- A new gate `term-daily-driver-smoke` (`M3OS_TERM_POLISH_REGRESSION=1`) runs all six arms; the
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
- **Selection is cleared by a grid mutation, never re-anchored.** `Selection` stores *display*
  coordinates, re-resolved through `display_cell()` at both paint and copy time, so it cannot
  survive a mutation of the grid it points at — a stale selection would highlight unrelated
  text and a later copy would put the wrong string on the clipboard. `Screen` therefore clears
  it in all eleven mutators (`put_char`, both `scroll_region_*`, `insert_chars`,
  `delete_chars`, `blank_cell`, `clear_buffer`, `resize`, both screen switches, and a
  view-offset change that actually moves). A real terminal instead **re-anchors** the selection
  to the underlying content — anchoring to `(scrollback_generation, absolute_row, col)` rather
  than to a display row — so a selection survives shell output scrolling underneath it. That is
  the follow-up; today the practical consequence is that a selection is lost the moment the
  shell prints anything, including a prompt repaint.
- **No implicit pointer grab in the dispatcher.** There is no pointer grab anywhere in the tree
  (`CompositorState::grab_state` is the keyboard `bind_table::GrabState`; a repo-wide grep for
  `pointer_grab` / `grab_surface` / `PointerGrab` is empty), so a drag that leaves the source
  surface is routed by hit-test on every event like any other motion. `term` compensates
  client-side with a `selecting` latch that keeps consuming pointer events until a button-up,
  and widens the release arm from `Up(0)` to `Up(_)` precisely because a compositor
  outbound-queue overflow can drop the matching edge and strand the latch forever. The correct
  fix is a real implicit grab in the compositor — press captures the pointer to the hit surface
  until the last button releases — which also removes the client-side latch and the
  lost-Up-edge hazard it works around. Until then a lost Up edge is recovered only by the next
  press, not by the compositor.
- **`InputDispatcher::forget_modifiers()` has no call site.** The keyboard-reconnect path lives
  in `KbdInputSource::try_lazy_reconnect`, a `&mut self` method on the source with no access to
  the dispatcher (which lives on `InputWiring`), so wiring a reconnect signal out to it was not
  attempted. Residual hazard, narrow: if `kbd_server` dies while Shift is physically held,
  pointer events carry a stale `MOD_SHIFT` until the next key event of *any* kind. The same
  call belongs on the session-lock and VT-switch paths.
- **Toplevel pointer coordinates are tile-relative, not buffer-relative.** A Toplevel's
  geometry rect is its *tile*, but `surface_screen_rect` (`compose.rs`) letterbox-centres a
  client's pixels inside that tile when the buffer is smaller. `deliver_origin` reports the
  tile origin, so between a `SurfaceResized` and the client's realloc the delivered coordinates
  are off by the centring delta; in steady state the client has resized to the tile and the
  delta is zero. Layer surfaces (bar, wallpaper, lockscreen, launcher, notifyd) carry their
  paint rect as their geometry and are always exact. Closing this needs the compositor to
  deliver the *painted* rect origin rather than the geometry rect, which means resolving the
  letterbox at route time.
- **The `term-daily-driver-smoke` wheel arms cannot be skipped.** The gate builds its own
  device set and unconditionally appends `-device usb-tablet,bus=xhci0.0`, so there is no
  PS/2-only mode and no skip branch with a printed reason. A lane-detecting variant would be
  the follow-up if the gate ever needs to run where a Report-protocol pointer is unavailable.
- **Ctrl+Shift+C is not driven by the gate**, and the clipboard read-back is a whole-line match
  on a known marker inside a multi-row payload rather than a byte-exact whole-payload compare
  (the deliberately tall drag captures the rows above and below the marker, so the payload is
  not known exactly). Copy is exercised through copy-on-release only; the Ctrl+Shift+C key path
  is covered by the `term::input` host tests.
- **The selection latch has no host test.** `handle_selection_pointer`, `SelectionOutcome`, the
  `selecting` latch, `write_all_bounded`, and the bell-on-rejected-copy all live in `main.rs`,
  which is `#[cfg(not(test))]` throughout inside a bin target with
  `required-features = ["os-binary"]` — nothing in it is compiled by `cargo test -p term`.
  Pinning it means lifting a `SelectionLatch` type (the `bool` plus the `PointerButton` →
  `SelectionOutcome` mapping) into `lib.rs`, leaving the `Screen` calls to the caller. Today it
  is covered only by the QEMU gate. The same applies to `display_server`, which is a
  `no_std`/`no_main` binary with no host test harness at all — its two behavior changes are
  covered by the `kernel_core::input::dispatch` tests plus the gate.
- **An oversized *selection* is never driven through the rejecting predicate in one test.**
  `oversized_selection_exceeds_the_clipboard_cap` asserts only that its 200×40 fixture exceeds
  `CLIPBOARD_MAX_BYTES`; `term::clipboard_payload_fits` is tested at the cap boundary
  separately. Composing them is a one-line addition to that test.
- **The bracketed-paste framing is not asserted anywhere but in host tests.** No program in the
  image sets `?2004h`, so on every lane `wrap_paste` correctly passes the payload through
  unframed and there is no framing on the wire to observe; and pasted bytes land on the PTY,
  whose only sink is the framebuffer, which a PPM cannot be OCR'd from. Proving the framed form
  in-guest needs an application (or a `tui-smoke` subcommand) that enables `?2004h` and echoes
  what it reads back to serial.
- Scrollback reflow on resize and a configurable/unbounded history size.
- Middle-click paste. The B.3 scope listed it as optional and it was not implemented — paste is
  Ctrl+Shift+V only; nothing in `term` binds a non-zero pointer button.
- OSC 52 clipboard (remote-program clipboard control) and primary-selection (middle-click)
  as a separate buffer from the clipboard.
- Word/line (double/triple-click) selection and URL/hyperlink detection (OSC 8).
- Search-in-scrollback (`/`-style incremental find).
- Clipboard MIME targets beyond UTF-8 text (images from `imgview`, rich text).
- Multi-frame clipboard transfer for offers larger than `CLIPBOARD_MAX_BYTES`.
