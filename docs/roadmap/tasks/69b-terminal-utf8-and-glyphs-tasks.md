# Phase 69b — UTF-8 Wire Decoding and Bitmap Glyph Expansion: Task List

**Status:** Complete
**Source Ref:** phase-69b
**Depends on:** Phase 22b (ANSI Escape) ✅, Phase 57 (Audio and Local Session) ✅, Phase 69 (Terminal Contract Foundations), Phase 69a (Termios Raw Mode)
**Goal:** Land UTF-8 wire decoding in `term`'s feed path; extend the bitmap font to cover the Latin-1 supplement and Unicode box-drawing block; wire the EAW double-width accounting stub; activate the `IUTF8` termios flag's erase behaviour. Nerd Font icons remain deferred to Phase 69c.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `Utf8Decoder` state machine in `kernel-core/src/utf8.rs` | None | Planned |
| B | `Screen::feed` codepoint feed; widen `ConsoleCmd::PutChar` to `u32` | A | Planned |
| C | Latin-1 supplement bitmap glyphs (U+0080–U+00FF) | None | Planned |
| D | Unicode box-drawing bitmap glyphs (U+2500–U+257F) | None | Planned |
| E | `resolve_glyph` accessor + fallback dot | C, D | Planned |
| F | EAW `width_of` stub + wide-cell accounting in `Screen::put_char` | B | Planned |
| G | `IUTF8` termios erase wiring in `kernel-core::tty::Ldisc::erase_one` | None | Planned |
| H | Validation: `tui-smoke utf8` subcommand | A, B, C, D, E, F, G | Planned |
| I | Documentation: Phase 22b / 69 / 69a cross-refs; appendix update; aligned legacy learning doc; kernel patch bump to 0.69.2 | H | Planned |

---

## Track A — UTF-8 Decoder

### A.1 — `Utf8Decoder` state machine

**File:** `kernel-core/src/utf8.rs` (new)
**Symbol:** `Utf8Decoder`, `DecoderOutput`
**Why it matters:** Without a strict decoder, byte streams from real apps land as Latin-1 garbage in cells.

**Acceptance:**
- [x] `Utf8Decoder::new()` returns a decoder in the `Initial` state.
- [x] `decode_byte(b: u8) -> DecoderOutput` returns `Pending`, `Codepoint(u32)`, or `Invalid`.
- [x] Every well-formed 1/2/3/4-byte sequence is decoded to the correct codepoint.
- [x] Overlong encodings (e.g. `\xC0\xAF` for `/`) are rejected as `Invalid`.
- [x] Surrogate codepoints (U+D800–U+DFFF) in 3-byte encoding are rejected as `Invalid`.
- [x] Codepoints above U+10FFFF are rejected.
- [x] After `Invalid`, the decoder resyncs on the next valid leading byte (W3C contract).
- [x] Host tests cover: every length, every malformed prefix, the four W3C resync cases, and the full happy-path coverage of all four leading-byte ranges.

---

## Track B — Codepoint Feed

### B.1 — Widen `ConsoleCmd::PutChar` to `u32`

**File:** `kernel-core/src/fb.rs`
**Symbol:** `ConsoleCmd::PutChar`
**Why it matters:** Cells already store `u32`; the parser payload was the bottleneck.

**Acceptance:**
- [x] `ConsoleCmd::PutChar(u32)` replaces the existing `PutChar(char)`.
- [x] Every existing caller continues to compile (the change is cast-only at the boundary).
- [x] Host tests cover: ASCII codepoints unchanged; non-BMP codepoint passes through verbatim.

### B.2 — `Screen::feed` byte → decoder → parser

**File:** `userspace/term/src/screen.rs`
**Symbol:** `Screen::feed`, `Screen::decoder`
**Why it matters:** This is where the byte stream becomes a codepoint stream.

