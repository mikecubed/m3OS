# Phase 69b - UTF-8 Wire Decoding and Bitmap Glyph Expansion

**Status:** Planned
**Source Ref:** phase-69b
**Depends on:** Phase 22b (ANSI Escape) ✅, Phase 57 (Audio and Local Session) ✅, Phase 69 (Terminal Contract Foundations), Phase 69a (Termios Raw Mode)
**Builds on:** Extends Phase 57 `term`'s byte-level `Screen::feed` to a UTF-8 codepoint feed; extends the bitmap font in `kernel-core::fb` (or `userspace/term`'s glyph module — implementer's call) from 7-bit ASCII to cover the Latin-1 supplement (U+0080–U+00FF) and Unicode box-drawing block (U+2500–U+257F). Phase 69a's `IUTF8` termios flag gains its first behavioural effect: when set, the ldisc's VERASE accounting recognises continuation bytes.
**Primary Components:** kernel-core/src/fb.rs (parser + font tables), userspace/term/src/screen.rs, userspace/term/src/render.rs, kernel-core/src/tty.rs (IUTF8 erase)

## Milestone Goal

Phase 69b makes `term` render the byte streams real TUI applications emit. After this phase: `mc`'s blue panel art, `htop`'s graph bars, `tmux`'s pane separators, and accented Western European text in `less` all render as their intended Unicode glyphs rather than `?` placeholders or split-cell garbage. Nerd Font icons are explicitly **not** in scope — they need a TTF loader and land in Phase 69c.

The phase deliberately splits "decode" from "render":

1. **Decode**: a UTF-8 state machine in `Screen::feed` that consumes byte sequences and produces `u32` codepoints (or replacement characters on malformed input).
2. **Render**: an extended bitmap font that covers Latin-1 supplement + Unicode box-drawing; a single fallback glyph (centred dot) for any codepoint outside the covered ranges; an East-Asian-Width-aware double-width handling stub (flags it for a future phase but never crashes).

## Why This Phase Exists

The current `Screen::feed` is a byte-at-a-time function that casts each input byte to `char` via `byte as char`. That works for 7-bit ASCII; for anything else it silently produces a Latin-1 codepoint (or, for high-bit-set bytes from a UTF-8 stream, the wrong codepoint at the wrong cell offset). Modern TUI apps emit UTF-8 unconditionally — even ASCII-text tools like `less` use box-drawing for tablular output, and `htop` paints CPU graph bars with `▁ ▂ ▃ ▄ ▅ ▆ ▇ █`. Without UTF-8 decoding the renderer sees four bytes per box-drawing character and paints four wrong cells.

The post-Phase-57 evaluation lists "UTF-8 decoding and font coverage" as gap #5; this phase closes the half that does not require a font loader.

## Learning Goals

- Understand the UTF-8 byte-sequence shape: 1-byte (0xxxxxxx), 2-byte (110xxxxx 10xxxxxx), 3-byte, 4-byte, and how the leading bits encode the length.
- Learn the standard malformed-input handling rule (the W3C / WHATWG approach: emit U+FFFD per ill-formed sequence, resync on next valid leading byte).
- See how a cell-grid terminal handles East Asian Width — wide glyphs occupy two cells, with the trailing cell marked "wide-continuation."
- Understand why bitmap fonts have a per-codepoint range cost (each new Unicode block is a static table) and why Nerd Font is deferred to TTF infrastructure.

## Feature Scope

### UTF-8 byte-stream decoder (Track A)

A new `Utf8Decoder` state machine in `kernel-core/src/utf8.rs` consumes one byte per call and returns either `DecoderOutput::Pending`, `DecoderOutput::Codepoint(u32)`, or `DecoderOutput::Invalid` (caller emits U+FFFD). The decoder is pure-logic, host-testable, and `no_std`.

### `Screen::feed` codepoint feed (Track B)

