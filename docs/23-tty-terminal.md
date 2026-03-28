# Phase 22: TTY and Terminal Control

This document describes the TTY and terminal control implementation in
m3OS. It covers the line discipline that sits between raw keyboard input
and userspace `read()`, the `termios` struct and its ABI compatibility,
the `ioctl` interface for querying and modifying terminal settings, the
`FdBackend::DeviceTTY` abstraction that makes `isatty()` work, and the
PTY skeleton stubs that will support terminal multiplexers in a later
phase.

## What a TTY Is and Why It Exists

A TTY (teletypewriter) is a kernel abstraction that interposes between a
raw input source (keyboard, serial port) and the userspace processes that
call `read(0, ...)`. Without a TTY layer, every application would need to
handle its own line editing: backspace, Ctrl-U, echo, carriage-return
translation, and signal generation. By placing this logic in the kernel,
every shell and interactive program inherits it for free.

The TTY layer is split across three layers in m3OS:

```
Keyboard interrupt
        |
        v
   [scancode ring buffer]
        |
        v
  stdin_feeder_task          <-- Phase 22 line discipline (kernel/src/main.rs)
  (kernel task, ring 0)
        |
        |-- reads termios flags from TTY0
        |-- processes edit operations (erase, kill, word-erase)
        |-- generates signals (SIGINT, SIGTSTP, SIGQUIT)
        |-- translates CR→NL (ICRNL), NL→CRNL (ONLCR)
        |
        v
  stdin circular buffer      <-- kernel/src/stdin.rs
        |
        v
  read(fd=0, ...)             <-- userspace sys_read, FdBackend::DeviceTTY
```

The `stdin_feeder_task` is a dedicated kernel task. It issues IPC calls
to the keyboard server to receive scancodes, translates them into bytes,
consults the current `termios` configuration held in the `TTY0` global,
applies the line discipline, and either buffers bytes in the edit buffer
(canonical mode) or pushes them directly to the stdin circular buffer
(raw mode).

## The `TtyState` Global

There is a single console TTY, `TTY0`, held in a `spin::Mutex`:

```rust
// kernel/src/tty.rs
pub struct TtyState {
    pub termios: Termios,
    pub winsize: Winsize,
    pub fg_pgid: u32,
    pub edit_buf: EditBuffer,
}

pub static TTY0: Mutex<TtyState> = Mutex::new(TtyState::new());
```

`fg_pgid` tracks the foreground process group. Signal characters
(Ctrl-C, Ctrl-Z, Ctrl-\) are delivered to every process in this group
via `send_signal_to_group(fg, sig)`. A mirrored copy of `fg_pgid` is
kept in the atomic `process::FG_PGID` so it can be read from interrupt
context without taking the TTY lock.

## The `Termios` Struct — Linux x86_64 ABI

The `Termios` struct is defined in `kernel-core/src/tty.rs` and is
`repr(C)` for binary compatibility with the Linux kernel ABI used by
musl's `ioctl(TCGETS)`:

```rust
// kernel-core/src/tty.rs
pub const NCCS: usize = 19;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub c_iflag: u32,      // input mode flags
    pub c_oflag: u32,      // output mode flags
    pub c_cflag: u32,      // control mode flags
    pub c_lflag: u32,      // local mode flags
    pub c_line: u8,        // line discipline (always 0)
    pub c_cc: [u8; NCCS],  // control characters (19 bytes)
}

pub const TERMIOS_SIZE: usize = 36;
```

Field offsets verified by unit test against the Linux ABI:

| Offset | Field | Size | Notes |
|--------|-------|------|-------|
| 0 | `c_iflag` | 4 bytes | Input processing flags |
| 4 | `c_oflag` | 4 bytes | Output processing flags |
| 8 | `c_cflag` | 4 bytes | Hardware control flags |
| 12 | `c_lflag` | 4 bytes | Local (line discipline) flags |
| 16 | `c_line` | 1 byte | Line discipline ID (0 = N_TTY) |
| 17 | `c_cc` | 19 bytes | Control character array |

Total: 36 bytes. This matches the kernel `struct termios` on Linux
x86_64, which is the format that `ioctl(TCGETS)` (0x5401) copies.

The `Termios` types live in `kernel-core` so they can be unit-tested on
the host with `cargo test -p kernel-core` without QEMU. The test suite
verifies struct size, field offsets, and default flag values:

