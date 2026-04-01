# Phase 26 — Text Editor: Task List

**Status:** Complete
**Source Ref:** phase-26
**Depends on:** Phase 22 (TTY) ✅, Phase 24 (Persistent Storage) ✅
**Goal:** A usable full-screen text editor (`edit`) runs inside the OS, enabling
users to create, edit, search, and save files from the shell. Ported from kibi
(a Rust kilo clone) with an m3OS platform backend.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | syscall-lib extensions + heap allocator | — | ✅ Done |
| B | Terminal raw mode verification | A | ✅ Done |
| C | Port kibi core (editor logic) | A | ✅ Done |
| D | m3OS platform backend | A | ✅ Done |
| E | File I/O and search | C, D | ✅ Done |
| F | Build system and shell integration | C | ✅ Done |
| G | Validation and documentation | E, F | ✅ Done |

### Implementation Notes

- **Binary name**: `/bin/edit` — short, easy to type, no conflict with existing
  utilities.
- **Crate name**: `edit` (package) / `edit` (binary), following the init/shell
  pattern.
- **Terminal model**: Hardcode VT100 escape sequences. No terminfo/termcap
  needed — QEMU serial console and the framebuffer console both support VT100.
- **Text buffer**: Kibi uses a `Vec<Row>` line array — adequate for the file
  sizes we expect. Gap buffer / rope is deferred.
- **Heap**: The editor needs dynamic allocation (`Vec`, `String`). Add a
  simple global allocator to userspace that uses `brk` or `mmap` under the hood.
  This allocator will be reusable by future Rust userspace programs.
- **no_std + alloc**: The editor crate uses `#![no_std]` with `extern crate alloc`
  for `Vec`, `String`, `format!`. The allocator is set up before entering the
  editor main loop.

## Prerequisite Analysis

Current state (post-Phase 25):
- TTY subsystem with full termios support: canonical mode, raw mode, echo,
  signal characters (`ISIG`), CR/LF translation
- IOCTL operations: `TCGETS`, `TCSETS`, `TCSETSW`, `TCSETSF` for termios;
  `TIOCGWINSZ` / `TIOCSWINSZ` for terminal window size (default 24x80)
- ANSI escape sequence parser in the framebuffer console: cursor movement
  (`CUU`, `CUD`, `CUF`, `CUB`), cursor positioning (`CUP`), erase line/screen
  (`EL`, `ED`), SGR color attributes — all VT100 sequences kibi requires
- File I/O syscalls: `open`, `read`, `write`, `close`, `lseek`, `fstat`
- Persistent FAT32 filesystem on virtio-blk (Phase 24) — files survive reboot
- tmpfs mounted at `/tmp` for scratch files
- Signal handling with user-space trampolines (Phase 19) — `SIGWINCH` support
  needed for terminal resize (verify or add)
- Rust no_std userspace binaries (init, sh0, ping) built with `cargo` targeting
  `x86_64-unknown-none`, using `syscall-lib` for OS interaction
- `syscall-lib` provides: `read`, `write`, `open`, `close`, `fork`, `execve`,
  `exit`, `waitpid`, `pipe`, `dup2`, `chdir`, `getcwd`, plus raw `syscall0`–
  `syscall6` for anything not yet wrapped
- `SYS_IOCTL` (16) constant exists in `syscall-lib` but has no high-level
  wrapper — needed for termios and window size
- `SYS_LSEEK` (8) constant exists but has no wrapper — needed for file ops
- SMP-aware kernel with up to 16 cores (Phase 25)

Already implemented (no new work needed):
- Raw terminal mode via termios `TCSETS` ioctl (clear `ICANON`, `ECHO`)
- ANSI escape sequences: cursor positioning, screen erase, colors
- File I/O syscalls in the kernel for open/read/write/close/lseek
- Persistent storage (FAT32 on virtio-blk)
- Rust no_std userspace build pipeline in xtask (`x86_64-unknown-none`,
  `-Zbuild-std=core,compiler_builtins`)
