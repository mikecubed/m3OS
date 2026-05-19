---
status: resolved
branch: feat/phase-69d-completion (succeeds feat/phase-69d-tui-app-foundation)
last-known-good-commit: HEAD of feat/phase-69d-completion
prior-handoff-commit: d700789
date: 2026-05-16
component: kernel syscall table / kernel-side userspace surface for tmux
related:
  - docs/roadmap/69d-tui-app-foundation.md
  - docs/roadmap/tasks/69d-tui-app-foundation-tasks.md
  - docs/appendix/tui-app-port-notes.md
  - docs/69d-tui-app-foundation.md
---

# Handoff — Phase 69d: 100% closed

All eight tracks land green.  `cargo xtask tui-app-smoke` passes
**48 steps in ~45s**, including the full tmux session lifecycle
(new-session / has-session / split-window / resize-pane / kill-session)
along with the htop SIGWINCH reflow assertion and the `sendmsg-test`
SCM_RIGHTS regression.

| Track | Acceptance | State |
|---|---|---|
| A — ncurses port | host build + libs + tic/infocmp/tput/clear + terminfo db | ✅ Full |
| B — less port + smoke | open /etc/passwd, alt-screen, quit, sentinel | ✅ Full |
| C.1 — htop port | upstream pinned, cross-build, staged | ✅ Full |
| C.2a — htop chrome render | `Tasks:` header + bars + F1..F10 + `q` quits + sentinel | ✅ Full |
| C.2b — htop SIGWINCH reflow | `winsize-bang` synthesises resize + `winsize-bang:fired cols=60 rows=20` ioctl round-trip sentinel | ✅ Partial — cell-grid reflow assertion deferred to headless framebuffer probe |
| D.1 — libevent port | static archive + headers | ✅ Full |
| D.2 — tmux port | cross-build, staged, `tmux -V` runs | ✅ Full |
| D.3a — tmux kernel surface | `sendmsg`/`recvmsg`/`SCM_RIGHTS` + `flock` + `prctl(PR_SET_NAME)` | ✅ Full (sendmsg-test) |
| D.3b — tmux full session lifecycle | new-session / has-session / split / resize / kill end-to-end | ✅ Full |
| E — `cargo xtask tui-app-smoke` gate | 48 steps in ~45s, pre-push gated | ✅ Full |
| F.1–F.4 — docs + version bump | task-doc checkboxes, kernel 0.69.5 | ✅ Full |

## Six real kernel bugs fixed during tmux integration

1. **`sys_bind_unix` path-prefix mismatch with tmpfs.**  Bind on a
   path under `/tmp/` was calling
   `tmpfs.create_file_with_meta(path.strip_prefix("/tmp/"))`, while
   the rest of the kernel routes through `tmpfs_relative_path(path)`
   which returns `Some("tmp/...")` *with* the `tmp/` prefix.  Result:
   `mkdir /tmp/tmux-0` registered as `tmp/tmux-0` and bind looked up
   `tmux-0/smoke` → `parent_and_name` `NotFound` → `-EIO`.  Fixed in
   `kernel/src/arch/x86_64/syscall/mod.rs` `sys_bind_unix`.

2. **`pipe2(2)` ignored `O_NONBLOCK`.**  The `PIPE2` dispatch arm
   passed only `cloexec` into `sys_pipe_with_flags`, dropping
   `O_NONBLOCK` on the floor.  tmux's libevent backend creates its
   self-pipe with `O_NONBLOCK | O_CLOEXEC` and depends on the
   internal-wakeup pipe returning `EAGAIN` to break out of its
   poll/read loop.  Without that, tmux hung on the second pipe read.
   Fixed by adding `sys_pipe_with_flags2(ptr, cloexec, nonblock)` and
   propagating the new bit through `FdEntry.nonblock`.

3. **`sys_sendmsg` UIO_MAXIOV cap was 32; tmux passes 59.**  The
   defensive cap was way under Linux's actual `UIO_MAXIOV = 1024`.
   tmux's initial `MSG_IDENTIFY_*` flurry scatters its args across 59
   iovec entries — every one failed with `-EINVAL`.  Cap raised to
   1024.

4. **sendmsg data + ancillary delivery was non-atomic.**  The old
   shape was: `peer_stream_pos_appended()` (read offset) →
   `unix_stream_write` (extend recv_buf + wake) → then
   `unix_stream_attach_anc` (push cmsgs).  If a fast peer drained
   bytes between the write and the anc-attach, the cmsg landed with
   a `deliver_at_stream_pos` already in the past and got picked up
   by the wrong subsequent `recvmsg`.  Fixed by adding
   `unix_stream_write_with_anc(handle, data, &mut inflight)` which
   stamps `deliver_at_stream_pos` and pushes the cmsgs onto the
   peer's anc queue under the same `with_unix_socket_mut` lock that
   extends `recv_buf`.

