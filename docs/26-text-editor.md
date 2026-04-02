# Phase 26 — Text Editor

**Aligned Roadmap Phase:** Phase 26
**Status:** Complete
**Source Ref:** phase-26

## Overview

Phase 26 adds `edit`, a minimal full-screen text editor for m3OS. It is a
no_std Rust binary based on kibi/kilo concepts: a line-based text buffer,
VT100 escape sequences for screen control, raw terminal mode, and a
single-file editing model.

The editor is available at `/bin/edit` and is set as the `$EDITOR`
environment variable.

## Architecture

### Entry Point and Allocator

The editor is a no_std Rust binary (`userspace/edit/`) that uses
`extern crate alloc` for dynamic allocation (`Vec`, `String`). A
userspace heap allocator in `syscall-lib/src/heap.rs` provides
`GlobalAlloc` using the `brk` syscall to grow the process heap.

```
_start() → register SIGWINCH handler → enable raw mode → parse argv
         → open file (if given) → enter main loop → disable raw mode → exit
```

### Terminal Raw Mode

On startup, the editor:
1. Saves the original termios via `tcgetattr(0)`
2. Configures raw mode: clears `ICANON`, `ECHO`, `IEXTEN`, `ISIG`,
   `ICRNL`, `IXON`, `OPOST`, `BRKINT`, `INPCK`, `ISTRIP`; sets `CS8`
3. On exit (or panic), restores the original termios

### Screen Refresh

Each frame:
1. **Scroll**: adjust `row_offset`/`col_offset` so the cursor is visible
2. **Hide cursor**: `\x1b[?25l`
3. **Cursor home**: `\x1b[H`
4. **Draw rows**: for each visible row, write the render string (tabs
   expanded to spaces), clear to EOL with `\x1b[K`
5. **Draw status bar**: reverse video (`\x1b[7m`), filename, modified
   indicator, line count, cursor position
6. **Draw message bar**: help text or prompt
7. **Reposition cursor**: `\x1b[<row>;<col>H`
8. **Show cursor**: `\x1b[?25h`
9. **Flush**: single `write(1, ...)` call via the append buffer

### Row Model

Each line is a `Row` struct:
- `chars: Vec<u8>` — the raw characters
- `render: Vec<u8>` — the display string (tabs expanded to spaces)
- `update_render()` — regenerates `render` when `chars` changes
- `cx_to_rx()` / `rx_to_cx()` — cursor position translation

### Key Reading

`read_key()` reads one byte from stdin, then handles multi-byte escape
sequences:
- `\x1b[A/B/C/D` → arrow keys
- `\x1b[5~/6~` → Page Up/Down
- `\x1b[1~/4~/H/F` → Home/End
- `\x1b[3~` → Delete
- Ctrl+letter → `Key::Ctrl(letter)`
- DEL (127) → backspace

### Text Editing Operations

- **Insert char**: inserts at cursor position in the current row
- **Insert newline**: splits the current row at cursor
- **Delete char**: removes character left of cursor; at line start, merges
  with previous line
- **Delete (forward)**: moves right then deletes

### File I/O

- **Open**: reads file via `open(O_RDONLY)` + `read()`, splits on `\n`
- **Save**: serializes rows to `\n`-joined buffer, writes via
  `open(O_WRONLY|O_CREAT|O_TRUNC)` + `write()`
- **Dirty tracking**: `modified` flag set on any edit, cleared on save
- **Quit protection**: requires 3 consecutive `Ctrl+Q` to quit with
  unsaved changes

### Search

Incremental search via `Ctrl+F`:
- Prompt appears in message bar
- Each keystroke searches for substring match across all rows
- Arrow keys navigate between matches
- Escape cancels and restores cursor position

### SIGWINCH

A signal handler sets an `AtomicBool` flag when `SIGWINCH` is received
(terminal resize). The main loop checks this flag each refresh and
updates `screen_rows`/`screen_cols` via `TIOCGWINSZ`.

## syscall-lib Extensions

Phase 26 added these wrappers to `syscall-lib`:
- `ioctl(fd, request, arg)` — generic ioctl
- `lseek(fd, offset, whence)` — file seek
- `tcgetattr(fd)` / `tcsetattr(fd, &termios)` — termios helpers
- `get_window_size(fd)` — TIOCGWINSZ wrapper
- `rt_sigaction(signum, act, oldact)` — signal handler installation
- `brk(addr)` — program break management
- `Termios`, `Winsize`, `SigAction` struct definitions
- Terminal flag constants (`ICANON`, `ECHO`, `CS8`, etc.)
- `SIGWINCH` signal constant

## Heap Allocator

`syscall-lib/src/heap.rs` provides `BrkAllocator`:
- Implements `GlobalAlloc` trait
- Uses `brk` syscall to grow the heap in page-sized increments
- First-fit free list for allocation
- Freed blocks returned to the free list head
- Enabled via `syscall-lib`'s `alloc` feature flag

## Key Bindings

| Key | Action |
|---|---|
| Arrow keys | Move cursor |
| Page Up/Down | Scroll by screen height |
| Home/End | Move to start/end of line |
| Printable chars | Insert text |
| Enter | Insert newline |
| Backspace/Ctrl+H | Delete char left |
| Delete | Delete char right |
| Ctrl+S | Save file |
| Ctrl+Q | Quit (3x to force with unsaved changes) |
| Ctrl+F | Search |

## Build Integration

- xtask builds `edit` with `-Zbuild-std=core,compiler_builtins,alloc`
- Binary registered in kernel initrd as `/bin/edit` and `/bin/edit.elf`
- `EDITOR=/bin/edit` set in init's environment
