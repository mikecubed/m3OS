# Phase 22b: ANSI Escape Sequence Processing

**Aligned Roadmap Phase:** Phase 22b
**Status:** Complete
**Source Ref:** phase-22b

This document describes the ANSI/VT100 escape sequence subsystem in m3OS:
why it exists, how the parser is architected across two crates, the state
machine internals, and how the framebuffer console executes the resulting
commands.

## Why ANSI Escape Sequences Are Needed

m3OS runs the Ion shell, which uses the `liner` library for interactive line
editing. `liner` does not write characters one at a time and hope for the
best -- it redraws the prompt in-place. Every time the user presses a key,
`liner` may emit a sequence such as:

1. `ESC [ 2K` (erase the entire current line)
2. `\r` (carriage return to column 0)
3. The full prompt string and current input buffer
4. `ESC [ ? 25 l` / `ESC [ ? 25 h` (hide/show cursor around the redraw)
5. `ESC [ <n> D` (cursor back by n columns to place caret at insert point)

Without escape sequence support, the framebuffer console would display the
raw `\x1b[2K` bytes as garbage characters, making the shell unusable. Phase
22b adds a complete VT100/ANSI subset sufficient for Ion's `liner` to work
correctly.

## Parser Architecture: Testability Split

The parser lives in `kernel-core/src/fb.rs` rather than in the `kernel`
crate. This split is fundamental to m3OS's testing strategy.

```
kernel-core/src/fb.rs          kernel/src/fb/mod.rs
+---------------------+         +-----------------------+
|  AnsiParser         |  <---   |  FbConsole            |
|  ConsoleCmd enum    |  used   |  execute_cmd()        |
|  SgrParams          |   by    |  apply_sgr()          |
|  EscState           |         |  clear_region()       |
|                     |         |  VGA color palette    |
|  Unit tests (17)    |         |  pixel renderer       |
+---------------------+         +-----------------------+
       |                                  |
  cargo test -p kernel-core          requires QEMU
  (runs on host, no QEMU needed)     (no_std, bare metal)
```

`kernel-core` is a `no_std` crate with `alloc` available. It contains pure
logic with no hardware dependencies. `cargo test -p kernel-core` runs all 17
parser unit tests directly on the host without launching QEMU. The kernel
crate imports the parser and feeds the resulting `ConsoleCmd` values to the
real framebuffer. This means parser bugs are caught by fast host tests rather
than slow QEMU runs.

## The State Machine

The parser is a four-state machine. Characters arrive one at a time via
`process_char()`. Each call consumes one character and returns a `ConsoleCmd`
to execute. The states are:

```
                    any non-'[' char
           +--------(discard, Nop)--------+
           |                              |
           v                              |
+--------+   ESC (0x1B)   +--------+     |
| Normal | ------------->  | Escape |-----+
+--------+                +--------+
    ^                          |
    |                          | '['
    |                   +------v------+
    |    final byte     |             |
    +----(0x40-0x7E)----+     Csi     |
    |    dispatch_csi() |             |
    |                   +------+------+
    |                          |
    |                          | '?'
    |                   +------v----------+
    |    final byte     |                 |
    +----(0x40-0x7E)----+   CsiPrivate    |
         dispatch_csi_  |                 |
         private()      +-----------------+
```

| State | Meaning |
|-------|---------|
| `Normal` | Text output mode. Printable characters produce `PutChar`, control characters produce their respective commands. |
| `Escape` | Saw `0x1B`. Waits for `[` to enter CSI mode. Any other character discards the escape and returns to Normal. |
| `Csi` | Inside `ESC [`. Accumulates decimal digit parameters separated by `;`. A byte in `0x40`--`0x7E` is the final byte and triggers dispatch. |
| `CsiPrivate` | Inside `ESC [ ?`. Identical digit/semicolon accumulation as `Csi`, but dispatches via `dispatch_csi_private()`. Used for DEC private sequences like DECTCEM. |

