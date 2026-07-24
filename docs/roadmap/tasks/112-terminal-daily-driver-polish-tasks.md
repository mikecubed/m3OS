# Phase 112 — Terminal Daily-Driver Polish (Scrollback + Selection/Clipboard): Task List

**Status:** Planned
**Source Ref:** phase-112
**Depends on:** Phase 22 (TTY and Terminal) ✅, Phase 56 (Display and Input) ✅, Phase 69–69d (Terminal TUI + ncurses) ✅, Phase 92b (USB HID Report Protocol) ✅, Phase 105 (compositor clipboard broker) ✅
**Goal:** Make `term`'s already-stored 1000-line scrollback viewable (Shift+PageUp/Down/Home/End everywhere, wheel on Report-protocol pointer lanes, with snap-to-bottom), fill in the missing unshifted page keys, and add mouse text selection + compositor-brokered copy/paste — all userspace-only, reusing the Phase 105 clipboard protocol and the Phase 69 bracketed-paste framing.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Scrollback viewport (`view_offset`, key + wheel bindings, snap-to-bottom) | — | ✅ Landed |
| B | Mouse selection + clipboard copy/paste in `term` | A (shared render/pointer seam) | Planned |
| C | QMP input plumbing + render-probe / clipboard round-trip gate | A, B | Planned |

Tracks A and B share the pointer-intake seam (the `PulledEvent::Pointer` arm of `main.rs`'s
event loop) and the `PutGlyph` emit path; land A first (it establishes the render seam the
highlight reuses). Both are userspace-only — no kernel change.

**Wheel scoping (read before starting A.3).** The wheel is *not* already arriving and merely
dropped. On the default PS/2 lane it is never produced: `MouseDecoder` fills `packet.wheel`
only in IntelliMouse 4-byte mode, and `kernel/src/arch/x86_64/ps2.rs` `init_mouse()` Step 4
deliberately stays in 3-byte framing with `try_intellimouse_handshake()` left
`#[allow(dead_code)]` and uncalled. The live producer is the Phase 92b `usb-hid` driver
(`userspace/drivers/usb-hid/src/main.rs:631`), and only for a **Report-protocol** pointer —
`classify_role` routes Boot-subclass devices (QEMU `usb-mouse`) to `DeviceRole::BootMouse`,
whose decoder discards the wheel byte (`kernel-core/src/usb/hid.rs:479`). QEMU's `usb-tablet`
is the device that yields wheel. PS/2 IntelliMouse is deferred (see the phase doc).

**Host tests are in scope per-task.** `term` ships a `[lib]` target for host testing and
`cargo xtask check` already runs `term`'s host tests. Every pure-logic acceptance item below
lands with its host test in the same task — not deferred to Track C.

**Line numbers drift.** Cite exact symbols; the line numbers below are as-of this writing and
should be treated as hints only.

---

## Track A — Scrollback viewport

### A.1 — `view_offset` field + scrollback read accessor

**File:** `userspace/term/src/screen.rs`
**Symbols:** `Screen`, `Screen::scrollback`, `Screen::scrollback_len`, `Screen::scroll_region_up`
**Why it matters:** The scrollback ring is currently write-only — only `scrollback_len()` reads it back — so the user can never see history. This adds the viewport state and the row read path.

**Acceptance:**
- [x] `Screen` gains `view_offset: usize` (`0` = live tail) and clamps it to `[0, scrollback_len()]`.
- [x] A `scrollback_row(i) -> Option<&[Cell]>` (or an iterator) exposes evicted rows for compositing; no change to eviction (`scroll_region_up`) or the cap (`SCROLLBACK_LINES = 1000`, `lib.rs:87`).
- [x] `view_offset` is forced to `0` whenever the primary region scrolls (new output) — the snap-to-bottom-on-output half.
- [x] **Host tests:** clamping at both ends (offset > `scrollback_len()` saturates; offset never goes negative), `scrollback_row` bounds, and that a full-screen primary scroll resets a non-zero offset to `0`.

### A.2 — Composite the viewport into the rendered frame

**Files:** `userspace/term/src/screen.rs` (emit path), `userspace/term/src/render.rs` (repaint trigger)
**Symbols:** `Screen::switch_to_primary`, `Screen::resize`, `Renderer::apply`/`compose` (`render.rs:223`/`:291`)
**Why it matters:** The renderer is command-driven and only ever paints the live grid; a view must be composited where `PutGlyph`s are generated (in `screen.rs`), not in `render.rs`.

