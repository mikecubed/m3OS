# Phase 37 — I/O Multiplexing: Task List

**Status:** Complete
**Source Ref:** phase-37
**Depends on:** Phase 22 (TTY) ✅, Phase 23 (Socket API) ✅, Phase 35 (True SMP) ✅
**Goal:** Replace the busy-wait `poll()` with wait-queue-driven blocking, add
`O_NONBLOCK` support to all FD types, implement `select()` and `epoll` for scalable
I/O readiness notification, and add `accept4()` for non-blocking socket acceptance.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Non-blocking I/O infrastructure | — | Complete |
| B | Non-blocking FD backends | A | Complete |
| C | Per-FD wait queues | — | Complete |
| D | Improved poll() | A, C | Complete |
| E | select() syscall | D | Complete |
| F | epoll interface | C, D | Complete |
| G | accept4() syscall | A | Complete |
| H | Integration testing and documentation | A–G | Complete |

---

## Track A — Non-Blocking I/O Infrastructure

Add the `O_NONBLOCK` flag to the FD layer and wire `fcntl(F_GETFL/F_SETFL)` so
any file descriptor can be toggled between blocking and non-blocking mode.

### A.1 — Add `nonblock` field to `FdEntry`

**File:** `kernel/src/process/mod.rs`
**Symbol:** `FdEntry`
**Why it matters:** Every read/write syscall path must be able to check whether the
FD is non-blocking. A per-FD flag is the simplest way to track this across all
backend types.

**Acceptance:**
- [x] `FdEntry` has a `nonblock: bool` field, defaulting to `false`
- [x] `fork()` and `dup()` propagate the flag to the new FD entry
- [x] `FdEntry` creation sites compile and pass `cargo xtask check`

### A.2 — Implement `fcntl(F_GETFL)` and `fcntl(F_SETFL)`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_fcntl`
**Why it matters:** The current `F_GETFL`/`F_SETFL` handlers are stubs that return 0.
Userspace relies on `fcntl(fd, F_SETFL, O_NONBLOCK)` to toggle non-blocking mode,
and `F_GETFL` to query it.

**Acceptance:**
- [x] `F_GETFL` returns `O_NONBLOCK` (0x800) if `nonblock` is set, 0 otherwise
- [x] `F_SETFL` with `O_NONBLOCK` sets `nonblock = true` on the FD
- [x] `F_SETFL` without `O_NONBLOCK` clears `nonblock = false`
- [x] Existing `F_DUPFD`, `F_GETFD`, `F_SETFD` behavior unchanged

### A.3 — Honor `SOCK_NONBLOCK` in `socket()` creation

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_socket`
**Why it matters:** The socket syscall already strips the `SOCK_NONBLOCK` flag but
does not set the FD's nonblock field. This must set `nonblock = true` on the new
socket FD when the flag is present.

**Acceptance:**
- [x] `socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0)` creates an FD with `nonblock = true`
- [x] `socket()` without `SOCK_NONBLOCK` creates an FD with `nonblock = false`

---

## Track B — Non-Blocking FD Backends

Wire `O_NONBLOCK` into each FD backend's read and write paths so they return
`EAGAIN` instead of blocking.

### B.1 — Non-blocking pipe read and write

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/pipe.rs`
**Symbol:** `sys_linux_read` (pipe path), `sys_linux_write` (pipe path)
**Why it matters:** Pipes are the simplest FD type with blocking behavior. The
`pipe_read()` function already returns `Err(true)` for would-block — this task
wires that to `EAGAIN` when the FD is non-blocking instead of entering the yield
loop.

**Acceptance:**
- [x] Non-blocking read on an empty pipe returns `-EAGAIN` immediately
- [x] Non-blocking write on a full pipe returns `-EAGAIN` immediately
- [x] Blocking pipe read/write still works (yield loop) when `nonblock = false`
- [x] Broken-pipe `EPIPE`/`SIGPIPE` behavior unchanged