- Process lifecycle: fork, exec, wait, exit
- Signal infrastructure (rt_sigaction, signal delivery)

Needs to be added or extended:
- `syscall-lib`: `ioctl()` wrapper (raw syscall3 for `SYS_IOCTL`)
- `syscall-lib`: `lseek()` wrapper (raw syscall3 for `SYS_LSEEK`)
- `syscall-lib`: termios struct definitions and `tcgetattr`/`tcsetattr` helpers
- `syscall-lib`: `winsize` struct and `get_window_size` helper
- Heap allocator for the editor process (kibi uses `Vec`, `String`) — either
  a simple bump/linked-list allocator in `syscall-lib` or a shared userspace
  allocator crate using `brk`/`mmap`
- `SIGWINCH` delivery when terminal size changes (verify or add kernel-side)

## Approach: Port Kibi

[Kibi](https://github.com/ilai-deutel/kibi) is a text editor in <=1024 lines
of Rust, a direct clone of antirez's kilo. MIT/Apache-2.0 dual license.

Why kibi:
- Same ~1000-line philosophy as kilo, but in Rust with memory safety
- Only two runtime dependencies: `unicode-width` (no_std-compatible) and `libc`
  (replaced by our platform backend)
- Clean platform abstraction: separate `unix.rs`, `windows.rs`, `wasi.rs`
  backends — we write an `m3os.rs` backend (~60-80 lines)
- Hardcodes VT100 escape sequences (no terminfo/ncurses needed)
- Features: cursor movement, scrolling, search, syntax highlighting, status bar,
  dirty-file tracking, configurable keybindings

Porting strategy:
1. Create `userspace/edit/` as a no_std Rust crate with `syscall-lib` dependency
2. Add a userspace heap allocator (needed for `Vec`/`String` in kibi)
3. Extend `syscall-lib` with ioctl/termios/lseek wrappers
4. Port kibi's core modules, replacing `std::fs`/`std::io` with syscall-lib calls
5. Write an `m3os.rs` platform backend implementing kibi's terminal interface
6. Build via xtask alongside init, sh0, ping

---

## Track A — syscall-lib Extensions and Heap Allocator

Extend `syscall-lib` with the system call wrappers kibi needs, and provide a
global allocator for userspace Rust binaries that need heap allocation.

### A.1 — Add `ioctl()` wrapper

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `ioctl`
**Why it matters:** All terminal control operations (termios, window size) go through the ioctl syscall interface.

**Acceptance:**
- [x] `ioctl(fd, request, arg)` wrapper calls `syscall3(SYS_IOCTL, fd, request, arg)`

### A.2 — Add `lseek()` wrapper

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `lseek`
**Why it matters:** File seeking is needed for random-access file I/O during editor save operations.

**Acceptance:**
- [x] `lseek(fd, offset, whence)` wrapper works with `SEEK_SET`, `SEEK_CUR`, `SEEK_END` constants

### A.3 — Add termios type definitions

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `Termios`
**Why it matters:** The editor must switch the terminal to raw mode, which requires the termios structure and flag constants.

**Acceptance:**
- [x] `struct Termios` matches kernel 36-byte layout (`c_iflag`, `c_oflag`, `c_cflag`, `c_lflag`, `c_cc[19]`)
- [x] Flag constants defined: `ICANON`, `ECHO`, `ISIG`, `ICRNL`, `OPOST`, `BRKINT`, `CS8`, etc.
- [x] `TCGETS`/`TCSETS`/`TCSETSF` ioctl numbers defined

### A.4 — Add `tcgetattr`/`tcsetattr` convenience functions

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `tcgetattr`, `tcsetattr`
**Why it matters:** These provide a safe, ergonomic API for reading and writing terminal settings.

**Acceptance:**
- [x] `tcgetattr(fd)` returns `Result<Termios, isize>` via ioctl
- [x] `tcsetattr(fd, termios)` returns `Result<(), isize>` via ioctl

### A.5 — Add `Winsize` struct and `get_window_size` helper

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `Winsize`, `get_window_size`
**Why it matters:** The editor needs terminal dimensions to lay out the screen correctly.

**Acceptance:**
- [x] `struct Winsize` matches kernel layout (`ws_row`, `ws_col`, `ws_xpixel`, `ws_ypixel`)
- [x] `get_window_size(fd)` uses `ioctl(fd, TIOCGWINSZ, &winsize)`

### A.6 — Create userspace heap allocator

**File:** `userspace/syscall-lib/src/heap.rs`
**Symbol:** `BrkAllocator`
**Why it matters:** The editor uses `Vec`, `String`, and `format!` which all require a global allocator in no_std.

**Acceptance:**
- [x] `GlobalAlloc` implementation using `brk` syscall
- [x] Reusable by future Rust userspace programs

### A.7 — Verify allocator works

**Files:** `userspace/edit/src/main.rs`
**Why it matters:** A broken allocator would cause silent corruption or panics in the editor.

**Acceptance:**
- [x] `Vec<u8>` push/len works without panic or OOM on small allocation

### A.8 — Add `rt_sigaction` wrapper and `SIGWINCH` constant

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `rt_sigaction`, `SIGWINCH`
**Why it matters:** The editor needs to handle terminal resize events via SIGWINCH signals.

**Acceptance:**
- [x] `rt_sigaction` wrapper present
- [x] `SIGWINCH` (28) constant defined

---

## Track B — Terminal Raw Mode Verification

Verify that the kernel's TTY layer supports the terminal operations a
full-screen editor needs before porting kibi.

### B.1 — Raw mode test binary

**Component:** `userspace/raw-test/` (test binary)
**Why it matters:** Validates that the kernel correctly implements immediate-character delivery and echo suppression.

**Acceptance:**
- [x] Switching to raw mode works (clear `ICANON`, `ECHO`, `IEXTEN`, `ISIG`, `ICRNL`, `OPOST`; set `CS8`)
- [x] Each keypress returned immediately without echo

### B.2 — Verify `TIOCGWINSZ` returns correct dimensions

**Component:** Kernel TTY / ioctl layer
**Why it matters:** Incorrect terminal dimensions would cause corrupted screen layout.

**Acceptance:**
- [x] `get_window_size(0)` returns (rows, cols) matching the terminal

### B.3 — Verify ANSI escape sequences in raw mode

**Component:** Kernel framebuffer console
**Why it matters:** Screen clear, cursor home, and cursor position report are essential for full-screen TUI rendering.

**Acceptance:**
- [x] `\x1b[2J` (clear screen), `\x1b[H` (cursor home) work in raw mode
- [x] `TIOCGWINSZ` fallback path works if `\x1b[6n` response is not supported

### B.4 — Verify arrow key escape sequences

**Component:** Kernel keyboard/TTY layer
**Why it matters:** Without correct key translation, cursor movement in the editor would be broken.

**Acceptance:**
- [x] Arrow keys, Home, End, Page Up/Down, Delete received correctly in raw mode

### B.5 — Verify or add SIGWINCH delivery

**Component:** Kernel TTY subsystem (`kernel/src/tty.rs`)
**Why it matters:** Terminal resize must notify the editor so it can redraw at the new dimensions.

**Acceptance:**
- [x] `TIOCSWINSZ` ioctl sends `SIGWINCH` to the foreground process group

---

## Track C — Port Kibi Core

Port kibi's editor logic into a no_std Rust crate. Replace `std` types with
`alloc` equivalents and syscall-lib calls.

### C.1 — Create `userspace/edit/` crate

**File:** `userspace/edit/src/main.rs`
**Why it matters:** This is the foundation for the entire editor binary.

**Acceptance:**
- [x] `Cargo.toml` depends on `syscall-lib` and `unicode-width`
- [x] `#![no_std]`, `#![no_main]`, global allocator setup, `_start` entry point
- [x] Compiles and runs (empty screen, immediate exit)

### C.2 — Port `Row` struct

**File:** `userspace/edit/src/main.rs`
**Symbol:** `Row`
**Why it matters:** The row model is the core data structure for text storage and rendering.

**Acceptance:**
- [x] `chars: Vec<u8>`, `render: Vec<u8>` (tabs expanded to spaces)
- [x] `update_render()` regenerates render string
- [x] `cx_to_rx()` converts cursor x to render x accounting for tab width

### C.3 — Port key reading

**File:** `userspace/edit/src/main.rs`
**Symbol:** `read_key`
**Why it matters:** The editor's entire input model depends on correctly parsing raw keystrokes and escape sequences.

**Acceptance:**
- [x] `read_key()` reads one byte from stdin, handles multi-byte escape sequences
- [x] Returns `Key` enum (`ArrowUp`, `ArrowDown`, `PageUp`, `Home`, `Del`, `Char(u8)`, `Ctrl(u8)`, etc.)

### C.4 — Port append buffer

**File:** `userspace/edit/src/main.rs`
**Symbol:** `ABuf`
**Why it matters:** Batching screen output into a single write call prevents visible flicker during refresh.

**Acceptance:**
- [x] `struct ABuf { buf: Vec<u8> }` with `push_str` and `flush` (single `write(1, ...)` call)

### C.5 — Port `refresh_screen()`

**File:** `userspace/edit/src/main.rs`
**Symbol:** `refresh_screen`
**Why it matters:** This is the core rendering loop that draws every visible element each frame.

**Acceptance:**
- [x] Hide/show cursor, draw rows with `\x1b[K`, status bar, message bar, reposition cursor
- [x] Flush append buffer in single write

### C.6 — Port cursor movement

**File:** `userspace/edit/src/main.rs`
**Why it matters:** Correct cursor movement with line-length snapping and wrapping is essential for usability.

**Acceptance:**
- [x] Arrow keys move `cx`/`cy` with line-boundary wrapping
- [x] Page Up/Down, Home/End implemented

### C.7 — Port vertical scrolling

**File:** `userspace/edit/src/main.rs`
**Why it matters:** Without scrolling, the editor cannot display files larger than the screen.

**Acceptance:**
- [x] `row_offset` maintained; only visible rows rendered

### C.8 — Port horizontal scrolling

**File:** `userspace/edit/src/main.rs`
**Why it matters:** Without horizontal scrolling, long lines would be truncated or overflow the screen.

**Acceptance:**
- [x] `col_offset` maintained; only visible columns rendered

### C.9 — Port text insertion

**File:** `userspace/edit/src/main.rs`
**Symbol:** `insert_char`
**Why it matters:** Character insertion is the most basic editing operation.

**Acceptance:**
- [x] `insert_char(c)` at cursor position; handles inserting at end of file

### C.10 — Port newline insertion

**File:** `userspace/edit/src/main.rs`
**Symbol:** `insert_newline`
**Why it matters:** Newline handling correctly splits lines, which is fundamental to multi-line editing.

**Acceptance:**
- [x] `insert_newline()` splits row at `cx`; handles beginning/end of line cases

### C.11 — Port character deletion

**File:** `userspace/edit/src/main.rs`
**Symbol:** `delete_char`
**Why it matters:** Backspace/delete must correctly handle line merging at line boundaries.

**Acceptance:**
- [x] `delete_char()` removes char left of cursor; merges lines at line beginning

### C.12 — Port `process_keypress()`

**File:** `userspace/edit/src/main.rs`
**Symbol:** `process_keypress`
**Why it matters:** This is the main input dispatch that maps all key bindings to editor actions.

**Acceptance:**
- [x] Printable chars insert, `Ctrl+Q` quits, `Ctrl+S` saves, `Ctrl+F` finds, arrows move, Backspace/Delete delete

---

## Track D — m3OS Platform Backend

Write the platform-specific glue that connects kibi's terminal interface to
m3OS syscalls.

### D.1 — Implement `enable_raw_mode()`

**File:** `userspace/edit/src/main.rs`
**Why it matters:** The editor cannot function without raw mode -- cooked mode buffers input by line.

**Acceptance:**
- [x] Saves original termios, clears `ICANON`, `ECHO`, `IEXTEN`, `ISIG`, `ICRNL`, `IXON`, `OPOST`, `BRKINT`, `INPCK`, `ISTRIP`; sets `CS8`

### D.2 — Implement `disable_raw_mode()`

**File:** `userspace/edit/src/main.rs`
**Why it matters:** Failing to restore terminal mode would leave the shell unusable after the editor exits.

**Acceptance:**
- [x] Restores saved termios on normal exit and on panic

### D.3 — Implement `get_window_size()`

**File:** `userspace/edit/src/main.rs`
**Why it matters:** The editor needs to know the terminal dimensions for screen layout.

**Acceptance:**
- [x] Returns `(rows, cols)` via `syscall_lib::get_window_size(0)`
- [x] Fallback via escape sequence if needed

### D.4 — Implement stdin reading

**File:** `userspace/edit/src/main.rs`
**Symbol:** `read_byte`
**Why it matters:** Single-byte reads are the foundation of the key reader for escape sequence assembly.

**Acceptance:**
- [x] `read_byte() -> Option<u8>` reads from stdin via `read(0, &mut buf, 1)`

### D.5 — Implement `register_sigwinch_handler()`

**File:** `userspace/edit/src/main.rs`
**Why it matters:** Terminal resize events must be detected so the editor can redraw at the correct dimensions.

**Acceptance:**
- [x] `rt_sigaction` registers a handler for `SIGWINCH` that sets a global `AtomicBool` flag

---

## Track E — File I/O and Search

Port kibi's file loading, saving, and search functionality.

### E.1 — Port `open_file()`

**File:** `userspace/edit/src/main.rs`
**Symbol:** `open_file`
**Why it matters:** Loading files from disk is a core editor capability.

**Acceptance:**
- [x] Opens file via `syscall_lib::open`, splits on `\n` (handles `\r\n`), populates `Vec<Row>`
- [x] Handles nonexistent files (start with empty buffer)

### E.2 — Port `save_file()`

**File:** `userspace/edit/src/main.rs`
**Symbol:** `save_file`
**Why it matters:** Saving files to persistent storage is the primary output operation of the editor.

**Acceptance:**
- [x] Serializes rows to `\n`-joined buffer, writes with `O_WRONLY | O_CREAT | O_TRUNC`
- [x] Prompts for filename if none set

### E.3 — Port dirty-file tracking

**File:** `userspace/edit/src/main.rs`
**Why it matters:** Prevents accidental data loss by warning before quitting with unsaved changes.

**Acceptance:**
- [x] `modified` flag set on edits, cleared on save
- [x] `Ctrl+Q` requires 3 consecutive presses to force quit with unsaved changes

### E.4 — Port `prompt()`

**File:** `userspace/edit/src/main.rs`
**Symbol:** `prompt`
**Why it matters:** The prompt mini-line is used for search queries and save-as filename entry.

**Acceptance:**
- [x] Mini input line in message bar with Enter/Escape/Backspace handling
- [x] Optional per-keystroke callback for incremental search

### E.5 — Port `find()`

**File:** `userspace/edit/src/main.rs`
**Symbol:** `find`
**Why it matters:** In-file search is a critical editor feature for navigating large files.

**Acceptance:**
- [x] Incremental search with arrow-key match navigation
- [x] Reverse video highlight on current match; cursor restored on cancel

---

## Track F — Build System and Shell Integration

### F.1 — Add `edit` to xtask build

**File:** `xtask/src/main.rs`
**Why it matters:** Without build system integration, the editor binary cannot be included in the OS image.

**Acceptance:**
- [x] `edit` built with `-Zbuild-std=core,compiler_builtins,alloc` and included in userspace builds

### F.2 — Register `edit` in initrd

**File:** `kernel/src/fs/ramdisk.rs`
**Why it matters:** The binary must be available in the filesystem for the shell to execute it.

**Acceptance:**
- [x] `include_bytes!("../initrd/edit.elf")` added; available at `/bin/edit` at boot

### F.3 — Verify shell exec

**Component:** Shell + exec integration
**Why it matters:** Users invoke the editor from the shell, so argument passing must work.

**Acceptance:**
- [x] `edit <filename>` works from shell; filename passed via argv

### F.4 — Set `EDITOR` environment variable

**File:** `userspace/init/src/main.rs`
**Why it matters:** Allows future programs to discover the default editor via `$EDITOR`.

**Acceptance:**
- [x] `EDITOR=/bin/edit` set in init environment

---

## Track G — Validation and Documentation

### G.1 — Editor launch acceptance

**Why it matters:** Confirms the editor binary loads and renders correctly.

**Acceptance:**
- [x] `edit` launches from shell with full-screen TUI, welcome message or tilde-prefixed empty lines

### G.2 — Cursor movement acceptance

**Acceptance:**
- [x] Arrow keys, Page Up/Down, Home/End move cursor correctly

### G.3 — Text editing acceptance

**Acceptance:**
- [x] Typing inserts text; Backspace and Delete remove characters

### G.4 — File save acceptance

**Acceptance:**
- [x] `Ctrl+S` saves to persistent FAT32 storage; verified with `cat`

### G.5 — Quit acceptance

**Acceptance:**
- [x] `Ctrl+Q` quits; dirty-file warning requires 3 presses to force quit

### G.6 — Search acceptance

**Acceptance:**
- [x] `Ctrl+F` searches with incremental updates; arrow keys navigate matches

### G.7 — New file acceptance

**Acceptance:**
- [x] Opening nonexistent filename creates empty buffer; saving creates file on disk

### G.8 — Vertical scrolling acceptance

**Acceptance:**
- [x] Scrolling works for files larger than terminal height (100+ lines)

### G.9 — Horizontal scrolling acceptance

**Acceptance:**
- [x] Horizontal scrolling works for lines wider than terminal width

### G.10 — Status bar acceptance

**Acceptance:**
- [x] Status bar shows filename, line count, cursor position, modified indicator

### G.11 — Terminal restoration acceptance

**Acceptance:**
- [x] Terminal restored to cooked mode on exit (no garbled shell prompt)

### G.12 — SMP acceptance

**Acceptance:**
- [x] Editor works correctly under SMP (no corruption with concurrent processes)

### G.13 — Lint and format

**Acceptance:**
- [x] `cargo xtask check` passes (clippy + fmt)

### G.14 — QEMU boot validation

**Acceptance:**
- [x] Editor launches, edits, saves, and quits without panics in both serial and framebuffer modes

### G.15 — Documentation

**File:** `docs/26-text-editor.md`
**Why it matters:** Documents the editor architecture for future maintainers and extension work.

**Acceptance:**
- [x] Covers kibi port rationale, raw mode setup, append buffer, row model, screen refresh loop, m3OS backend, key mapping, file I/O, heap allocator

---

## Deferred Until Later

These items are explicitly out of scope for Phase 26:

- Syntax highlighting (kibi supports it; defer to Phase 26b to keep the initial
  port focused — enable once the core editor is stable)
- Multiple file editing (split views, tabs)
- Undo/redo beyond single-character level
- Copy/paste (requires clipboard abstraction)
- Mouse support
- ncurses or TUI library
- terminfo/termcap database
- Line wrapping mode (soft wrap)
- Full Unicode / multi-byte character editing (kibi uses `unicode-width` for
  display width but full grapheme cluster editing is complex)
- Plugin or macro system
- Configuration file (kibi supports `.kibirc`; defer until needed)
- Colorscheme / theme support

---

## Documentation Notes

- Phase 26 adds the first userspace heap allocator (`BrkAllocator`), which is
  reusable by all future Rust userspace programs.
- The `ioctl`, `tcgetattr`, `tcsetattr`, `lseek`, and `get_window_size` wrappers
  added to `syscall-lib` are general-purpose and used by later phases (29, 30).
- The text editor is the first program to use raw terminal mode in the OS.
