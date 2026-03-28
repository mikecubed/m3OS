# Phase 22b — ANSI Escape Sequence Support: Task List

**Depends on:** Phase 22 (TTY and Terminal Control) + Ion Interactive Fixes (PR #32)
**Goal:** Implement minimal VT100/ANSI escape sequence processing in the
framebuffer console so Ion's `liner` library can redraw prompts in-place and
cursor-addressed programs produce readable output. This is the display
counterpart to Phase 22's TTY/termios work.

## Problem Statement

Ion's `liner` library puts the terminal in raw mode and redraws the prompt on
every keystroke using ANSI escape sequences. The framebuffer console currently
ignores these sequences, causing each redraw to append rather than overwrite
in place. The console also lacks carriage return (`\r`) support, so
cursor-to-column-0 operations fail silently.

## Current State (post-Phase 22 + PR #32)

- **`FbConsole`** (`kernel/src/fb/mod.rs:255–271`): struct with `buf`,
  `width`, `height`, `stride`, `bytes_per_pixel`, `pixel_format`,
  `cursor_col`, `cursor_row`
- **`put_char()`** (line 405): handles `\n` (newline) and `\x08` (backspace)
  only — no `\r`, no `\t`, no escape sequences
- **`write_str()`** (line 451): simple char-by-char loop calling `put_char()`
- **Colors**: hardcoded white-on-black (`FG`/`BG` constants, lines 243–252)
- **Font**: IBM VGA 8x16, ASCII 0x20–0x7E (95 glyphs)
- **Scroll**: `scroll_up()` copies framebuffer memory up by one text row
- **Output path**: `sys_linux_write()` → `crate::fb::write_str(s)` for
  `DeviceTTY`/`Stdout` backends
- **No parsing state** for multi-byte escape sequences

## Minimum Viable Subset

These are the escape sequences `liner`/`termion` actually emit during normal
line editing. Supporting just these makes Ion's prompt usable:

| Sequence | Name | Meaning |
|----------|------|---------|
| `\r` (0x0D) | CR | Carriage return — move cursor to column 0 |
| `\x1b[K` | EL (0) | Erase from cursor to end of line |
| `\x1b[nG` | CHA | Cursor to column n (1-based) |
| `\x1b[nA` | CUU | Cursor up n rows |
| `\x1b[nB` | CUD | Cursor down n rows |
| `\x1b[nC` | CUF | Cursor forward n columns |
| `\x1b[nD` | CUB | Cursor back n columns |
| `\x1b[H` / `\x1b[n;mH` | CUP | Cursor position (row n, col m) |
| `\x1b[J` | ED (0) | Erase from cursor to end of screen |
| `\x1b[2J` | ED (2) | Erase entire screen |
| `\x1b[?25l` | DECTCEM | Hide cursor (no-op for now) |
| `\x1b[?25h` | DECTCEM | Show cursor (no-op for now) |
| `\x1b[m` / `\x1b[0m` | SGR reset | Reset attributes (no-op initially) |
| `\x1b[n;...m` | SGR | Set graphic rendition (color support) |

## Track Layout

| Track | Scope | Dependencies |
|---|---|---|
| A | Control characters (`\r`, `\t`) | — |
| B | Escape sequence parser state machine | — |
| C | Cursor movement sequences | A, B |
| D | Erase sequences | B, C |
| E | SGR / color support (stretch) | B |
| F | Validation and cleanup | All |

---

## Track A — Missing Control Characters

Add basic control character handling that the console currently lacks.

| Task | Description | Status |
|---|---|---|
| P22b-T001 | Handle `\r` (0x0D) in `put_char()`: set `cursor_col = 0` without advancing row | Done |
| P22b-T002 | Handle `\t` (0x09) in `put_char()`: advance `cursor_col` to next 8-column tab stop; wrap if past last column | Done |
| P22b-T003 | Handle `\x1b` (ESC, 0x1B) in `put_char()`: do not render as visible glyph — route to escape parser (Track B) | Done |
| P22b-T004 | `cargo xtask check` passes; sh0 and Ion still boot | Done |

## Track B — Escape Sequence Parser State Machine

Add a CSI (Control Sequence Introducer) parser to `FbConsole`. The parser
needs to handle the `ESC [ <params> <final>` pattern where params are
semicolon-separated decimal numbers.

| Task | Description | Status |
|---|---|---|
| P22b-T005 | Add `EscState` enum to `FbConsole`: `Normal`, `Escape` (saw ESC), `Csi` (saw ESC [), `CsiPrivate` (saw ESC [ ?) | Done |
| P22b-T006 | Add parser fields to `FbConsole`: `esc_state: EscState`, `esc_params: [u16; 8]`, `esc_param_count: usize`, `esc_private: bool` | Done |
| P22b-T007 | Refactor `write_str()` / `put_char()`: route characters through `process_char()` which dispatches based on `esc_state` | Done |
| P22b-T008 | Implement `Normal` state: printable chars → render; `\x1b` → transition to `Escape`; control chars (`\r`, `\n`, `\x08`, `\t`) handled directly | Done |
| P22b-T009 | Implement `Escape` state: `[` → transition to `Csi`; any other char → discard and return to `Normal` (unknown escape) | Done |
| P22b-T010 | Implement `Csi` state: digit chars accumulate into current param; `;` advances to next param; `?` sets private flag and transitions to `CsiPrivate`; letter (0x40–0x7E) is the final byte → dispatch to handler and return to `Normal` | Done |
| P22b-T011 | Add `dispatch_csi()` method: match on final byte and delegate to Track C/D/E handlers; unknown sequences silently ignored | Done |
| P22b-T012 | Add `kernel-core` unit tests for parser: verify state transitions for `\x1b[2J`, `\x1b[10;20H`, `\x1b[?25l`, `\x1b[m`, malformed sequences | Done |
| P22b-T013 | `cargo xtask check` passes | Done |

## Track C — Cursor Movement Sequences

Implement the CSI sequences that move the cursor.

| Task | Description | Status |
|---|---|---|
| P22b-T014 | Implement `CUU` (`\x1b[nA`): move cursor up n rows (default n=1); clamp at row 0 | Done |
| P22b-T015 | Implement `CUD` (`\x1b[nB`): move cursor down n rows (default n=1); clamp at last row | Done |
| P22b-T016 | Implement `CUF` (`\x1b[nC`): move cursor forward n columns (default n=1); clamp at last column | Done |
| P22b-T017 | Implement `CUB` (`\x1b[nD`): move cursor back n columns (default n=1); clamp at column 0 | Done |
| P22b-T018 | Implement `CHA` (`\x1b[nG`): move cursor to absolute column n (1-based, default 1); clamp to valid range | Done |
| P22b-T019 | Implement `CUP` (`\x1b[n;mH`): move cursor to row n, column m (both 1-based, default 1;1); clamp to valid range | Done |
| P22b-T020 | Add `kernel-core` unit tests: verify cursor position after each movement sequence, clamping at boundaries | Done |
| P22b-T021 | `cargo xtask check` passes | Done |

## Track D — Erase Sequences

Implement the CSI sequences that clear portions of the screen.

| Task | Description | Status |
|---|---|---|
| P22b-T022 | Add `clear_region()` helper to `FbConsole`: fill a rectangular region of character cells with spaces (background color) | Done |
| P22b-T023 | Implement `EL` (`\x1b[K` / `\x1b[0K`): erase from cursor to end of current line | Done |
| P22b-T024 | Implement `EL` (`\x1b[1K`): erase from start of line to cursor | Done |
| P22b-T025 | Implement `EL` (`\x1b[2K`): erase entire current line | Done |
| P22b-T026 | Implement `ED` (`\x1b[J` / `\x1b[0J`): erase from cursor to end of screen | Done |
| P22b-T027 | Implement `ED` (`\x1b[2J`): erase entire screen (clear all cells, do NOT reset cursor — programs often follow with `\x1b[H`) | Done |
| P22b-T028 | Implement `DECTCEM` hide/show cursor (`\x1b[?25l` / `\x1b[?25h`): store `cursor_visible: bool` in `FbConsole`; actual cursor rendering is a stretch goal — for now just track the flag | Done |
| P22b-T029 | Add `kernel-core` unit tests for erase operations (verify cursor position unchanged after erase) | Done |
| P22b-T030 | `cargo xtask check` passes | Done |

## Track E — SGR / Color Support (Stretch)

Implement Select Graphic Rendition for basic color output. This is a stretch
goal — Ion's prompt works without color, but `ls --color` and colored error
messages benefit from it.

| Task | Description | Status |
|---|---|---|
| P22b-T031 | Add `fg_color: Colour` and `bg_color: Colour` fields to `FbConsole`; initialize to white-on-black | Done |
| P22b-T032 | Update `render_char_at()` to use `self.fg_color` / `self.bg_color` instead of hardcoded `FG` / `BG` | Done |
| P22b-T033 | Implement SGR reset (`\x1b[0m`): restore `fg_color` and `bg_color` to defaults | Done |
| P22b-T034 | Implement SGR 30–37 (standard foreground colors): map to VGA color palette | Done |
| P22b-T035 | Implement SGR 40–47 (standard background colors): map to VGA color palette | Done |
| P22b-T036 | Implement SGR 1 (bold/bright): use bright color variants for foreground (90–97 equivalent) | Done |
| P22b-T037 | Implement SGR 39 (default foreground) and SGR 49 (default background): reset to white / black | Done |
| P22b-T038 | Define VGA color palette: 8 standard + 8 bright colors matching xterm defaults | Done |
| P22b-T039 | `cargo xtask check` passes | Done |

## Track F — Validation and Cleanup

| Task | Description | Status |
|---|---|---|
| P22b-T040 | Acceptance: Ion prompt redraws in-place on each keystroke (no appending/garbling) | Deferred (manual QEMU visual test) |
| P22b-T041 | Acceptance: Ion prompt appears at correct position after running a command | Deferred (manual QEMU visual test) |
| P22b-T042 | Acceptance: `echo -e '\x1b[2J\x1b[H'` clears the screen (if Ion supports echo -e, or test via C program) | Deferred (manual QEMU visual test) |
| P22b-T043 | Acceptance: backspace in Ion erases the character and cursor moves back visually | Deferred (manual QEMU visual test) |
| P22b-T044 | Acceptance: long command lines wrap correctly and can be edited | Deferred (manual QEMU visual test) |
| P22b-T045 | Acceptance: external command output (`ls`, `cat`) displays correctly after Ion prompt | Deferred (manual QEMU visual test) |
| P22b-T046 | Acceptance: sh0 still works correctly (cooked mode echo + line discipline unaffected) | Deferred (manual QEMU visual test) |
| P22b-T047 | Acceptance: `cargo xtask check` passes (clippy + fmt + host tests) | Done |
| P22b-T048 | Acceptance: QEMU boot — no panics, no regressions from Phase 22 | Done |
| P22b-T049 | Remove `docs/roadmap/tasks/22-ion-interactive-followup.md` — all issues resolved by PR #32 | Done |
| P22b-T050 | Update `docs/08-roadmap.md`: mark ANSI escape support as completed | Done |

---

## Prerequisite Analysis

### Files to Modify

| File | Changes |
|---|---|
| `kernel/src/fb/mod.rs` | Escape parser state machine, cursor movement, erase, color — main implementation target |
| `kernel-core/src/fb.rs` (new) | Pure-logic escape parser and tests (host-testable without framebuffer) |
| `kernel-core/src/lib.rs` | Add `pub mod fb;` for the new module |

### Design Decision: Parser Location

The escape sequence parser should live in `kernel-core` so it can be unit-tested
on the host. The `kernel-core` parser produces commands (MoveCursor, Erase,
SetColor, PutChar) that `kernel/src/fb/mod.rs` executes against the real
framebuffer. This follows the existing pattern where `kernel-core/src/tty.rs`
holds the `Termios`/`EditBuffer` logic and `kernel/src/tty.rs` holds the
runtime state.

---

## Parallelization Strategy

**Wave 1:** Tracks A and B in parallel — control characters are independent
from the parser state machine.

**Wave 2 (after A + B):** Tracks C and D in parallel — cursor movement and
erase operations both need the parser but are independent of each other.

**Wave 3 (after C + D):** Track E (stretch) — color support builds on the
parser and rendering infrastructure.

**Wave 4:** Track F — validation once everything is wired up.

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Parser mishandles malformed sequences | Medium | Low | Silently discard unknown sequences; return to Normal state on any unrecognized byte |
| Performance regression from per-char state machine | Low | Low | State check is a single match on an enum; no allocations |
| liner emits sequences we don't handle | Medium | Medium | Start with the minimum viable subset; add sequences as discovered |
| Color rendering breaks on non-RGB pixel formats | Low | Medium | Test with BGR and greyscale formats; color palette works through existing `write_pixel()` |
| Parser logic hard to test without framebuffer | Medium | High | Put parser in `kernel-core` for host testing; framebuffer only executes parsed commands |

---

## Related

- [Phase 22 Design Doc](../22-tty-pty.md) — "Next: VT100 / ANSI Escape Sequence Processing" section
- [Phase 22 Task List](22-tty-pty-tasks.md) — deferred items list includes ANSI support
- [Ion Interactive Followup](22-ion-interactive-followup.md) — resolved by PR #32
