# Phase 37 - I/O Multiplexing

**Status:** Complete
**Source Ref:** phase-37
**Depends on:** Phase 22 (TTY) ✅, Phase 23 (Socket API) ✅, Phase 35 (True SMP) ✅
**Builds on:** Replaces the busy-wait `poll()` from Phase 21 with wait-queue-driven
blocking, extends the FD infrastructure with non-blocking flags, and adds `select()`
and `epoll` for scalable I/O readiness notification.
**Primary Components:** syscall (poll/select/epoll/fcntl), pipe, net (sockets),
pty, stdin, process (FdEntry), task (wait_queue)

## Milestone Goal

Programs can wait on multiple file descriptors simultaneously without busy-waiting.
`select()`, an improved `poll()`, and `epoll` provide scalable I/O readiness
notification. Non-blocking I/O (`O_NONBLOCK`) works for sockets, pipes, and PTYs.
Event-driven servers become possible.

## Why This Phase Exists

The current `poll()` implementation uses a yield loop — it repeatedly scans all
requested file descriptors, yields the CPU, and scans again. This wastes CPU time
and scales poorly. Real network servers (like `telnetd`) need to efficiently wait
on many connections simultaneously. Without proper I/O multiplexing, every server
either busy-waits or spawns a thread per connection. This phase introduces the
standard Unix I/O multiplexing APIs that make event-driven programming possible.

## Learning Goals

- Understand the I/O multiplexing problem: one thread, many connections.
- Learn the evolution from `select()` (O(n) scan) to `poll()` (no fd limit) to
  `epoll` (O(1) event delivery).
- See how non-blocking I/O interacts with multiplexing to build efficient servers.
- Understand level-triggered vs edge-triggered notification.

## Feature Scope

### Non-Blocking I/O

Implement `O_NONBLOCK` flag for all FD types:

| FD type | Non-blocking read behavior | Non-blocking write behavior |
|---|---|---|
| Pipe (read end) | Return `EAGAIN` if empty | Return `EAGAIN` if full |
| Pipe (write end) | N/A | Return `EAGAIN` if full |
| Socket (TCP) | Return `EAGAIN` if no data | Return `EAGAIN` if send buffer full |
| Socket (UDP) | Return `EAGAIN` if no datagram | Return `EAGAIN` if send buffer full |
| PTY master | Return `EAGAIN` if no data | Return `EAGAIN` if buffer full |
| PTY slave | Return `EAGAIN` if no data | Return `EAGAIN` if buffer full |
| TTY (stdin) | Return `EAGAIN` if no input | N/A |

**Setting non-blocking:**
- `open()` with `O_NONBLOCK`
- `fcntl(fd, F_SETFL, O_NONBLOCK)`
- `socket()` with `SOCK_NONBLOCK`
- `accept4()` with `SOCK_NONBLOCK`

### Improved `poll()`

The current `poll()` busy-waits in a loop checking fd readiness. Fix this:

1. Register the calling task on each fd's wait queue.
2. Block the task (yield to scheduler).
3. When any fd becomes ready, its wait queue wakes the task.
4. Task re-scans the `pollfd` array and returns ready fds.
5. Handle timeout: schedule a timer wakeup.

### `select()` Syscall

Implement `select()` (syscall 23) and `pselect6()` (syscall 270):

```c
int select(int nfds, fd_set *readfds, fd_set *writefds, fd_set *exceptfds,
           struct timeval *timeout);
```

- Internally convert to poll-style wait.
- `fd_set` is a bitmap — supports up to `FD_SETSIZE` (1024) descriptors.
- Return the number of ready fds, modifying the fd_sets in place.

### `epoll` Interface

Implement the Linux epoll API for scalable I/O:

```c
int epoll_create1(int flags);           // syscall 291
int epoll_ctl(int epfd, int op, int fd, struct epoll_event *event);  // syscall 233
int epoll_wait(int epfd, struct epoll_event *events, int maxevents, int timeout);  // syscall 232
```

**Design:**
- `epoll_create1()` creates an epoll file descriptor (a new FD backend type).
- `epoll_ctl(EPOLL_CTL_ADD/MOD/DEL)` registers/modifies/removes fds.
- `epoll_wait()` blocks until events are ready, then copies them to userspace.
- Support `EPOLLIN`, `EPOLLOUT`, `EPOLLHUP`, `EPOLLERR` events.
- Level-triggered mode (default) — reports readiness as long as the condition holds.
- Edge-triggered mode (`EPOLLET`) — reports only transitions (stretch goal).

**Kernel data structure:**
```rust
struct EpollInstance {
    interest_list: Vec<EpollEntry>,  // registered fds + events
    ready_list: VecDeque<EpollEntry>, // fds with pending events
    wait_queue: WaitQueue,           // tasks blocked in epoll_wait
}
```

### `accept4()` Syscall

Add `accept4()` (syscall 288) which accepts with `SOCK_NONBLOCK` and `SOCK_CLOEXEC`:
```c
int accept4(int sockfd, struct sockaddr *addr, socklen_t *addrlen, int flags);
```

