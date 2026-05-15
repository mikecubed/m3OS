# Phase 69a - Termios Raw Mode and Line Discipline

**Status:** Complete
**Source Ref:** phase-69a
**Depends on:** Phase 22 (TTY/PTY) ✅, Phase 29 (PTY Subsystem) ✅, Phase 69 (Terminal Contract Foundations)
**Builds on:** Extends the Phase 22 kernel TTY line discipline (`kernel/src/tty.rs`) with the full POSIX termios surface — input/local mode flag plumbing, VMIN/VTIME, IUTF8, OPOST controls — so editors and pagers can take byte-accurate, unbuffered input. The Phase 22 console line discipline already owns canonical-mode line editing; this phase makes the cooked/raw distinction first-class and routes all of it through both kernel TTY0 and the PTY pair from Phase 29.
**Primary Components:** kernel/src/tty.rs, kernel-core/src/tty.rs, kernel-core/src/pty.rs, kernel/src/arch/x86_64/syscall (TCGETS/TCSETS), userspace/syscall-lib (termios FFI shape)

## Milestone Goal

Phase 69a lands a working termios contract. Userspace can:

- read and write the full `termios` struct via `tcgetattr` / `tcsetattr` against either the console TTY or a PTY slave;
- flip `ICANON` off to get byte-by-byte input;
- flip `ECHO` off to suppress local echo;
- set `VMIN` / `VTIME` to control blocking behaviour on raw reads;
- flip `ISIG` off to disable kernel signal generation on Ctrl-C / Ctrl-Z;
- flip `OPOST` off / on to control output post-processing (NL → CRNL translation).

This is the missing half of "the terminal contract" — Phase 69 handles the *output* wire protocol, this phase handles the *input* contract that lets editors run. With both, a hypothetical `vi` port has the kernel-facing pieces it needs; what remains is glyph coverage (69b/c) and the library shim (69d).

## Why This Phase Exists

Editors (vim, nvim, less, mc, htop, tmux) all begin by reading the current termios, clearing `ICANON | ECHO | ISIG | IEXTEN`, setting `VMIN=1 VTIME=0`, and `tcsetattr`ing it back. If `tcsetattr` is a stub or the flag bits do not actually change line-discipline behaviour, the editor reads either nothing (canonical mode swallows the keypress until Enter) or echoes everything (the editor sees its own redraw bytes as input). Phase 22 set up the termios *struct* and canonical line editor; Phase 29 added the PTY pair; neither phase wired the runtime flag bits through to actual ldisc behaviour.

The post-Phase-57 evaluation lists "Raw/cbreak termios modes" as gap #1 for TUI compatibility. This phase closes it.

## Learning Goals

- Understand the POSIX termios flag layout (`c_iflag`, `c_oflag`, `c_cflag`, `c_lflag`, `c_cc[NCCS]`) and how each flag changes line-discipline behaviour.
- Learn the difference between cooked (canonical) and raw/cbreak mode.
- See how `VMIN` and `VTIME` interact to give the three classic POSIX read behaviours (blocking, polling, timed).
- Understand how `ISIG` ties to the kernel's signal-from-terminal path (Ctrl-C → SIGINT, Ctrl-Z → SIGTSTP, Ctrl-\ → SIGQUIT).
- Learn how `OPOST` and `ONLCR` shape the byte stream the application sees vs the byte stream the device receives.

## Feature Scope

### Termios struct fidelity (Track A)

`kernel-core::tty::Termios` is widened to the full POSIX shape (4 mode words + `NCCS=19` control-char array, matching musl/Linux). Existing fields are preserved; new fields default to the "sane cooked mode" baseline so existing callers see no behaviour change until they explicitly opt into raw mode.

### `tcgetattr` / `tcsetattr` syscalls (Track B)

The kernel TTY ioctl path gains `TCGETS` (0x5401), `TCSETS` (0x5402), `TCSETSW` (0x5403), and `TCSETSF` (0x5404). The PTY ioctl path implements the same set against the slave fd. The `tcsendbreak` family is deferred.

### `c_iflag` plumbing (Track C)

Implement `IGNCR` (ignore CR on input), `INLCR` (map NL → CR on input), `ICRNL` (map CR → NL on input), `IUTF8` (input is UTF-8; affects ERASE behaviour with multibyte sequences — *closed in Phase 69b: when `IUTF8` is set, `EditBuffer::erase_one_codepoint` walks back across UTF-8 continuation bytes plus the leading byte so VERASE removes one whole codepoint per press; when cleared, the legacy one-byte-per-erase behaviour is preserved*), `IXON` / `IXOFF` (software flow control; honoured by the ldisc).