```rust
#[test]
fn termios_field_offsets() {
    let t = Termios::default_cooked();
    let base = &t as *const _ as usize;
    assert_eq!(&t.c_iflag as *const _ as usize - base, 0);
    assert_eq!(&t.c_oflag as *const _ as usize - base, 4);
    assert_eq!(&t.c_cflag as *const _ as usize - base, 8);
    assert_eq!(&t.c_lflag as *const _ as usize - base, 12);
    assert_eq!(&t.c_line as *const _ as usize - base, 16);
    assert_eq!(&t.c_cc   as *const _ as usize - base, 17);
}
```

### The `Winsize` Struct

Window size is an 8-byte `repr(C)` struct, also in `kernel-core`:

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Winsize {
    pub ws_row:    u16,
    pub ws_col:    u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}
```

The default is 24 rows by 80 columns (pixel dimensions zero, as the
framebuffer resolution is not plumbed through).

## Terminal Modes

The `c_lflag` field controls the three classic terminal modes:

| Mode | `c_lflag` | Behavior |
|------|-----------|----------|
| **Cooked** (canonical) | `ICANON \| ECHO \| ECHOE \| ISIG \| IEXTEN` | Input buffered per-line; edit keys active; signals generated |
| **cbreak** | `ECHO \| ISIG` (no `ICANON`) | Each byte delivered immediately; signal chars still active; echo on |
| **Raw** | 0 | Every byte delivered immediately; no processing whatsoever |

m3OS boots with cooked-mode defaults, matching a Linux login terminal:

```rust
pub const fn default_cooked() -> Self {
    Termios {
        c_iflag: ICRNL,
        c_oflag: OPOST | ONLCR,
        c_cflag: B38400 | CS8 | CREAD | HUPCL,
        c_lflag: ICANON | ECHO | ECHOE | ISIG | IEXTEN,
        c_line:  0,
        c_cc,   // ^C, ^D, DEL, ^U, etc.
    }
}
```

### `c_lflag` Flag Reference

| Constant | Octal | Meaning |
|----------|-------|---------|
| `ISIG` | `000001` | Enable signal-generating characters (VINTR, VQUIT, VSUSP) |
| `ICANON` | `000002` | Canonical mode — line-buffered input, edit chars active |
| `ECHO` | `000010` | Echo input characters back to the terminal |
| `ECHOE` | `000020` | VERASE prints `\x08 \x08` (visual backspace) |
| `ECHOK` | `000040` | VKILL visually erases the entire line |
| `ECHONL` | `000100` | Echo newlines even when `ECHO` is off |
| `IEXTEN` | `100000` | Extended processing (enables VWERASE, VLNEXT) |

## Line Discipline Processing

The line discipline runs in `stdin_feeder_task`. On each iteration the
task pulls one scancode from the keyboard server via IPC, translates it
to a byte, reads the current `termios` flags, and then processes the byte
according to the current mode.

### Input Flag Processing (c_iflag)

Before any line-editing logic, the raw byte passes through input flag
translation:

```rust
// ICRNL: translate CR (0x0D) to NL (0x0A) on input.
let byte = if (c_iflag & ICRNL != 0) && byte == b'\r' {
    b'\n'
} else {
    byte
};
```

The `ICRNL` flag is set by default. Most keyboards and serial terminals
send `\r` (0x0D) on Enter; `ICRNL` canonically converts it to `\n` so
that userspace always receives `\n` as the line terminator.

The `INLCR` and `IGNCR` flags (translate NL→CR, ignore CR) are defined
but not yet processed by the feeder.

### Signal Character Processing (ISIG)

When `ISIG` is set in `c_lflag`, the feeder checks the byte against
the signal characters in `c_cc` before any other processing:

```rust
if isig {
    let signal = if byte == c_cc_arr[VINTR] {
        Some((process::SIGINT, "^C"))
    } else if byte == c_cc_arr[VSUSP] {
        Some((process::SIGTSTP, "^Z"))
    } else if byte == c_cc_arr[VQUIT] {
        Some((process::SIGQUIT, "^\\"))
    } else {
        None
    };

    if let Some((sig, name)) = signal {
        let fg = process::FG_PGID.load(core::sync::atomic::Ordering::Relaxed);
        if fg != 0 {
            if canonical {
                tty::TTY0.lock().edit_buf.clear();
            }
            shell_print(my_id, console_ep, name);
            shell_print(my_id, console_ep, "\n");
            process::send_signal_to_group(fg, sig);
        } else {
            stdin::push_char(byte);
        }
        continue;
    }
}
```

Signal characters are checked against the `c_cc` array at runtime —
the kernel does not hardcode ASCII values. This means an application
can rebind them (e.g., move VINTR from Ctrl-C to Ctrl-A) via
`TCSETS`.

The edit buffer is cleared when a signal is sent so that partial input
from the interrupted line does not persist into the next prompt.

### Canonical Mode Edit Operations

When `ICANON` is set, bytes accumulate in `edit_buf` (a 4096-byte
array in `TtyState`). The line is delivered to the stdin buffer only
when a newline or `VEOF` arrives.

The feeder handles four edit operations in order:

**VERASE (default: DEL = 0x7F)** — Erase the last character:

```rust
if byte == c_cc_arr[VERASE] || byte == 0x7F {
    let erased = tty::TTY0.lock().edit_buf.erase_char();
    if erased.is_some() && echo_on && (c_lflag & ECHOE != 0) {
        shell_print(my_id, console_ep, "\x08 \x08");
    }
    continue;
}
```

The `"\x08 \x08"` sequence moves the cursor back one column, prints a
space to blank the character, and moves back again — the standard
destructive backspace sequence.

**VKILL (default: Ctrl-U = 0x15)** — Erase the entire line:

```rust
if byte == c_cc_arr[VKILL] {
    let n = tty::TTY0.lock().edit_buf.kill_line();
    if n > 0 && echo_on && (c_lflag & ECHOK != 0) {
        for _ in 0..n {
            shell_print(my_id, console_ep, "\x08 \x08");
        }
    }
    continue;
}
```

**VWERASE (default: Ctrl-W = 0x17)** — Erase the previous word.
Requires `IEXTEN` to be enabled conceptually (though the current
implementation processes it whenever present in `c_cc`):

```rust
if byte == c_cc_arr[VWERASE] {
    let n = tty::TTY0.lock().edit_buf.word_erase();
    if n > 0 && echo_on {
        for _ in 0..n {
            shell_print(my_id, console_ep, "\x08 \x08");
        }
    }
    continue;
}
```

`word_erase()` first skips trailing spaces, then erases non-space
characters back to the previous word boundary:

```rust
// kernel-core/src/tty.rs
pub fn word_erase(&mut self) -> usize {
    let orig = self.len;
    // Skip trailing spaces.
    while self.len > 0 && self.buf[self.len - 1] == b' ' {
        self.len -= 1;
    }
    // Erase non-space characters.
    while self.len > 0 && self.buf[self.len - 1] != b' ' {
        self.len -= 1;
    }
    orig - self.len
}
```

**VEOF (default: Ctrl-D = 0x04)** — End of file:

```rust
if byte == c_cc_arr[VEOF] {
    let mut t = tty::TTY0.lock();
    if t.edit_buf.is_empty() {
        drop(t);
        stdin::signal_eof();      // next read() returns 0
    } else {
        // Non-empty buffer: flush it without a newline terminator.
        let len = t.edit_buf.len;
        for i in 0..len {
            stdin::push_char(t.edit_buf.buf[i]);
        }
        t.edit_buf.clear();
    }
    continue;
}
```

When the edit buffer is empty, `VEOF` signals EOF to the stdin layer
by setting an atomic flag; `read()` will return 0. When the edit
buffer has content, `VEOF` flushes the buffered data without appending
`\n`, which is how `read()` can return a partial line (e.g., pressing
Ctrl-D mid-word in a shell).

**Newline** — Deliver the accumulated line:

```rust
if byte == b'\n' {
    let mut t = tty::TTY0.lock();
    let len = t.edit_buf.len;
    for i in 0..len {
        stdin::push_char(t.edit_buf.buf[i]);
    }
    t.edit_buf.clear();
    drop(t);
    stdin::push_char(b'\n');

    if echo_on || (c_lflag & ECHONL != 0) {
        if c_oflag & ONLCR != 0 {
            shell_print(my_id, console_ep, "\r\n");
        } else {
            shell_print(my_id, console_ep, "\n");
        }
    }
    continue;
}
```

The newline character itself is always appended to the delivered data
so that `fgets()` and similar functions can detect the line boundary.

### Raw / cbreak Mode

When `ICANON` is not set, every byte is pushed to the stdin buffer
immediately without buffering:

```rust
} else {
    // Raw / cbreak mode: push byte immediately.
    stdin::push_char(byte);

    if echo_on {
        if c_oflag & ONLCR != 0 && byte == b'\n' {
            shell_print(my_id, console_ep, "\r\n");
        } else {
            shell_print(my_id, console_ep, s);
        }
    }
}
```

In this mode, `VMIN` and `VTIME` semantics would apply (minimum bytes
before `read()` returns, optional timeout). The current implementation
delivers each byte as it arrives regardless of `VMIN`/`VTIME`.

## Output Processing (c_oflag)

Output flag processing is applied during echo and when userspace writes
to a TTY FD:

| Flag | Octal | Meaning |
|------|-------|---------|
| `OPOST` | `000001` | Enable output processing |
| `ONLCR` | `000004` | Map NL to CR+NL on output |

The `ONLCR` translation is applied in two places:

1. **During echo**: when the line is delivered (newline case above),
   the feeder sends `"\r\n"` instead of `"\n"` if `ONLCR` is set.
2. **During raw-mode echo**: same translation for each byte pushed in
   raw mode.

On a physical terminal or QEMU serial port, `\n` alone moves the
cursor down but not to column 0. `ONLCR` ensures that the terminal
cursor returns to the start of the next line.

## The `c_cc` Control Character Array

`c_cc` is a 19-element byte array indexed by the `V*` constants. The
default values match Linux:

| Index constant | Index | Default byte | Key |
|----------------|-------|--------------|-----|
| `VINTR` | 0 | 0x03 | Ctrl-C |
| `VQUIT` | 1 | 0x1C | Ctrl-\ |
| `VERASE` | 2 | 0x7F | DEL |
| `VKILL` | 3 | 0x15 | Ctrl-U |
| `VEOF` | 4 | 0x04 | Ctrl-D |
| `VTIME` | 5 | 0 | timeout (tenths of seconds) |
| `VMIN` | 6 | 1 | min bytes per `read()` |
| `VSTART` | 8 | 0x11 | Ctrl-Q (XON) |
| `VSTOP` | 9 | 0x13 | Ctrl-S (XOFF) |
| `VSUSP` | 10 | 0x1A | Ctrl-Z |
| `VEOL` | 11 | 0 | secondary EOL |
| `VWERASE` | 14 | 0x17 | Ctrl-W |
| `VLNEXT` | 15 | 0x16 | Ctrl-V |

All `c_cc` entries can be overwritten via `TCSETS`. An entry of `0xFF`
(or `_POSIX_VDISABLE`) disables that character.

## The `ioctl` Interface

Syscall 16 (`ioctl`) is the primary mechanism for userspace to read and
modify terminal settings. The kernel's `sys_linux_ioctl` function handles
all TTY-related request codes:

```rust
// kernel/src/arch/x86_64/syscall.rs
const TCGETS:     u64 = 0x5401;
const TCSETS:     u64 = 0x5402;
const TCSETSW:    u64 = 0x5403;
const TCSETSF:    u64 = 0x5404;
const TIOCGPGRP:  u64 = 0x540F;
const TIOCSPGRP:  u64 = 0x5410;
const TIOCGWINSZ: u64 = 0x5413;
const TIOCSWINSZ: u64 = 0x5414;
```

Any `ioctl` on a non-TTY file descriptor returns `ENOTTY (-25)`:

```rust
let is_tty = matches!(
    &backend,
    Some(FdBackend::DeviceTTY { .. })
        | Some(FdBackend::PtyMaster { .. })
        | Some(FdBackend::PtySlave { .. })
);
if !is_tty {
    return NEG_ENOTTY;
}
```

This is the mechanism that `isatty(3)` relies on: it calls
`ioctl(fd, TCGETS, &t)` and checks whether the return value is `ENOTTY`.

### `TCGETS` (0x5401) — Get Termios

Copies the 36-byte `TTY0.termios` to the userspace pointer:

```rust
TCGETS => {
    let tty = crate::tty::TTY0.lock();
    let src = unsafe {
        core::slice::from_raw_parts(
            &tty.termios as *const _ as *const u8,
            TERMIOS_SIZE,
        )
    };
    crate::mm::user_mem::copy_to_user(arg, src)?;
    0
}
```

### `TCSETS` (0x5402) — Set Termios (TCSANOW)

Applies the new settings immediately (no drain):

```rust
TCSETS => {
    let mut buf = [0u8; TERMIOS_SIZE];
    crate::mm::user_mem::copy_from_user(&mut buf, arg)?;
    let new_termios = unsafe {
        core::ptr::read_unaligned(buf.as_ptr() as *const kernel_core::tty::Termios)
    };
    crate::tty::TTY0.lock().termios = new_termios;
    0
}
```

### `TCSETSW` (0x5403) — Set Termios (TCSADRAIN)

On real terminals this drains pending output before applying settings.
Since m3OS output is synchronous (writes to the console server complete
before the syscall returns), this is equivalent to `TCSETS`.

### `TCSETSF` (0x5404) — Set Termios (TCSAFLUSH)

Flushes unread input before applying the new settings:

```rust
TCSETSF => {
    // ...read new_termios...
    crate::stdin::flush();                  // discard unread bytes
    let mut tty = crate::tty::TTY0.lock();
    tty.edit_buf.clear();                   // discard edit buffer
    tty.termios = new_termios;
    0
}
```

This is what programs like `ssh` and terminal emulators use when
switching from cooked to raw mode: they want to discard any partially
typed input that was buffered before the mode change.

### `TIOCGPGRP` / `TIOCSPGRP` (0x540F / 0x5410) — Process Group

```rust
TIOCGPGRP => {
    let tty = crate::tty::TTY0.lock();
    let pgid = tty.fg_pgid;
    let bytes = (pgid as i32).to_ne_bytes();
    crate::mm::user_mem::copy_to_user(arg, &bytes)?;
    0
}