### B.2 — Non-blocking socket read and write

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/net/mod.rs`
**Symbol:** `sys_recvfrom`, `sys_sendto`
**Why it matters:** Network servers need non-blocking sockets to avoid stalling on
a single slow client. TCP recv already has a yield loop that must check the
nonblock flag.

**Acceptance:**
- [x] Non-blocking TCP recv with no data returns `-EAGAIN`
- [x] Non-blocking UDP recvfrom with no datagram returns `-EAGAIN`
- [x] Non-blocking send on a full buffer returns `-EAGAIN`
- [x] `MSG_DONTWAIT` flag still works independently of the FD's nonblock setting

### B.3 — Non-blocking PTY read and write

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/pty.rs`
**Symbol:** `sys_linux_read` (PTY path), `sys_linux_write` (PTY path)
**Why it matters:** PTY master/slave pairs carry terminal I/O for remote sessions.
Non-blocking PTY reads are needed for multiplexed terminal servers.

**Acceptance:**
- [x] Non-blocking read on PTY master with empty s2m buffer returns `-EAGAIN`
- [x] Non-blocking read on PTY slave with empty m2s buffer returns `-EAGAIN`
- [x] Non-blocking write on a full PTY ring buffer returns `-EAGAIN`
- [x] Blocking PTY I/O unchanged when `nonblock = false`

### B.4 — Non-blocking TTY/stdin read

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/stdin.rs`
**Symbol:** `sys_linux_read` (stdin/TTY path)
**Why it matters:** Interactive programs may want to poll stdin without blocking.
The current stdin read path uses a yield loop that must respect the nonblock flag.

**Acceptance:**
- [x] Non-blocking read on stdin with no pending input returns `-EAGAIN`
- [x] Non-blocking read on a `DeviceTTY` fd with no input returns `-EAGAIN`
- [x] Blocking stdin read unchanged when `nonblock = false`

---

## Track C — Per-FD Wait Queues

Add wait queues to each pollable FD backend so that `poll()`, `select()`, and
`epoll` can sleep until data arrives instead of busy-waiting.

### C.1 — Add wait queue to pipe table

**File:** `kernel/src/pipe.rs`
**Symbol:** `PIPES` (global pipe table)
**Why it matters:** When data is written to a pipe (or a writer closes), tasks
blocked in poll on the read end must be woken. A per-pipe wait queue provides
the wakeup mechanism.

**Acceptance:**
- [x] Each pipe slot has an associated `WaitQueue`
- [x] `pipe_write()` calls `wake_all()` on the pipe's wait queue after writing data
- [x] `pipe_read()` calls `wake_all()` after consuming data (wakes write-blocked pollers)
- [x] Writer/reader close wakes the wait queue (EOF / broken pipe notification)

### C.2 — Add wait queue to socket table

**File:** `kernel/src/net/mod.rs`
**Symbol:** `SOCKET_TABLE`
**Why it matters:** Socket state changes (data arrival, connection accepted,
connection closed) must wake tasks blocked in poll/epoll on that socket.

**Acceptance:**
- [x] Each socket slot has an associated `WaitQueue`
- [x] TCP data arrival wakes the socket's wait queue
- [x] TCP connection establishment wakes the listening socket's wait queue
- [x] Socket close/shutdown wakes the wait queue
- [x] UDP datagram arrival wakes the socket's wait queue

### C.3 — Add wait queue to PTY table

**File:** `kernel/src/pty.rs`
**Symbol:** `PTY_TABLE`
**Why it matters:** PTY master/slave data transfers must wake tasks that are
polling the other end of the PTY pair.

**Acceptance:**
- [x] Each PTY pair has two wait queues: one for master waiters, one for slave waiters
- [x] Write to PTY master wakes slave waiters; write to PTY slave wakes master waiters
- [x] PTY close wakes both wait queues

### C.4 — Add wait queue to stdin

**File:** `kernel/src/stdin.rs`
**Symbol:** `STDIN`
**Why it matters:** Keyboard input must wake tasks that are polling stdin for
read readiness.

**Acceptance:**
- [x] `stdin` module has a `WaitQueue`
- [x] `push_char()` calls `wake_all()` after enqueuing input
- [x] `signal_eof()` calls `wake_all()`

---

## Track D — Improved `poll()`

Rewrite `sys_poll()` to use wait queues instead of the current busy-wait loop.

### D.1 — Implement FD readiness query helper

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `fd_poll_events`
**Why it matters:** Both `poll()` and `select()` need to query an FD's current
readiness (POLLIN, POLLOUT, POLLHUP, POLLERR). Extracting this into a helper
avoids duplicating the per-backend readiness checks.

**Acceptance:**
- [x] `fd_poll_events(fd_entry) -> u16` returns the current event mask for any FD type
- [x] Covers PipeRead, PipeWrite, Socket (TCP/UDP), PtyMaster, PtySlave, Stdin, DeviceTTY
- [x] Returns `POLLHUP` when the remote end is closed (broken pipe, TCP FIN)
- [x] Returns `POLLERR` on error conditions (TCP RST)

### D.2 — Implement FD wait queue registration helper

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `fd_register_waiter`
**Why it matters:** `poll()` must register the calling task on the wait queue of
each monitored FD before blocking. A helper keeps the registration logic in one
place for reuse by `select()`.

**Acceptance:**
- [x] `fd_register_waiter(fd_entry)` registers the current task on the FD's wait queue
- [x] Supports all pollable FD backends (pipe, socket, PTY, stdin/TTY)
- [x] Returns a handle or list for later deregistration
- [x] Non-pollable FD types (Ramdisk, Tmpfs, DevNull) are skipped

### D.3 — Rewrite `sys_poll()` with wait-queue blocking

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_poll`
**Why it matters:** This is the core improvement — the current yield loop wastes
CPU. The new implementation scans once, blocks on wait queues if nothing is ready,
and wakes only when an FD becomes ready or the timeout expires.

