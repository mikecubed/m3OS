---
status: open
branch: feat/phase-69d-tui-app-foundation (PR #176)
last-known-good-commit: d700789
date: 2026-05-16
component: kernel-core/term / kernel syscall table / userspace tui-app-smoke harness
related:
  - docs/roadmap/69d-tui-app-foundation.md
  - docs/roadmap/tasks/69d-tui-app-foundation-tasks.md
  - docs/appendix/tui-app-port-notes.md
  - docs/69d-tui-app-foundation.md
---

# Handoff — Phase 69d: closing the last 5% to 100% acceptance

## Status at handoff

Phase 69d shipped as kernel 0.69.4 with these results:

| Track | Acceptance | State on `d700789` |
|---|---|---|
| A — ncurses port | host build + libs + tic/infocmp/tput/clear + terminfo db | ✅ Full |
| B — less port + smoke | open /etc/passwd, alt-screen renders first line, quit, sentinel | ✅ Full |
| C.1 — htop port | upstream pinned, cross-build, staged | ✅ Full |
| C.2a — htop chrome render | `Tasks:` header + CPU/Mem bars + F1..F10 strip render; `q` quits; sentinel | ✅ Full |
| C.2b — htop SIGWINCH reflow | scripted resize causes second frame to reflect new dimensions | ⚠ Deferred — harness extension required |
| D.1 — libevent port | static archive + headers | ✅ Full |
| D.2 — tmux port | cross-build, staged, `tmux -V` runs | ✅ Full |
| D.3 — tmux full session lifecycle | new-session / split / resize / detach | ⚠ Deferred — kernel syscalls absent |
| E — `cargo xtask tui-app-smoke` gate | 30-step scripted run, per-app `:ok`/`:fail`, ≤5 min, pre-push gated | ✅ Full |
| F.1–F.4 — docs + version bump | post-57 closeout, ports appendix, aligned legacy doc, kernel 0.69.4 | ✅ Full |

Two acceptance items remain. This doc tells the next contributor exactly
what is missing, where the surfaces are, and what an end-to-end fix
looks like.

## Gap 1 — htop SIGWINCH reflow (Track C.2 resize half)

### Symptom

`cargo xtask tui-app-smoke` drives htop through chrome render + quit but
does **not** synthesize a window-size change during the run. The
acceptance text reads:

> Synthesizes a `SurfaceResized` to a smaller geometry; asserts the
> second frame's cell grid reflects the new dimensions (header
> truncated or wrapped per htop's policy).

### Why the surface is already there

The kernel and userspace already support window-size changes end-to-end:

- `kernel-core::display::protocol::ServerMessage::SurfaceResized { width, height }` exists (Phase 69 Track D).
- `Screen::resize` accepts new dimensions and emits the right `RenderCommand` stream.
- `ioctl(fd, TIOCSWINSZ, &winsize)` lands in `userspace/syscall-lib` (Phase 69a) and the kernel path delivers `SIGWINCH` to the foreground process group while updating `tty.winsize`.
- `userspace/tui-smoke` exercises the TIOCSWINSZ ioctl in its `resize` subcommand — see `userspace/tui-smoke/src/main.rs:297` (`fn run_resize`).

So every primitive ncurses queries (`TIOCGWINSZ` after a `SIGWINCH`) already returns the new dimensions on m3OS today. The missing piece is the **harness** driving the resize.

### What's missing

`cargo xtask tui-app-smoke` types keystrokes into the QEMU serial pipe but has no path to issue `TIOCSWINSZ` on the slave PTY where htop is running. The harness needs a sidecar that:

1. Identifies htop's PID once it has launched.
2. Walks `/proc/<pid>/fd/0` (or the controlling-tty link) to find its slave PTY.
3. Issues `TIOCSWINSZ` on that PTY.

### Implementation outline

**Option A — guest-side helper binary (recommended).** Add a tiny `winsize-bang` userspace tool (~50 lines) that takes a PID and a new geometry, opens the PID's `/proc/<pid>/fd/0`, and issues `TIOCSWINSZ`. The harness then runs `winsize-bang $! 24 80` after launching htop in the background.

**Option B — control character.** Extend the m3OS TTY driver to recognize a magic byte sequence on the serial input that triggers a programmable `TIOCSWINSZ` on the active TTY. Cleaner but more invasive.

**Option C — direct QEMU monitor command.** Use QEMU's HMP to send a synthetic SIGWINCH or trigger the terminal-size-change event on the serial back-end. Requires QEMU monitor wiring in xtask — heaviest but no guest changes.

Recommend option A: small, targeted, doesn't require new kernel features.

### Files to touch (option A)

- `userspace/winsize-bang/` — new crate, ~50 lines (modeled on `userspace/whoami` or `userspace/id`).
- `Cargo.toml` workspace `members`.
- `xtask/src/main.rs` `bins` table in `build_userspace_bins` — add `("winsize-bang", "winsize-bang", false)`.
- `kernel/src/fs/ramdisk.rs` — add `WINSIZE_BANG_ELF` include + `BIN_ENTRIES` entry + `FlatFile` listing.
- `xtask/src/main.rs` `tui_app_smoke_steps` — between the `htop` launch step and the `q` step, insert:
  - `SmokeStep::Send` with `winsize-bang $(pidof htop) 20 60\n`
  - `SmokeStep::Sleep { millis: 500 }`
  - `SmokeStep::Wait { pattern: "<a marker the resize is observable through>", … }`
  
  htop redraws the chrome on SIGWINCH; the harness can wait for a second occurrence of `Tasks:` (or for a row count change visible via `[24;1H` cursor positioning) to assert the reflow.

### Acceptance after fix

- The smoke gate sequence is: htop launches → first `Tasks:` observed → `winsize-bang` issued → second redraw observed at the new dimensions → `q` quits → `:ok` sentinel.
- Task list checkbox in `docs/roadmap/tasks/69d-tui-app-foundation-tasks.md` for "SurfaceResized synthesis" flips to `[x]`.

### Estimated effort

Half a day. The hardest part is verifying the second-redraw assertion text. The 69-series serial output buffer is long enough that a redraw is visible — likely a fresh `\e[2;1H` cursor positioning followed by the truncated `Tasks:` line.

## Gap 2 — tmux full session lifecycle (Track D.3)

### Symptom

`cargo xtask tui-app-smoke` binary-integrity probe (`tmux -V`) succeeds.
A full `tmux new-session -d -s smoke 'sleep 60'` does not — the kernel
logs (visible via `M3OS_SMOKE_SERIAL_DUMP=…`):

```
[WARN] unhandled syscall 46 (args: …)   # sendmsg
[WARN] unhandled syscall 47 (args: …)   # recvmsg  (likely; tmux uses both)
[WARN] unhandled syscall 73 (args: …)   # flock
[WARN] unhandled syscall 157 (args: …)  # prctl (cosmetic — PR_SET_NAME)
```

…and tmux fails to talk to its server.

### Root cause

tmux's client/server architecture relies on:

- **`sendmsg(2)` / `recvmsg(2)`** — to pass file descriptors (the slave PTY) over the Unix-domain control socket. ancillary-data (`SCM_RIGHTS`) is the load-bearing primitive — without it, the server cannot inherit the client's PTY.
- **`flock(2)`** — to lock the Unix socket inode against concurrent `tmux new-session` calls.
- **`prctl(2)` PR_SET_NAME** — sets the process name visible in `ps`. Cosmetic; can stub.

m3OS' syscall dispatcher (`kernel/src/arch/x86_64/syscall/mod.rs`, the trailing `_ => log::warn!("unhandled syscall …")` arm) returns `-ENOSYS` for these numbers. tmux's libc wrapper then surfaces `EINVAL` / `ENOSYS` to the application, which gives up.

### Where the pieces already are

- Unix-domain socket primitives — `SOCKET` (41), `BIND` (49), `LISTEN` (50), `ACCEPT` (43), `CONNECT` (42), `SOCKETPAIR` (53), and the AF_UNIX byte-stream `read`/`write` paths — are already implemented in `kernel/src/arch/x86_64/syscall/mod.rs` and `kernel/src/net/unix.rs`. Test coverage lives in `userspace/unix-socket-test`.
- The `READ` / `WRITE` Unix-socket data paths use a simple in-memory ring. No fd-passing today.

### Implementation outline

#### 2a. `prctl` (cosmetic, trivial)

Lowest-cost first. In `kernel/src/arch/x86_64/syscall/mod.rs` add:

```rust
pub const PRCTL: u64 = 157;
// ...
PRCTL => {
    // PR_SET_NAME (15) → store in process struct, useful for /proc/<pid>/comm.
    // Everything else → return 0 (no-op) so tmux's cosmetic call is silent.
    let op = arg0;
    if op == 15 /* PR_SET_NAME */ {
        let name_ptr = arg1 as *const u8;
        // copy_from_user into proc.comm[0..15]
    }
    0
}
```

Half an hour. Stubs out the warning.

#### 2b. `flock` (small)

```rust
pub const FLOCK: u64 = 73;
// ...
FLOCK => {
    // Per-fd advisory lock. Use the per-fd extension table on FileDescription.
    // LOCK_SH (1), LOCK_EX (2), LOCK_UN (8), LOCK_NB (4).
    // For tmux's purpose, a single global per-fd flag is sufficient.
    sys_flock(arg0 as i32, arg1 as i32) as u64
}
```

Add a `flock_state: AtomicU8` to `FileDescription` and gate `LOCK_EX` against existing locks. ~half a day if you want a correct implementation; ~15 minutes if you want a stub that always returns 0 (which would actually let tmux through because tmux uses flock for its own coordination, not for protection from other processes).

#### 2c. `sendmsg` / `recvmsg` with `SCM_RIGHTS` (load-bearing)

This is the work that closes the gap.

**Signature.** Both are scatter-gather socket I/O with optional ancillary data:

```c
ssize_t sendmsg(int sockfd, const struct msghdr *msg, int flags);
ssize_t recvmsg(int sockfd, struct msghdr *msg, int flags);

struct msghdr {
    void         *msg_name;        // optional address
    socklen_t     msg_namelen;
    struct iovec *msg_iov;         // scatter/gather array
    size_t        msg_iovlen;
    void         *msg_control;     // ancillary data (e.g. SCM_RIGHTS)
    size_t        msg_controllen;
    int           msg_flags;
};
```

For tmux on m3OS:

- `msg_name` is unused (Unix-domain socket is connected).
- `msg_iov` is `msg_iovlen` scatter entries — copy each through, same shape as `readv`/`writev` which already exist.
- `msg_control` contains `SCM_RIGHTS` cmsg with file descriptors. The kernel must:
  1. Parse the cmsg header on send.
  2. Look up each fd in the sender's fd table.
  3. Duplicate (refcount-up) the underlying `FileDescription` and pin a handle on the socket's "in-flight ancillary queue" until the matching `recvmsg` arrives.
  4. On recv, install the file in the receiver's fd table at the lowest free slot.

**Storage.** `kernel/src/net/unix.rs` `UnixSocket` already has a byte ring. Add a parallel `ancillary_queue: VecDeque<InflightFd>` where `InflightFd { fd_clone: FileDescription, attach_offset: usize }`. On read, when the byte cursor crosses an `attach_offset`, the ancillary fd is materialized into the caller's fd table and a matching `cmsg` is appended to the caller's `msg_control` buffer.

**Files to touch:**
- `kernel/src/arch/x86_64/syscall/mod.rs` — add `SENDMSG=46`, `RECVMSG=47`, dispatch arms.
- `kernel/src/syscall/socket.rs` (new or extension of existing socket dispatch).
- `kernel/src/net/unix.rs` — `UnixSocket` ancillary queue.
- `kernel-core/src/...` — if msghdr / cmsg helpers should be pure-logic and testable on the host.
- `userspace/sendmsg-test/` — new smoke binary that does `socketpair → sendmsg(SCM_RIGHTS for stdin) → recvmsg → assert recovered fd reads same bytes`. Wire into `cargo xtask regression`.

**Acceptance.**
- `userspace/sendmsg-test` runs from sh0 and prints `SENDMSG_SMOKE:scm-rights:ok`.
- `cargo xtask tui-app-smoke` step matrix flips back to the full lifecycle:
  - `tmux -L smoke new-session -d -s smoke sleep 60`
  - `tmux -L smoke list-sessions` → waits for `smoke:` in output
  - `tmux -L smoke split-window -h -t smoke` → waits for visible divider character in the cell grid
  - `tmux -L smoke resize-pane -t smoke -R 5` → asserts divider column moved
  - `tmux -L smoke kill-session -t smoke`
  - `:ok` sentinel

### Estimated effort

- `prctl` stub: ~30 minutes
- `flock` (stub-acceptable version): ~30 minutes
- `sendmsg`/`recvmsg` with SCM_RIGHTS: **3-5 days** for a correct implementation, including the in-flight ancillary queue, refcount management, and a host-testable codec in kernel-core.

Recommend doing `prctl` + `flock` first as a warm-up; they remove the warning noise and don't unblock anything but they're cheap. Then sink real time into the `sendmsg` path.

## Cross-cutting work item — m3OS syscall coverage audit

While in the syscall table for tmux, the next contributor should also
audit which other "common Linux" syscalls return `-ENOSYS` and rank them
by how many TUI / CLI apps they block. `cargo xtask tui-app-smoke` with
`M3OS_SMOKE_SERIAL_DUMP=…` is the fastest way to enumerate them — grep
the dump for `unhandled syscall N`.

## Files this handoff touched at last-known-good commit

- `xtask/src/main.rs` — `tui_app_smoke_steps` is where the resize step will land
- `xtask/src/port_build.rs` — `build_htop` / `build_tmux` show the link-fix pattern; replicate `CURSES_LIBS` / `LIBTINFO_LIBS` envar discipline for any future ncurses-linked port
- `kernel/src/arch/x86_64/syscall/mod.rs` line 2069 — the `_ => log::warn!("unhandled syscall …")` arm is where the new dispatches land
- `kernel/src/net/unix.rs` — the AF_UNIX byte-stream implementation that needs the ancillary queue

## How to verify "100 percent" once both gaps close

```bash
# 1. Build everything from clean
cargo xtask clean
cargo xtask check                           # clippy / fmt / host tests
cargo xtask port build ncurses              # ~2 min cold
cargo xtask port build libevent             # ~30 s
cargo xtask port build less                 # ~30 s
cargo xtask port build htop                 # ~1 min
cargo xtask port build tmux                 # ~2 min

# 2. Run the gates
cargo xtask tui-smoke         --timeout 180
cargo xtask termios-smoke     --timeout 120
cargo xtask tui-app-smoke     --timeout 300
cargo xtask smoke-test        --timeout 180
M3OS_TUI_APP_REGRESSION=1 git push           # pre-push runs tui-app-smoke

# 3. Flip the task-doc checkboxes for "SurfaceResized synthesis" and
#    "tmux session lifecycle" from [ ] to [x] in
#    docs/roadmap/tasks/69d-tui-app-foundation-tasks.md.
```

When all three smoke gates pass with the full tmux lifecycle + htop
SIGWINCH reflow in place, 69d is 100 percent done.
