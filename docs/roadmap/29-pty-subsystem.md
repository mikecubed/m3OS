# Phase 29 — PTY Subsystem

**Status:** Complete
**Source Ref:** phase-29
**Depends on:** Phase 22 (TTY) ✅, Phase 27 (User Accounts) ✅
**Builds on:** Extends the TTY/termios infrastructure from Phase 22 with master/slave PTY pairs; uses user ownership from Phase 27 for PTY permissions
**Primary Components:** kernel/src/pty.rs, kernel-core/src/pty.rs, userspace/pty-test/

## Milestone Goal

Implement real pseudo-terminal (PTY) pairs so that multiple independent terminal
sessions can run simultaneously. A PTY master/slave pair connects a terminal emulator
(or network server) to a shell process, enabling remote access (telnet, SSH) and local
terminal multiplexing.

## Why This Phase Exists

The OS has a single console TTY from Phase 22, which means only one interactive session
can run at a time. To support remote login (telnet/SSH), terminal multiplexers, or
multiple local terminals, the kernel needs a way to dynamically create virtual terminal
pairs. PTYs are the Unix mechanism for this: a master side driven by an application
(e.g., a telnet server) and a slave side that looks like a real terminal to the shell.
Without PTYs, the OS cannot be a multi-session, networked system.

## Learning Goals

- Understand the Unix PTY model: master side (application) and slave side (shell).
- Learn how terminal line discipline sits between the PTY pair, processing input/output.
- See why PTYs are the mechanism that makes SSH, screen, tmux, and xterm work.
- Understand the `openpty`/`posix_openpt`/`grantpt`/`unlockpt` API.

## Feature Scope

### Kernel Changes

- **PTY device pairs**: Allocate master/slave fd pairs. Writing to the master appears as
  input on the slave (and vice versa). The slave side has a termios line discipline.
- **`/dev/ptmx`** — opening this device allocates a new PTY pair and returns the master fd.
- **`/dev/pts/N`** — the slave side of PTY number N.
- New syscalls / ioctls:
  - `openpty()` (or `posix_openpt` + `grantpt` + `unlockpt` + `ptsname`)
  - `TIOCSCTTY` — set controlling terminal
  - `TIOCNOTTY` — release controlling terminal
  - `TIOCGPTN` — get PTY number
- **Session and process group support**: `setsid()` to create a new session, so the
  slave PTY becomes the controlling terminal for the session leader.
- Connect PTY slave to existing termios infrastructure from Phase 22.

### Userspace Demonstration

- A simple `screen`-like program that opens a PTY pair and forwards I/O between the
  real terminal and the PTY, proving the plumbing works.
- Or: spawn a second shell on a PTY and switch between them.

## Important Components and How They Work

### PTY Allocator

A kernel-side table of PTY pairs, each with a ring buffer connecting master and slave.
Opening `/dev/ptmx` allocates a new entry from the table and returns the master fd.
The corresponding slave is accessible as `/dev/pts/N`.

### Ring Buffers

Each PTY pair uses ring buffers (implemented in `kernel-core/src/pty.rs`) to pass data
between master and slave sides. Writing to the master enqueues bytes that the slave
reads, and vice versa.

### Session Management

`setsid()` creates a new session, and `TIOCSCTTY` assigns the PTY slave as the
controlling terminal for that session. This ensures signals like `SIGHUP` are delivered
correctly when the master side closes.

## How This Builds on Earlier Phases

- **Extends Phase 22 (TTY):** Reuses the termios line discipline and cooked/raw mode processing for the PTY slave side.
- **Extends Phase 19 (Signals):** Uses SIGHUP and job control signals delivered through the PTY controlling terminal.
- **Extends Phase 27 (User Accounts):** PTY ownership and permissions are tied to the logged-in user.

## Implementation Outline

1. Implement the PTY allocator: a table of PTY pairs, each with a ring buffer connecting
   master and slave.
2. Implement `/dev/ptmx` as a special device that allocates a new PTY pair.
3. Connect the slave side to the termios line discipline from Phase 22.
4. Implement `setsid()` syscall for creating new sessions.
5. Implement `TIOCSCTTY` ioctl to assign a controlling terminal.
6. Modify `fork`/`exec` in login/shell to open a PTY slave as stdin/stdout/stderr for
   new sessions.
7. Write a simple test program that spawns a shell on a PTY and relays I/O.
8. Verify that terminal features (raw mode, ANSI escapes, signals) work through PTYs.

## Acceptance Criteria

- Opening `/dev/ptmx` returns a master fd and allocates a `/dev/pts/N` slave.
- Writing to the master fd appears as input on the slave fd.
- Writing to the slave fd appears as output on the master fd.
- termios settings (raw mode, echo, line discipline) work on the slave side.
- A shell spawned on a PTY slave behaves identically to the console shell.
- `setsid()` creates a new session; `TIOCSCTTY` assigns the controlling terminal.
- `SIGHUP` is delivered to the foreground process group when the master fd is closed.
- At least 8 simultaneous PTY pairs can be allocated.

## Companion Task List

- [Phase 29 Task List](./tasks/29-pty-subsystem-tasks.md)

## How Real OS Implementations Differ

Linux has evolved through several PTY implementations:
- Legacy BSD PTYs (`/dev/ptyXX` + `/dev/ttyXX` pairs, fixed count)
- Unix98 PTYs (`/dev/ptmx` + `/dev/pts/N`, dynamically allocated) — this is what we implement
- The PTY layer in Linux is deeply integrated with the TTY subsystem, supporting
  multiple line disciplines, packet mode, and window size propagation.

Our implementation is simplified: a fixed pool of PTY pairs with basic ring buffers.
This is sufficient for remote access and terminal multiplexing.

## Deferred Until Later

- Packet mode (for flow control signaling)
- Window size change propagation (`SIGWINCH` through PTY)
- Terminal multiplexer (screen/tmux-style)
- Dynamic PTY allocation beyond the fixed pool
- `/dev/tty` (controlling terminal device node)
