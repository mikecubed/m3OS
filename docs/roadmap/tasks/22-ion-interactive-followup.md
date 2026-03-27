# Phase 22 Follow-up: Ion Interactive Mode

**Status:** Deferred — sh0 works correctly with Phase 22 TTY layer
**Depends on:** Phase 22 TTY infrastructure (complete)
**Branch:** `feat/phase-22-tty`

## Problem Summary

Ion shell boots and runs the PROMPT command successfully, but never enters
its interactive read loop. The `ion#` prompt never appears on screen, and
typed commands are not executed. sh0 works perfectly with the same TTY
infrastructure.

## Root Cause Analysis

Ion uses Rust's `std::process::Command` to run `/bin/PROMPT` (a C binary
that writes the prompt string to stdout). `Command::spawn` on Linux+musl
goes through `posix_spawn` (not fork+exec), which involves:

1. Creating a CLOEXEC socketpair for exec-error detection
2. Creating pipes for stdout/stderr capture
3. Calling `libc::posix_spawn()` (musl's implementation uses `clone()`)
4. Parent reads from CLOEXEC socket to verify exec succeeded
5. Parent reads captured stdout to get PROMPT's output

Ion gets **stuck at step 4** — blocked in `recvfrom(fd=8, buf, 8, 0)` on
the CLOEXEC socketpair read end. The write end should be closed by
close-on-exec during the child's `execve`, but the pipe writer refcount
never reaches 0.

## Investigation Timeline

### Fix 1: Blocking recvfrom (SOLVED)
`sys_recvfrom` was unconditionally non-blocking (always returning -EAGAIN).
Rust std's `Command::spawn` calls `recv()` in blocking mode on the CLOEXEC
socket. Fixed by reading the `flags` argument and only using non-blocking
mode when `MSG_DONTWAIT` (0x40) is set.

**Commit:** `3f70183` — `fix: make recvfrom block by default`

### Fix 2: poll() syscall (SOLVED)
Ion uses `poll()` to multiplex between its signal self-pipe and stdin.
Syscall 7 was not implemented. Added proper poll with fd-readiness checking.

**Commit:** `a379988` — `feat: add poll syscall`

### Fix 3: HOME=/tmp (SOLVED)
Ion tried to create `~/.config/ion/initrc` and `~/.local/share/ion/history`
under `HOME=/` (read-only ramdisk). Both failed with EROFS. Changed to
`HOME=/tmp` (writable tmpfs).

**Commit:** `f6ddbbb` — `fix: set HOME=/tmp`

### Fix 4: FD_CLOEXEC implementation (PARTIAL)
Implemented close-on-exec flag tracking:
- `cloexec: bool` field on `FdEntry`
- `fcntl(F_GETFD/F_SETFD)` tracks the flag
- `pipe2`/`socketpair` with `O_CLOEXEC`/`SOCK_CLOEXEC` set the flag
- `execve` calls `close_cloexec_fds()` before loading new image
- `dup2` clears FD_CLOEXEC on target fd (POSIX requirement)

**Commit:** `5cdcb7e` — `feat: implement FD_CLOEXEC`
**Commit:** `5018995` — `fix: dup2 must clear FD_CLOEXEC`

### Remaining Issue: Ion still stuck after CLOEXEC fix

With CLOEXEC, the socketpair write end (fd 9) IS correctly closed during
exec. However, ion still doesn't proceed to its interactive loop. Tracing
showed:

- After PROMPT runs, ion makes NO further ioctl calls (no TCGETS/TCSETS)
- Ion never calls `read(0, ...)` or `poll()` after PROMPT
- The `recvfrom` on the CLOEXEC pipe may still be blocking

#### Key Diagnostic Data

**Child fd table at exec time (pid=5 executing /bin/PROMPT):**
```
fd=0  TTY            cloexec=false   ← stdin (correct)
fd=1  PipeWrite(0)   cloexec=false   ← stdout redirected to capture pipe
fd=2  other           cloexec=false   ← stderr
fd=3  PipeRead(0)    cloexec=true    ← signal pipe read (closed by CLOEXEC)
fd=4  PipeWrite(0)   cloexec=true    ← signal pipe write (closed by CLOEXEC)
fd=5  other           cloexec=true    ← closed by CLOEXEC
fd=6  PipeWrite(0)   cloexec=true    ← closed by CLOEXEC
fd=7  other           cloexec=true    ← closed by CLOEXEC
fd=9  PipeWrite(1)   cloexec=true    ← CLOEXEC socketpair write (closed)
```

**Anomalies:**
- fd=1 is `PipeWrite(pipe_id=0)` — PROMPT's stdout goes to pipe_id=0
  (the signal self-pipe!), not a separate capture pipe. This means the
  stdout capture and signal pipe share pipe_id=0.
- fd=8 (CLOEXEC socketpair read end) is **missing** from the child's fd
  table — it was either already closed or overwritten before exec.
- Multiple fds reference pipe_id=0 with different roles, suggesting pipe
  slot reuse or incorrect fd setup.

## Hypotheses for Next Investigation

### H1: Pipe ID Reuse Bug
When musl's `posix_spawn` child code processes file_actions (close/dup2),
it may close fds that decrement pipe_id=0's refcounts to zero, freeing
the slot. A subsequent `pipe2()` call then reuses slot 0 for the stdout
capture pipe, conflicting with the signal self-pipe.

**Test:** Add logging to `create_pipe()` to see if pipe_id=0 gets freed
and reallocated. Check `pipe_close_reader`/`pipe_close_writer` calls
during the posix_spawn file_actions phase.

### H2: posix_spawn vs fork+exec Path Mismatch
Rust std tries `posix_spawn` first. If it succeeds, the CLOEXEC
socketpair is never created by Rust std (it's only created in the
fork+exec fallback path). But we see pipe_id=1 (the socketpair) in the
log. This suggests posix_spawn returns `None` and falls through to
fork+exec. Need to confirm which path is taken.

**Test:** Add a log inside `sys_clone` to distinguish posix_spawn's
`clone()` from Rust std's `fork()`. Check if both the posix_spawn AND
fork+exec paths are running (double-spawning the child).

### H3: Blocking Read on Wrong Pipe
The parent reads from fd=8 (pipe_id=1 read end) expecting the CLOEXEC
result. But if pipe_id=1's writer refcount includes stale references
from the child's inherited fds, the read blocks forever.

**Test:** Track pipe_id=1's writer_count through the full
fork→exec→exit lifecycle. Log every increment/decrement with the
calling pid and fd number.

### H4: Termion/Liner Initialization Failure
Even if the CLOEXEC pipe issue is resolved, ion's `liner` library
calls `stdout().into_raw_mode()` which requires `tcgetattr`/`tcsetattr`.
If this fails silently, ion's interactive loop may not start.

**Test:** After fixing the CLOEXEC blocking, check if TCGETS (0x5401)
and TCSETS (0x5402) ioctls appear in the trace. If not, liner may be
failing to initialize for a different reason.

## What Works

- TTY cooked mode line discipline (ICANON, ECHO, ECHOE, ISIG)
- Keyboard input → stdin delivery with echo
- Line editing: backspace (^H), kill line (^U), word erase (^W)
- Signal characters: ^C (SIGINT), ^Z (SIGTSTP), ^\ (SIGQUIT)
- EOF: ^D delivers EOF to reader
- ICRNL/ONLCR translation
- `ioctl(TCGETS/TCSETS)` reads/writes termios correctly
- `ioctl(TIOCGWINSZ/TIOCSWINSZ)` reads/writes window size
- `ioctl(TIOCGPGRP/TIOCSPGRP)` manages foreground process group
- SIGWINCH delivery on window size change
- `isatty()` returns true for TTY fds, false for files
- `FdBackend::DeviceTTY` with fstat reporting S_IFCHR
- PTY skeleton stubs (`/dev/ptmx`, `/dev/pts/N`)
- `poll()` syscall with correct fd-readiness checking
- `recvfrom()` with blocking/non-blocking modes
- `FD_CLOEXEC` tracking and close-on-exec during execve
- sh0 shell fully interactive with TTY layer

## Files Modified in Phase 22

| File | Changes |
|---|---|
| `kernel-core/src/tty.rs` | Termios, Winsize, EditBuffer structs + tests |
| `kernel-core/src/lib.rs` | Added `tty` module |
| `kernel/src/tty.rs` | TtyState, TTY0 static, PTY allocator |
| `kernel/src/stdin.rs` | EOF signaling, flush |
| `kernel/src/main.rs` | Line discipline in stdin_feeder_task |
| `kernel/src/process/mod.rs` | DeviceTTY, PtyMaster/PtySlave, SIGWINCH, SIGQUIT, FD_CLOEXEC |
| `kernel/src/pipe.rs` | (debug traces removed) |
| `kernel/src/arch/x86_64/syscall.rs` | ioctl, poll, recvfrom, CLOEXEC, FdEntry.cloexec |
| `userspace/init/src/main.rs` | HOME=/tmp, shell selection |
| `.github/workflows/release.yml` | musl target |
| `docs/08-roadmap.md` | Phase 22 completed |
| `docs/roadmap/tasks/22-tty-pty-tasks.md` | All tasks marked done |

## Recommended Approach for Next Session

1. Start fresh session with this document as context
2. Focus on H1 (pipe ID reuse) first — it's the most likely root cause
3. Add targeted tracing to `create_pipe`, `pipe_close_reader/writer`
   with pipe_id=0 specifically during the PROMPT spawn lifecycle
4. Consider whether musl's `posix_spawn` can be made to work, or if
   ion should be patched to use fork+exec directly
5. Alternative: build ion with `--features=...` to disable liner and
   use a simpler readline that doesn't require posix_spawn
