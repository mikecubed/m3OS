# Phase 112 — Terminal Daily-Driver Polish (Scrollback + Selection/Clipboard): Task List

**Status:** Planned
**Source Ref:** phase-112
**Depends on:** Phase 22 (TTY and Terminal) ✅, Phase 56 (Display and Input) ✅, Phase 69–69d (Terminal TUI + ncurses) ✅, Phase 105 (compositor clipboard broker) ✅
**Goal:** Make `term`'s already-stored 1000-line scrollback viewable (wheel + Shift+PageUp/Down/Home/End with snap-to-bottom) and add mouse text selection + compositor-brokered copy/paste — both userspace-only, reusing the Phase 105 clipboard protocol and the Phase 69 bracketed-paste framing.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Scrollback viewport (`view_offset`, wheel + key bindings, snap-to-bottom) | — | Planned |
| B | Mouse selection + clipboard copy/paste in `term` | A (shared render/pointer seam) | Planned |
| C | Render-probe + clipboard round-trip gate | A, B | Planned |

Tracks A and B share the pointer-intake seam (`main.rs:395`) and the `PutGlyph` emit path;
land A first (it establishes the render seam the highlight reuses). Both are userspace-only —
no kernel change.

---

## Track A — Scrollback viewport

### A.1 — `view_offset` field + scrollback read accessor

**File:** `userspace/term/src/screen.rs`
**Symbols:** `Screen` (`screen.rs:331`), `scrollback` (`screen.rs:345`), `scrollback_len` (`screen.rs:590`)
**Why it matters:** The scrollback ring is currently write-only — only `scrollback_len()` reads it back — so the user can never see history. This adds the viewport state and the row read path.

**Acceptance:**
- [ ] `Screen` gains `view_offset: usize` (`0` = live tail) and clamps it to `[0, scrollback_len()]`.
- [ ] A `scrollback_row(i) -> &[Cell]` (or an iterator) exposes evicted rows for compositing; no change to eviction (`scroll_region_up`, `screen.rs:1013`) or the cap (`SCROLLBACK_LINES = 1000`, `lib.rs:87`).
- [ ] `view_offset` is forced to `0` whenever the primary region scrolls (new output) — the snap-to-bottom-on-output half.

### A.2 — Composite the viewport into the rendered frame

**Files:** `userspace/term/src/screen.rs` (emit path), `userspace/term/src/render.rs` (repaint trigger)
**Symbols:** `switch_to_primary` (`screen.rs:488`), `resize` (`screen.rs:550`), `Renderer::apply`/`compose` (`render.rs:223`/`:291`)
**Why it matters:** The renderer is command-driven and only ever paints the live grid; a view must be composited where `PutGlyph`s are generated (in `screen.rs`), not in `render.rs`.

**Acceptance:**
- [ ] A `compose_view(out)` emits, for `view_offset > 0`, the top rows from `scrollback[len - view_offset ..]` and the remaining rows from the live `buf`; for `view_offset == 0` output is byte-identical to today.
- [ ] Changing `view_offset` triggers a full repaint (the `switch_to_primary`-style re-emit) so no stale live-grid cells remain.
- [ ] Scrollback view is **primary-screen only** (matching the eviction guard at `screen.rs:1024`); on the alternate screen `view_offset` is inert.

### A.3 — Wheel + Shift key bindings

**Files:** `userspace/term/src/main.rs` (pointer dispatch), `userspace/term/src/input.rs` (key decode), `userspace/term/src/mouse.rs`
**Symbols:** `PulledEvent::Pointer` dispatch (`main.rs:395`), `MouseReporter::encode` (`mouse.rs:250`, `None` at `mouse.rs:257`), `special_key_sequence` (`input.rs:100`), `PointerEvent.wheel_dy` (`kernel-core/src/input/events.rs:206`)
**Why it matters:** Wheel scrolls already arrive but are dropped (`button == None` → `encode` returns `None`); Shift+PageUp/Down is unhandled (`special_key_sequence` covers arrows only, never reads `MOD_SHIFT`).

**Acceptance:**
- [ ] When the app has **not** enabled mouse reporting, a `PointerEvent` with `button == None` and non-zero `wheel_dy` adjusts `view_offset` (up = older) instead of being dropped at `main.rs:397`; when mouse reporting is on (`update_mouse_mode`, `main.rs:859`), the wheel is reported to the app as today.
- [ ] `special_key_sequence` (or a `translate` pre-pass, `input.rs:43`) reads `MOD_SHIFT` and maps Shift+PageUp/PageDown to a ±(rows−1) offset page and Shift+Home/End to oldest-line / live-tail, consuming them locally (no PTY bytes).
- [ ] Any key that produces PTY output first snaps `view_offset` to `0` — the snap-to-bottom-on-keystroke half.

---

## Track B — Mouse selection + clipboard copy/paste

### B.1 — Selection state + highlight render

**Files:** `userspace/term/src/main.rs`, `userspace/term/src/screen.rs`
**Symbols:** pointer intake (`main.rs:395`), the future-work anchor (`main.rs:786`), the `PutGlyph` emit path (`screen.rs:488`/`:550`)
**Why it matters:** `term` has no selection/highlight state at all; this is the documented future track at `main.rs:786`.

