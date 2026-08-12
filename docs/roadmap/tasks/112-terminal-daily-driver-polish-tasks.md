# Phase 112 — Terminal Daily-Driver Polish (Scrollback + Selection/Clipboard): Task List

**Status:** ✅ Complete — with four acceptance items re-scoped rather than delivered (one host test in B.3, three gate assertions in C.2). Each is left **unticked** below with the reason, and carried into the phase doc's "Deferred Until Later".
**Source Ref:** phase-112
**Depends on:** Phase 22 (TTY and Terminal) ✅, Phase 56 (Display and Input) ✅, Phase 69–69d (Terminal TUI + ncurses) ✅, Phase 92b (USB HID Report Protocol) ✅, Phase 105 (compositor clipboard broker) ✅
**Goal:** Make `term`'s already-stored 1000-line scrollback viewable (Shift+PageUp/Down/Home/End everywhere, wheel on Report-protocol pointer lanes, with snap-to-bottom), fill in the missing unshifted page keys, and add mouse text selection + compositor-brokered copy/paste, reusing the Phase 105 clipboard protocol and the Phase 69 bracketed-paste framing. Feature work is userspace-only; one ring-0 fix rides along because the Track C gate exposed it — a PTY `write()` returning `0` on a full ring, which killed the shell inside `term` whenever output outran the drain (see the phase doc, "The PTY write-zero fix").

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Scrollback viewport (`view_offset`, key + wheel bindings, snap-to-bottom) | — | ✅ Landed |
| B | Mouse selection + clipboard copy/paste in `term` | A (shared render/pointer seam) | ✅ Landed |
| C | QMP input plumbing + render-probe / clipboard round-trip gate | A, B | ✅ Landed (C.2 re-scoped — three acceptance items unticked) |

Tracks A and B share the pointer-intake seam (the `PulledEvent::Pointer` arm of `main.rs`'s
event loop) and the `PutGlyph` emit path; land A first (it establishes the render seam the
highlight reuses). Both feature tracks are userspace-only; the only ring-0 change in the phase is
the PTY write-zero fix that Track C's gate uncovered.

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
- [x] **Adjacent gap closed in the same function:** `KEYSYM_DELETE` → `ESC[3~`. `special_key_sequence` handled eight keysyms where `hid_poll` handles nine, so the Delete key produced literally nothing in `term` (it is not `0x08`, not a Ctrl chord, and `0xE019` fails the `symbol <= 0x7F` passthrough). Emitted unconditionally with respect to modifiers — Shift+Delete is the same four bytes and is *not* a viewport bind. `KEYSYM_INSERT` remains deliberately silent, matching `hid_poll.rs`'s documented omission.
- [x] **Host tests:** a table over (symbol, modifiers) → `KeyOutcome` covering all four page keys shifted and unshifted, arrows unchanged, Ctrl+letter unchanged. The `hid_poll` cross-check is a **range sweep over the whole private-use keysym block `0xE000..=0xE0FF`**, not a hardcoded array — the hardcoded array is exactly why the missing Delete arm was invisible, and the sweep now fails automatically in *both* directions if either table starts or stops handling a keysym. `unshifted_special_keys_are_the_expected_nine` pins the nine actual sequences (the sweep alone would pass if both tables were blanked) and `insert_writes_nothing` pins the Delete/Insert asymmetry as deliberate.

### A.4 — Wheel binding (Report-protocol lanes)

**Files:** `userspace/term/src/main.rs` (pointer dispatch), `userspace/term/src/mouse.rs`
**Symbols:** the `PulledEvent::Pointer` arm of the main loop, `MouseReporter::encode` (`mouse.rs:250`, `None` at `mouse.rs:257`), `update_mouse_mode`, `PointerEvent.wheel_dy` (`kernel-core/src/input/events.rs:206`)
**Why it matters:** `encode` returns `None` for `PointerButton::None` and never reads `wheel_dx`/`wheel_dy`, so even on a lane where the wheel *is* produced (USB HID Report protocol) it dies in `term`. Split from A.3 because it is lane-conditional while the key bindings are universal.

