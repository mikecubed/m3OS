# Phase 42b — Async Executor: Task List

**Status:** Complete
**Source Ref:** phase-42b
**Depends on:** Phase 37 (I/O Multiplexing) ✅, Phase 43 (SSH Server) ✅
**Goal:** Add a minimal cooperative single-threaded async executor to userspace,
then refactor sshd to use it — eliminating the sunset-local fork patches and all
synchronous workarounds documented in `docs/appendix/sunset-local-fork.md`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Syscall-lib wrappers and crate skeleton | — | ✅ Complete |
| B | Waker and Task (pure logic, host-testable) | A | ✅ Complete |
| C | Reactor (poll-based I/O readiness) | B | ✅ Complete |
| D | Executor (`block_on` loop) | B, C | ✅ Complete |
| E | AsyncFd (pollable file descriptor futures) | C, D | ✅ Complete |
| F | SSHD refactor (async session, sunset waker integration) | A–E | ✅ Complete |
| G | Sunset fork elimination and integration testing | F | ✅ Complete |

---

## Track A — Syscall-lib Wrappers and Crate Skeleton

Add missing syscall wrappers that the reactor needs, and create the async-rt
crate with dual `std`/`no_std` support for host testing.

### A.1 — Add `PollFd` struct and `poll()` wrapper to syscall-lib

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `PollFd`, `poll`
**Why it matters:** The reactor needs `poll()` to block on multiple file
descriptors. Currently sshd inlines `syscall3(SYS_POLL, ...)` directly — a
proper wrapper makes it reusable and testable.

**Acceptance:**
- [ ] `PollFd` is a `#[repr(C)]` struct with `fd: i32`, `events: i16`, `revents: i16`
- [ ] `fn poll(fds: &mut [PollFd], timeout_ms: i32) -> isize` wraps `syscall3(7, ...)`
- [ ] `SYS_POLL` constant exported
- [ ] Existing sshd session.rs updated to use `syscall_lib::PollFd` and `syscall_lib::poll()` instead of local copies

### A.2 — Add `fcntl()` and `set_nonblocking()` to syscall-lib

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `fcntl`, `set_nonblocking`
**Why it matters:** Async I/O requires non-blocking file descriptors so that
`read()`/`write()` return `EAGAIN` instead of blocking. The reactor re-polls
when readiness is signalled.

**Acceptance:**
- [ ] `fn fcntl(fd: i32, cmd: u64, arg: u64) -> isize` wraps `syscall3(72, ...)`
- [ ] `fn set_nonblocking(fd: i32) -> isize` reads current flags with `F_GETFL`, sets `O_NONBLOCK` with `F_SETFL`
- [ ] Constants `SYS_FCNTL`, `F_GETFL`, `F_SETFL`, `O_NONBLOCK` exported

### A.3 — Create async-rt crate skeleton

**Files:**
- `userspace/async-rt/Cargo.toml`
- `userspace/async-rt/src/lib.rs`

**Symbol:** `async_rt`
**Why it matters:** A dedicated crate keeps the executor reusable by any
userspace daemon, not just sshd. The dual `std`/`no_std` feature flag pattern
(matching kernel-core) enables host-side unit testing with `cargo test`.

**Acceptance:**
- [ ] `userspace/async-rt/` exists with `#![cfg_attr(not(feature = "std"), no_std)]`
- [ ] Default feature is `std` (for host tests); `no_std + alloc` for kernel target
- [ ] Added to workspace members in root `Cargo.toml`
- [ ] Module stubs: `task.rs`, `reactor.rs`, `executor.rs`, `io.rs`
- [ ] `cargo test -p async-rt` passes (empty)
- [ ] `cargo xtask check` passes

---

## Track B — Waker and Task (Pure Logic, Host-Testable)

Implement the `core::task::Waker` via `RawWakerVTable` and the internal `Task`
type. All tests run on the host — no kernel or QEMU needed.

### B.1 — Test and implement Task struct with woken flag

**File:** `userspace/async-rt/src/task.rs`
**Symbol:** `Task`, `task_waker`
**Why it matters:** The Waker is the core async primitive — sunset calls
`waker.wake()` when it has data ready, and the executor must know which task
to re-poll. A `Cell<bool>` flag is sufficient for single-threaded use.

