# Phase 36 - I/O Multiplexing

## Milestone Goal

Programs can wait on multiple file descriptors simultaneously without busy-waiting.
`select()`, an improved `poll()`, and `epoll` provide scalable I/O readiness
notification. Non-blocking I/O (`O_NONBLOCK`) works for sockets, pipes, and PTYs.
Event-driven servers become possible.

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

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 23 (Socket API) | Socket fds to multiplex |
| Phase 22 (TTY) | PTY/TTY fds to multiplex |
| Phase 35 (SMP) | Wait queues infrastructure |

## Implementation Outline

1. Implement `O_NONBLOCK` for pipes (simplest case).
2. Implement `O_NONBLOCK` for sockets.
3. Implement `fcntl(F_GETFL/F_SETFL)` for non-blocking flag.
4. Rewrite `poll()` to use wait queues instead of busy-wait loop.
5. Implement `select()` on top of the improved poll infrastructure.
6. Implement `epoll_create1`, `epoll_ctl`, `epoll_wait`.
7. Implement `accept4()`.
8. Write test programs: echo server using poll, epoll-based multi-client server.
9. Verify the telnet server works with improved poll (no busy-wait).
10. Stress test with many concurrent connections.

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

- Phase 36 Task List — *not yet created*

## How Real OS Implementations Differ

Linux has a rich set of I/O mechanisms:
- **io_uring** — the newest, highest-performance async I/O interface (submission/completion rings).
- **epoll** — the standard for network servers (nginx, Node.js, etc.).
- **signalfd, timerfd, eventfd** — unify signals, timers, and events as fds for epoll.
- **splice/tee/vmsplice** — zero-copy data movement between fds.
- **FUSE** — filesystem in userspace (via poll/epoll on /dev/fuse).

BSD uses **kqueue** instead of epoll — similar concept, different API.

Our implementation provides the essential poll/epoll layer without io_uring or
the specialized fd types. This is sufficient for building real network servers.

## Deferred Until Later

- io_uring (submission/completion ring)
- Edge-triggered epoll (EPOLLET) — complex semantics
- signalfd, timerfd, eventfd
- splice/tee/vmsplice (zero-copy)
- POSIX AIO (aio_read, aio_write)
- kqueue compatibility
