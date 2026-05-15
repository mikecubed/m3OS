# Termios Raw Mode and Line Discipline

**Aligned Roadmap Phase:** Phase 69a
**Status:** Complete
**Source Ref:** phase-69a
**Supersedes Legacy Doc:** new

## Overview

Phase 22 brought up the kernel TTY with a minimal `termios` struct
that only honoured `ICANON`, `ECHO`, and `ISIG`; Phase 29 added the
PTY pair but inherited the same minimal-termios caveat.  Phase 69a
closes both: the kernel-core `Termios` is widened to the full POSIX
shape (4 mode words plus a 19-byte control-character array), every
flag bit listed in the table below is actually wired into the
line-discipline state machine on both the console TTY0 and per-PTY-pair
ldisc, the four POSIX `VMIN` / `VTIME` quadrants are honoured by the
PTY slave read path through a `WaitQueue`-deadline park, and the four
`TCGETS` / `TCSETS` / `TCSETSW` / `TCSETSF` ioctls are wired against
both surfaces.  Userspace gets `cfmakeraw` and a `tcsetattr_when(when)`
helper in `syscall-lib`, plus a new `tcsmoke` validator binary driven
by the `cargo xtask termios-smoke` gate.

## What This Doc Covers

- The full POSIX `termios` flag layout and which bits this phase wires
  through to actual ldisc behaviour.
- The cooked / raw / cbreak modes and how `ICANON` flips the slave
  read path between buffered-line and byte-by-byte delivery.
- The four POSIX `VMIN` / `VTIME` raw-mode read quadrants.
- The `ISIG` signal-from-terminal path (VINTR/VQUIT/VSUSP →
  SIGINT/SIGQUIT/SIGTSTP via `send_signal_to_group`).
- `tcgetattr` / `tcsetattr` / `cfmakeraw` on both kernel TTY0 and the
  PTY slave; the PTY master returns `-ENOTTY`.

## Core Implementation

The pure-logic line-discipline state lives in
`kernel-core/src/tty.rs::LineDiscipline`: a single `process_byte`
function consumes one byte at a time, applies the input-mode
transforms (IGNCR/INLCR/ICRNL/IXON/IXOFF, and IEXTEN's VLNEXT /
VDISCARD), checks ISIG against the c_cc signal characters, and either
buffers the byte in the canonical edit buffer or returns it for raw
delivery.  The PTY pair (`kernel-core/src/pty.rs::PtyPairState`)
carries two new bits next to the existing termios: `ldisc_output_suspended`
(set by VSTOP under IXON, cleared by VSTART) and `ldisc_deadline_ticks`
(the absolute monotonic-tick deadline for the active VMIN/VTIME timer).

The kernel side dispatches:

- `pty_master_write` (`kernel/src/arch/x86_64/syscall/mod.rs`): inline
  IGNCR/INLCR/ICRNL → IXON VSTOP/VSTART → IEXTEN VLNEXT/VDISCARD →
  ISIG signal-from-terminal → canonical edit-buffer or raw m2s.
- `pty_slave_read` + `block_on_pty_slave_read`: VMIN/VTIME four-case
  decision plus a `WaitQueue` park that wakes on either a byte event
  or the absolute-tick deadline.
- `sys_linux_ioctl::sys_linux_ioctl`: TCGETS/TCSETS/TCSETSW/TCSETSF on
  TTY0 and PTY slave; PTY master returns `-ENOTTY`.
- `sys_linux_write` Stdout / DeviceTTY arm: honours OPOST + ONLCR so
  Phase 69's wire-protocol assumption (raw bytes when OPOST is off)
  holds on the kernel TTY0 path too.

Userspace consumes the contract via `userspace/syscall-lib`:
`Termios`, `tcgetattr`, `tcsetattr`, `tcsetattr_when(when)` (with
TCSANOW / TCSADRAIN / TCSAFLUSH), and `cfmakeraw`.

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/tty.rs` | Widened `Termios` + every flag constant + `cooked_default`/`raw_default`; `LineDiscipline` state machine including IXON output-suspension, IEXTEN VLNEXT-pending, raw-buffered byte counter, VMIN/VTIME poll/arm helpers. |
| `kernel-core/src/pty.rs` | Per-pair `ldisc_output_suspended` and `ldisc_deadline_ticks`. |
| `kernel/src/tty.rs` | Console TTY0 owns the active `LineDiscipline` + foreground process group. |
| `kernel/src/arch/x86_64/syscall/mod.rs` | TCGETS/TCSETS/TCSETSW/TCSETSF ioctl branches; `pty_master_write` IXON/VLNEXT/VDISCARD plumbing; `pty_slave_read` + `block_on_pty_slave_read` VMIN/VTIME timer; Stdout/DeviceTTY OPOST+ONLCR handling. |
| `userspace/syscall-lib/src/lib.rs` | `Termios`, `tcgetattr`, `tcsetattr`, `tcsetattr_when`, `cfmakeraw`, `TCSANOW`/`TCSADRAIN`/`TCSAFLUSH`, full IXON/IXOFF/IUTF8/ECHOK/ECHONL/Vxxxx constants. |
| `userspace/tcsmoke/src/main.rs` | Validator binary with subcommands `round-trip` / `icanon-off` / `echo-off` / `vmin-vtime` / `isig` / `opost-off`. |
| `xtask/src/main.rs` | `cmd_termios_smoke` + `TC_SMOKE_SUBCOMMANDS` driving the post-login subcommand harness. |

## How This Phase Differs From Earlier Termios Work

- **Phase 22** brought up the console TTY ldisc and a 36-byte termios
  struct, but only `ICANON | ECHO | ISIG` actually changed behaviour.
  IGNCR / INLCR / ICRNL were wired in pieces; IXON / IXOFF / IUTF8 /
  IEXTEN / VMIN / VTIME / OPOST-off were no-ops.  Phase 69a closes
  every one of those gaps without changing the on-disk struct shape.
- **Phase 29** added the per-PTY-pair termios but kept the same
  caveat — the PTY slave read path special-cased canonical mode and
  fell back to "drain m2s" in raw mode without honouring VMIN/VTIME.
  Phase 69a threads the four-quadrant timer through the kernel
  `WaitQueue` deadline.

## Closure of Related Phases

- **Phase 22 — TTY/PTY:** the "minimal termios" caveat at the bottom
  of `docs/roadmap/22-tty-pty.md` (cooked/raw switching, window size,
  skeleton PTY allocation) is closed.  After 69a the kernel ldisc
  honours the full POSIX flag set on both the kernel TTY0 and PTY
  paths.
- **Phase 29 — PTY Subsystem:** the PTY-side termios contract
  (TCGETS/TCSETS/TCSETSW/TCSETSF on the slave fd, full
  c_iflag/c_oflag/c_lflag/c_cc plumbing, VMIN/VTIME timer through
  `pty_slave_read`, master TCGETS → ENOTTY) is closed in this phase.

## Related Roadmap Docs

- [Phase 69a roadmap doc](./roadmap/69a-terminal-termios.md)
- [Phase 69a task doc](./roadmap/tasks/69a-terminal-termios-tasks.md)

## Deferred or Later-Phase Topics

- `tcsendbreak` and the BREAK condition — no serial-line driver yet.
- Hardware flow control (`CRTSCTS`) — no UART pin wiring.
- Baud-rate selection beyond `B38400` placeholder — irrelevant for the
  virtual console / PTY paths.
- IUTF8-driven multi-byte ERASE — round-trips today, behavioural
  effect lands in Phase 69b alongside the UTF-8 wire decoder.