**Acceptance:**
- [x] `Screen` carries a `Utf8Decoder` field, initialised to `Initial`.
- [x] `feed(byte)` first calls `decoder.decode_byte(byte)`; on `Pending`, returns immediately; on `Codepoint(c)`, passes `c` through the parser; on `Invalid`, treats it as `Codepoint(0xFFFD)`.
- [x] ASCII escape sequences (which are pure ASCII) flow through unchanged — the decoder completes a 1-byte codepoint per byte.
- [x] BEL (`0x07`) interception (Phase 57 behaviour) still fires before the decoder is consulted — preserves Phase 57 behaviour exactly.

---

## Track C — Latin-1 Supplement Bitmap

### C.1 — `GLYPH_TABLE_LATIN1`

**File:** `kernel-core/src/session/glyph_tables.rs` (new — placed alongside the existing ASCII font tables in `kernel-core/src/session/font_data.rs` rather than `kernel-core/src/fb.rs`; `fb.rs` is the ANSI parser, not a glyph table)
**Symbol:** `GLYPH_TABLE_LATIN1: [Glyph; 128]`
**Why it matters:** Western-European accented text (`é`, `ü`, `ç`) is the most common non-ASCII content.

**Acceptance:**
- [x] Table is exactly 128 entries covering U+0080–U+00FF.
- [x] Each glyph is the same bitmap shape as the existing ASCII font (8×16).
- [x] Glyphs are derived in a documented `const fn` from the public-domain IBM VGA font (for letter glyphs) and hand-tuned for symbol glyphs; provenance is documented in `glyph_tables.rs`.
- [x] A host test renders a representative subset (`é`, `Ü`, `ñ`, `©`, `±`) and asserts inked pixels are present; `latin1_e_acute_bitmap_has_diacritic_then_base` pins the exact bitmap shape for `é`.

---

## Track D — Box-Drawing Bitmap

### D.1 — `GLYPH_TABLE_BOX_DRAWING`

**File:** `kernel-core/src/session/glyph_tables.rs`
**Symbol:** `GLYPH_TABLE_BOX_DRAWING: [Glyph; 128]`
**Why it matters:** Without box-drawing, mc/tmux/htop/less' tabular output renders as broken cell boundaries.

**Acceptance:**
- [x] Table is exactly 128 entries covering U+2500–U+257F.
- [x] Single-line and double-line variants are visually distinct (`render_edge_spec` paints a second parallel rail for `EdgeKind::Double`).
- [x] A host test renders the four corners (U+250C, U+2510, U+2514, U+2518) + a T-junction (U+252C) and asserts the matching directional strokes (`box_drawing_corners_have_two_strokes_each`, `box_drawing_t_junctions_have_three_strokes`); the plain horizontal U+2500 is pinned to a centre-only band by `box_drawing_horizontal_is_just_centre_band`.

---

## Track E — Glyph Resolver

### E.1 — `resolve_glyph` + fallback dot

**File:** `kernel-core/src/session/glyph_tables.rs`
**Symbol:** `resolve_glyph(codepoint: u32) -> &'static Glyph`
**Why it matters:** Single dispatch point keeps the renderer agnostic to which tables are present.

**Acceptance:**
- [x] 0x20..=0x7E → ASCII table (via the existing `font_data::GLYPH_BITMAPS`).
- [x] 0xA0..=0xFF → Latin-1 table (visible-range subset; 0xA0 NBSP renders blank).
- [x] 0x2500..=0x257F → box-drawing table.
- [x] Everything else → centred-dot fallback (a single static bitmap).
- [x] 0x00..=0x1F + 0x7F + 0x80..=0x9F + 0xA0 → blank (`BLANK_GLYPH`; control characters never render).
- [x] Host test (`resolver_dispatches_each_range_to_its_table`): every range boundary returns the expected table.

---

## Track F — East Asian Width Accounting

### F.1 — `width_of(codepoint) -> u8`

**File:** `kernel-core/src/session/glyph_tables.rs`
**Symbol:** `width_of`
**Why it matters:** Future CJK / emoji blocks require the cell grid to allocate two cells per glyph; landing the accounting now means later phases just add tables.

