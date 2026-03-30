# Phase 29 — PTY Subsystem

## Overview

Phase 29 implements pseudo-terminal (PTY) pairs, enabling multiple
independent terminal sessions. A PTY master/slave pair connects a
terminal emulator (or network server) to a shell process — this is the
mechanism behind SSH, screen, tmux, and xterm.

The implementation provides a fixed pool of 16 PTY pairs with
bidirectional ring buffers, per-PTY terminal settings, line discipline
processing on the slave side, session management (`setsid`/`getsid`),
and controlling terminal association (`TIOCSCTTY`/`TIOCNOTTY`).

## How PTYs Work

A PTY pair consists of two connected endpoints:

```
 Master side                    Slave side
 (terminal emulator,     ←→    (shell, application)
  SSH server, screen)

 write(master) ──→ m2s ring buffer ──→ read(slave)
 read(master)  ←── s2m ring buffer ←── write(slave)
```

- **Master side**: The controlling application. Writing to the master
  feeds input to the slave (as if the user typed it). Reading from the
  master receives the slave's output (as if displayed on a screen).

- **Slave side**: Looks and behaves like a real terminal. The slave
  has its own termios settings, line discipline, and foreground process
  group — identical to a hardware console.

The line discipline sits between the master and slave on the input
path. When the slave is in canonical (cooked) mode, input from the
master is line-buffered, echoed, and signal characters (^C, ^Z, ^\)
generate signals. In raw mode, bytes pass through unprocessed.

## Data Structures

### PtyRingBuffer (kernel-core)

A 4 KiB circular buffer, same design as the pipe ring buffer but
without reader/writer refcounts (PTY lifecycle is managed separately).

```rust
pub struct PtyRingBuffer {
    buf: [u8; 4096],
    read_pos: usize,
    count: usize,
}
```

Methods: `read()`, `write()`, `is_empty()`, `is_full()`, `available()`,
`space()`. All operations are bounded — writes return the number of
bytes accepted, reads return the number of bytes delivered.

### PtyPairState (kernel-core)

Per-PTY pair state containing both ring buffers and terminal settings:

```rust
pub struct PtyPairState {
    pub m2s: PtyRingBuffer,        // master-to-slave (input to slave)
    pub s2m: PtyRingBuffer,        // slave-to-master (output from slave)
    pub termios: Termios,          // slave-side terminal settings
    pub winsize: Winsize,          // terminal window size (24x80 default)
    pub edit_buf: EditBuffer,      // canonical-mode line editing buffer
    pub slave_fg_pgid: u32,        // foreground process group on slave
    pub master_refcount: u32,      // open FD references to master side
    pub slave_refcount: u32,       // open FD references to slave side
    pub eof_pending: bool,         // ^D on empty line sets EOF for next read
    pub locked: bool,              // slave locked until unlockpt()
}
```

A new pair starts with `master_refcount = 1`, `slave_refcount = 0`,
`locked = true`. Refcounts are incremented on fork/dup and decremented
on close. SIGHUP is only sent when the last master reference closes
(refcount reaches 0). The slave must be unlocked via `TIOCSPTLCK`
before it can be opened.

### PTY Table (kernel)

A fixed array of 16 `Option<PtyPairState>` slots protected by a
spinlock mutex. `alloc_pty()` scans for the first `None` slot.
`free_pty()` sets the slot back to `None` when both master and slave
are closed.

```rust
pub static PTY_TABLE: Mutex<[Option<PtyPairState>; 16]>;
```

## Device Interface

### /dev/ptmx

Opening `/dev/ptmx` allocates a new PTY pair and returns the master
file descriptor. The corresponding slave starts locked.

```
fd = open("/dev/ptmx", O_RDWR);  // allocates PTY pair, returns master fd
```

### /dev/pts/N

Opening `/dev/pts/N` returns the slave file descriptor for PTY number
N. Fails with `-EIO` if the slave is still locked, `-ENOENT` if the
PTY doesn't exist.

### Typical openpty() sequence

```
master_fd = open("/dev/ptmx");
ioctl(master_fd, TIOCSPTLCK, 0);     // unlock slave
ioctl(master_fd, TIOCGPTN, &num);    // get PTY number
slave_fd = open("/dev/pts/{num}");    // open slave
```

