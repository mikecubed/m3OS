# Terminal UTF-8 Wire Decoding and Bitmap Glyph Expansion

**Aligned Roadmap Phase:** Phase 69b
**Status:** Complete
**Source Ref:** phase-69b
**Supersedes Legacy Doc:** new

## Overview

Phase 57 brought up the userspace `term` graphical terminal emulator
with a byte-level `Screen::feed` that cast each input byte to `char`
via `byte as char` — fine for 7-bit ASCII, silently wrong for any
high-bit input. Modern TUI applications emit UTF-8 unconditionally:
`less` uses U+2500-block box-drawing for tables, `htop` paints CPU
graphs with U+2581..U+2588 vertical-block characters, and even
plain-text logs frequently contain Latin-1 accented letters from
locale-aware tooling. Without a UTF-8 decoder the renderer saw three
or four bytes per box-drawing character and painted three or four
wrong cells.

Phase 69b closes that gap in two layers:

1. **Decode** — a new strict UTF-8 state machine in
   `kernel-core/src/utf8.rs` (`Utf8Decoder` + `DecoderOutput`)
   consumes one byte per call and surfaces `Pending`,
   `Codepoint(u32)`, or `Invalid`. The W3C / WHATWG
   replacement-character contract is honoured: every malformed
   sequence yields exactly one `Invalid` and the decoder resyncs on
   the next valid leading byte. `Screen::feed` routes every byte
   through the decoder before reaching the Phase 22b ANSI parser; on
   `Invalid` the screen renders U+FFFD.
2. **Render** — the bitmap font in `kernel-core::session` is extended
   with `GLYPH_TABLE_LATIN1` (U+0080..=U+00FF) and
   `GLYPH_TABLE_BOX_DRAWING` (U+2500..=U+257F), plus a single
   centred-dot fallback glyph for any codepoint outside the covered
   ranges. A unified `resolve_glyph(codepoint)` accessor dispatches
   ASCII / Latin-1 / box-drawing / fallback, and East-Asian-Width
   accounting (`width_of`) reserves two cells per CJK / halfwidth-
   fullwidth codepoint so future phases that ship those glyph tables
   slot in cleanly. The Phase 22b `ConsoleCmd::PutChar` payload is
   widened from `char` to `u32` so any Unicode scalar can flow from
   parser to renderer without a lossy round-trip.

The phase also activates the Phase 69a `IUTF8` termios bit:
`EditBuffer::erase_one_codepoint(iutf8)` walks back across UTF-8
continuation bytes plus the leading byte when `IUTF8` is set, so a
canonical-mode VERASE press removes one whole codepoint rather than
one byte. With `IUTF8` cleared, the legacy byte-by-byte behaviour is
preserved.

Validation is byte-level: a new `tui-smoke utf8` subcommand exercises
the decoder, the glyph resolver, wide-cell accounting, and the
IUTF8-aware erase against a host-mode `Screen` + `EditBuffer`. The
existing `cargo xtask tui-smoke` gate runs the new subcommand
alongside the others.

Nerd Font / TTF infrastructure and CJK glyph tables remain explicitly
deferred (Phase 69c and a later phase respectively).

## What This Doc Covers

- The strict UTF-8 byte-stream contract (1/2/3/4-byte sequence shape,
  overlong rejection, surrogate rejection, U+10FFFF cap).
- The W3C / WHATWG replacement-character rule and how the
  one-byte-one-output `Utf8Decoder` honours it.
- The three Unicode ranges the Phase 69b font covers and the
  centred-dot fallback policy for everything else.
- The East-Asian-Width accounting stub and how `Cell::wide_continuation`
  keeps the cell grid honest for codepoints whose glyph tables are
  not yet present.
- The `IUTF8` termios bit's first behavioural effect in the line
  discipline (`EditBuffer::erase_one_codepoint`).
- The `tui-smoke utf8` validation harness.

## Core Implementation

`kernel-core/src/utf8.rs` is a pure-logic, `no_std`, allocation-free
state machine. The internal `State` enum names every intermediate
position (`Initial`, `Awaiting2`, `Awaiting3a`, `Awaiting3b`,
`Awaiting4a`, `Awaiting4b`, `Awaiting4c`); each variant carries the
codepoint value accumulated so far. `decode_byte` dispatches on the
current state, applies the leading-byte / continuation-byte rules,
and returns one of the three `DecoderOutput` variants. Overlong 2-byte
encodings are rejected at the leading byte (`0xC0`, `0xC1`); overlong
3-byte and 4-byte encodings are rejected at the trailing byte when the
combined value falls below the minimum for the sequence length.
Out-of-range 4-byte leaders (`>= 0xF5`) and stray continuation bytes
(`10xxxxxx` at the start of a sequence) are also rejected eagerly.
Surrogates (U+D800..=U+DFFF) emitted as 3-byte UTF-8 are caught at
the trailing byte; codepoints above U+10FFFF are caught at the
trailing 4-byte position.