**Acceptance:**
- [x] Returns 2 for ranges: U+2E80–U+9FFF, U+3000–U+30FF, U+AC00–U+D7AF, U+F900–U+FAFF, U+FE30–U+FE4F, U+FF00–U+FF60, U+FFE0–U+FFE6.
- [x] Returns 1 for all other codepoints.
- [x] Host tests cover each range boundary (`width_of_canonical_cjk_ranges_returns_two`, `width_of_non_cjk_returns_one`).

### F.2 — Wide-cell accounting in `Screen::put_char`

**File:** `userspace/term/src/screen.rs`
**Symbol:** `Cell::wide_continuation`, `Screen::put_char`
**Why it matters:** A wide glyph must occupy two cells without overlapping with subsequent writes.

**Acceptance:**
- [x] `Cell` gains `wide_continuation: bool`.
- [x] `put_char` reserves `(row, col)` for the codepoint and marks `(row, col+1)` as `wide_continuation = true`, codepoint = 0.
- [x] A wide glyph at the last column wraps to the next row.
- [x] The renderer's PutGlyph stream emits exactly one paint command for the leading cell (the trailing continuation cell does not paint).
- [x] Host tests cover: place a width-2 codepoint, overwrite its continuation cell with another character, assert the original glyph is correctly invalidated and the new character paints at the expected column (`utf8_wide_codepoint_reserves_two_cells`, `utf8_overwrite_trail_of_wide_cleans_lead`, `utf8_wide_codepoint_wraps_at_last_column`).

---

## Track G — IUTF8 Erase

### G.1 — IUTF8-aware `erase_one` in the ldisc

**File:** `kernel-core/src/tty.rs`
**Symbol:** `Ldisc::erase_one`
**Why it matters:** Canonical-mode terminals must erase a full codepoint when IUTF8 is set, not just a single byte.

**Acceptance:**
- [x] When `IUTF8` is cleared (legacy): `erase_one` removes exactly one byte from the line buffer.
- [x] When `IUTF8` is set: `erase_one` removes the trailing continuation bytes (10xxxxxx) plus the leading byte.
- [x] If the buffer trailing bytes are malformed UTF-8 (e.g. a stray leading byte): erase exactly one byte and let the next erase handle the next.
- [x] Host tests cover: erase ASCII (1 byte), erase 2-byte Latin-1, erase 3-byte box-drawing, erase 4-byte emoji, erase across malformed input.

---

## Track H — Validation

### H.1 — `tui-smoke utf8` subcommand

**File:** `userspace/tui-smoke/src/main.rs`
**Symbol:** `cmd_utf8`
**Why it matters:** Phase 69b's acceptance is byte-stream → cell-state correctness; this is the gate.

**Acceptance:**
- [x] Writes a 3-byte UTF-8 sequence for U+2500; asserts `Screen::cell(0, 0).codepoint == 0x2500` and the resolved glyph pointer matches `GLYPH_TABLE_BOX_DRAWING[0]`.
- [x] Writes a lone continuation byte `\x80`; asserts `cell(0, 0).codepoint == 0xFFFD` and the resolved glyph is the fallback dot.
- [x] Writes a 4-byte CJK char U+4E2D; asserts `cell(0, 0).codepoint == 0x4E2D` and `cell(0, 1).wide_continuation == true`; the resolved glyph is the fallback dot (CJK block is not covered until a later phase).
- [x] Sets `IUTF8`, pushes a 2-byte Latin-1 codepoint into a canonical-mode buffer, calls `erase_one_codepoint(true)`, asserts the entire codepoint is removed.
- [x] Prints `TUI_SMOKE:utf8:ok` on success.

### H.2 — Gate via existing `cargo xtask tui-smoke`

**File:** `xtask/src/main.rs`
**Symbol:** `tui_smoke` subcommand
**Why it matters:** Reuse Phase 69's gate rather than spinning up a new one.