**Acceptance:**
- [ ] Test: construct a `Task` wrapping a trivial future, create a `Waker` from it, call `wake()`, assert `task.is_woken()` returns true
- [ ] Test: calling `wake()` twice is idempotent (no panic, still woken)
- [ ] `Task` stores `future: Pin<Box<dyn Future<Output = ()>>>` and `woken: Cell<bool>`
- [ ] `task_waker(task: &Task) -> Waker` builds a `Waker` from `RawWakerVTable`

### B.2 — Test Waker clone and drop semantics

**File:** `userspace/async-rt/src/task.rs`
**Symbol:** `RawWakerVTable` (clone, wake, wake_by_ref, drop)
**Why it matters:** Sunset clones wakers internally (e.g., storing input_waker
and output_waker separately). The clone/drop vtable must be correct to avoid
use-after-free or double-free.

**Acceptance:**
- [ ] Test: clone a Waker, wake via the clone, verify the original Task is woken
- [ ] Test: drop both original and clone without panic
- [ ] Waker data uses `Rc<WakerInner>` for refcounting (single-threaded, no Arc needed)

### B.3 — Test self-pipe wake integration

**File:** `userspace/async-rt/src/task.rs`
**Symbol:** `WAKE_PIPE_FD`
**Why it matters:** When sunset calls `waker.wake()` from inside `input()` or
`consume_output()`, the executor may be blocked in `poll()`. Writing a byte to
the self-pipe unblocks `poll()` so the executor can re-poll the woken task.

**Acceptance:**
- [ ] Test (std): create a pipe, set `WAKE_PIPE_FD` to write end, call `wake()`, read 1 byte from read end — succeeds
- [ ] Test (std): calling `wake()` when `WAKE_PIPE_FD` is -1 (not set) does not panic — just sets the flag
- [ ] `WAKE_PIPE_FD` is a `static Cell<i32>` initialized to -1

---

## Track C — Reactor (Poll-Based I/O Readiness)

The reactor owns a self-pipe and a list of FD interests. It calls `poll()` and
wakes the appropriate wakers when FDs become ready. Host-testable using real
OS pipes under `std`.

### C.1 — Test and implement Reactor construction

**File:** `userspace/async-rt/src/reactor.rs`
**Symbol:** `Reactor`, `Reactor::new`
**Why it matters:** The reactor's self-pipe is the mechanism that allows
wakers to interrupt a blocked `poll()` call. If the pipe is not created
correctly, the executor deadlocks.

**Acceptance:**
- [ ] Test (std): `Reactor::new()` succeeds, `wake_read_fd` and `wake_write_fd` are valid FDs
- [ ] Self-pipe created via `pipe()` (libc under std, syscall-lib under no_std)
- [ ] `WAKE_PIPE_FD` set to write end on construction
- [ ] `interests: Vec<Interest>` initialized empty

### C.2 — Test FD registration and poll wakeup

**File:** `userspace/async-rt/src/reactor.rs`
**Symbol:** `Reactor::register`, `Reactor::poll_once`
**Why it matters:** This is the core I/O readiness loop. The reactor must
correctly build the `pollfd` array, call `poll()`, and wake the right wakers
when FDs become ready.

**Acceptance:**
- [ ] Test (std): create a pipe, register read-end for POLLIN with a waker, write a byte to write-end, call `poll_once(100)` — waker is called, returns 1
- [ ] Test (std): register two pipes, write to only one, verify only that pipe's waker fires
- [ ] `poll_once()` builds `PollFd` array from interests plus self-pipe read-end
- [ ] Ready FDs trigger `waker.wake_by_ref()` for read or write waker as appropriate
- [ ] Self-pipe bytes drained after each `poll_once()`

### C.3 — Test poll timeout (no ready FDs)

**File:** `userspace/async-rt/src/reactor.rs`
**Symbol:** `Reactor::poll_once`
**Why it matters:** The executor must not spin-loop when no FDs are ready.
The timeout ensures the executor yields to the kernel scheduler.

**Acceptance:**
- [ ] Test (std): register a pipe but do not write, call `poll_once(50)`, verify it returns 0 after approximately 50ms (not instantly)
- [ ] No wakers called when poll times out

### C.4 — Test self-pipe wakeup interrupts poll