`userspace/term/src/screen.rs::Screen::feed` carries a fresh
`Utf8Decoder` field. Each byte goes through the decoder first:
- `Pending` short-circuits the feed (no cell update, no parser
  invocation).
- `Codepoint(c)` is routed through the Phase 22b ANSI parser when
  `c < 0x80` (every escape-sequence byte is ASCII, so the existing
  CSI grammar keeps working); for `c >= 0x80` it goes straight to
  `Screen::put_char(c)` because non-ASCII codepoints never appear
  inside an escape sequence.
- `Invalid` maps to the constant `REPLACEMENT_CHARACTER` (U+FFFD)
  and follows the `c >= 0x80` path.

`Screen::put_char` honours East-Asian-Width: a width-2 codepoint
reserves `(row, col)` for the leading half and marks `(row, col + 1)`
as a `Cell { codepoint: 0, wide_continuation: true }`. If the width-2
glyph would land at the last column, the cell is blanked and the
glyph wraps to the next row so the wide pair stays adjacent. If a
write would overwrite the trailing half of an existing wide glyph, the
leader is blanked first so the renderer drops its stale pixels;
similarly if the new write would overwrite the leader, the trailing
continuation cell is blanked. The renderer emits exactly one
`RenderCommand::PutGlyph` for the leading cell of a wide pair.

`kernel-core/src/session/glyph_tables.rs` builds both new bitmap
tables at compile time via `const fn`s, references them through
`'static` slice borrows, and exposes a single `resolve_glyph(codepoint)
-> &'static Glyph` accessor:

- ASCII printables (U+0020..=U+007F) → existing
  `font_data::GLYPH_BITMAPS`.
- Latin-1 supplement visible range (U+00A1..=U+00FF) → 128-entry
  `GLYPH_TABLE_LATIN1`. C1 controls (U+0080..=U+009F) plus NBSP
  (U+00A0) render blank.
- Box-drawing block (U+2500..=U+257F) → 128-entry
  `GLYPH_TABLE_BOX_DRAWING`. Each glyph is generated from an edge
  spec `(north, east, south, west)` with light / heavy / double
  variants; arcs (U+256D..=U+2570), diagonals + X (U+2571..=U+2573),
  and half-line endpoints (U+2574..=U+257F) are handled by explicit
  case arms.
- Control characters (U+0000..=U+001F, U+007F, U+0080..=U+009F,
  U+00A0) → `BLANK_GLYPH`.
- Everything else → `FALLBACK_DOT_GLYPH`, a 2×2 inked block at the
  cell centre.

`BasicBitmapFont::glyph_or_fallback(codepoint) -> &'static Glyph` is
the visible-placeholder accessor — renderers that want every
codepoint to paint (including CJK, where no table ships in Phase 69b)
call this method and get the centred-dot fallback for uncovered
ranges.

