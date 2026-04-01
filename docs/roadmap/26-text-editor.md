# Phase 26 — Text Editor

**Status:** Complete
**Source Ref:** phase-26
**Depends on:** Phase 22 (TTY/Terminal) ✅, Phase 24 (Persistent Storage) ✅
**Builds on:** Uses raw terminal mode and ANSI escape sequences from Phase 22; saves files to persistent storage from Phase 24
**Primary Components:** userspace/edit/, userspace/syscall-lib/

## Milestone Goal

A usable text editor runs inside the OS. Users can create, edit, and save files from
the shell. This is the foundational tool that makes all subsequent "do real work inside
the OS" phases possible — you cannot write code, configuration files, or documents
without an editor.

## Why This Phase Exists

Without an in-OS text editor, users must create and modify files from outside the
system (e.g., on the host and then rebuild the disk image). This makes the OS
unusable for any self-hosted development or configuration workflow. A text editor
is the minimum tool required for the OS to become a productive environment rather
than a demonstration. Building it also exercises the terminal subsystem, file I/O,
and signal handling together in a real application, validating that those subsystems
work correctly in concert.

## Learning Goals

- Understand how terminal-based editors use raw mode, ANSI escape sequences, and
  screen coordinates to build a full-screen TUI.
- Learn how editors manage in-memory text buffers (gap buffer vs. line array).
- See how a real C program exercises the OS's terminal, file I/O, and signal subsystems
  together.

## Feature Scope

**Primary: Port [kibi](https://github.com/ilai-deutel/kibi) (Rust kilo clone)**

Kibi is a text editor in <=1024 lines of Rust by Ilai Deutel, a direct port of
antirez's [kilo](https://github.com/antirez/kilo). MIT/Apache-2.0 dual license.

Why kibi over kilo (C) or other editors:
- Same ~1000-line philosophy as kilo, but in Rust with memory safety
- Only two runtime dependencies: `unicode-width` (no_std-compatible) and `libc`
  (replaced by our m3OS platform backend)
- Clean platform abstraction: separate `unix.rs`, `windows.rs`, `wasi.rs`
  backends — we write an `m3os.rs` backend (~60-80 lines)
- Keeps the codebase uniform with init, sh0, and ping (all Rust, no_std)
- Hardcodes VT100 escape sequences (no terminfo/ncurses dependency)
- Features: cursor movement, scrolling, search, syntax highlighting, status bar

Required OS features (all already implemented):
- Raw terminal mode (`termios` / `tcsetattr`) — Phase 22
- ANSI escape sequences (cursor movement, erase, colors) — Phase 22b
- File I/O (`open`, `read`, `write`, `close`) — Phase 12
- Signal handling (`SIGWINCH` for terminal resize) — Phase 19
- `ioctl(TIOCGWINSZ)` for terminal size — Phase 22

The editor supports:
- Open, edit, and save files
- Line-by-line scrolling and cursor movement
- Search (find text) with incremental matching
- Status bar showing filename, line number, modification state
- Dirty-file tracking with quit confirmation

**Fallback: Minimal `ed`-style line editor**

If the kibi port proves too complex, a line editor (`ed` clone) provides basic
file editing with much simpler terminal requirements. This is historically accurate —
early Unix development was done entirely in `ed`.

## Important Components and How They Work

### m3OS Platform Backend

An `m3os.rs` backend (~60-80 lines) replaces kibi's `libc` dependency with
direct syscall-lib calls. It implements terminal I/O (raw mode enable/disable,
read/write, window size query) using the m3OS syscall ABI.

### Userspace Heap Allocator

Kibi uses `Vec`/`String` via `alloc`, so this phase establishes a userspace heap
allocator pattern. The allocator built here is reusable by future phases (compiler
bootstrap, build tools, etc.).

### syscall-lib Extensions

The syscall wrapper library gains ioctl, lseek, termios, and winsize wrappers to
support the editor's terminal interaction needs.

## How This Builds on Earlier Phases

- **Extends Phase 22 (TTY/Terminal):** uses raw mode, termios, TIOCGWINSZ, and ANSI escape sequences
- **Extends Phase 24 (Persistent Storage):** saves and loads files from the persistent filesystem
- **Extends Phase 12 (POSIX Compat):** exercises file I/O syscalls (open, read, write, close) in a real application
- **Extends Phase 19 (Signals):** handles SIGWINCH for terminal resize events
- **Establishes:** the userspace heap allocator pattern reused by later phases

## Implementation Outline

1. Extend `syscall-lib` with ioctl, lseek, termios, and winsize wrappers.
2. Add a userspace heap allocator (kibi uses `Vec`/`String` via `alloc`).
3. Create `userspace/edit/` crate (no_std, `x86_64-unknown-none` target).
4. Write an m3OS platform backend (~60-80 lines) implementing terminal I/O.
5. Port kibi's core modules, replacing `std::fs`/`std::io` with syscall-lib.
6. Verify raw mode, file save/load, and search work end-to-end.
7. Add to xtask build, initrd, and set `$EDITOR=/bin/edit`.

## Acceptance Criteria

- The editor launches from the shell and displays a full-screen TUI.
- Arrow keys, Page Up/Down, Home/End move the cursor correctly.
- Users can type text, delete characters, and insert new lines.
- `Ctrl+S` (or equivalent) saves the file to persistent storage.
- `Ctrl+Q` (or equivalent) quits the editor.
- `Ctrl+F` (or equivalent) searches for text within the file.
- Opening a nonexistent filename creates a new empty file on save.
- The editor correctly handles files larger than the terminal height (scrolling).

## Companion Task List

- [Phase 26 Task List](./tasks/26-text-editor-tasks.md)

## How Real OS Implementations Differ

- Real systems ship with multiple editors (vi, nano, emacs) and editor infrastructure
  like terminfo/termcap databases that abstract terminal capabilities. Our approach
  hardcodes VT100 escape sequences, which is fine because QEMU's serial console and
  virtually all modern terminal emulators support VT100.
- A production OS would also provide shared libraries for TUI development (ncurses),
  which we defer.
- A notable side effect of this phase is establishing the pattern for Rust userspace
  programs that need heap allocation (`Vec`, `String`, `format!`). The userspace heap
  allocator built here is reusable by future phases (compiler bootstrap, build
  tools, etc.).

## Deferred Until Later

- ncurses or equivalent TUI library
- terminfo/termcap database
- Multiple file editing (split views, tabs)
- Undo/redo (kilo supports single-level undo; full undo tree is deferred)
- Plugin or macro system
- Mouse support