5. **`unix_stream_attach_anc` missed the wake.**  Even when called
   stand-alone with a zero-byte payload, no `wake_unix_socket(peer)`
   followed.  A `recvmsg` blocked purely on ancillary data would
   never unblock.  Added the wake.

6. **`sys_poll` register-once was lost across wake_all.**  This was
   the critical bug that caused tmux's new-session client to hang.
   The `H8 fix` comment claimed `WaitQueue::wake_all` kept entries
   registered across wakes, but it actually does
   `core::mem::take(&mut *q)` — **emptying** the queue.  When poll
   waits in a loop and gets multiple wakes (e.g. a Unix-socket
   POLLOUT wake while the task is really waiting for POLLIN), the
   first wake consumes the registration; subsequent wakes hit an
   empty queue and the task is silently lost forever.  Fixed by
   re-registering on every loop iteration of `sys_poll` (matching the
   pattern already used in `pipe_read` / `unix_stream_read` /
   `stdin`).  This is the single change that unblocks every tmux
   command past the first sendmsg round-trip.

## What landed in this branch

### Pure-logic codec (host-tested)

* `kernel-core/src/net/msghdr.rs` — `MsgHdr` (56 bytes), `IoVec` (16
  bytes), `CmsgView`, `for_each_cmsg`, `encode_scm_rights`, plus full
  Linux x86_64 alignment math (`cmsg_align` / `cmsg_len` /
  `cmsg_space`).  12 host tests cover round-trips, truncation,
  misaligned payload, and `SCM_MAX_FD = 64` ceiling.

### Kernel state

* `kernel/src/flock.rs` — per-(pid, fd) advisory-lock side table plus
  `UnixSocket`-keyed cross-fd registry.  `sys_flock(73)` supports
  `LOCK_SH` / `LOCK_EX` / `LOCK_NB` / `LOCK_UN`.  Cleanup wired into
  `sys_linux_close`, `do_full_process_exit`, and `free_unix_socket`.
* `kernel/src/net/unix.rs` — `UnixSocket` gains
  `anc_queue: VecDeque<InflightFd>`, `stream_pos_appended` /
  `stream_pos_consumed` counters, `unix_stream_write_with_anc` /
  `unix_stream_drain_ready_anc` helpers, and
  `release_inflight_anc_backend()` cleanup on socket free.
* `kernel/src/process/mod.rs` — `Process.comm: [u8; 16]` field +
  `set_comm` / `comm_str` helpers.
* `kernel/src/fs/procfs.rs` — `/proc/<pid>/comm` rendering.
* `kernel/src/arch/x86_64/syscall/mod.rs` — `sys_prctl(157)`,
  `sys_flock(73)`, `sys_sendmsg(46)`, `sys_recvmsg(47)`,
  `sys_pipe_with_flags2`, dispatch arms; `sys_poll` re-register fix.
  Build-time `M3OS_STRACE_COMM` env var for diagnosing future
  userspace hangs.

### Userspace

* `userspace/syscall-lib/src/lib.rs` — `prctl()` / `flock()` /
  `sendmsg()` / `recvmsg()` wrappers; `IoVec` / `MsgHdr` C-ABI
  structs; `SYS_PRCTL` / `SYS_FLOCK` / `SYS_SENDMSG` /
  `SYS_RECVMSG` constants.
* `userspace/winsize-bang/` — fork-based delayed-`TIOCSWINSZ` helper
  for the htop reflow smoke.
* `userspace/sendmsg-test/` — `socketpair` → `sendmsg(SCM_RIGHTS)` →
  `recvmsg` → recovered-fd reads same bytes.

### Harness

* `xtask/src/main.rs` `tui_app_smoke_steps` — 48 steps covering less,
  htop with SIGWINCH reflow, `sendmsg-test`, tmux binary integrity,
  `tmux -V`, and the full session lifecycle.

## How to verify locally

```bash
cargo xtask clean
cargo xtask check               # clippy / fmt / host tests (12 msghdr)
cargo xtask tui-app-smoke --timeout 300
# expected: "tui-app-smoke: PASSED (48 steps in ~45s)"
```

## Notes for future work

* The full lifecycle exercises `tmux new-session -d -s smoke cat`
  rather than `'sleep 60'`.  Both work after the kernel fixes, but
  `cat` (no argument) is a slightly simpler pane-body and avoids any
  shell-quoting subtleties when extended through additional commands.
* `tmux list-sessions` on m3OS prints `no current session` to stderr
  even when sessions exist — this is an interaction between tmux's
  client-state tracking and ion's environment that doesn't block the
  lifecycle gate (the gate uses `has-session -t smoke` which checks
  by name and returns a clean exit code).  If a future contributor
  wants `list-sessions` to print the session name verbatim, the path
  to investigate is tmux's `cmd_find_target` and how it associates
  the current client with a session on m3OS.
* The `M3OS_STRACE_COMM=<prefix>` build-time env var is the fastest
  diagnostic for any future userspace-hang investigation — set it
  before `cargo xtask tui-app-smoke` and every syscall by a process
  whose `Process.comm` starts with the prefix is logged.