`EditBuffer::erase_one_codepoint(iutf8: bool)` walks back from the
buffer end. With `iutf8 == true`, it consumes UTF-8 continuation
bytes (`10xxxxxx`) up to and including the leading byte; with
`iutf8 == false`, it removes exactly one byte. The walk is capped at
four bytes (the UTF-8 maximum length) so a malformed all-continuation
stream cannot run away across the buffer. The canonical-mode ldisc
hot path (`LineDiscipline::process_byte`, VERASE branch) calls
`erase_one_codepoint(self.termios.c_iflag & IUTF8 != 0)`.

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/utf8.rs` | New — `Utf8Decoder` + `DecoderOutput`; strict UTF-8 state machine with W3C resync. |
| `kernel-core/src/fb.rs` | Extended — `ConsoleCmd::PutChar(u32)` (widened from `char`). |
| `kernel-core/src/session/glyph_tables.rs` | New — `GLYPH_TABLE_LATIN1`, `GLYPH_TABLE_BOX_DRAWING`, `resolve_glyph`, `FALLBACK_DOT_GLYPH`, `BLANK_GLYPH`, `width_of`. |
| `kernel-core/src/session/font.rs` | Extended — `BasicBitmapFont::glyph` now dispatches through `resolve_glyph`; new `glyph_or_fallback` accessor for the visible-placeholder path. |
| `kernel-core/src/tty.rs` | Extended — `EditBuffer::erase_one_codepoint(iutf8)`; canonical ldisc VERASE calls it. |
| `userspace/term/src/screen.rs` | Extended — `Utf8Decoder` field, `feed` byte-stream path, `Cell::wide_continuation`, wide-cell accounting in `put_char`, `REPLACEMENT_CHARACTER` constant. |
| `kernel/src/fb/mod.rs`, `userspace/console_server/src/main.rs` | Extended — `put_visible_char` / `render_char_at` codepoint argument widened to `u32`. |
| `userspace/tui-smoke/src/main.rs` | Extended — new `cmd_utf8` subcommand drives the byte-stream → cell-state → glyph-bitmap chain. |
| `xtask/src/main.rs` | Extended — `TUI_SMOKE_SUBCOMMANDS` matrix now includes `utf8`. |
| `docs/appendix/term-escape-sequences.md` | Extended — new "UTF-8 input" and "Glyph coverage" sections. |

## How This Phase Differs From Earlier Terminal Work

- **Phase 22b** introduced the `AnsiParser` + `ConsoleCmd` IR with a
  `PutChar(char)` payload. The `char` cap forced every renderer to
  treat input as `u32` internally anyway (cells already store
  `u32`); Phase 69b widens the payload so the parser stops being the
  lossy choke-point.
- **Phase 57** brought up the `term` graphical terminal emulator
  with a `Screen::feed` that did `byte as char`. That worked for
  ASCII but silently produced Latin-1 codepoints for high-bit input.
  Phase 69b inserts the UTF-8 decoder between the byte stream and
  the parser; ASCII escape sequences are unaffected because every
  escape byte is < 0x80 and decodes in one step.
- **Phase 69** introduced the `Screen::feed` extension point for
  alternate-screen, bracketed-paste, mouse, and DECSCUSR routing.
  The Phase 69b decoder slots into that same extension point —
  every byte still passes through `feed` first, the parser still
  sees codepoints, and the only new state is the decoder field on
  `Screen`.
- **Phase 69a** added the `IUTF8` termios bit to the line discipline
  but only round-tripped the value through `tcgetattr` / `tcsetattr`.
  Phase 69b gives the bit its first behavioural effect: VERASE
  removes one whole codepoint per press when `IUTF8` is set.

## Closure of Related Phases

- **Phase 22b — ANSI Escape Parser**: the "Unicode beyond ASCII"
  deferral is closed for `term` (Latin-1 + box-drawing coverage via
  `resolve_glyph`; everything else falls through to the centred-dot
  fallback). The `ConsoleCmd::PutChar` payload is widened from `char`
  to `u32` so the parser carries the full Unicode scalar through.
- **Phase 57 — Audio and Local Session**: the `Screen::feed`
  "byte-cast-to-char" entry point is upgraded to a UTF-8 decoder +
  parser pipeline. BEL (`0x07`) interception still fires before the
  decoder is consulted so Phase 57's audio-bell mapping is exactly
  preserved.
- **Phase 69 — Terminal Contract Foundations**: the `Screen::feed`
  extension point introduced for alt-screen now also hosts the UTF-8
  decoder. The Phase 69 deferral "UTF-8 wire decoding" is closed.
- **Phase 69a — Terminal Termios**: `IUTF8` gains its first
  behavioural effect via `EditBuffer::erase_one_codepoint(true)`.

## Related Roadmap Docs

- [Phase 69b roadmap doc](./roadmap/69b-terminal-utf8-and-glyphs.md)
- [Phase 69b task doc](./roadmap/tasks/69b-terminal-utf8-and-glyphs-tasks.md)

## Deferred or Later-Phase Topics

- **TTF / OTF font loader + Nerd Font asset embedding** → Phase 69c.
  Adding a glyph atlas, font loader, and the Nerd Font private-use-area
  codepoints (U+E000..=U+F8FF, U+F0000+) requires runtime font
  infrastructure the bitmap-only Phase 69b deliberately avoids.
- **CJK glyph tables**. The EAW accounting machinery (`width_of` +
  `Cell::wide_continuation`) is in place so future phases that ship
  the tables only need to extend the resolver; landing the tables
  themselves is a pure data change.
- **Unicode normalisation** (NFC / NFD) — `term` does not currently
  transform input.
- **Combining-character handling beyond a single base glyph** — the
  cell grid carries one codepoint per cell. Combining sequences like
  `e` + U+0301 paint as two separate cells.
- **Bi-directional text (BiDi)** — out of scope until a future
  display-server phase ships paragraph-level text shaping.
- **Variation selectors and emoji ZWJ sequences** — same dependency
  as BiDi; these require the full text-shaping path.