**Acceptance:**
- [x] A `compose_view(out)` emits, for `view_offset > 0`, the top rows from `scrollback[len - view_offset ..]` and the remaining rows from the live `buf`; for `view_offset == 0` output is byte-identical to today.
- [x] Changing `view_offset` triggers a full repaint (the `switch_to_primary`-style re-emit) so no stale live-grid cells remain.
- [x] Scrollback view is **primary-screen only** (matching the eviction guard in `scroll_region_up`, `if primary && full_screen`); on the alternate screen `view_offset` is inert.
- [x] **Host tests:** `compose_view` at `view_offset == 0` produces the same `RenderCommand` sequence as the existing full re-emit; at `offset == k` the top `k` rows carry known scrollback codepoints and the rest carry live-grid ones; on the alternate screen a non-zero `view_offset` changes nothing.

### A.3 — Key bindings (unshifted VT page keys + Shift viewport binds) and the `translate` return channel

**Files:** `userspace/term/src/input.rs`, `userspace/term/src/main.rs`
**Symbols:** `InputHandler::translate` (`input.rs:43`), `special_key_sequence` (`input.rs:100`), `MOD_SHIFT`/`MOD_CTRL` (`kernel-core/src/input/events.rs:28`/`:30`), the VT table at `kernel-core/src/input/hid_poll.rs:147`
**Why it matters:** `special_key_sequence` covers only the four arrows and takes no modifiers, and `input.rs` imports only `MOD_CTRL`. So Shift+PageUp is unhandled *and* plain PageUp/Home/End emit nothing at all — paging in `less`/`htop` is broken today. Separately, `translate` returns `()`, so it can express neither "consumed locally, no PTY bytes" nor "bytes were written" (the snap-to-bottom trigger), and `InputHandler` holds no `Screen` reference (nor should it — it is host-tested standalone).

**Acceptance:**
- [x] `InputHandler::translate` returns a typed outcome (e.g. `enum KeyOutcome { WroteBytes, Consumed, ViewScroll(ViewCmd), None }`); `main.rs` — which owns both the `Screen` and the PTY fd — applies it. `InputHandler` gains no `Screen` reference and stays host-testable in isolation.
- [x] `special_key_sequence` takes the modifier state; `input.rs` imports `MOD_SHIFT`.
- [x] **Unshifted** PageUp/PageDown/Home/End emit the standard VT sequences (`ESC[5~`, `ESC[6~`, `ESC[H`, `ESC[F`), sourced from the existing `hid_poll.rs` table — closing the pre-existing paging gap.
- [x] Shift+PageUp/PageDown map to a ±(rows−1) viewport page and Shift+Home/End to oldest-line / live-tail, returning `ViewScroll` and consuming the key locally (no PTY bytes).
- [x] Any key that produces PTY output makes `main.rs` snap `view_offset` to `0` first — the snap-to-bottom-on-keystroke half.
- [x] **Host tests:** a table over (symbol, modifiers) → `KeyOutcome` covering all four keys shifted and unshifted, arrows unchanged, Ctrl+letter unchanged, and that the unshifted sequences match `hid_poll.rs` byte-for-byte.

### A.4 — Wheel binding (Report-protocol lanes)

**Files:** `userspace/term/src/main.rs` (pointer dispatch), `userspace/term/src/mouse.rs`
**Symbols:** the `PulledEvent::Pointer` arm of the main loop, `MouseReporter::encode` (`mouse.rs:250`, `None` at `mouse.rs:257`), `update_mouse_mode`, `PointerEvent.wheel_dy` (`kernel-core/src/input/events.rs:206`)
**Why it matters:** `encode` returns `None` for `PointerButton::None` and never reads `wheel_dx`/`wheel_dy`, so even on a lane where the wheel *is* produced (USB HID Report protocol) it dies in `term`. Split from A.3 because it is lane-conditional while the key bindings are universal.

**Acceptance:**
- [x] When the app has **not** enabled mouse reporting, a `PointerEvent` with `button == None` and non-zero `wheel_dy` adjusts `view_offset` (up = older) instead of being dropped; when mouse reporting is on (`update_mouse_mode`), the wheel is reported to the app as today.
- [x] Behaviour is a documented no-op on lanes with no Report-protocol pointer (PS/2-only boots) — `wheel_dy` is simply always `0` there; no code path asserts a wheel exists.
- [x] **Host tests:** synthetic `PointerEvent`s with `button == None` + `wheel_dy = ±1` produce the expected `view_offset` deltas with mouse reporting off, and are handed to `encode` unchanged with it on.

---

## Track B — Mouse selection + clipboard copy/paste

### B.1 — Selection state + highlight render