The `EscState` enum and `AnsiParser` struct are defined in `kernel-core`:

```rust
pub enum EscState {
    Normal,
    Escape,
    Csi,
    CsiPrivate,
}

pub struct AnsiParser {
    state: EscState,
    params: [u16; MAX_PARAMS],  // MAX_PARAMS = 8
    param_count: usize,
}
```

The parser type (`AnsiParser`) is `Clone` (but not `Copy`) and is stored
directly by value inside `FbConsole` (not heap-allocated). The command type
it produces, `ConsoleCmd`, *is* `Copy` thanks to using `SgrParams` with
inline storage, so parsed commands can be passed around by value cheaply:

```rust
struct FbConsole {
    // ...
    parser: AnsiParser,
    fg_color: Colour,
    bg_color: Colour,
    cursor_visible: bool,
}
```

## The ConsoleCmd Enum

`process_char()` returns a `ConsoleCmd` on every call. Most calls during an
escape sequence return `Nop` (the sequence is still being accumulated). The
final byte of a sequence returns the meaningful command.

```rust
pub enum ConsoleCmd {
    PutChar(char),
    CarriageReturn,
    Newline,
    Backspace,
    Tab,
    CursorUp(u16),
    CursorDown(u16),
    CursorForward(u16),
    CursorBack(u16),
    CursorHorizontalAbsolute(u16),
    CursorPosition(u16, u16),
    EraseLine(u16),
    EraseDisplay(u16),
    SetCursorVisible(bool),
    Sgr(SgrParams),
    Nop,
}
```

`ConsoleCmd` is `Copy`. This matters because `AnsiParser::process_char` takes
`&mut self` for state updates and returns the command by value -- the caller
(`FbConsole::write_str`) can immediately pass the returned value to
`execute_cmd` without any lifetime entanglement.

| Variant | VT100 sequence | Payload |
|---------|---------------|---------|
| `PutChar(c)` | (printable ASCII) | The character to render |
| `CarriageReturn` | `\r` | — |
| `Newline` | `\n` | — |
| `Backspace` | `\x08` | — |
| `Tab` | `\t` | — |
| `CursorUp(n)` | `ESC [ n A` (CUU) | Rows to move up |
| `CursorDown(n)` | `ESC [ n B` (CUD) | Rows to move down |
| `CursorForward(n)` | `ESC [ n C` (CUF) | Columns to move right |
| `CursorBack(n)` | `ESC [ n D` (CUB) | Columns to move left |
| `CursorHorizontalAbsolute(n)` | `ESC [ n G` (CHA) | 1-based column |
| `CursorPosition(row, col)` | `ESC [ r ; c H` (CUP) | Both 1-based |
| `EraseLine(mode)` | `ESC [ n K` (EL) | 0/1/2 |
| `EraseDisplay(mode)` | `ESC [ n J` (ED) | 0/1/2 |
| `SetCursorVisible(bool)` | `ESC [ ? 25 h/l` (DECTCEM) | `true`=show |
| `Sgr(SgrParams)` | `ESC [ n ; ... m` | Inline array of params |
| `Nop` | (in-progress or unknown) | — |

`SgrParams` uses inline storage to avoid allocation:

```rust
pub struct SgrParams {
    pub params: [u16; MAX_PARAMS],  // up to 8 SGR values
    pub count: usize,
}
```

This keeps `ConsoleCmd` `Copy` without requiring a heap allocation for every
`ESC [ 31;42m` sequence.

## CSI Parameter Parsing

When the parser is in `Csi` or `CsiPrivate` state, digit characters (`0`--`9`)
and semicolons (`;`) accumulate parameters into the inline array:

```rust
'0'..='9' => {
    if self.param_count == 0 {
        self.param_count = 1;
    }
    let idx = self.param_count - 1;
    if idx < MAX_PARAMS {
        self.params[idx] = self.params[idx]
            .saturating_mul(10)
            .saturating_add(c as u16 - b'0' as u16);
    }
    ConsoleCmd::Nop
}
';' => {
    if self.param_count == 0 {
        self.param_count = 1;   // First param was implicitly 0.
    }
    if self.param_count < MAX_PARAMS {
        self.param_count += 1;
    }
    ConsoleCmd::Nop
}
```