**File:** `userspace/async-rt/src/reactor.rs`
**Symbol:** `Reactor::poll_once`
**Why it matters:** When sunset calls `waker.wake()` while the executor is
blocked in `poll()`, the self-pipe write must cause `poll()` to return
immediately rather than waiting for the full timeout.

**Acceptance:**
- [ ] Test (std): spawn a thread that sleeps 10ms then calls `wake()`, call `poll_once(5000)` on main thread — returns in ~10ms, not 5000ms
- [ ] Self-pipe byte is drained after wakeup

### C.5 — Implement FD deregistration

**File:** `userspace/async-rt/src/reactor.rs`
**Symbol:** `Reactor::deregister`
**Why it matters:** When a socket or PTY is closed, its entry must be removed
from the reactor to avoid polling a closed FD (which returns POLLNVAL).

**Acceptance:**
- [ ] Test (std): register a pipe, deregister it, call `poll_once(10)` — no waker called, no error
- [ ] `deregister(fd)` removes the entry from the interests vec

---

## Track D — Executor (`block_on` Loop)

The executor drives futures to completion by polling them when their waker
fires, using the reactor for I/O readiness between polls. Host-testable.

### D.1 — Test and implement block_on for immediately-ready futures

**File:** `userspace/async-rt/src/executor.rs`
**Symbol:** `block_on`
**Why it matters:** The simplest case — a future that returns `Ready` on first
poll. This validates the basic executor structure and waker plumbing without
any I/O.

**Acceptance:**
- [ ] Test: `block_on(&mut reactor, async { 42 })` returns `42`
- [ ] Test: `block_on(&mut reactor, async { "hello" })` returns `"hello"`
- [ ] `block_on()` pins the future, creates a waker, polls once, returns if Ready

### D.2 — Test block_on with pending-then-ready future

**File:** `userspace/async-rt/src/executor.rs`
**Symbol:** `block_on`
**Why it matters:** Most real futures return `Pending` at least once. The
executor must re-poll after the waker fires. This validates the wake → re-poll
cycle without I/O involvement.

**Acceptance:**
- [ ] Test: a future that returns `Pending` on first poll (storing the waker), then an external call to `waker.wake()`, then `Ready(99)` on second poll — `block_on()` returns `99`
- [ ] The executor does not busy-spin — it calls `reactor.poll_once()` between re-polls

### D.3 — Test block_on with reactor-driven wakeup

**File:** `userspace/async-rt/src/executor.rs`
**Symbol:** `block_on`
**Why it matters:** This ties the executor to real I/O. A future awaits a pipe
becoming readable; the executor blocks in `poll()` until data arrives, then
re-polls the future to completion.

**Acceptance:**
- [ ] Test (std): create a pipe, spawn a thread that writes after 20ms, `block_on()` a future that registers the read-end with the reactor and awaits readiness — returns successfully
- [ ] The executor blocks in `reactor.poll_once()` (not spinning) while waiting

---

## Track E — AsyncFd (Pollable File Descriptor Futures)

`AsyncFd` wraps a raw file descriptor and provides `readable()` / `writable()`
futures that integrate with the reactor. This is the user-facing I/O API.

### E.1 — Test and implement AsyncFd::readable()

**File:** `userspace/async-rt/src/io.rs`
**Symbol:** `AsyncFd`, `AsyncFd::readable`, `ReadableFuture`
**Why it matters:** This is the primary I/O primitive for the sshd refactor.
`sock.readable().await` replaces the manual poll loop for socket readiness.

**Acceptance:**
- [ ] Test (std): create a pipe, write data, `block_on(async_fd.readable())` resolves immediately
- [ ] Test (std): create a pipe, no data, spawn thread to write after 20ms, `block_on(async_fd.readable())` resolves after data arrives
- [ ] `readable()` returns a future that registers with the reactor on first poll and resolves when POLLIN is ready
- [ ] The waker is stored in the reactor's interest entry for the FD

### E.2 — Test and implement AsyncFd::writable()

**File:** `userspace/async-rt/src/io.rs`
**Symbol:** `AsyncFd::writable`, `WritableFuture`
**Why it matters:** Needed for flushing sunset's output buffer to the TCP
socket without blocking the session.

**Acceptance:**
- [ ] Test (std): `block_on(async_fd.writable())` resolves immediately for a pipe with buffer space
- [ ] `writable()` returns a future that registers POLLOUT with the reactor