**Files:** `userspace/term/src/main.rs`, `userspace/term/src/screen.rs`
**Symbols:** the `PulledEvent::Pointer` arm, the future-work anchor in `pull_one_event`'s doc comment, the `PutGlyph` emit path, `Screen::cell` / `Screen::cell_primary` (existing live-grid accessors — reuse rather than add new ones)
**Why it matters:** `term` has no selection/highlight state at all; this is the documented future track in `pull_one_event`.

**Acceptance:**
- [ ] A `Selection { anchor, extent, mode: Linear|Block, active }` tracks press-anchor / drag-extend / release-commit against cell coordinates (grid hit-test accounts for the current cols/rows and any scrollback offset).
- [ ] Selection drives only when the app has not grabbed the mouse (Shift-drag force-selects when it has — the xterm override); otherwise pointer events still go to `MouseReporter::encode`.
- [ ] Covered cells render inverted (fg/bg swap) in the compose path; clearing the selection repaints cleanly.
- [ ] **Host tests:** anchor/extent normalization (drag up-left vs. down-right yield the same ordered range), `Linear` vs. `Block` coverage predicates, and that hit-test maps pixel → cell correctly at the grid edges.

### B.2 — Clipboard verbs on `term`'s `DisplayClient`

**Files:** `userspace/term/src/display.rs`, `userspace/term/src/main.rs`, `kernel-core/src/display/protocol.rs`, `userspace/lib/desktop_client/src/lib.rs`
**Symbols:** `DisplayClient`; `ClientMessage::SetClipboard`/`RequestClipboard` + `ServerMessage::ClipboardData` (`protocol.rs:521`/`:529`/`:601`); `MimeTag::TextPlainUtf8` (`protocol.rs:232`); `CLIPBOARD_MAX_BYTES` (currently `desktop_client/src/lib.rs:40`)
**Why it matters:** `term` has no clipboard code and does not depend on `desktop_client` (confirmed in its `Cargo.toml`); the verbs must ride the single `"display"` handle `term` already holds, keeping its one-connection model. But `CLIPBOARD_MAX_BYTES` lives in `desktop_client` — the very crate `term` must not depend on — while its value (3900) is derived from `protocol.rs`'s `MAX_FRAME_BODY_LEN = 4096`. Relocate rather than duplicate.

**Acceptance:**
- [ ] `CLIPBOARD_MAX_BYTES` moves to `kernel-core/src/display/protocol.rs` beside `MAX_FRAME_BODY_LEN`, keeping its derivation comment; `desktop_client` re-exports it so existing callers (`clip-smoke`, m3ui) are untouched.
- [ ] `DisplayClient::set_clipboard(&self, text: &str) -> bool` frames `SetClipboard { TextPlainUtf8, len, client_token }` + raw bytes (≤ `CLIPBOARD_MAX_BYTES`, over-long input rejected not truncated) and `ipc_call`s the display handle (mirrors `desktop_client::set_clipboard`, `lib.rs:195`).
- [ ] `DisplayClient::get_clipboard(&self) -> Option<Vec<u8>>` sends `RequestClipboard`, takes the reply bulk, decodes `ClipboardData` (mirrors `desktop_client::get_clipboard`, `lib.rs:224`); a zero-length `ClipboardData` (empty clipboard) yields `Some(vec![])`, not `None`.
- [ ] No second client library or extra IPC connection is introduced.

### B.3 — Copy / paste key + pointer bindings

**Files:** `userspace/term/src/main.rs`, `userspace/term/src/input.rs`
**Symbols:** `wrap_paste` (`input.rs:152`), `InputHandler::translate` (`input.rs:43`), `Cell` (`screen.rs`, note `wide_continuation` and the `0x20` blank codepoint)
**Why it matters:** Copy/paste must be bound without colliding with Ctrl+C (SIGINT); the paste direction must be bracketed so a shell can refuse to execute pasted bytes. Serialization is not "read the chars" — `Cell` carries a `wide_continuation` flag whose cell must be skipped or CJK text double-emits.

**Acceptance:**
- [ ] Copy on selection-release **and** on Ctrl+Shift+C: serialize the selected cells to UTF-8 and `set_clipboard` them. Serialization: skip cells with `wide_continuation == true`; trim trailing cells whose `codepoint == 0x20` per row; join rows with `\n`.
- [ ] Ctrl+Shift+V (and optional middle-click) `get_clipboard`s and injects via `wrap_paste` (bracketed `ESC[200~`…`ESC[201~` when the mode is enabled) — never as a plain byte run.
- [ ] Plain Ctrl+C / Ctrl+V are unchanged (SIGINT / literal), i.e. the clipboard binds require Shift.
- [ ] **Host tests:** serialization over a fixture grid — trailing-blank trim, `\n` join, a double-width glyph yielding one codepoint not two, an all-blank row yielding an empty line, and a selection larger than `CLIPBOARD_MAX_BYTES` being rejected.