**Acceptance:**
- [ ] A `Selection { anchor, extent, mode: Linear|Block, active }` tracks press-anchor / drag-extend / release-commit against cell coordinates (grid hit-test accounts for the current cols/rows and any scrollback offset).
- [ ] Selection drives only when the app has not grabbed the mouse (Shift-drag force-selects when it has — the xterm override); otherwise pointer events still go to `MouseReporter::encode`.
- [ ] Covered cells render inverted (fg/bg swap) in the compose path; clearing the selection repaints cleanly.

### B.2 — Clipboard verbs on `term`'s `DisplayClient`

**Files:** `userspace/term/src/display.rs`, `userspace/term/src/main.rs`
**Symbols:** `DisplayClient` (`display.rs:248`, connect `:283`); `ClientMessage::SetClipboard`/`RequestClipboard` + `ServerMessage::ClipboardData` (`kernel-core/src/display/protocol.rs:521`/`:529`/`:601`); `MimeTag::TextPlainUtf8` (`protocol.rs:232`); `CLIPBOARD_MAX_BYTES = 3900` (`desktop_client/src/lib.rs:40`)
**Why it matters:** `term` has no clipboard code and does not depend on `desktop_client`; the verbs must ride the single `"display"` handle `term` already holds, keeping its one-connection model.

**Acceptance:**
- [ ] `DisplayClient::set_clipboard(&self, text: &str) -> bool` frames `SetClipboard { TextPlainUtf8, len, client_token }` + raw bytes (≤ 3900) and `ipc_call`s the display handle (mirrors `desktop_client::set_clipboard`, `lib.rs:195`).
- [ ] `DisplayClient::get_clipboard(&self) -> Option<Vec<u8>>` sends `RequestClipboard`, takes the reply bulk, decodes `ClipboardData` (mirrors `desktop_client::get_clipboard`, `lib.rs:224`).
- [ ] No second client library or extra IPC connection is introduced.

### B.3 — Copy / paste key + pointer bindings

**Files:** `userspace/term/src/main.rs`, `userspace/term/src/input.rs`
**Symbols:** `wrap_paste` (`input.rs:152`), `InputHandler::translate` (`input.rs:43`, `MOD_CTRL` at `:81`)
**Why it matters:** Copy/paste must be bound without colliding with Ctrl+C (SIGINT); the paste direction must be bracketed so a shell can refuse to execute pasted bytes.

**Acceptance:**
- [ ] Copy on selection-release **and** on Ctrl+Shift+C: serialize the selected cells to UTF-8 (per-row trailing-blank trim, `\n` join) and `set_clipboard` them.
- [ ] Ctrl+Shift+V (and optional middle-click) `get_clipboard`s and injects via `wrap_paste` (bracketed `ESC[200~`…`ESC[201~`) — never as a plain byte run.
- [ ] Plain Ctrl+C / Ctrl+V are unchanged (SIGINT / literal), i.e. the clipboard binds require Shift.

---

## Track C — Gate

### C.1 — `term-daily-driver-smoke` (scrollback render probe + clipboard round-trip)

**Files:** `xtask/src/main.rs` (new `cmd_term_polish_smoke` + QMP/PPM driver), `.githooks/pre-push` (`M3OS_TERM_POLISH_REGRESSION` gate), `AGENTS.md` + `docs/appendix/regression-gates.md` (gate row)
**Symbols:** QMP/PPM plumbing (`xtask/src/qmp.rs`, `xtask/src/ppm.rs`), the `htop-render-probe` pattern
**Why it matters:** A serial `Wait` proves a program ran, not that the screen scrolled or the highlight painted; the framebuffer must be inspected. The clipboard arm extends `clipboard-smoke` through `term`.

**Acceptance:**
- [ ] Scrollback arm: fill past one page, inject wheel-up / Shift+PageUp over QMP, screendump, and assert evicted rows are visible; inject a keystroke and assert the frame snaps back to the live tail.
- [ ] Selection/clipboard arm: drive a selection over known text, Ctrl+Shift+C, read the compositor `ClipboardStore` back from a second client and assert byte-equality; Ctrl+Shift+V and assert bracketed bytes on the PTY. Highlight visible on the dump.
- [ ] Gate is opt-in (`M3OS_TERM_POLISH_REGRESSION=1`), off in the default pre-push set until stabilized; production build unaffected.

---

## Documentation Notes

- Call out that the scrollback ring existed since Phase 57 G.4 but was unviewable — this
  phase adds the *viewport*, not the storage.
- Note that the wheel event was already delivered by the Phase 56 input path and merely
  dropped in `term`; the fix reads `wheel_dy`, it does not add a new event.
- `term` becomes the second real clipboard client after the Phase 105 `clip-smoke` gate;
  reference the unchanged `ClipboardStore` broker.
- Prefer exact symbols over line numbers where `main.rs`/`screen.rs` line numbers may drift.