### E.3 — Test AsyncFd bidirectional relay pattern

**File:** `userspace/async-rt/src/io.rs`
**Symbol:** `AsyncFd`
**Why it matters:** The sshd data relay reads from one FD and writes to
another. This test validates the complete pattern: await readable on source,
read, await writable on dest, write.

**Acceptance:**
- [ ] Test (std): create two pipe pairs (simulating socket + pty), write to pipe A, `block_on()` a future that reads from A and writes to B, verify data arrives at B
- [ ] No data loss or deadlock

---

## Track F — SSHD Refactor (Async Session)

Incrementally convert the synchronous sshd session loop to use the async
executor. Each step preserves existing behavior before adding the next.

### F.1 — Replace inline poll with syscall-lib wrappers

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `PollFd`, `poll`
**Why it matters:** Safe intermediate step — removes the local `PollFd` struct
and inline `syscall3` call, replacing them with `syscall_lib::PollFd` and
`syscall_lib::poll()`. No behavior change.

**Acceptance:**
- [ ] Local `PollFd` struct and `poll()` fn removed from session.rs
- [ ] Replaced with `use syscall_lib::{PollFd, poll}`
- [ ] QEMU smoke test: ssh login and interactive shell still work

### F.2 — Add async-rt dependency to sshd

**File:** `userspace/sshd/Cargo.toml`
**Symbol:** `async-rt`
**Why it matters:** Wires the executor crate into sshd so the async refactor
can begin.

**Acceptance:**
- [ ] `async-rt = { path = "../async-rt", default-features = false, features = ["alloc"] }` in sshd Cargo.toml
- [ ] `cargo xtask check` passes

### F.3 — Wrap run_session in block_on

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `run_session`, `async_session`
**Why it matters:** The outermost structural change. `run_session()` becomes a
thin wrapper around `block_on(async_session(...))`. The inner logic initially
remains synchronous inside the async block — this is a mechanical refactor.

**Acceptance:**
- [ ] `run_session()` creates a `Reactor` and calls `block_on(&mut reactor, async_session(...))`
- [ ] `async_session()` is an `async fn` containing the existing session logic
- [ ] QEMU smoke test: ssh login still works (no behavioral change)

### F.4 — Convert socket read path to async

**Files:**
- `userspace/sshd/src/session.rs`

**Symbol:** `AsyncFd`, `sock.readable().await`
**Why it matters:** Replaces the manual poll + blocking read on the socket FD
with `sock.readable().await` + non-blocking read. Eliminates `sock_pending_buf`.

**Acceptance:**
- [ ] Socket FD set to non-blocking via `set_nonblocking()`
- [ ] `sock.readable().await` replaces the POLLIN check on the socket
- [ ] `sock_pending_buf` and its drain loop removed
- [ ] QEMU smoke test: ssh login and data transfer still work

### F.5 — Convert PTY read path to async

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `AsyncFd`, `pty.readable().await`
**Why it matters:** Same conversion for the PTY master FD. Eliminates
`pty_pending_buf` and the manual PTY poll entry.

**Acceptance:**
- [ ] PTY master FD set to non-blocking
- [ ] `pty.readable().await` replaces the POLLIN check on the PTY
- [ ] `pty_pending_buf` and its drain loop removed
- [ ] QEMU smoke test: shell output relayed correctly over SSH

### F.6 — Wire sunset wakers to executor

**Files:**
- `userspace/sshd/src/session.rs`

**Symbol:** `runner.set_input_waker`, `runner.set_output_waker`
**Why it matters:** This is the key integration point. Sunset's Runner has
built-in waker support (`set_input_waker`, `set_output_waker`,
`set_channel_read_waker`, `set_channel_write_waker`) designed for async use.
Wiring these to the executor's waker means sunset directly wakes the executor
when it is ready for input or has output to flush — eliminating the 200ms poll
timeout and manual backpressure buffers.

**Acceptance:**
- [ ] `runner.set_input_waker(waker)` called with the current task's waker
- [ ] `runner.set_output_waker(waker)` called with the current task's waker
- [ ] `runner.set_channel_read_waker()` / `set_channel_write_waker()` wired for PTY relay
- [ ] The fixed 200ms poll timeout replaced with waker-driven wake
- [ ] QEMU smoke test: interactive latency noticeably improved (keystrokes echo faster)