TIOCSPGRP => {
    let mut bytes = [0u8; 4];
    crate::mm::user_mem::copy_from_user(&mut bytes, arg)?;
    let pgid = i32::from_ne_bytes(bytes) as u32;
    crate::tty::TTY0.lock().fg_pgid = pgid;
    crate::process::FG_PGID.store(pgid, Ordering::Relaxed);
    0
}
```

Shell job control uses `TIOCSPGRP` to move a background job into the
foreground. The shell sets the TTY's foreground group to the child's
process group before waiting for it.

### `TIOCGWINSZ` / `TIOCSWINSZ` (0x5413 / 0x5414) — Window Size

```rust
TIOCGWINSZ => {
    let tty = crate::tty::TTY0.lock();
    let src = unsafe {
        core::slice::from_raw_parts(
            &tty.winsize as *const _ as *const u8,
            WINSIZE_SIZE,
        )
    };
    crate::mm::user_mem::copy_to_user(arg, src)?;
    0
}

TIOCSWINSZ => {
    // ...read new_ws...
    let changed = tty.winsize.ws_row != new_ws.ws_row
               || tty.winsize.ws_col != new_ws.ws_col;
    tty.winsize = new_ws;
    let fg = tty.fg_pgid;
    drop(tty);
    if changed && fg != 0 {
        crate::process::send_signal_to_group(fg, crate::process::SIGWINCH);
    }
    0
}
```

`TIOCSWINSZ` sends `SIGWINCH` (signal 28) to the foreground process
group only when the dimensions actually change. The default disposition
for `SIGWINCH` is `Ignore`, so unless the program has installed a
handler (e.g., a terminal editor recalculating layout), it has no
effect.

## `FdBackend::DeviceTTY` and `isatty()`

Every new process is created with FDs 0, 1, and 2 all pointing to
`FdBackend::DeviceTTY { tty_id: 0 }`:

```rust
// kernel/src/process/mod.rs
fn new_fd_table() -> [Option<FdEntry>; MAX_FDS] {
    let mut table = [NONE_FD; MAX_FDS];
    table[0] = Some(FdEntry {
        backend: FdBackend::DeviceTTY { tty_id: 0 },
        readable: true, writable: false, ..
    });
    table[1] = Some(FdEntry {
        backend: FdBackend::DeviceTTY { tty_id: 0 },
        readable: false, writable: true, ..
    });
    table[2] = Some(FdEntry {
        backend: FdBackend::DeviceTTY { tty_id: 0 },
        readable: false, writable: true, ..
    });
    table
}
```

The `tty_id` field is reserved for multi-TTY support. Currently only
`tty_id: 0` (the single console TTY0) exists.

### Read Path (FD 0)

`sys_read` on a `DeviceTTY` fd yield-loops on the stdin circular
buffer until data is available, interrupting on pending signals:

```rust
FdBackend::Stdin | FdBackend::DeviceTTY { .. } => {
    loop {
        if crate::stdin::has_data() {
            let n = crate::stdin::read(&mut tmp[..capped]);
            if n > 0 {
                copy_to_user(buf_ptr, &tmp[..n])?;
                return n as u64;
            }
        }
        if has_pending_signal() {
            return NEG_EINTR;
        }
        crate::task::yield_now();
        restore_caller_context(pid, saved_user_rsp);
    }
}
```

### Write Path (FD 1 / FD 2)

Writes to a `DeviceTTY` fd go through the console server via IPC,
exactly as `FdBackend::Stdout` would. The `fstat` handler returns
`S_IFCHR | 0o620` (character device, permissions `rw--w----`) with
an `rdev` encoding of `(5 << 8) | tty_id`, matching the Linux
`/dev/tty0` device number.

### `isatty()` via `ENOTTY`

musl's `isatty(fd)` implementation calls `ioctl(fd, TCGETS, &t)` and
returns 1 if the call succeeds (returns 0) and 0 if it returns
`ENOTTY`. Because `DeviceTTY` FDs successfully handle `TCGETS` while
all other FD types return `ENOTTY`, the standard `isatty()` function
works correctly without any special-case kernel code.

### `poll()` Integration

`DeviceTTY` FDs participate in `poll()` by checking the stdin buffer:

```rust
FdBackend::DeviceTTY { .. } | FdBackend::Stdin => {
    if crate::stdin::has_data() {
        revents = events & POLLIN;
    }
}
```

## The Stdin Circular Buffer

The stdin buffer (`kernel/src/stdin.rs`) is a 4096-byte circular buffer
with a separate EOF flag:

```rust
const STDIN_BUF_SIZE: usize = 4096;