---

## Track C — QMP plumbing + gate

### C.1 — QMP button / wheel injection

**File:** `xtask/src/qmp.rs`
**Symbols:** `QmpClient::execute`, existing `press_key` / `press_chord` / `send_pointer_rel` / `send_pointer_abs`
**Why it matters:** `qmp.rs` can inject keys and pointer *motion* but has **no button press/release and no wheel**. C.2's scrollback arm needs wheel injection and its selection arm needs press → motion → release; neither can be written until these exist. Both are `input-send-event` `btn` events.

**Acceptance:**
- [ ] `send_button(button: &str, down: bool)` (and/or a `click`/`drag` convenience) emits `input-send-event` with `{"type":"btn","data":{"down":…,"button":"left"}}`.
- [ ] `send_wheel(dy: i32)` emits the corresponding `wheel-up` / `wheel-down` btn events, repeated `|dy|` times.
- [ ] Host tests assert the emitted JSON shape (the existing `ascii_to_qkeys` tests are the pattern).

### C.2 — `term-daily-driver-smoke` (scrollback render probe + clipboard round-trip)

**Files:** `xtask/src/main.rs` (new `cmd_term_polish_smoke` + QMP/PPM driver), `.githooks/pre-push` (`M3OS_TERM_POLISH_REGRESSION` gate), `AGENTS.md` + `docs/appendix/regression-gates.md` (gate row)
**Symbols:** QMP/PPM plumbing (`xtask/src/qmp.rs`, `xtask/src/ppm.rs`), the `htop-render-probe` pattern, the device set from `cmd_usb_report_smoke`
**Why it matters:** A serial `Wait` proves a program ran, not that the screen scrolled or the highlight painted; the framebuffer must be inspected.
**Scoping notes:** (1) This is a **new composite lane**, not a small extension of `clipboard-smoke` — `clipboard_smoke_steps()` merely runs `/bin/clip-smoke` standalone from sh0 and never launches `term`. (2) The wheel sub-arm requires `-device qemu-xhci -device usb-tablet`; `usb-mouse` is Boot-subclass and its wheel byte is discarded. (3) The harness watches serial, not `term`'s PTY, so the bracketed-paste assertion needs an explicit oracle — run `cat -v` inside the `term` so `ESC[200~` renders as visible `^[[200~`, and assert on the screendump (or via a debug sentinel `term` emits on paste).

**Acceptance:**
- [ ] Scrollback arm (all lanes): fill past one page, inject Shift+PageUp over QMP, screendump, and assert evicted rows are visible; inject a keystroke and assert the frame snaps back to the live tail.
- [ ] Wheel sub-arm (Report-protocol lane only): same assertion driven by `send_wheel`, on the `qemu-xhci` + `usb-tablet` device set. Skips with a printed reason on lanes without it.
- [ ] Alternate-screen arm: launch an htop/less and assert the wheel reaches the app rather than moving the viewport.
- [ ] Selection/clipboard arm: drive a selection with `send_button` + motion over known text, Ctrl+Shift+C via `press_chord`, read the compositor `ClipboardStore` back from a second client and assert byte-equality; Ctrl+Shift+V and assert the bracketed bytes via the `cat -v` oracle. Highlight visible on the dump.
- [ ] Gate is opt-in (`M3OS_TERM_POLISH_REGRESSION=1`), off in the default pre-push set until stabilized; production build unaffected.

---

## Documentation Notes

- Call out that the scrollback ring existed since Phase 57 G.4 but was unviewable — this
  phase adds the *viewport*, not the storage.
- **Do not repeat the "the wheel already arrives, term just drops it" framing** — it is false
  on the default PS/2 lane, where no wheel is ever produced. Document the two-producer
  reality (PS/2 3-byte framing vs. USB HID Report protocol) and that Shift+PageUp is the
  universal binding.
- Record PS/2 IntelliMouse as a named deferred item, with the reason `init_mouse()` avoids
  4-byte framing, so the gap is on record rather than silently assumed away.
- Note that unshifted PageUp/PageDown/Home/End were missing from `term` entirely before this
  phase — the fix is adjacent, in the same function, and unblocks `less`/`htop` paging.
- `term` becomes the second real clipboard client after the Phase 105 `clip-smoke` gate;
  reference the unchanged `ClipboardStore` broker.
- Note the `CLIPBOARD_MAX_BYTES` relocation to `protocol.rs` — the one shared-crate change in
  an otherwise `term`-local phase.
- Prefer exact symbols over line numbers where `main.rs`/`screen.rs` line numbers may drift.
