# Phase 22b — ANSI Parser Enhancement

**Status:** Complete
**Source Ref:** phase-22b
**Depends on:** Phase 22 (TTY and Terminal Control) ✅, Phase 21 (Ion Shell Integration) ✅, Phase 9 (Framebuffer and Shell) ✅
**Builds on:** Extends Phase 9's `FbConsole` (which only handled `\n` and `\x08`) and Phase 22's TTY layer (which gave userspace cooked/raw termios switching) by adding a host-testable VT100/ANSI escape-sequence parser. Without this phase the framebuffer rendered Ion's `liner` redraw control bytes as garbage glyphs.
**Primary Components:** `kernel-core/src/fb.rs`, `kernel/src/fb/mod.rs`

## Milestone Goal

Make the framebuffer console behave like a real VT100-compatible terminal so the Ion shell's `liner` library can redraw its prompt in place on every keystroke. After this phase, `ESC [ ... ` sequences for cursor movement, line/screen erase, cursor visibility, and 8+8 VGA SGR colors all execute correctly, and the parser is small enough and pure enough that 17 unit tests run on the host with `cargo test -p kernel-core` — no QEMU required.

## Why This Phase Exists

Phase 22 fixed the input side of the terminal: `tcgetattr` / `tcsetattr` work, raw mode delivers each keystroke immediately, signals are generated for `^C` / `^Z`. But the *output* side was still the Phase 9 framebuffer console, which understood only `\n` and `\x08`. As soon as Ion's `liner` switched to raw mode and started emitting redraw sequences (`ESC [ 2 K`, `\r`, `ESC [ ? 25 l`, `ESC [ n D`), the console printed the literal ESC byte and the bracketed payload as visible characters. Each keystroke appended a new corrupted prompt instead of overwriting the previous one.

A purely additive fix is needed because the Phase 22 line discipline lives in userspace and writes pre-rendered bytes through `sys_linux_write` — the kernel cannot intercept the redraw at a higher level. The framebuffer console itself must learn to consume CSI sequences. Putting the parser in `kernel-core` keeps the logic testable on the host and matches the existing pattern where `kernel-core/src/tty.rs` holds termios/edit-buffer logic and the `kernel/` crate holds the runtime state.

## Learning Goals

- How a CSI (Control Sequence Introducer) parser is structured as a small four-state machine (`Normal`, `Escape`, `Csi`, `CsiPrivate`) driven one byte at a time.
- Why the parser is split between `kernel-core` (pure logic, host-testable) and `kernel` (real framebuffer execution) — the same separation pattern used for termios and the line discipline.
- How a `ConsoleCmd` IR (`PutChar`, `CursorPosition`, `EraseLine`, `Sgr(SgrParams)`, `Nop`) decouples the parser from the renderer and avoids allocations by using inline arrays.
- How VT100 default-parameter conventions ("zero means 1 for movement, 0 for erase") are encoded with a single `param(idx, default)` helper.
- Why malformed sequences must produce `Nop` and return to `Normal` rather than panicking or sticking — a stuck parser would suppress all subsequent visible output.
- Why a `spin::Mutex` around the framebuffer console means `write_str` must never be called from an interrupt handler.

## Feature Scope

### Control character handling

`FbConsole` recognizes `\r` (carriage return → `cursor_col = 0` without row change), `\t` (tab → advance to the next 8-column boundary `(col + 8) & !7`, wrapping with scroll if necessary), and `\x1B` (ESC → swallowed silently, transitions the parser to `Escape`). The earlier Phase 9 console only recognized `\n` and `\x08`.

### CSI state machine

A four-state parser in `kernel-core/src/fb.rs` accumulates up to 8 decimal parameters separated by `;`, supports the DEC private intermediate `?`, and dispatches on a final byte in `0x40`–`0x7E`. Saturating arithmetic on parameter accumulation prevents overflow on pathological inputs like `ESC [ 99999999999 A`.

### Cursor movement (CUU / CUD / CUF / CUB / CHA / CUP)

All six positioning commands clamp at the edges of the character grid. Relative moves use `saturating_sub` and `cmp::min`; absolute moves convert from VT100 1-based indices to 0-based internal indices with a saturating subtract that handles `ESC [ 0 ; 0 H` cleanly.

### Erase commands (EL / ED / DECTCEM)

`ESC [ n K` erases line regions (modes 0/1/2) and `ESC [ n J` erases display regions (modes 0/1/2) without moving the cursor. `ESC [ ? 25 h` and `ESC [ ? 25 l` toggle a stored `cursor_visible` flag. Erase work is delegated to `clear_region` which paints whole 8x16 character cells with the current background colour.

### SGR / 8+8 VGA colour palette

`ESC [ n ; ... m` supports parameter `0` (reset), `1` (bold/bright — maps the current foreground to its bright variant if it is one of the 8 standard colours), `30`–`37` and `90`–`97` (foreground), `40`–`47` and `100`–`107` (background), and `39` / `49` (default fg/bg). 256-colour and truecolour parameters are recognized as `Sgr` but produce no visible effect — they are silently ignored.

## Important Components and How They Work