`Screen::feed` is refactored to push each input byte through the UTF-8 decoder before consulting the ANSI parser. Escape sequences (whose bytes are all ASCII) pass through the decoder unaffected — each byte completes a 1-byte codepoint and is then routed to the parser. `ConsoleCmd::PutChar(c)` is widened from `char` to `u32` so the cell's codepoint can be any valid Unicode scalar.

### Latin-1 supplement bitmap glyphs (Track C)

Add a `GLYPH_TABLE_LATIN1` static covering U+0080–U+00FF. Each glyph is the same 8×16 (or whatever the existing font size is) bitmap format the kernel framebuffer console already uses. Glyphs are hand-drawn / extracted from a public-domain VGA Latin-1 set.

### Unicode box-drawing bitmap glyphs (Track D)

Add `GLYPH_TABLE_BOX_DRAWING` covering U+2500–U+257F (128 glyphs). This is the smallest single block that unlocks TUI panel/pane rendering for tmux, mc, htop, ranger, less' line-drawing fallback, and most C ncurses apps.

### Glyph resolver (Track E)

A new `resolve_glyph(codepoint: u32) -> &'static [u8; GLYPH_BYTES]` function dispatches:

- 0x20..=0x7E → ASCII table (existing)
- 0xA0..=0xFF → Latin-1 table
- 0x2500..=0x257F → Box-drawing table
- everything else → fallback centred-dot `·` glyph

The function is `const`-friendly so the dispatch compiles down to a small jump table.

### East Asian Width stub (Track F)

A `width_of(codepoint: u32) -> u8` function returning 1 for all currently-covered codepoints and 2 for the U+2E80–U+9FFF / U+3000–U+30FF / U+AC00–U+D7AF / U+F900–U+FAFF / U+FE30–U+FE4F / U+FF00–U+FF60 / U+FFE0–U+FFE6 blocks even though those glyphs are not yet rendered. `Screen::put_char` honours the width: width-2 glyphs occupy `(row, col)` and `(row, col+1)`, with the trailing cell marked `Cell { codepoint: 0, wide_continuation: true }`. The renderer skips wide-continuation cells when painting. The actual CJK glyph tables land in a future phase; this track ensures cell accounting is correct so future blocks slot in cleanly.

### IUTF8 termios effect (Track G)

When Phase 69a's `IUTF8` flag is set in `c_iflag`, the canonical-mode VERASE accounting recognises UTF-8 continuation bytes — pressing backspace erases one codepoint, not one byte. The kernel TTY ldisc and the PTY ldisc both honour this; raw mode is unaffected.

### Validation (Track H)

A new `tui-smoke utf8` subcommand (extending the Phase 69 binary):

- write a 3-byte UTF-8 sequence for U+2500 (`─`), assert the cell at (0, 0) carries codepoint `0x2500` and the rendered pixel block matches the box-drawing horizontal-line bitmap;
- write an invalid lone continuation byte (`\x80`), assert the cell carries `0xFFFD` and the rendered pixel block matches the replacement glyph;
- write a 4-byte CJK character (e.g. U+4E2D), assert the cell pair `(0, 0)`/`(0, 1)` reflects the wide-cell accounting (codepoint set + continuation marker; renderer paints the fallback glyph since the CJK block is not yet covered);
- with `IUTF8` set, type a 2-byte Latin-1 character on a canonical-mode terminal and press backspace, assert exactly one codepoint is erased (not one byte).

## Important Components and How They Work

### `kernel-core/src/utf8.rs` (new) — UTF-8 decoder

Pure-logic state machine. `Utf8Decoder::new()` starts in the `Initial` state. `decode_byte(b)` returns one of:

- `Pending` — partial sequence, more bytes needed.
- `Codepoint(u32)` — full codepoint decoded; decoder reset to `Initial`.
- `Invalid` — malformed sequence; caller emits U+FFFD; decoder resets.

### `userspace/term/src/screen.rs` — codepoint feed

`Screen::feed` becomes `feed(byte)` → invokes decoder → on `Codepoint(c)` it calls the existing per-codepoint path. `ConsoleCmd::PutChar` is widened from `char` to `u32`. `Cell::codepoint` is already `u32`, so no struct widening is needed.