**Acceptance:**
- [x] When the app has **not** enabled mouse reporting, a `PointerEvent` with `button == None` and non-zero `wheel_dy` adjusts `view_offset` (up = older) instead of being dropped — `MouseReporter::classify` returns `PointerAction::ScrollView(wheel_dy * wheel_rows)`.
- [x] When mouse reporting **is** on, the notch is reported to the application as the xterm wheel pseudo-buttons: `MouseReporter::encode` maps `wheel_dy > 0` to button `WHEEL_UP = 64` and `wheel_dy < 0` to `WHEEL_DOWN = 65` (`mouse.rs`), always encoded as a *press* — a notch has no release edge, so SGR reports terminate in upper-case `M` and press-only X10 (`?9h`) receives it too. Note this is **not** "as before": before this phase `encode` returned `None` for every `PointerButton::None` event, so a tracking app saw no wheel at all.
- [x] Behaviour is a documented no-op on lanes with no Report-protocol pointer (PS/2-only boots) — `wheel_dy` is simply always `0` there; no code path asserts a wheel exists.
- [x] **Host tests:** `wheel_scrolls_viewport_when_app_is_not_tracking` (viewport delta with reporting off), `wheel_reports_to_the_app_instead_of_scrolling_the_viewport` (the 64/65 report with it on), `zero_wheel_delta_is_ignored_not_a_zero_scroll`, `zero_wheel_delta_reports_nothing_while_tracking`, and `wheel_report_fits_max_bytes` (the widest SGR wheel report, `\x1b[<65;65535;65535M` = 18 bytes, stays inside `MAX_BYTES = 24`).

---

## Track B — Mouse selection + clipboard copy/paste

### B.1 — Selection state + highlight render

**Files:** `userspace/term/src/main.rs`, `userspace/term/src/screen.rs`
**Symbols:** the `PulledEvent::Pointer` arm, the future-work anchor in `pull_one_event`'s doc comment, the `PutGlyph` emit path, `Screen::cell` / `Screen::cell_primary` (existing live-grid accessors — reuse rather than add new ones)
**Why it matters:** `term` has no selection/highlight state at all; this is the documented future track in `pull_one_event`.