### `fcntl` Improvements

Extend `fcntl()` to support:
- `F_GETFL` — return file status flags (including `O_NONBLOCK`)
- `F_SETFL` — set file status flags (`O_NONBLOCK`, `O_APPEND`)

## Important Components and How They Work

### `FdEntry` Non-Blocking Flag

The `FdEntry` struct in `kernel/src/process/mod.rs` gains a `nonblock: bool` field.
This flag is checked by every read/write syscall path. When set, blocking operations
return `EAGAIN` instead of entering a yield loop.

### Per-FD Wait Queues

Each pollable FD backend (pipes, sockets, PTYs, stdin) gets an associated
`WaitQueue`. When data arrives or space becomes available, the producer wakes all
tasks sleeping on that queue. The existing `WaitQueue` from Phase 35 provides the
sleep/wake primitives.

### `EpollInstance` FD Backend

A new `FdBackend::Epoll { instance_id }` variant. The kernel maintains a global
table of `EpollInstance` structs. Each instance tracks its interest set and a
interest list. `epoll_wait()` scans the interest list for current readiness
(level-triggered), registers on monitored FDs' wait queues, and blocks until
an event occurs. There is no separate ready list — readiness is always
re-evaluated at scan time using `fd_poll_events()`.

### Improved `sys_poll()` Flow

The rewritten poll replaces the yield loop with proper blocking:
1. Scan all fds for immediate readiness (fast path).
2. If none ready, register the calling task on each fd's wait queue via
   `WaitQueue::register()` (non-blocking registration).
3. Re-check readiness after registration (closes the TOCTOU window).
4. Block via `scheduler::block_current_unless_woken()` with a shared atomic
   woken flag.
5. On wakeup, deregister from all wait queues and re-scan.
6. Return ready fds or loop if timeout has not expired.

## How This Builds on Earlier Phases

- Extends Phase 21 by replacing the busy-wait `poll()` with wait-queue-driven blocking.
- Extends Phase 22 (TTY) and Phase 29 (PTY) by adding non-blocking I/O support
  and pollability to terminal file descriptors.
- Extends Phase 23 (Socket API) by adding non-blocking socket I/O, `accept4()`,
  and socket pollability via wait queues.
- Reuses the `WaitQueue` infrastructure from Phase 35 (True SMP) as the core
  blocking/waking mechanism for all multiplexing APIs.
- Extends Phase 21's `fcntl()` stub by implementing `F_GETFL`/`F_SETFL` for real.

## Implementation Outline

1. Add `nonblock` flag to `FdEntry` and wire `fcntl(F_GETFL/F_SETFL)`.
2. Implement `O_NONBLOCK` for pipes (simplest case).
3. Implement `O_NONBLOCK` for sockets.
4. Implement `O_NONBLOCK` for PTYs and TTY/stdin.
5. Add per-FD wait queues to pipes, sockets, PTYs, and stdin.
6. Rewrite `poll()` to use wait queues instead of busy-wait loop.
7. Implement `select()` on top of the improved poll infrastructure.
8. Implement `epoll_create1`, `epoll_ctl`, `epoll_wait`.
9. Implement `accept4()`.
10. Write test programs: echo server using poll, epoll-based multi-client server.
11. Verify the telnet server works with improved poll (no busy-wait).

## Acceptance Criteria

- `O_NONBLOCK` read on an empty pipe returns `EAGAIN` instead of blocking.
- `poll()` blocks efficiently (no CPU spin) and wakes when data arrives.
- `select()` works for read/write readiness on pipes and sockets.
- `epoll_wait()` returns ready events for registered fds.
- An epoll-based echo server handles 10+ simultaneous connections.
- `accept4()` with `SOCK_NONBLOCK` creates non-blocking accepted sockets.
- `fcntl(F_SETFL, O_NONBLOCK)` makes an existing fd non-blocking.
- CPU usage is near zero when waiting for I/O (no busy-wait).
- All existing tests pass without regression.

## Companion Task List

- [Phase 37 Task List](./tasks/37-io-multiplexing-tasks.md)

## How Real OS Implementations Differ

- Linux has **io_uring** — the newest, highest-performance async I/O interface
  using submission/completion rings shared between kernel and userspace.
- Linux's **epoll** is the standard for network servers (nginx, Node.js, etc.)
  and supports edge-triggered mode, `EPOLLONESHOT`, and `EPOLLEXCLUSIVE`.
- **signalfd, timerfd, eventfd** unify signals, timers, and inter-thread events
  as regular file descriptors that work with epoll.
- **splice/tee/vmsplice** enable zero-copy data movement between fds.
- BSD uses **kqueue** instead of epoll — similar concept, different API, arguably
  cleaner design.
- Real implementations handle thousands of concurrent fds; our MAX_FDS is 32.

## Deferred Until Later

- io_uring (submission/completion ring)
- Edge-triggered epoll (EPOLLET) — complex semantics
- signalfd, timerfd, eventfd
- splice/tee/vmsplice (zero-copy)
- POSIX AIO (aio_read, aio_write)
- kqueue compatibility