Saturating arithmetic prevents overflow on pathological inputs like
`ESC [ 99999999999A`. Each new digit is appended to the current parameter
slot. A semicolon advances to the next slot.

Parameter retrieval uses `param(idx, default)`:

```rust
fn param(&self, idx: usize, default: u16) -> u16 {
    if idx < self.param_count {
        let v = self.params[idx];
        if v == 0 { default } else { v }
    } else {
        default
    }
}
```

The "zero means default" convention is standard VT100 behavior: `ESC [ A`
and `ESC [ 0 A` and `ESC [ 1 A` are all treated as "cursor up 1". The
`default` argument encodes the implicit default for each command. For
movement commands the default is 1; for erase commands (`EL`, `ED`) the
default is 0 (cursor-to-end).

### Dispatch

When a byte in `0x40`--`0x7E` is seen, it is the "final byte" and
`dispatch_csi()` or `dispatch_csi_private()` is called:

```rust
fn dispatch_csi(&self, final_byte: char) -> ConsoleCmd {
    match final_byte {
        'A' => ConsoleCmd::CursorUp(self.param(0, 1)),
        'B' => ConsoleCmd::CursorDown(self.param(0, 1)),
        'C' => ConsoleCmd::CursorForward(self.param(0, 1)),
        'D' => ConsoleCmd::CursorBack(self.param(0, 1)),
        'G' => ConsoleCmd::CursorHorizontalAbsolute(self.param(0, 1)),
        'H' => ConsoleCmd::CursorPosition(self.param(0, 1), self.param(1, 1)),
        'J' => ConsoleCmd::EraseDisplay(self.param(0, 0)),
        'K' => ConsoleCmd::EraseLine(self.param(0, 0)),
        'm' => { /* SGR — see below */ }
        _ => ConsoleCmd::Nop,
    }
}
```

Unknown final bytes produce `Nop` -- the parser never panics or corrupts
state on unrecognized sequences.

## Control Characters

Control characters are handled in `process_normal()` before any escape
sequence logic:

| Byte | Escape | `ConsoleCmd` | `execute_cmd` behavior |
|------|--------|-------------|------------------------|
| `0x0D` | `\r` | `CarriageReturn` | `cursor_col = 0` |
| `0x0A` | `\n` | `Newline` | `cursor_col = 0; cursor_row += 1`; scroll if past last row |
| `0x08` | `\x08` | `Backspace` | `cursor_col -= 1` (wraps to previous row); render `' '` at new position |
| `0x09` | `\t` | `Tab` | Advance to next 8-column boundary: `(cursor_col + 8) & !7` |
| `0x1B` | ESC | `Nop` | Transitions to `Escape` state; consumed silently |

Tab alignment is computed with a bitmask: `(col + 8) & !7` rounds up to the
next multiple of 8. If the result exceeds the column count, the cursor wraps
to the next row (with scrolling if necessary).

Backspace wraps across line boundaries: if `cursor_col == 0` and
`cursor_row > 0`, it moves to the last column of the previous row and clears
that cell. If already at `(0, 0)`, it is a no-op.

## Cursor Movement Commands

All cursor movement in `execute_cmd()` uses clamping to keep the cursor
within the valid character grid.

### Relative Movement (CUU / CUD / CUF / CUB)

```rust
ConsoleCmd::CursorUp(n) => {
    self.cursor_row = self.cursor_row.saturating_sub(n as usize);
}
ConsoleCmd::CursorDown(n) => {
    self.cursor_row = core::cmp::min(self.cursor_row + n as usize, rows - 1);
}
ConsoleCmd::CursorForward(n) => {
    self.cursor_col = core::cmp::min(self.cursor_col + n as usize, cols - 1);
}
ConsoleCmd::CursorBack(n) => {
    self.cursor_col = self.cursor_col.saturating_sub(n as usize);
}
```