struct StdinState {
    buf: [u8; STDIN_BUF_SIZE],
    read_pos: usize,
    count: usize,
}

static STDIN: Mutex<StdinState> = Mutex::new(StdinState::new());
static EOF_PENDING: AtomicBool = AtomicBool::new(false);
```

The EOF flag is separate from the data buffer so that `has_data()`
returns `true` when EOF is pending (waking a blocked `read()`), and
`read()` can atomically consume the flag and return 0:

```rust
pub fn read(dst: &mut [u8]) -> usize {
    if EOF_PENDING.compare_exchange(true, false, Acquire, Relaxed).is_ok() {
        return 0;
    }
    STDIN.lock().read(dst)
}
```

`TCSETSF` calls `stdin::flush()` to discard the buffer and clear the
EOF flag, ensuring a clean slate when switching terminal modes.

## PTY Skeleton Stubs

Phase 22 allocates a monotonic PTY pair ID counter and defines three
new `FdBackend` variants, but defers the actual read/write data path:

```rust
// kernel/src/tty.rs
static NEXT_PTY_ID: AtomicU32 = AtomicU32::new(0);

pub fn alloc_pty() -> u32 {
    NEXT_PTY_ID.fetch_add(1, Ordering::Relaxed)
}
```

```rust
// kernel/src/process/mod.rs
FdBackend::PtyMaster { pty_id: u32 },
FdBackend::PtySlave  { pty_id: u32 },
```

Read and write syscalls on PTY master/slave FDs return `ENOSYS`.
`ioctl` on a PTY master supports `TIOCGPTN` (return the PTY number):

```rust
if req == TIOCGPTN {
    if let Some(FdBackend::PtyMaster { pty_id }) = &backend {
        copy_to_user(arg, &(*pty_id).to_ne_bytes())?;
        return 0;
    }
}
```

The no-op stubs `TIOCSPTLCK` (unlock PTY) and `TIOCGRANTPT` (grant
PTY access) return success so that programs calling `posix_openpt()` +
`grantpt()` + `unlockpt()` do not immediately fail.

## Window Size and SIGWINCH

The terminal window size is stored as a `Winsize` in `TTY0` and exposed
via `TIOCGWINSZ`. Applications that care about the terminal dimensions
(editors, pagers, `tput cols`) call `TIOCGWINSZ` at startup.

When the window is resized (e.g., QEMU window resized, or a future
terminal emulator sending `TIOCSWINSZ`), the kernel:

1. Updates `TTY0.winsize` atomically under the TTY mutex.
2. Checks whether the new dimensions differ from the old.
3. If changed and a foreground process group exists, calls
   `send_signal_to_group(fg_pgid, SIGWINCH)`.

The flow from `TIOCSWINSZ` to SIGWINCH delivery:

```
userspace: ioctl(tty_fd, TIOCSWINSZ, &new_ws)
                |
                v
         sys_linux_ioctl(TIOCSWINSZ)
                |
         tty.winsize = new_ws
         if changed && fg != 0:
                |
                v
         send_signal_to_group(fg, 28 /* SIGWINCH */)
                |
                v
         for each pid in process group:
             proc.pending_signals |= (1 << 28)
                |
                v
         on next syscall return:
             check_pending_signals() → deliver SIGWINCH
             default disposition: Ignore
             (or invoke user handler if installed)