### `kernel-core/src/fb.rs` — `AnsiParser` and `ConsoleCmd`

The pure-logic parser. `AnsiParser::process_char(&mut self, c: char) -> ConsoleCmd` consumes one character, updates `state` / `params` / `param_count`, and returns a `ConsoleCmd`. Most calls inside an in-flight escape sequence return `ConsoleCmd::Nop`; the final byte returns the meaningful command. `ConsoleCmd` is `Copy` (made possible by `SgrParams` storing parameters in an inline `[u16; 8]` rather than a `Vec`), so it can be passed around by value without lifetime entanglement. The crate is `no_std` with `alloc` available; the unit tests use `Vec` from `alloc` to collect outputs but the parser itself does not allocate.

### `kernel-core/src/fb.rs` — dispatch helpers

`dispatch_csi(final_byte)` and `dispatch_csi_private(final_byte)` map the final byte (`A`/`B`/`C`/`D`/`G`/`H`/`J`/`K`/`m` for CSI; `h`/`l` for CSI private) to the appropriate `ConsoleCmd`. Unknown final bytes return `ConsoleCmd::Nop`. The helper `param(idx, default)` implements the VT100 "zero means default" convention so callers do not duplicate the check.

### `kernel/src/fb/mod.rs` — `FbConsole::execute_cmd`

`FbConsole` embeds an `AnsiParser` by value (not heap-allocated). `FbConsole::write_str` iterates over the input string's chars, calls `parser.process_char(c)`, and immediately passes the returned `ConsoleCmd` to `execute_cmd`. `execute_cmd` is the bridge between the parser IR and the actual framebuffer: it updates `cursor_row` / `cursor_col` for movement commands, calls `clear_region` for erase commands, calls `apply_sgr(&SgrParams)` to update `fg_color` / `bg_color`, and calls `put_visible_char` / `render_char_at` for `PutChar`.

### `kernel/src/fb/mod.rs` — `apply_sgr` and the VGA palette

`apply_sgr` walks the SGR parameter list left to right, mutating `fg_color` and `bg_color` in place. Two static palettes (`VGA_COLORS[8]` and `VGA_BRIGHT_COLORS[8]`) hold the standard 8 and bright 8 VGA RGB triples; SGR 1 (bold) maps the current foreground to its bright sibling by index lookup, so `ESC [ 1 ; 1 m` is idempotent.

### `kernel/src/fb/mod.rs` — global `write_str` and the spin lock

`pub fn fb::write_str(s: &str)` acquires `CONSOLE: spin::Mutex<Option<FbConsole>>` and forwards to `FbConsole::write_str`. Because the parser state lives inside `FbConsole`, an escape sequence split across two `write_str` calls (which `core::fmt::Write` may produce) resumes correctly on the next call. The mutex makes concurrent kernel-task writes safe but means `write_str` must never be called from an interrupt handler — it would spin-deadlock if the interrupted code held the lock.

## How This Builds on Earlier Phases

- Extends Phase 9's `FbConsole` (`kernel/src/fb/mod.rs`) by replacing the simple `put_char` switch with a parser-driven `process_char` → `execute_cmd` pipeline; the original `\n` and `\x08` paths are preserved verbatim through `ConsoleCmd::Newline` and `ConsoleCmd::Backspace`.
- Reuses Phase 22's `kernel-core` testability split (`kernel-core/src/tty.rs` for termios logic, now `kernel-core/src/fb.rs` for parser logic), so the rule "pure logic in `kernel-core`, hardware in `kernel`" is consistent across the TTY stack.
- Unblocks the `liner` line editor introduced in Phase 21 — Ion's interactive prompt would `\r` + redraw on every keystroke, but Phase 9 ignored `\r`. After Phase 22b the redraw is visually correct.
- Uses the same 8x16 IBM VGA bitmap font and pixel-format-aware `write_pixel` introduced in Phase 9; no new font work.

## Implementation Outline

1. Add `kernel-core/src/fb.rs`. Define `ConsoleCmd`, `SgrParams`, `EscState`, and `AnsiParser`. Wire `pub mod fb;` in `kernel-core/src/lib.rs`.
2. Implement `AnsiParser::process_char` as a four-arm match on `state`. Handle `Normal` first (printable → `PutChar`, control chars → matched `ConsoleCmd`, `\x1B` → transition to `Escape`). Then `Escape` (`[` → `Csi`, anything else → reset to `Normal` and return `Nop`). Then `Csi` and `CsiPrivate` with shared digit/`;` accumulation and a final-byte dispatch table.
3. Add the 17 unit tests in `kernel-core/src/fb.rs` covering printables, control characters, CSI sequences, default parameters, malformed escape recovery, and post-dispatch state.
4. In `kernel/src/fb/mod.rs`, embed an `AnsiParser` field in `FbConsole`. Replace the existing `put_char` body with the new `write_str` loop that forwards each character through `parser.process_char` and `execute_cmd`.
5. Implement `execute_cmd` arm by arm. Cursor movement uses saturating arithmetic and `cmp::min`. Erase uses a new `clear_region(col_start, row_start, col_end, row_end)` helper that fills each cell pixel-by-pixel with the current background colour.
6. Add `fg_color` / `bg_color` / `cursor_visible` fields to `FbConsole`. Replace the hardcoded `FG`/`BG` constants in `render_char_at` with `self.fg_color` / `self.bg_color`. Implement `apply_sgr` with the VGA palette and bold/bright mapping.
7. Run `cargo xtask check` and `cargo test -p kernel-core` until all 17 host tests pass and clippy/fmt are clean. Smoke-test in QEMU that Ion's prompt redraws in place.