The `openpty()` wrapper in `syscall-lib` encapsulates this sequence.

## I/O Data Paths

### Master write → slave read (input path)

When the master writes, bytes flow through the line discipline before
reaching the slave reader:

1. **Input flag transforms**: ICRNL (CR→NL), INLCR (NL→CR), IGNCR
   (ignore CR) are applied per the slave's `c_iflag`.

2. **Signal generation** (ISIG): If the byte matches a signal
   character (VINTR=^C, VQUIT=^\, VSUSP=^Z), the corresponding
   signal is sent to the slave's foreground process group. The byte
   is consumed and not delivered.

3. **Canonical mode** (ICANON): Bytes are buffered in the `EditBuffer`.
   Line editing (VERASE=backspace, VKILL=kill line, VWERASE=word
   erase) is processed. A complete line (terminated by newline or
   VEOF=^D) is made available for the slave's `read()`.

4. **Echo** (ECHO): Input characters are echoed back through the s2m
   buffer so they appear on the master's read side. ECHOE echoes
   erase as backspace-space-backspace. ECHOK echoes kill as newline.

5. **Raw mode** (!ICANON): Bytes go directly into the m2s ring buffer
   without buffering or line editing.

### Slave write → master read (output path)

Output processing is simpler:

1. **OPOST + ONLCR**: When output processing is enabled, newlines are
   converted to CR-NL pairs (standard terminal behavior).

2. Bytes go into the s2m ring buffer for the master to read.

## Blocking Behavior

PTY reads use the same yield-loop pattern as pipe reads:

- **Master read**: Yields until s2m has data, or returns EOF (0) if
  slave is closed.
- **Slave read (canonical)**: Yields until edit buffer contains a
  complete line (newline present).
- **Slave read (raw)**: Yields until m2s has data.
- **Both**: Return `EINTR` if a signal is pending during the wait.

## Session Management

### setsid() (syscall 112)

Creates a new session. The calling process becomes the session leader:
- `session_id` is set to the caller's PID
- `pgid` is set to the caller's PID (new process group)
- `controlling_tty` is cleared (no controlling terminal)

Fails with `EPERM` if the caller is already a session leader or if
another process shares the caller's process group ID.

### getsid(pid) (syscall 124)

Returns the session ID of the specified process (or calling process
if `pid == 0`).

### Controlling Terminal

After `setsid()`, a process can acquire a controlling terminal via
`ioctl(slave_fd, TIOCSCTTY, 0)`. The process must be a session leader
with no existing controlling terminal. `TIOCNOTTY` releases it.

The `controlling_tty` field in the Process struct tracks this:

```rust
pub enum ControllingTty {
    Console,    // hardware console (TTY0)
    Pty(u32),   // pseudo-terminal with given PTY ID
}
```

Processes started from the console default to
`Some(ControllingTty::Console)`. After `setsid()`, it becomes `None`
until `TIOCSCTTY` assigns a PTY.

## Signal Delivery

### SIGHUP on master close

When the last FD referencing a PTY master is closed:

1. `SIGHUP` is sent to the slave's foreground process group
2. `SIGCONT` is sent to the same group (in case processes are stopped)
3. The PTY is marked as disconnected (`master_open = false`)
4. Subsequent slave reads return EOF (0)
5. Subsequent slave writes return `-EIO`

This matches Unix behavior: closing an SSH connection sends SIGHUP to
all processes in the remote shell session.

### Signal generation via control characters

When ISIG is enabled on the slave's termios:

| Character | Signal | Default key |
|---|---|---|
| VINTR | SIGINT | ^C (0x03) |
| VQUIT | SIGQUIT | ^\ (0x1C) |
| VSUSP | SIGTSTP | ^Z (0x1A) |

Signals are delivered to the slave's foreground process group via
`send_signal_to_group()`.

## Ioctl Dispatch

All terminal ioctls (TCGETS, TCSETS, TIOCGPGRP, TIOCSPGRP,
TIOCGWINSZ, TIOCSWINSZ) check whether the FD is a PTY. If so, they
operate on the PTY pair's state instead of the console's TTY0:

| Ioctl | PTY behavior |
|---|---|
| TCGETS/TCSETS/TCSETSW/TCSETSF | Get/set the PTY's termios |
| TIOCGPGRP/TIOCSPGRP | Get/set the PTY's slave_fg_pgid |
| TIOCGWINSZ/TIOCSWINSZ | Get/set the PTY's winsize (SIGWINCH on change) |
| TIOCGPTN | Return PTY number (master FDs only) |
| TIOCSPTLCK | Lock/unlock slave (master FDs only) |
| TIOCGRANTPT | No-op (permissions not enforced yet) |
| TIOCSCTTY | Set controlling terminal (slave FDs, session leader only) |
| TIOCNOTTY | Release controlling terminal |

## FD Lifecycle

PTY FDs use the existing `FdBackend::PtyMaster { pty_id }` and
`FdBackend::PtySlave { pty_id }` variants. Multiple FDs can reference
the same PTY pair (e.g., after fork or dup2).

- **fork**: Child inherits all PTY FDs. No re-allocation — they share
  the same PTY pair via the global PTY_TABLE.
- **exec (cloexec)**: FDs with `cloexec = true` are closed during
  exec, triggering the same close logic (master close → SIGHUP,
  slave close → lifecycle check).
- **close**: Sets `master_open = false` or `slave_open = false`. If
  both are false, `free_pty()` releases the slot.

## Userspace API

`syscall-lib` provides these wrappers:

```rust
pub fn openpty() -> Result<(i32, i32), i32>;  // (master_fd, slave_fd)
pub fn setsid() -> i64;                        // returns session ID
pub fn getsid(pid: u32) -> i64;               // returns session ID
```

## Testing

### Host tests (cargo test -p kernel-core)

7 tests for `PtyRingBuffer`: read/write, wraparound, full, partial
read, partial write, zero-length operations, pair state defaults.

### QEMU userspace test (pty-test binary)

8 acceptance tests run inside QEMU:
1. `ptmx_open` — open `/dev/ptmx` returns valid FD
2. `tiocgptn` — TIOCGPTN returns PTY number 0..15
3. `slave_open` — unlock + open `/dev/pts/N` succeeds
4. `slave_locked` — open locked slave returns error
5. `raw_io` — master→slave raw mode data transfer
6. `s2m_io` — slave→master raw mode data transfer
7. `multiple_ptys` — allocate 8 simultaneous pairs
8. `master_close_eof` — master close delivers EOF to slave reader

## Deferred Items

These are explicitly out of scope for Phase 29:

- `/dev/tty` generic controlling terminal device
- Packet mode (`TIOCPKT`) for flow control signaling
- Terminal multiplexer (screen/tmux-style)
- Dynamic PTY allocation beyond the fixed 16-slot pool
- PTY ownership and permission enforcement (`grantpt()` is a no-op)
- Orphaned process group handling (full POSIX job control)
- Background process group stop (`SIGTTIN`/`SIGTTOU`)
- Multiple line disciplines (only N_TTY supported)
- Real `/dev/pts` filesystem (devpts)

## Files Changed

| File | Change |
|---|---|
| `kernel-core/src/pty.rs` | **New** — PtyRingBuffer, PtyPairState, unit tests |
| `kernel-core/src/lib.rs` | Added `pub mod pty` |
| `kernel/src/pty.rs` | **New** — PTY_TABLE, alloc/free/close lifecycle |
| `kernel/src/main.rs` | Added `mod pty` |
| `kernel/src/tty.rs` | Removed old `alloc_pty` skeleton |
| `kernel/src/process/mod.rs` | Added `session_id`, `controlling_tty`, `ControllingTty` enum; PTY cleanup in close/cloexec |
| `kernel/src/arch/x86_64/syscall.rs` | PTY read/write/close/open/ioctl, setsid/getsid, fork session inheritance |
| `userspace/syscall-lib/src/lib.rs` | `openpty()`, `setsid()`, `getsid()` wrappers |
| `userspace/pty-test/` | **New** — PTY acceptance test program |
| `kernel/Cargo.toml` | Version bump to 0.29.0 |