### F.7 — Remove break-after-resume and error-as-recoverable patterns

**File:** `userspace/sshd/src/session.rs`
**Symbol:** inner event loop
**Why it matters:** The async executor handles the flush → poll → re-poll
sequencing naturally. The manual break-after-resume pattern, the
error-as-recoverable fallback, and the `continue`-vs-`break` distinctions
documented in `sunset-local-fork.md` become unnecessary.

**Acceptance:**
- [ ] Inner `loop { flush; progress; break }` pattern replaced with straightforward async event handling
- [ ] `Err` from `runner.progress()` treated as a real error (not silently recovered)
- [ ] Lazy PTY allocation at shell time removed if `SessionPty` events now arrive reliably
- [ ] QEMU smoke test: full session lifecycle works (login → shell → commands → logout)

---

## Track G — Sunset Fork Elimination and Integration Testing

Remove the sunset-local patches and verify the upstream-compatible crate works
with the async executor.

### G.1 — Remove BadUsage recovery patch from sunset-local

**File:** `sunset-local/src/runner.rs`
**Symbol:** `Runner::progress` (line ~295)
**Why it matters:** The BadUsage error was caused by the synchronous event loop
violating sunset's expected async sequencing. With a proper async executor
driving `progress()` and I/O as cooperating tasks, `resume_event` stickiness
should not occur. If it does, this task fails and the patch stays.

**Acceptance:**
- [ ] Lines 293–301 reverted to upstream behavior: `if prev.needs_resume() { return error::BadUsage.fail(); }`
- [ ] QEMU smoke test: ssh login, authentication, PTY allocation, shell — all succeed without BadUsage
- [ ] Multiple sequential SSH sessions work (connect, run commands, disconnect, reconnect)

### G.2 — Evaluate window size patch necessity

**File:** `sunset-local/src/config.rs`
**Symbol:** `DEFAULT_WINDOW`, `DEFAULT_MAX_PACKET`
**Why it matters:** The 32KB window size is independent of sync/async — it is a
throughput setting. This task evaluates whether to keep it, upstream it, or
accept the 1KB default.

**Acceptance:**
- [ ] Decision documented: keep 32KB (fork stays for config), or upstream via Config API, or accept 1KB
- [ ] If keeping 32KB: window size patch remains, document as sole reason for fork
- [ ] If upstreaming: PR or issue opened on sunset repository

### G.3 — QEMU integration test: full SSH session lifecycle

**Files:**
- `xtask/src/main.rs` (smoke test steps)

**Symbol:** SSH smoke test
**Why it matters:** End-to-end validation that the async executor, refactored
sshd, and (potentially) unpatched sunset work together for real SSH sessions.

**Acceptance:**
- [ ] SSH login with password authentication succeeds
- [ ] SSH login with public key authentication succeeds
- [ ] Interactive shell commands work (ls, cat, echo, pipes)
- [ ] Multiple simultaneous SSH sessions work (two concurrent connections)
- [ ] Session cleanup on disconnect (PTY closed, child reaped, socket closed)
- [ ] No memory leaks observable via `meminfo` after repeated connect/disconnect cycles

### G.4 — Update sunset-local fork documentation

**File:** `docs/appendix/sunset-local-fork.md`
**Symbol:** documentation
**Why it matters:** The fork documentation must reflect the current state —
which patches were eliminated, which remain, and what changed.

**Acceptance:**
- [ ] Patch 1 (BadUsage) section updated: eliminated by async executor, or still needed with explanation
- [ ] Patch 2 (Window size) section updated with decision from G.2
- [ ] Workarounds section updated: which are removed, which remain
- [ ] "What Would Need to Change" section updated to reflect current state

---

## Documentation Notes

- This phase extends Phase 43 (SSH Server) by replacing the synchronous poll
  loop with a proper async executor, eliminating sunset fork patches.
- The async-rt crate is general-purpose and may be reused by future network
  daemons (e.g., a hypothetical httpd or ftpd).
- The dual `std`/`no_std` pattern for host testing follows the kernel-core
  precedent established in Phase 33.
- No kernel changes are required — all necessary syscalls (poll, pipe, fcntl,
  nanosleep) already exist from earlier phases.