**Acceptance:**
- [x] A `Selection { anchor, extent, mode: Linear|Block, active }` tracks press-anchor / drag-extend / release-commit against cell coordinates (grid hit-test accounts for the current cols/rows and any scrollback offset).
- [x] Selection drives only when the app has not grabbed the mouse (Shift-drag force-selects when it has — the xterm override); otherwise pointer events still go to `MouseReporter::encode`.
- [x] Covered cells render inverted (fg/bg swap) in the compose path; clearing the selection repaints cleanly.
- [x] **Host tests:** anchor/extent normalization (drag up-left vs. down-right yield the same ordered range), `Linear` vs. `Block` coverage predicates, and that hit-test maps pixel → cell correctly at the grid edges. The hit-test lives in the library as `term::pointer_cell` (`lib.rs`) precisely so it is host-testable — `main.rs` is `#[cfg(not(test))]` throughout and nothing in it is compiled by `cargo test -p term`. Seven tests cover the origin pixel, the first pixel of an interior cell, the last cell of the grid, clamping past the right/bottom edges, negative coordinates (including `i32::MIN`), an `abs_position: None` event, and a degenerate 0×0 grid.
- [x] A selection is **invalidated** (cleared, and the view marked dirty) by every grid mutation — `Screen::invalidate_selection` is wired into `switch_to_alt`, `switch_to_primary`, `resize`, `set_view_offset`, `put_char`, `scroll_region_up`, `scroll_region_down`, `insert_chars`, `delete_chars`, `blank_cell`, and `clear_buffer`. `Selection` stores *display* coordinates re-resolved through `display_cell()` at both paint and copy time, so a selection that outlived a mutation would highlight unrelated text and copy the wrong bytes. A no-op viewport set (wheeling against either end of history) deliberately does not clear.
- [x] **Host tests:** nine invalidation tests, including that `resize` still leaves `take_view_dirty()` true (the hook sits below `resize`'s own `view_dirty = false`), that a clamped-to-no-op `set_view_offset` keeps the selection, and that output arriving with no selection active does not dirty the view (the cost guard).

### B.2 — Clipboard verbs on `term`'s `DisplayClient`

**Files:** `userspace/term/src/display.rs`, `userspace/term/src/main.rs`, `kernel-core/src/display/protocol.rs`, `userspace/lib/desktop_client/src/lib.rs`
**Symbols:** `DisplayClient`; `ClientMessage::SetClipboard`/`RequestClipboard` + `ServerMessage::ClipboardData` (`protocol.rs:521`/`:529`/`:601`); `MimeTag::TextPlainUtf8` (`protocol.rs:232`); `CLIPBOARD_MAX_BYTES` (currently `desktop_client/src/lib.rs:40`)
**Why it matters:** `term` has no clipboard code and does not depend on `desktop_client` (confirmed in its `Cargo.toml`); the verbs must ride the single `"display"` handle `term` already holds, keeping its one-connection model. But `CLIPBOARD_MAX_BYTES` lives in `desktop_client` — the very crate `term` must not depend on — while its value (3900) is derived from `protocol.rs`'s `MAX_FRAME_BODY_LEN = 4096`. Relocate rather than duplicate.

**Acceptance:**
- [x] `CLIPBOARD_MAX_BYTES` moves to `kernel-core/src/display/protocol.rs` beside `MAX_FRAME_BODY_LEN`, keeping its derivation comment; `desktop_client` re-exports it so existing callers (`clip-smoke`, m3ui) are untouched.
- [x] `DisplayClient::set_clipboard(&self, text: &str) -> bool` frames `SetClipboard { TextPlainUtf8, len, client_token }` + raw bytes (≤ `CLIPBOARD_MAX_BYTES`, over-long input rejected not truncated) and `ipc_call`s the display handle (mirrors `desktop_client::set_clipboard`, `lib.rs:195`).
- [x] `DisplayClient::get_clipboard(&self) -> Option<Vec<u8>>` sends `RequestClipboard`, takes the reply bulk, decodes `ClipboardData` (mirrors `desktop_client::get_clipboard`, `lib.rs:224`); a zero-length `ClipboardData` (empty clipboard) yields `Some(vec![])`, not `None`.
- [x] No second client library or extra IPC connection is introduced.

### B.3 — Copy / paste key + pointer bindings

**Files:** `userspace/term/src/main.rs`, `userspace/term/src/input.rs`
**Symbols:** `wrap_paste` (`input.rs:152`), `InputHandler::translate` (`input.rs:43`), `Cell` (`screen.rs`, note `wide_continuation` and the `0x20` blank codepoint)
**Why it matters:** Copy/paste must be bound without colliding with Ctrl+C (SIGINT); the paste direction must be bracketed so a shell can refuse to execute pasted bytes. Serialization is not "read the chars" — `Cell` carries a `wide_continuation` flag whose cell must be skipped or CJK text double-emits.

**Acceptance:**
- [x] Copy on selection-release **and** on Ctrl+Shift+C: serialize the selected cells to UTF-8 and `set_clipboard` them. Serialization: skip cells with `wide_continuation == true`; trim trailing cells whose `codepoint == 0x20` per row; join rows with `\n`.
- [x] Ctrl+Shift+V `get_clipboard`s and injects via `wrap_paste` (bracketed `ESC[200~`…`ESC[201~` when the mode is enabled) — never as a plain byte run. The optional middle-click binding was **not** implemented; nothing in `term` binds a non-zero pointer button.
- [x] Plain Ctrl+C / Ctrl+V are unchanged (SIGINT / literal), i.e. the clipboard binds require Shift.
- [x] A rejected copy is **reported**, not swallowed: `copy_selection` returns `false` and both call sites ring the terminal bell in addition to the `term: clipboard copy rejected` serial line. (`term` has no status line, and painting a message into the cell grid would overwrite the application's own output and be repainted away on its next refresh.) A copy with no selection returns `true`, so a stray click never beeps.
- [x] A paste that cannot be written in full is reported rather than silently truncated: `paste_clipboard` writes through `write_all_bounded`, which distinguishes backpressure (`EAGAIN` / zero-byte write → 25 ms sleep, ~1 s budget) from a hard errno, and logs `term: paste truncated` on failure. A short write that dropped the closing `ESC[201~` would strand the application in bracketed-paste mode.
- [x] **Host tests:** serialization over a fixture grid — trailing-blank trim, `\n` join, a double-width glyph yielding one codepoint not two, an all-blank row yielding an empty line.
- [x] **Host tests:** the clipboard cap predicate `term::clipboard_payload_fits` (`lib.rs`) — at the cap, one byte over, empty, and a multi-byte UTF-8 payload whose *byte* length exceeds the cap while its *character* count does not (pinning that the check is on encoded bytes). `DisplayClient::set_clipboard` calls the same predicate and rejects rather than truncates.
- [ ] **Host test — an oversized selection driven through the rejecting predicate.** `screen.rs`'s `oversized_selection_exceeds_the_clipboard_cap` builds a 200×40 selection and asserts only that the *fixture* exceeds `CLIPBOARD_MAX_BYTES`; it never calls `clipboard_payload_fits` or `set_clipboard`, so the selection → rejection chain itself is not exercised in one test. The two halves are each covered (serialization above, the predicate above) but not composed. See "Deferred Until Later" in the phase doc.

---

## Track C — QMP plumbing + gate

### C.1 — QMP button / wheel injection

**File:** `xtask/src/qmp.rs`
**Symbols:** `QmpClient::execute`, existing `press_key` / `press_chord` / `send_pointer_rel` / `send_pointer_abs`
**Why it matters:** `qmp.rs` can inject keys and pointer *motion* but has **no button press/release and no wheel**. C.2's scrollback arm needs wheel injection and its selection arm needs press → motion → release; neither can be written until these exist. Both are `input-send-event` `btn` events.

**Acceptance:**
- [x] `send_button(button: &str, down: bool)` (and/or a `click`/`drag` convenience) emits `input-send-event` with `{"type":"btn","data":{"down":…,"button":"left"}}`.
- [x] `send_wheel(dy: i32)` emits the corresponding `wheel-up` / `wheel-down` btn events, repeated `|dy|` times.
- [x] Host tests assert the emitted JSON shape (the existing `ascii_to_qkeys` tests are the pattern).

### C.2 — `term-daily-driver-smoke` (scrollback render probe + clipboard round-trip)

**Files:** `xtask/src/main.rs` (new `cmd_term_polish_smoke` + QMP/PPM driver), `.githooks/pre-push` (`M3OS_TERM_POLISH_REGRESSION` gate), `AGENTS.md` + `docs/appendix/regression-gates.md` (gate row)
**Symbols:** `cmd_term_polish_smoke`, `capture_settled`, `capture_term_frame`, `changed_rows_in_band`, `band_pixels_matching`, `clip_payload_before_anchor`, `window_has_exact_line`; QMP/PPM plumbing (`xtask/src/qmp.rs`, `xtask/src/ppm.rs`), the `htop-render-probe` pattern, the device set from `cmd_usb_report_smoke`
**Why it matters:** A serial `Wait` proves a program ran, not that the screen scrolled or the highlight painted; the framebuffer must be inspected.
**Scoping notes:** (1) This is a **new composite lane**, not a small extension of `clipboard-smoke` — `clipboard_smoke_steps()` merely runs `/bin/clip-smoke` standalone from sh0 and never launches `term`. (2) The wheel arms require `-device qemu-xhci -device usb-tablet`; `usb-mouse` is Boot-subclass and its wheel byte is discarded, so the gate *drops* the `usb-mouse` the xhci `DeviceSet` attaches and leaves exactly one pointer on the bus — QMP `input-send-event` cannot be addressed at an input device, so with both present QEMU would deliver the notch to whichever claims the event class. (3) **The bracketed framing is not observable on this lane and the gate does not assert it.** The `cat -v` oracle sketched here does not work: `ESC[200~`/`ESC[201~` are emitted only when the *application* has set `?2004h`, and a tree-wide grep for `2004` finds the mode bit in `term::screen` and the framing in `term::input::wrap_paste` and nothing else — no program in the image enables it, so `wrap_paste` correctly passes the payload through unframed and there is nothing on the wire to render. The framed form is covered by the `wrap_paste` host tests. (4) The alternate-screen arm uses `tui-smoke mouse-live`, a purpose-built in-guest probe, not a ported TUI: htop's process list can legitimately change zero pixels on a scroll, nothing guarantees ncurses enables mouse tracking under `TERM=m3os-term`, and pulling a port build into this gate would cost minutes.

**Acceptance (six arms as landed):**
- [x] **Arm 1 — scrollback (all lanes):** `dmesg` fills far past the 25-row live grid, injected Shift+PageUp must change ≥20 scanlines versus the settled live-tail frame, and a plain `x` keystroke must change ≥20 scanlines versus the scrolled frame **and** land within `TERM_QUIET_SCANLINES` (96) of the live-tail frame. `x` rather than Return: an ordinary printable key writes PTY bytes without scrolling the primary region, so the snap is attributable to the keystroke path and not to `scroll_region_up`'s snap-on-any-primary-scroll.
- [x] **Arm 2 — Report-protocol wheel → viewport:** the same assertion driven by `send_wheel(5)` on the `qemu-xhci` + `usb-tablet` device set, followed by a Shift+End that must return the frame to within 96 scanlines of the pre-wheel baseline (a viewport still stuck in history would make every later arm select evicted rows).
- [x] **Arm 3 — selection + clipboard:** a QMP left-button drag (`drag_abs`) over an `echo M3OS_COPY_ME` line must change ≥20 scanlines (the highlight), and copy-on-release must land the text in the compositor `ClipboardStore`, read back by an **independent** client (`clip-smoke --paste`) and asserted as a *whole trimmed line* equal to the marker — a line merely *containing* it would be the echoed command row, not the copied output. The read-back is sequenced by an `echo-args CLIPSYNCA` anchor rather than by counting sentinel occurrences (`serial_history` is a rolling 192 KiB window, so counts taken minutes apart are not comparable).
- [x] **Arm 4 — paste reaches the PTY:** Ctrl+Shift+V into a bare `cat` must change ≥20 scanlines *and* leave the frame's black-pixel ratio ≥0.15 (`term` paints black, the compositor teal — a frame that changed because `term` died is not a pass). The `cat` sink is load-bearing: pasting a multi-row selection at a shell prompt would execute every line in it.
- [x] **Arm 5 — wheel → application on the alternate screen:** `/bin/tui-smoke mouse-live` takes the alternate screen, enables `?1000h` + `?1006h` itself, decodes `term`'s SGR reports, and floods half the grid per notch. Injection waits on its `TUI_SMOKE:mouse-live:ready` sentinel (injecting earlier is a race with a *wrong answer* — until `?1000h` reaches `term` the notch is consumed by term's own viewport). One wheel-up must produce `cb=64` on serial plus ≥20 000 saturated-red pixels in the top half dominating the bottom; one wheel-down must produce `cb=65` plus ≥20 000 blue in the bottom half dominating the top, with the red band gone. On `q` the probe's own tally must read exactly `up=1` / `down=1` / `ok`. Afterwards the primary buffer must be back (black ratio ≥0.15) and a Shift+End must change nothing (≤96 scanlines) — no notch leaked into `term`'s viewport.
- [x] **Arm 6 — Shift-drag override, while the app still holds the mouse:** an unshifted drag must change ≤96 scanlines (the application owns it), and the identical drag via `drag_abs_with_mods(&["shift"], …)` must change ≥20 **and** replace the compositor clipboard offer (read back with its own `CLIPSYNCB` anchor, asserting arm 3's marker is no longer the standing offer). This is the only arm that exercises the compositor's live-modifier stamping end to end.
- [x] Gate is opt-in (`M3OS_TERM_POLISH_REGRESSION=1`, `.githooks/pre-push`), off in the default pre-push set until stabilized; production build unaffected.
- [ ] **Wheel arm skips with a printed reason on lanes without a Report-protocol pointer.** Not implemented: `cmd_term_polish_smoke` unconditionally builds its own device set and appends `-device usb-tablet,bus=xhci0.0`, so the lane always exists and there is no skip branch to take. The gate therefore has no PS/2-only mode; the wheel arms are simply always run against the tablet it attaches.
- [ ] **Ctrl+Shift+C driven over QMP, and a byte-exact whole-payload compare.** The gate copies via **copy-on-release** only — there is no `press_chord(&["ctrl","shift","c"], …)` anywhere in it — and it asserts a *whole-line* match on the marker inside a multi-row payload rather than byte-equality of the entire offer (the tall drag deliberately captures the rows above and below the marker, so the payload is not known exactly). Ctrl+Shift+C's key path is covered only by the `term::input` host tests.
- [ ] **Bracketed `ESC[200~` / `ESC[201~` bytes asserted on the wire.** Not assertable on this lane — see scoping note (3). Covered by the `term::input::wrap_paste` host tests instead.

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
- Note the `CLIPBOARD_MAX_BYTES` relocation to `protocol.rs` — one of the two shared-crate
  changes in an otherwise `term`-local phase.
- **Document the pointer coordinate-space change.** The other shared-crate change —
  `PointerRouteDecision::deliver_origin` + `InputDispatcher::modifiers()` in
  `kernel_core::input::dispatch`, consumed by `display_server` — is the widest-reaching
  behavior change in the phase and is not `term`-specific. Every `display_server` client now
  receives **surface-local** pointer coordinates and **live** keyboard modifiers. See the phase
  doc's "Pointer coordinate space and live modifiers" section; the residual letterbox delta and
  the missing implicit pointer grab are recorded as deferrals, not glossed.
- Record the terminfo consequence: `xtask/terminfo/m3os-term.ti` gained `kdch1`, `khome`,
  `kend`, `kpp`, `knp`, and it is compiled by `tic` at image-build time, so
  `cargo xtask clean` is required before the next run/gate or the existing `disk.img` keeps the
  old compiled entry. `kich1` is deliberately absent — `term` sends nothing for Insert.
- Prefer exact symbols over line numbers where `main.rs`/`screen.rs` line numbers may drift.