**Acceptance:**
- [x] First scan: if any FD is ready, return immediately (fast path preserved)
- [x] No FDs ready: register on all wait queues and block via `WaitQueue::sleep()`
- [x] Wakeup: re-scan all FDs and return the ready count
- [x] Timeout of 0: non-blocking scan only (existing behavior)
- [x] Timeout of -1: block indefinitely
- [x] Positive timeout: block with timeout (yield-loop-with-timer if no kernel timer API)
- [x] Deregister from all wait queues before returning
- [x] CPU usage near zero while blocked (no spin)
- [x] Existing `telnetd` works without regression

---

## Track E — `select()` Syscall

Implement `select()` and `pselect6()` using the improved poll infrastructure.

### E.1 — Implement `sys_select()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_select`
**Why it matters:** `select()` is the oldest Unix I/O multiplexing API. Many C
programs and libraries (including musl's internal code) use it. It must be
implemented for POSIX compatibility.

**Acceptance:**
- [x] Syscall 23 dispatches to `sys_select()`
- [x] Reads `fd_set` bitmaps from userspace for read, write, and except sets
- [x] Checks readiness of each set bit using `fd_poll_events()`
- [x] Blocks using the same wait-queue mechanism as `poll()` when no FDs are ready
- [x] Returns the total number of ready FDs
- [x] Writes modified `fd_set` bitmaps back to userspace (only ready bits set)
- [x] `NULL` fd_set pointer means "don't check this set"
- [x] `nfds` > `MAX_FDS` is clamped to `MAX_FDS`

### E.2 — Implement `sys_pselect6()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_pselect6`
**Why it matters:** `pselect6()` is the modern variant used by musl. It takes a
`timespec` instead of `timeval` and atomically sets a signal mask.

**Acceptance:**
- [x] Syscall 270 dispatches to `sys_pselect6()`
- [x] Handles `timespec` timeout (seconds + nanoseconds)
- [x] Signal mask argument accepted but not applied (signals not yet masked)
- [x] Internally reuses the `select()` implementation

---

## Track F — `epoll` Interface

Implement the Linux epoll API as a new FD backend type.

### F.1 — Add `EpollInstance` data structure

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/process/mod.rs`
**Symbol:** `EpollInstance`, `FdBackend::Epoll`
**Why it matters:** epoll needs a kernel object to track the interest set (which FDs
to monitor and what events) and a ready list (which FDs currently have events).
This is stored in a global table indexed by instance ID.

**Acceptance:**
- [x] `EpollInstance` struct with interest list, ready list, and wait queue
- [x] Global `EPOLL_TABLE` with `Mutex` protection (max 16 instances)
- [x] `FdBackend::Epoll { instance_id: usize }` variant added to `FdBackend` enum
- [x] epoll FDs are closeable — closing frees the instance

### F.2 — Implement `sys_epoll_create1()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_epoll_create1`
**Why it matters:** Creates the epoll instance and returns it as a file descriptor.
This is the entry point for all epoll usage.

**Acceptance:**
- [x] Syscall 291 dispatches to `sys_epoll_create1()`
- [x] Allocates an `EpollInstance` and installs it as a new FD
- [x] `EPOLL_CLOEXEC` flag sets `cloexec = true` on the FD
- [x] Returns the new FD number, or `-EMFILE` if FD table is full
- [x] Returns `-ENOMEM` if epoll table is full

### F.3 — Implement `sys_epoll_ctl()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_epoll_ctl`
**Why it matters:** Adds, modifies, or removes FDs from the epoll interest set.
This is how userspace tells the kernel which FDs to monitor.

**Acceptance:**
- [x] Syscall 233 dispatches to `sys_epoll_ctl()`
- [x] `EPOLL_CTL_ADD` (1): adds FD with requested events to interest list
- [x] `EPOLL_CTL_MOD` (3): updates events for an already-registered FD
- [x] `EPOLL_CTL_DEL` (2): removes FD from interest list
- [x] Returns `-EEXIST` for duplicate ADD, `-ENOENT` for MOD/DEL on unregistered FD
- [x] Reads `epoll_event` struct from userspace (events: u32, data: u64)
- [x] Supports `EPOLLIN`, `EPOLLOUT`, `EPOLLHUP`, `EPOLLERR` event flags

### F.4 — Implement `sys_epoll_wait()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_epoll_wait`
**Why it matters:** This is the core blocking call. It returns ready events to
userspace, blocking if none are ready. This is what makes epoll useful for
event-driven servers.

**Acceptance:**
- [x] Syscall 232 dispatches to `sys_epoll_wait()`
- [x] Scans interest list for ready FDs (level-triggered: check current readiness)
- [x] If ready events found, copies up to `maxevents` to userspace and returns count
- [x] If no ready events, blocks on the epoll instance's wait queue
- [x] Timeout of 0: non-blocking scan only
- [x] Timeout of -1: block indefinitely
- [x] Positive timeout: block with timeout
- [x] Returns `-EINVAL` for `maxevents <= 0`

### F.5 — Wire epoll wakeups into FD backends

**Files:**
- `kernel/src/pipe.rs`
- `kernel/src/net/mod.rs`
- `kernel/src/pty.rs`
- `kernel/src/stdin.rs`
**Symbol:** (various write/close paths)
**Why it matters:** When a monitored FD becomes ready, the epoll instance's wait
queue must be woken so `epoll_wait()` can return. This connects the per-FD events
to the epoll notification mechanism.

**Acceptance:**
- [x] Pipe write/close wakes any epoll instance monitoring that pipe
- [x] Socket data arrival / connection / close wakes epoll
- [x] PTY data transfer wakes epoll
- [x] stdin input wakes epoll
- [x] Wakeup is efficient — only epoll instances monitoring the specific FD are woken

---

## Track G — `accept4()` Syscall

### G.1 — Implement `sys_accept4()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_accept4`
**Why it matters:** `accept4()` is the modern accept that avoids a separate
`fcntl()` call to set non-blocking mode on the accepted socket. Event-driven
servers need this to accept connections without a race window.

**Acceptance:**
- [x] Syscall 288 dispatches to `sys_accept4()`
- [x] Reuses the existing `sys_accept()` logic for connection acceptance
- [x] `SOCK_NONBLOCK` flag sets `nonblock = true` on the accepted FD
- [x] `SOCK_CLOEXEC` flag sets `cloexec = true` on the accepted FD
- [x] Flags of 0 behaves identically to `accept()`

---

## Track H — Integration Testing and Documentation

### H.1 — Poll efficiency validation

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_poll`
**Why it matters:** The primary goal of this phase is eliminating busy-wait. This
task validates that poll truly blocks without spinning.

**Acceptance:**
- [x] `poll()` on an idle pipe does not consume CPU (no yield loop)
- [x] Data written to a pipe immediately wakes a poll waiter
- [x] `telnetd` runs with improved poll — no regression in remote shell behavior
- [x] Multi-FD poll wakes when any one of the monitored FDs becomes ready

### H.2 — Non-blocking I/O validation

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_fcntl`
**Why it matters:** Validates the complete non-blocking I/O lifecycle: set flag,
read returns EAGAIN, set blocking again, read blocks normally.

**Acceptance:**
- [x] `fcntl(fd, F_SETFL, O_NONBLOCK)` followed by read on empty pipe returns `-EAGAIN`
- [x] `fcntl(fd, F_GETFL)` returns `O_NONBLOCK` for non-blocking FDs
- [x] `fcntl(fd, F_SETFL, 0)` clears non-blocking and read blocks again
- [x] `SOCK_NONBLOCK` in `socket()` creates a non-blocking socket

### H.3 — epoll echo server test

**File:** `userspace/` (new test binary or shell test)
**Symbol:** n/a
**Why it matters:** An epoll-based echo server is the canonical use case. This
validates that all epoll operations work end-to-end.

**Acceptance:**
- [x] Test creates an epoll instance, adds a listening socket
- [x] Accepts connections and adds them to epoll
- [x] Echoes data back on readable sockets
- [x] Handles 10+ simultaneous connections without error

### H.4 — Regression test suite

**File:** `xtask/src/main.rs`
**Symbol:** `test`
**Why it matters:** Syscall and FD changes are high-risk. Every existing test must
continue to pass.

**Acceptance:**
- [x] All QEMU integration tests pass
- [x] All kernel-core host tests pass
- [x] `cargo xtask check` clean (no warnings)
- [x] Existing pipe, socket, and PTY workloads function correctly

### H.5 — Update documentation

**Files:**
- `docs/roadmap/37-io-multiplexing.md`
- `docs/roadmap/tasks/37-io-multiplexing-tasks.md`
- `docs/roadmap/README.md`
**Symbol:** n/a
**Why it matters:** Roadmap docs must reflect completion status and any scope
changes discovered during implementation.

**Acceptance:**
- [x] Phase 37 design doc updated with completion status
- [x] Task list updated with completion status
- [x] Roadmap README row updated from "Planned" to "Complete"
- [x] Companion Task List link in design doc points to the task file

---

## Documentation Notes

- Phase 37 replaces the Phase 21 busy-wait `poll()` with wait-queue-driven blocking.
- The `FdEntry` struct gains a `nonblock` field — this is a new per-FD flag not
  present in earlier phases.
- The `fcntl()` stubs for `F_GETFL`/`F_SETFL` (Phase 21) are replaced with real
  implementations.
- Per-FD wait queues are new infrastructure; the `WaitQueue` type from Phase 35
  is reused but each pollable backend now owns one.
- `epoll` introduces a new `FdBackend` variant — the first FD type that monitors
  other FDs rather than representing an I/O stream.
- `accept4()` is a new syscall that wraps the existing `accept()` logic with flag
  support.