```

`SIGWINCH` (28) has default disposition `Ignore` in the process signal
table. A terminal application that wants to react to resizes must
install a handler via `rt_sigaction`.

## Data Flow Summary

```
KEY PRESS
    |
    v
[keyboard interrupt]
    |  scancode pushed to kbd ring buffer
    |
    v
[stdin_feeder_task] (kernel task)
    |
    |--- reads c_lflag, c_iflag, c_oflag, c_cc from TTY0
    |
    +--- ICRNL: \r -> \n
    |
    +--- ISIG: VINTR -> SIGINT
    |          VSUSP -> SIGTSTP    --> send_signal_to_group()
    |          VQUIT -> SIGQUIT
    |
    +--- ICANON mode:
    |       VERASE -> edit_buf.erase_char()
    |       VKILL  -> edit_buf.kill_line()
    |       VWERASE-> edit_buf.word_erase()
    |       VEOF   -> stdin::signal_eof() or flush buffer
    |       '\n'   -> flush edit_buf + push '\n' to stdin
    |       other  -> edit_buf.push(byte)
    |
    +--- raw mode:
    |       stdin::push_char(byte)
    |
    v
[stdin circular buffer]
    |
    v
sys_read(fd=0) on FdBackend::DeviceTTY
    |  yield-loop until has_data()
    |
    v