### `c_oflag` plumbing (Track D)

Implement `OPOST` (post-process output), `ONLCR` (NL → CRNL on output when `OPOST` is set). Writes from userspace go through the output discipline only when `OPOST` is set; when cleared, bytes pass through unmodified — the wire-protocol foundation Phase 69 depends on.

### `c_lflag` plumbing (Track E)

Implement `ICANON` (canonical-mode line editor on/off), `ECHO` (local echo on/off), `ECHOE`/`ECHOK`/`ECHONL` (echo behaviour subflags), `ISIG` (generate signals on INTR/QUIT/SUSP), `IEXTEN` (extended input processing — gates VEOL2 / DISCARD / LNEXT). When `ICANON` is cleared the line discipline switches to the byte-by-byte raw path.

### `c_cc` control characters (Track F)

Honour `VMIN` and `VTIME` on raw reads:
- `VMIN > 0, VTIME == 0`: block until at least `VMIN` bytes are available.
- `VMIN == 0, VTIME > 0`: return immediately if data is available, otherwise wait up to `VTIME * 100 ms`.
- `VMIN > 0, VTIME > 0`: inter-byte timer; return when either `VMIN` is hit or `VTIME * 100 ms` elapses after the first byte.
- `VMIN == 0, VTIME == 0`: poll — return whatever is available, including zero bytes.

Honour `VINTR` / `VQUIT` / `VSUSP` (when `ISIG` is on) as the signal-generating bytes; honour `VERASE` / `VKILL` / `VEOF` / `VEOL` / `VEOL2` (when `ICANON` is on) for line editing.

### Signal-from-terminal path (Track G)

When `ISIG` is on and the ldisc sees `VINTR` / `VQUIT` / `VSUSP`, it sends `SIGINT` / `SIGQUIT` / `SIGTSTP` to the controlling terminal's foreground process group via the existing `send_signal_to_group` (the same call Phase 69's TIOCSWINSZ uses for SIGWINCH).

### Userspace surface (Track H)

`userspace/syscall-lib/src/lib.rs` gains `tcgetattr(fd)`, `tcsetattr(fd, optional_actions, &termios)`, `cfmakeraw(&mut termios)`, and a public `Termios` struct matching the kernel ABI. The five existing termios-touching userspace callers (`pty-test`, `shell`, `term`, `login`, `ion`) are audited; none need to change today, but the new API is what 69d's ncurses port will consume.

### Validation (Track I)