`saturating_sub` handles moving above row 0 or before column 0 -- the cursor
stops at the boundary rather than wrapping or underflowing. `cmp::min` handles
moving past the last row or column.

### Absolute Movement (CHA / CUP)

```rust
ConsoleCmd::CursorHorizontalAbsolute(n) => {
    let col = (n as usize).saturating_sub(1);   // convert 1-based to 0-based
    self.cursor_col = core::cmp::min(col, cols - 1);
}
ConsoleCmd::CursorPosition(row, col) => {
    let r = (row as usize).saturating_sub(1);
    let c = (col as usize).saturating_sub(1);
    self.cursor_row = core::cmp::min(r, rows - 1);
    self.cursor_col = core::cmp::min(c, cols - 1);
}
```

VT100 uses 1-based row and column indices. The kernel subtracts 1 to convert
to 0-based internal indices. `saturating_sub(1)` handles the edge case of a
0-valued parameter (which is nominally invalid per VT100 but occurs in
practice when code emits `ESC [ 0 ; 0 H`).

## Erase Sequences

### EL — Erase in Line (`ESC [ n K`)

| Mode (`n`) | Region erased |
|-----------|---------------|
| 0 (default) | Cursor position to end of line |
| 1 | Start of line to cursor position (inclusive) |
| 2 | Entire line |

```rust
ConsoleCmd::EraseLine(mode) => {
    match mode {
        0 => self.clear_region(self.cursor_col, self.cursor_row,
                               cols, self.cursor_row + 1),
        1 => self.clear_region(0, self.cursor_row,
                               self.cursor_col + 1, self.cursor_row + 1),
        2 => self.clear_region(0, self.cursor_row,
                               cols, self.cursor_row + 1),
        _ => {}
    }
}
```

The cursor does not move for any EL variant.

### ED — Erase in Display (`ESC [ n J`)

| Mode (`n`) | Region erased |
|-----------|---------------|
| 0 (default) | Cursor to end of screen |
| 1 | Start of screen to cursor |
| 2 | Entire screen (cursor stays put) |

Mode 0 erases in two passes: the partial line from cursor to end of current
row, then all rows below. Mode 1 does the same in reverse. Mode 2 is a single
`clear_region(0, 0, cols, rows)` call. The cursor position is never changed
by ED.

### `clear_region()`

```rust
fn clear_region(&mut self, col_start: usize, row_start: usize,
                col_end: usize, row_end: usize) {
    let bg = self.bg_color;
    for row in row_start..core::cmp::min(row_end, rows) {
        for col in col_start..core::cmp::min(col_end, cols) {
            let px_x = col * CHAR_W;
            let px_y = row * CHAR_H;
            for gy in 0..CHAR_H {
                for gx in 0..CHAR_W {
                    self.write_pixel(px_x + gx, px_y + gy, bg);
                }
            }
        }
    }
}
```

`clear_region` fills each character cell with the current background color
pixel by pixel, using the 8x16 glyph grid. Boundaries are clamped against
`rows` and `cols` so out-of-range arguments from mode calculations cannot
reach `write_pixel` with invalid coordinates.

## SGR Color Support

SGR (`ESC [ n ; ... m`, final byte `m`) sets foreground/background colors and
text attributes. Parameters are processed left to right:

```rust
fn apply_sgr(&mut self, sgr: &SgrParams) {
    for i in 0..sgr.count {
        match sgr.params[i] {
            0        => { self.fg_color = FG; self.bg_color = BG; }  // reset
            1        => {                                              // bold
                if let Some(idx) = VGA_COLORS.iter().position(|&c| c == self.fg_color) {
                    self.fg_color = VGA_BRIGHT_COLORS[idx];
                }
            }
            n @ 30..=37 => { self.fg_color = VGA_COLORS[(n - 30) as usize]; }
            39       => { self.fg_color = FG; }   // default fg
            n @ 40..=47 => { self.bg_color = VGA_COLORS[(n - 40) as usize]; }
            49       => { self.bg_color = BG; }   // default bg
            n @ 90..=97  => { self.fg_color = VGA_BRIGHT_COLORS[(n - 90) as usize]; }
            n @ 100..=107 => { self.bg_color = VGA_BRIGHT_COLORS[(n - 100) as usize]; }
            _ => {} // unknown SGR — ignored
        }
    }
}
```

### VGA Color Palette

The console implements the classic 8+8 VGA color palette. Standard colors
(SGR 30--37 for foreground, 40--47 for background):

| Index | Name | R | G | B |
|-------|------|---|---|---|
| 0 | Black | 0x00 | 0x00 | 0x00 |
| 1 | Red | 0xAA | 0x00 | 0x00 |
| 2 | Green | 0x00 | 0xAA | 0x00 |
| 3 | Yellow/Brown | 0xAA | 0x55 | 0x00 |
| 4 | Blue | 0x00 | 0x00 | 0xAA |
| 5 | Magenta | 0xAA | 0x00 | 0xAA |
| 6 | Cyan | 0x00 | 0xAA | 0xAA |
| 7 | White (light gray) | 0xAA | 0xAA | 0xAA |

Bright colors (SGR 90--97 for foreground, 100--107 for background):

| Index | Name | R | G | B |
|-------|------|---|---|---|
| 0 | Bright Black (dark gray) | 0x55 | 0x55 | 0x55 |
| 1 | Bright Red | 0xFF | 0x55 | 0x55 |
| 2 | Bright Green | 0x55 | 0xFF | 0x55 |
| 3 | Bright Yellow | 0xFF | 0xFF | 0x55 |
| 4 | Bright Blue | 0x55 | 0x55 | 0xFF |
| 5 | Bright Magenta | 0xFF | 0x55 | 0xFF |
| 6 | Bright Cyan | 0x55 | 0xFF | 0xFF |
| 7 | Bright White | 0xFF | 0xFF | 0xFF |

Default foreground is pure white (`0xFF, 0xFF, 0xFF`); default background is
pure black (`0x00, 0x00, 0x00`).

### Bold / Bright Attribute (SGR 1)

SGR 1 (bold) maps the current foreground to its bright variant if it is
one of the 8 standard VGA colors. The operation is idempotent -- applying
bold multiple times has no additional effect:

```rust
1 => {
    // Only map standard colors to their bright variants.
    // Already-bright or non-palette colors are left unchanged.
    if let Some(idx) = VGA_COLORS.iter().position(|&col| col == self.fg_color) {
        self.fg_color = VGA_BRIGHT_COLORS[idx];
    }
}
```

If the current foreground exactly matches one of the 8 standard VGA colors,
it is replaced with the corresponding bright palette entry. If the foreground
is already a bright color or a non-palette color, it is left unchanged. This
ensures that sequences like `\x1b[1;1;1m` behave identically to `\x1b[1m`.

Note that SGR bold does not affect background color, text weight, or any
other attribute -- the framebuffer font is a fixed 8x16 bitmap with no bold
variant. The only visible effect is the color change.

### SGR Reset

`ESC [ m` (no parameters) and `ESC [ 0 m` both reset all attributes. The
parser normalizes the no-parameter case:

```rust
'm' => {
    let count = if self.param_count == 0 { 1 } else { self.param_count };
    let mut sgr = SgrParams { params: [0; MAX_PARAMS], count };
    if self.param_count == 0 {
        sgr.params[0] = 0;   // treat as explicit SGR 0
    } else {
        sgr.params[..self.param_count]
            .copy_from_slice(&self.params[..self.param_count]);
    }
    ConsoleCmd::Sgr(sgr)
}
```

This ensures `apply_sgr` always sees at least one parameter (value 0) for a
bare `ESC [ m`, avoiding a `count == 0` loop that would silently do nothing
instead of resetting.