userspace read(0, buf, n) returns
```

## Limitations

The following are not implemented in this phase:

- `VMIN` and `VTIME` non-blocking read semantics. Currently raw mode
  delivers each byte as it arrives, which is `VMIN=1, VTIME=0`. A
  `VMIN=0` non-blocking read and `VTIME`-based timeouts are not
  implemented.
- `INLCR` and `IGNCR` input flag processing. The constants are defined
  but the feeder does not apply them.
- Output processing for `sys_write` (FD 1/2). `ONLCR` is only applied
  during echo from the feeder, not during `write()` to a TTY FD. A
  userspace `write(1, "hello\n", 6)` does not get `\r\n` translation.
- `OPOST` is not checked before output processing. All output is
  treated as if `OPOST` is set.
- `XOFF`/`XON` software flow control (`IXON`/`IXOFF` flags, `VSTART`/
  `VSTOP` characters). `c_cc[VSTART]` and `c_cc[VSTOP]` have correct
  defaults but are not acted on.
- `VLNEXT` (Ctrl-V, "literal next") is not implemented. Ctrl-V is
  supposed to cause the next character to be treated as a literal byte
  rather than a control character.
- PTY read/write data paths. `FdBackend::PtyMaster` and `PtySlave`
  return `ENOSYS` for all I/O. A ring buffer connecting master and
  slave is deferred to Phase 23+.
- Multiple TTY instances. `tty_id` is reserved in the `DeviceTTY`
  variant but all FDs point to `tty_id: 0`. Supporting `/dev/tty1`,
  `/dev/pts/N` etc. is deferred.
- `tcgetattr`/`tcsetattr` for the serial console (COM1). The serial
  stdin feeder task bypasses the TTY layer entirely; it pushes bytes
  directly to the stdin buffer without consulting `termios`.
- `TIOCSCTTY` (set controlling terminal). The controlling terminal
  concept exists implicitly (TTY0 is always the controlling terminal
  of every process) but the ioctl is not implemented.