### `kernel-core/src/fb.rs` — extended glyph tables

`GLYPH_TABLE_ASCII` (existing) is joined by `GLYPH_TABLE_LATIN1` and `GLYPH_TABLE_BOX_DRAWING`. The single `resolve_glyph(codepoint)` accessor is the only public API.

### `kernel-core/src/tty.rs` — IUTF8 erase

`Ldisc::erase_one` becomes IUTF8-aware: when the flag is set, it scans back from the buffer end skipping continuation bytes (10xxxxxx) until it finds a leading byte, then erases the whole codepoint.

## How This Builds on Earlier Phases

- Extends Phase 22b's ANSI parser by widening its `PutChar` payload from `char` to `u32`.
- Builds on Phase 69's `Screen::feed` (which Phase 69 already extends for alt-screen) by inserting the UTF-8 decoder before the ANSI parser.
- Activates Phase 69a's `IUTF8` flag (Track G in 69a only round-trips the bit).
- Extends the framebuffer-console glyph tables in `kernel-core::fb` that Phase 9 first introduced.

## Implementation Outline

1. Build `kernel-core/src/utf8.rs` with the byte-stream decoder; host tests cover every well-formed length, every malformed prefix, and the W3C replacement contract.
2. Widen `ConsoleCmd::PutChar` to carry `u32`; update `userspace/term/src/screen.rs::feed` to push bytes through the decoder.
3. Add `GLYPH_TABLE_LATIN1` (U+0080–U+00FF) to `kernel-core::fb`.
4. Add `GLYPH_TABLE_BOX_DRAWING` (U+2500–U+257F) to `kernel-core::fb`.
5. Add the `resolve_glyph(codepoint)` accessor + fallback centred-dot glyph; wire the renderer to it.
6. Add `width_of` + wide-cell accounting in `Screen::put_char`; renderer skips continuation cells.
7. Wire `IUTF8` in `kernel-core::tty::Ldisc::erase_one`; both kernel TTY and PTY honour it.
8. Extend `tui-smoke` with the four UTF-8 checks; gate via the existing `cargo xtask tui-smoke`.
9. Cross-ref Phase 22b, 69, 69a docs; extend `docs/appendix/term-escape-sequences.md` with a UTF-8 section.
10. Kernel patch bump to 0.69.2.

## Acceptance Criteria

- `Utf8Decoder` accepts every well-formed sequence (1–4 byte) and produces the correct codepoint.
- `Utf8Decoder` emits exactly one `Invalid` per ill-formed sequence and resyncs on the next valid leading byte (W3C contract).
- `tui-smoke utf8` lands the cell-grid assertions described in the Validation track and prints `:ok`.
- `mc`-style ASCII art using box-drawing characters renders as a single continuous box (no broken cell boundaries; verified via a tui-smoke canned snapshot).
- With `IUTF8` set and ICANON on, typing a 2-byte Latin-1 character and pressing backspace removes the entire codepoint.
- `cargo xtask tui-smoke` continues to pass.

## Companion Task List

- [Phase 69b Task List](./tasks/69b-terminal-utf8-and-glyphs-tasks.md)

## How Real OS Implementations Differ

- Linux VT uses a runtime-loadable Unicode → glyph map (`setfont`); m3OS bakes the maps in at compile time.
- xterm sources its glyphs from server-side X fonts and supports per-region font fallback; m3OS uses a single bitmap font through Phase 69b — fallback / TTF lands in Phase 69c.
- macOS Terminal.app and iTerm2 implement Unicode 15.0 EAW; m3OS implements only the always-wide CJK ranges in 69b and defers ambiguous-width handling.

## Deferred Until Later

- TTF/OTF font loader + Nerd Font asset embedding → Phase 69c.
- CJK glyph tables (the EAW machinery is in place; tables alone are a future phase).
- Unicode normalisation (NFC/NFD) — `term` does not currently transform input.
- Combining-character handling beyond a single base glyph.
- Bi-directional text (BiDi).
- Variation selectors and emoji ZWJ sequences.