## DECTCEM Cursor Visibility

`ESC [ ? 25 h` shows the cursor; `ESC [ ? 25 l` hides it:

```rust
fn dispatch_csi_private(&self, final_byte: char) -> ConsoleCmd {
    match final_byte {
        'h' => {
            if self.param(0, 0) == 25 {
                ConsoleCmd::SetCursorVisible(true)
            } else {
                ConsoleCmd::Nop
            }
        }
        'l' => {
            if self.param(0, 0) == 25 {
                ConsoleCmd::SetCursorVisible(false)
            } else {
                ConsoleCmd::Nop
            }
        }
        _ => ConsoleCmd::Nop,
    }
}
```

Only parameter 25 (the cursor) is recognized. Other DEC private mode numbers
(`?1`, `?7`, `?47`, etc.) produce `Nop`. `execute_cmd` stores the visibility
flag in `FbConsole::cursor_visible`. The current implementation does not
render a visible cursor block -- the field is stored but not yet used for
cursor drawing. When cursor rendering is implemented, this flag will gate it.

## How `write_str()` Feeds the Pipeline

The public API entry point is `fb::write_str(s: &str)`, which acquires the
global `spin::Mutex` and calls the internal `FbConsole::write_str`:

```rust
pub fn write_str(s: &str) {
    if let Some(ref mut console) = *CONSOLE.lock() {
        console.write_str(s);
    }
}

// Inside FbConsole:
fn write_str(&mut self, s: &str) {
    for c in s.chars() {
        let cmd = self.parser.process_char(c);
        self.execute_cmd(cmd);
    }
}
```

The pipeline for a single string:

```
write_str("ESC[2J")
    |
    +-- process_char('\x1b') --> Nop          (state: Normal -> Escape)
    |
    +-- process_char('[')    --> Nop          (state: Escape -> Csi, reset params)
    |
    +-- process_char('2')    --> Nop          (params[0] = 2, param_count = 1)
    |
    +-- process_char('J')    --> EraseDisplay(2)  (state: Csi -> Normal)
                                    |
                                    v
                             execute_cmd(EraseDisplay(2))
                                    |
                                    v
                             clear_region(0, 0, cols, rows)
```

The parser holds per-string state across `write_str` calls because `AnsiParser`
is embedded in `FbConsole` -- if an escape sequence is split across two
`write_str` calls (which `fmt::Write` can do), the state machine resumes
correctly on the next call.

The mutex ensures `write_str` is safe to call from multiple kernel tasks
without a data race on the framebuffer pointer or parser state. Because the
mutex is a `spin::Mutex` (not a blocking mutex), `write_str` must not be
called from an interrupt handler -- it would spin-deadlock if the interrupted
code held the console lock.

## Test Coverage

The 17 unit tests in `kernel-core/src/fb.rs` run on the host with
`cargo test -p kernel-core`. Two helper functions simplify test writing:

```rust
fn parse_str(s: &str) -> Vec<ConsoleCmd> {
    let mut parser = AnsiParser::new();
    s.chars().map(|c| parser.process_char(c)).collect()
}

fn parse_str_last(s: &str) -> ConsoleCmd {
    let mut parser = AnsiParser::new();
    let mut last = ConsoleCmd::Nop;
    for c in s.chars() { last = parser.process_char(c); }
    last
}
```

`parse_str` captures every intermediate `Nop` (useful for verifying that
characters inside an escape sequence do not produce visible output).
`parse_str_last` captures only the final command (useful for testing
complete sequences).