A new `tcsmoke` userspace binary (or new subcommand on `tui-smoke` from Phase 69 — implementer's call) exercises:

- get → modify → set → get round-trip on every mode word;
- `ICANON` off → write a single byte → verify the read returns one byte without waiting for newline;
- `ECHO` off → write to PTY master → verify the PTY slave reads the byte but the master does not see an echo;
- `ISIG` on + write `VINTR` → verify SIGINT is delivered to the foreground process group;
- `VMIN=2 VTIME=0` → verify a `read` for 4 bytes returns after exactly 2 bytes are available.

## Important Components and How They Work

### `kernel-core/src/tty.rs` — termios + ldisc state

The pure-logic line-discipline state machine. After 69a it tracks: current termios flags, the canonical-mode edit buffer (existing), a new "raw-mode pending byte" path, and the VMIN/VTIME timer state.

### `kernel/src/tty.rs` — kernel TTY adapter

Drives `kernel-core::tty` from the keyboard interrupt + write path. After 69a, the kernel TTY routes incoming bytes through the ldisc's `consume(byte)` function which returns `Action::DeliverToLine`, `Action::DeliverToReader`, `Action::SignalForeground(SignalKind)`, or `Action::Discard` depending on flag state.

### `kernel-core/src/pty.rs` — PTY pair

The PTY shares the ldisc state shape with the console TTY. Both call into the same `kernel-core::tty` ldisc functions; the difference is only in how bytes are sourced (keyboard for TTY0, master-write for the slave) and how reads are blocked.

### `kernel/src/arch/x86_64/syscall/mod.rs` — ioctl branch

Adds the four `TCGETS`/`TCSETS`/`TCSETSW`/`TCSETSF` cases alongside the existing `TIOCSWINSZ` branch.

### `userspace/syscall-lib/src/lib.rs` — userspace shim

Public `Termios`, `tcgetattr`, `tcsetattr`, `cfmakeraw`. Wraps the kernel ABI in the same shape musl exposes so future C ports compile cleanly.

## How This Builds on Earlier Phases

- Extends Phase 22's `kernel_core::tty::Termios` from the minimal canonical-mode shape to the full POSIX flag set.
- Extends Phase 29's PTY pair to honour the new flag bits through the shared ldisc.
- Reuses Phase 19's signal-delivery path (`send_signal_to_group`) for `ISIG`-driven SIGINT/SIGQUIT/SIGTSTP.
- Builds on Phase 69's `?2004` bracketed-paste mode bit — both phases agree that "the wire protocol speaks bytes, not lines."

## Implementation Outline

1. Widen `kernel-core::tty::Termios` to the full POSIX shape; pick defaults that preserve current cooked-mode behaviour.
2. Implement `TCGETS` / `TCSETS` / `TCSETSW` / `TCSETSF` ioctl branches on both TTY0 and PTY slave paths; copy-to-user / copy-from-user the struct.
3. Implement `c_iflag` arms in `kernel-core::tty::consume`.
4. Implement `c_oflag` arms in the kernel TTY / PTY write path; bypass post-processing when `OPOST` is off.
5. Implement `c_lflag::ICANON` switch; add the raw-mode pending-byte path.
6. Implement `c_lflag::ECHO` / `ECHOE` / `ECHOK` / `ECHONL` / `IEXTEN`.
7. Implement `c_cc::VMIN` / `VTIME` timer state in the ldisc; thread through the blocking-read primitive.
8. Implement `c_lflag::ISIG` + `VINTR` / `VQUIT` / `VSUSP` → signal-from-terminal path.
9. Expose `tcgetattr` / `tcsetattr` / `cfmakeraw` and the `Termios` struct from `userspace/syscall-lib`.
10. Build `tcsmoke` (or extend `tui-smoke`); add a `cargo xtask termios-smoke` gate.
11. Cross-reference Phase 22 + Phase 29 docs; update `docs/appendix/term-escape-sequences.md` with a new "Termios contract" section.
12. Author the aligned legacy learning doc at `docs/69a-terminal-termios.md` following the template in `docs/appendix/doc-templates.md` (Overview, What This Doc Covers, Key Files, Closure of Related Phases, Related Roadmap Docs).
13. Kernel version bump to 0.69.1 (patch bump — userspace surface + kernel ldisc only; no ABI break beyond the new ioctls).

## Acceptance Criteria

- `tcgetattr` + `tcsetattr` round-trip every documented flag bit without loss.
- After `cfmakeraw(&mut termios); tcsetattr(fd, TCSANOW, &termios)`, a single keypress in `term` is delivered to userspace as a single byte without waiting for newline and without local echo.
- With `ICANON` off and `VMIN=2 VTIME=0`, a 4-byte `read` returns exactly 2 bytes once 2 bytes are available.
- With `ISIG` on, typing Ctrl-C in `term` sends SIGINT to the foreground process group (validated by a self-installed handler).
- With `OPOST` off, a `write("foo\n")` on the PTY master is delivered to the slave reader as the four bytes `foo\n` (no CRNL expansion).
- `cargo xtask termios-smoke` boots, exercises every check, and reports `:ok` for each.

## Companion Task List

- [Phase 69a Task List](./tasks/69a-terminal-termios-tasks.md)

## How Real OS Implementations Differ

- Linux `n_tty` is a large kernel module with explicit per-flag dispatch; m3OS keeps the ldisc state machine in `kernel-core` for host-testability.
- BSD termios uses slightly different default flag combinations (`CRTSCTS`, `MDMBUF`); m3OS follows Linux defaults to match musl/glibc shims.
- macOS implements `IUTF8` via a separate codeset table; m3OS folds it into the 69b UTF-8 decoder hook.

## Deferred Until Later

- `tcsendbreak` and the BREAK condition (no serial console driver yet).
- Hardware flow control flags (`CRTSCTS`) — no UART pin wiring.
- Session/job control (`tcsetpgrp` / `tcgetpgrp` already exist; expanding job-control semantics is its own phase).
- `c_cflag` baud-rate fields — irrelevant for PTY and virtual console; placeholder accessors only.