## Acceptance Criteria

- `cargo test -p kernel-core` passes all 17 parser unit tests including `test_malformed_escape_recovery`, `test_unknown_csi_sequence`, and `test_state_after_sequence`.
- `cargo xtask check` passes (clippy `-D warnings`, rustfmt, and host tests).
- In QEMU with Ion as the boot shell, the prompt redraws in place on every keystroke — typing characters does not produce stacked, garbled prompt copies.
- A program that emits `ESC [ 2 J ESC [ H` clears the screen and homes the cursor without leaving raw bytes on the framebuffer.
- An SGR sequence such as `ESC [ 31 m hello ESC [ 0 m` renders `hello` in red and resets to default foreground/background afterwards.
- A malformed sequence like `ESC X A` discards `ESC X` and renders `A` normally — the parser does not get stuck in `Escape`.
- The Phase 22 cooked-mode line discipline (`^H` / `^U` / `^W` / `^C` / `^D`) and sh0 fallback continue to work without regression.

## Companion Task List

- [Phase 22b Task List](./tasks/22b-ansi-escape-tasks.md)

The legacy aligned learning doc [`docs/22b-ansi-escape.md`](../22b-ansi-escape.md) walks through the parser, dispatch tables, VGA palette, and limitations in tutorial form.

## How Real OS Implementations Differ

- Linux's `vt` console driver implements full ECMA-48 plus a large set of extensions: scroll regions (DECSTBM), tab-stop manipulation (`ESC H`, `CSI g`), cursor save/restore (DECSC/DECRC), reverse linefeed (`ESC M`), character set selection (G0/G1/G2/G3 with `SI`/`SO`), and dozens of additional DEC private modes. m3OS's parser handles roughly a dozen sequences.
- Real terminal emulators (xterm, alacritty, kitty) parse 24-bit truecolour SGR (`ESC [ 38 ; 2 ; r ; g ; b m`) and 256-colour palette SGR (`ESC [ 38 ; 5 ; n m`); m3OS recognizes the parameter shape but ignores the colour values and falls back to the current foreground.
- Production terminals also parse OSC sequences (`ESC ] ... BEL` for window titles, hyperlinks), DCS (Device Control Strings), and SS3-style function key encodings; m3OS handles only CSI.
- Linux's parser runs in ring 0 inside `drivers/tty/vt/vt.c` with no separate "core" crate; m3OS keeps the parser in `kernel-core` specifically so `cargo test -p kernel-core` can validate it on the host without booting QEMU.

## Deferred Until Later

- 24-bit and 256-colour SGR (`38 ; 2 ; r ; g ; b` and `38 ; 5 ; n`) — *closed in Phase 69 (`SgrParams::ops` yields `FgIndexed` / `BgIndexed` / `FgRgb` / `BgRgb` variants; the kernel framebuffer console still ignores them while `userspace/term/src/screen.rs` resolves them via `XTERM_256_PALETTE` + `color_to_bgra`).*
- DEC private mode set/reset for any code other than `?25` (DECTCEM) — *closed in Phase 69 (`ConsoleCmd::DecPrivateMode { code, set }` covers `?1049`, `?47`, `?9`, `?1000`, `?1006`, `?2004`; consumers that do not recognise a code drop it silently).*
- DECSCUSR cursor shape (`CSI <n> SP q`) — *closed in Phase 69 (`ConsoleCmd::CursorShape { shape }` for `n` ∈ 0..=6; the parser exits the new `CsiIntermediate` state on the final byte).*
- SGR underline (4), italic (3), inverse (7), strikethrough (9), and their reset variants 21–29. The 8x16 bitmap font has no variant glyphs.
- Visible cursor block rendering. `cursor_visible` is stored but `execute_cmd` does not yet draw a block at the current position; Ion hides the cursor during redraws so the missing render is invisible in normal use.
- Scroll regions (`ESC [ r ; s r`, DECSTBM) — the scroll region is always the full screen.
- Reverse index `ESC M` and other non-`[` escape sequences — currently silently discarded by the `Escape`-state default arm.
- Cursor position report (`ESC [ 6 n`) and other terminal-response sequences — Ion does not query the terminal so no input path exists for sending the response back.
- Customizable tab stops (`ESC H` set, `CSI g` clear) — tab stops are fixed at every 8 columns.
- OSC and DCS sequences (window titles, hyperlinks, sixel) — out of scope for a bitmap kernel console.
- Unicode beyond ASCII printable range — characters outside `0x20`–`0x7E` render as a filled-block placeholder because the IBM CP437 font is shipped as ASCII only.