| Test | Sequence | Verifies |
|------|---------|---------|
| `test_printable_chars` | `"AB"` | PutChar emitted for each character |
| `test_control_chars` | `\r \n \x08 \t` | Control character dispatch |
| `test_erase_display_2j` | `ESC[2J` | EraseDisplay(2) |
| `test_cursor_position` | `ESC[10;20H` | CursorPosition(10, 20) |
| `test_cursor_position_default` | `ESC[H` | Default params → CursorPosition(1, 1) |
| `test_dectcem_hide` | `ESC[?25l` | SetCursorVisible(false) |
| `test_dectcem_show` | `ESC[?25h` | SetCursorVisible(true) |
| `test_sgr_reset` | `ESC[m` | Sgr with params[0]=0 (bare reset) |
| `test_sgr_explicit_reset` | `ESC[0m` | Sgr with params[0]=0 (explicit reset) |
| `test_sgr_color` | `ESC[31;42m` | Two-param SGR preserved correctly |
| `test_malformed_escape_recovery` | `ESC X A` | Non-'[' after ESC → discard, resume normal |
| `test_cursor_movement` | `ESC[5A` `ESC[3B` `ESC[C` `ESC[2D` `ESC[15G` | All four CU* and CHA |
| `test_erase_in_line` | `ESC[K` `ESC[0K` `ESC[1K` `ESC[2K` | All three EL modes |
| `test_erase_in_display` | `ESC[J` `ESC[0J` `ESC[2J` | ED modes 0 and 2 |
| `test_interleaved_text_and_escapes` | `A ESC[2J B` | Normal output not disrupted by sequences |
| `test_unknown_csi_sequence` | `ESC[5z` | Unknown final byte → Nop |
| `test_state_after_sequence` | `ESC[2J` | Parser returns to Normal after dispatch |

The `test_malformed_escape_recovery` test is particularly important: it
verifies that a malformed sequence (`ESC` followed by something other than
`[`) does not leave the parser stuck in `Escape` state, which would suppress
subsequent printable output.

## Limitations

The following features are not implemented in Phase 22b:

- **24-bit / 256-color SGR**: `ESC [ 38 ; 2 ; r ; g ; b m` (truecolor) and
  `ESC [ 38 ; 5 ; n m` (256-color palette) are unrecognized and produce
  `Nop`. Ion's default color theme uses only the 16 VGA colors.

- **SGR attributes beyond bold/reset**: Underline (4), italic (3), inverse
  (7), strikethrough (9), and all their reset variants (21--29) are silently
  ignored. The 8x16 bitmap font has no variant glyphs for these.

- **Cursor rendering**: `cursor_visible` is stored but the console does not
  draw a cursor block at the current position. Ion's `liner` hides the cursor
  during redraws (`ESC[?25l`) and shows it after (`ESC[?25h`), so the
  missing rendering is not visible during normal use.

- **Scroll regions**: `ESC [ r ; s r` (DECSTBM) is not recognized. The
  scroll region is always the full screen. Ion does not require scroll region
  support.

- **Line feed / reverse linefeed**: `ESC M` (reverse index / scroll down) is
  not implemented. Escape sequences that begin with `ESC` followed by a
  non-`[` character are silently discarded.

- **Report sequences**: `ESC [ 6 n` (cursor position report) and similar
  response sequences are not implemented. Ion does not query the terminal
  position.

- **Tab stop customization**: Tab stops are fixed at every 8 columns. `ESC H`
  (set tab stop) and `ESC [ g` (clear tab stops) are not handled.

- **Double-width / double-height lines**: Not applicable to a bitmap console.

- **Pixel format U8 (greyscale)**: The `write_pixel` function handles this
  format with a luminance approximation (`0.299*R + 0.587*G + 0.114*B`
  approximated as integer multiply-shift), but VGA colors at full saturation
  produce very similar grey levels (e.g., red `0xAA` and green `0xAA` both
  map to mid-grey), making color-coded output illegible on a greyscale
  display. In practice QEMU provides an RGB or BGR framebuffer.

- **Unicode beyond ASCII**: Characters outside `0x20`--`0x7E` are rendered
  as a filled-block placeholder glyph (all pixels set to foreground color).
  The IBM CP437 font covers only ASCII printable characters. Ion's `liner`
  emits only ASCII in its prompt and control sequences.