**Acceptance:**
- [x] The `tui_smoke` subcommand now also runs `tui-smoke utf8` and asserts `:ok` via the `TUI_SMOKE_SUBCOMMANDS` matrix in `xtask/src/main.rs`.
- [x] Total runtime increase < 5 s (one extra send/wait pair; same 15 s per-subcommand cap as the others).

---

## Track I — Documentation and Release

### I.1 — Cross-reference Phase 22b, 69, 69a docs

**Files:**
- `docs/roadmap/22b-ansi-parser-enhancement.md`
- `docs/roadmap/69-terminal-tui-capabilities.md`
- `docs/roadmap/69a-terminal-termios.md`

**Symbol:** N/A
**Why it matters:** Phase 22b's PutChar widening, Phase 69's parser extension, and Phase 69a's IUTF8 flag all converge here.

**Acceptance:**
- [x] Phase 22b doc notes that `ConsoleCmd::PutChar` was widened from `char` to `u32` in Phase 69b.
- [x] Phase 69 doc's `Deferred Until Later` section for UTF-8 is updated to `(closed in Phase 69b)`.
- [x] Phase 69a doc notes that the `IUTF8` flag's behavioural effect landed in Phase 69b.

### I.2 — Extend `docs/appendix/term-escape-sequences.md`

**File:** `docs/appendix/term-escape-sequences.md`
**Symbol:** N/A
**Why it matters:** Canonical reference for what `term` renders.

**Acceptance:**
- [x] New "UTF-8 input" section documents the byte-stream contract and replacement-character rule.
- [x] New "Glyph coverage" section enumerates the three covered Unicode ranges and the fallback policy.

### I.3 — Create the aligned legacy learning doc

**File:** `docs/69b-terminal-utf8-and-glyphs.md`
**Symbol:** (new document)
**Why it matters:** Learners need a self-contained reference for the UTF-8 wire-decoding contract and the bitmap-glyph expansion — the byte-stream state machine, the W3C replacement rule, the three covered Unicode ranges + fallback policy, the EAW double-width cell accounting stub, and the `IUTF8` erase behaviour — without conflating it with Phase 22b's ANSI parser, Phase 57's `term` bring-up, or Phase 69a's broader termios contract. The aligned legacy doc is the canonical companion to the roadmap design doc per `docs/appendix/doc-templates.md`.

**Acceptance:**
- [x] `docs/69b-terminal-utf8-and-glyphs.md` exists with all template fields populated.
- [x] Overview paragraph explains what Phase 57 left as "byte-cast-to-char" feed and what Phase 69b closes.
- [x] Key Files table cites every changed file (decoder, parser-IR widening, glyph tables module, font module, tty module, screen module, tui-smoke, xtask wiring, appendix doc).
- [x] Closure of Related Phases section cross-refs Phase 22b, 57, 69, 69a.
- [x] Deferred or Later-Phase Topics section enumerates TTF / Nerd Font, CJK tables, Unicode normalisation, combining characters, BiDi, variation selectors + emoji ZWJ.
- [x] Related Roadmap Docs links design doc and task doc.

### I.4 — Kernel patch bump to 0.69.2

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version`
**Why it matters:** Patch bump per phase.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.69.2"`.
- [x] `Cargo.lock` regenerated (by the first build below).
- [x] `AGENTS.md` version cursor updated.
- [x] `cargo xtask check` passes (run below).

---

## Documentation Notes

- The UTF-8 decoder is intentionally in `kernel-core` (not `userspace/term`) so the kernel framebuffer console can use it once a follow-up phase widens that path.
- The 128 + 128 new bitmap glyphs add ~4 KB to the kernel binary (8×16 × 2 bits). This is the cheapest possible coverage upgrade and is the right baseline before paying for TTF infrastructure in 69c.
- EAW double-width is wired before any wide-glyph tables ship so that adding tables later is a pure data change.
- Nerd Font private-use-area codepoints (U+E000–U+F8FF, U+F0000+) render as the fallback dot through 69b; this is expected — full Nerd Font lands with the TTF loader in 69c.
