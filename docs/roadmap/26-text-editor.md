# Phase 26 - Text Editor

## Milestone Goal

A usable text editor runs inside the OS. Users can create, edit, and save files from
the shell. This is the foundational tool that makes all subsequent "do real work inside
the OS" phases possible — you cannot write code, configuration files, or documents
without an editor.

## Learning Goals

- Understand how terminal-based editors use raw mode, ANSI escape sequences, and
  screen coordinates to build a full-screen TUI.
- Learn how editors manage in-memory text buffers (gap buffer vs. line array).
- See how a real C program exercises the OS's terminal, file I/O, and signal subsystems
  together.

## Feature Scope

**Primary: Port `e3` (tiny editor)**

[e3](https://sites.google.com/site/e3editor/) is a ~15 KB editor written in x86
assembly with C wrappers. It supports WordStar, Emacs, vi, and pstrict keybindings.
However, its assembly core may be difficult to port.

**Recommended alternative: Port `kilo` or write a kilo-inspired editor**

[kilo](https://github.com/antirez/kilo) is a minimal text editor in ~1000 lines of C
by Salvatore Sanfilippo (antirez). It requires only VT100 escape sequences and a
handful of POSIX calls — exactly what m3OS already supports.

Required OS features (all already implemented):
- Raw terminal mode (`termios` / `tcsetattr`) — Phase 22
- ANSI escape sequences (cursor movement, erase, colors) — Phase 22b
- File I/O (`open`, `read`, `write`, `close`) — Phase 12
- Signal handling (`SIGWINCH` for terminal resize) — Phase 19
- `ioctl(TIOCGWINSZ)` for terminal size — Phase 22

The editor should support:
- Open, edit, and save files
- Line-by-line scrolling and cursor movement
- Search (find text)
- Syntax highlighting (optional, but kilo supports it in <200 extra lines)
- Status bar showing filename, line number, modification state

**Fallback: Minimal `ed`-style line editor**

If full-screen editing proves too complex, a line editor (`ed` clone) provides basic
file editing with much simpler terminal requirements. This is historically accurate —
early Unix development was done entirely in `ed`.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 22 (TTY) | Raw mode, termios, TIOCGWINSZ |
| Phase 22b (ANSI) | Cursor positioning, screen erase, colors |
| Phase 24 (Persistent Storage) | Save files that survive reboot |

## Implementation Outline

1. Cross-compile kilo (or a kilo-inspired editor) with musl on the host.
2. Add the binary to the disk image at `/bin/edit` (or `/bin/kilo`).
3. Verify raw mode works: editor takes over the full terminal screen.
4. Test file operations: create a new file, edit it, save it, reopen it.
5. Test search functionality.
6. Add syntax highlighting for `.c` files (stretch goal).
7. Add the editor to PATH and create a shell alias (`$EDITOR`).

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

Real systems ship with multiple editors (vi, nano, emacs) and editor infrastructure
like terminfo/termcap databases that abstract terminal capabilities. Our approach
hardcodes VT100 escape sequences, which is fine because QEMU's serial console and
virtually all modern terminal emulators support VT100. A production OS would also
provide shared libraries for TUI development (ncurses), which we defer.

## Deferred Until Later

- ncurses or equivalent TUI library
- terminfo/termcap database
- Multiple file editing (split views, tabs)
- Undo/redo (kilo supports single-level undo; full undo tree is deferred)
- Plugin or macro system
- Mouse support
