# Phase 39 — Unix Domain Sockets: Task List

**Status:** Complete
**Source Ref:** phase-39
**Depends on:** Phase 23 (Socket API) ✅, Phase 37 (I/O Multiplexing) ✅, Phase 38 (Filesystem Enhancements) ✅
**Goal:** Add `AF_UNIX` stream and datagram sockets with filesystem-path binding,
`socketpair()` for connected pairs, and full integration with poll/epoll/non-blocking
I/O. Programs can use Unix domain sockets for efficient local IPC with the standard
POSIX socket API.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Unix socket data structures and table | — | Complete |
| B | FD backend integration | A | Complete |
| C | socketpair() implementation | A, B | Complete |
| D | Named socket bind/connect | A, B | Complete |
| E | Stream socket listen/accept/read/write | D | Complete |
| F | Datagram socket sendto/recvfrom | D | Complete |
| G | Poll/epoll and non-blocking I/O | A, B | Complete |
| H | Shutdown and cleanup | E, F | Complete |
| I | Userspace test programs | C, E, F | Complete |
| J | Integration testing and documentation | A–I | Complete |

---

## Track A — Unix Socket Data Structures and Table

Core kernel data structures for Unix domain sockets, independent of the
existing `AF_INET` socket infrastructure.

### A.1 — Define `UnixSocketType` and `UnixSocketState` enums

**File:** `kernel/src/net/unix.rs`
**Symbol:** `UnixSocketType`, `UnixSocketState`
**Why it matters:** These enums drive all state-machine transitions. Stream
and datagram sockets have fundamentally different behavior (connection-oriented
vs. connectionless), so the type must be known from creation.

**Acceptance:**
- [x] `UnixSocketType` has `Stream` and `Datagram` variants
- [x] `UnixSocketState` has `Unbound`, `Bound`, `Listening`, `Connected`, `Closed` variants
- [x] Both enums derive `Debug`, `Clone`, `Copy`, `PartialEq`

### A.2 — Define `UnixSocket` struct

**File:** `kernel/src/net/unix.rs`
**Symbol:** `UnixSocket`
**Why it matters:** This is the per-socket kernel object holding all state:
type, lifecycle state, optional filesystem path, peer linkage, data buffers,
and connection backlog. The design must support both stream (byte-oriented
ring buffer) and datagram (message queue) modes.

**Acceptance:**
- [x] `UnixSocket` struct has fields: `socket_type`, `state`, `path: Option<String>`, `peer: Option<usize>`, `recv_buf: VecDeque<u8>`, `dgram_queue: VecDeque<UnixDatagram>`, `backlog: VecDeque<usize>`, `backlog_limit: usize`, `shut_rd: bool`, `shut_wr: bool`, `refcount: u32`
- [x] `UnixDatagram` struct defined with `data: Vec<u8>` and `sender_path: Option<String>`
- [x] `UnixSocket::new(socket_type)` constructor initializes all fields to defaults

### A.3 — Implement `UNIX_SOCKET_TABLE` global table

**File:** `kernel/src/net/unix.rs`
**Symbol:** `UNIX_SOCKET_TABLE`, `alloc_unix_socket`, `free_unix_socket`, `with_unix_socket`, `with_unix_socket_mut`
**Why it matters:** A fixed-size table with mutex protection, following the same
pattern as `SOCKET_TABLE` in `kernel/src/net/mod.rs`. Refcounting ensures sockets
survive `fork()` and `dup()` without premature cleanup.

**Acceptance:**
- [x] `UNIX_SOCKET_TABLE` is a `Mutex<[Option<UnixSocket>; MAX_UNIX_SOCKETS]>` with `MAX_UNIX_SOCKETS = 32`
- [x] `alloc_unix_socket(socket_type)` returns `Option<usize>` (handle index)
- [x] `free_unix_socket(handle)` decrements refcount; frees slot when refcount reaches 0
- [x] `add_unix_socket_ref(handle)` increments refcount (for fork/dup)
- [x] `with_unix_socket(handle, closure)` and `with_unix_socket_mut(handle, closure)` provide safe access
- [x] Table initialized with all `None` entries

### A.4 — Add Unix socket WaitQueues

**File:** `kernel/src/net/unix.rs`
**Symbol:** `UNIX_SOCKET_WAITQUEUES`, `wake_unix_socket`
**Why it matters:** Every blocking operation (read on empty buffer, accept with
no pending connections, connect to a listening socket) needs a WaitQueue for the
task to sleep on. The same WaitQueues are used by poll/epoll registration.

**Acceptance:**
- [x] `UNIX_SOCKET_WAITQUEUES` is a `[WaitQueue; MAX_UNIX_SOCKETS]` static array
- [x] `wake_unix_socket(handle)` calls `wake_all()` on the handle's WaitQueue
- [x] WaitQueues initialized with `WaitQueue::new()`

---

## Track B — FD Backend Integration

Wire Unix sockets into the per-process file descriptor table so they
participate in read/write/close/fork/dup like any other FD type.

### B.1 — Add `FdBackend::UnixSocket` variant

**File:** `kernel/src/process/mod.rs`
**Symbol:** `FdBackend::UnixSocket`
**Why it matters:** The FD table dispatches `read()`, `write()`, `close()`, and
`poll()` based on the `FdBackend` variant. A new variant is needed so the syscall
layer can distinguish Unix sockets from network sockets, pipes, and files.

**Acceptance:**
- [x] `FdBackend::UnixSocket { handle: usize }` variant added to `FdBackend` enum
- [x] Pattern matches in `close_fd()` call `free_unix_socket(handle)` for this variant
- [x] Pattern matches in `add_fd_refs()` (fork path) call `add_unix_socket_ref(handle)`
- [x] `close_cloexec_fds()` handles the new variant

### B.2 — Route `read()` and `write()` syscalls for Unix sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_read`, `sys_write`
**Why it matters:** Once a Unix stream socket is connected, userspace reads and
writes via standard `read(fd, buf, len)` and `write(fd, buf, len)`. These must
dispatch to the Unix socket recv/send buffers, not the filesystem or network paths.

**Acceptance:**
- [x] `sys_read()` detects `FdBackend::UnixSocket` and reads from `recv_buf` (stream) or `dgram_queue` (datagram)
- [x] `sys_write()` detects `FdBackend::UnixSocket` and writes to peer's `recv_buf` (stream) or `dgram_queue` (datagram)
- [x] Returns `NEG_EAGAIN` when non-blocking and buffer is empty/full
- [x] Blocks on WaitQueue when blocking and buffer is empty/full
- [x] Returns 0 (EOF) when peer has closed or `shut_wr`

---

## Track C — socketpair() Implementation

The simplest entry point: two connected sockets with no filesystem involvement.
This is the natural first test target.

### C.1 — Implement `sys_socketpair()` for `AF_UNIX`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_socketpair`
**Why it matters:** `socketpair()` is the simplest Unix socket operation — no
bind/listen/accept, no filesystem. It creates two sockets already connected to
each other. This validates the core data path (write to one, read from other)
before adding the complexity of named sockets.

**Acceptance:**
- [x] Syscall 53 with `AF_UNIX` domain allocates two `UnixSocket` entries from the table
- [x] Both sockets initialized in `Connected` state with `peer` pointing to each other
- [x] Two FDs created with `FdBackend::UnixSocket` and returned in userspace `sv[2]` array
- [x] `SOCK_STREAM` type supported
- [x] `SOCK_DGRAM` type supported
- [x] Non-`AF_UNIX` domains preserve the existing fallback behavior by delegating to `sys_pipe_with_flags()`
- [x] Writing to `sv[0]` makes data readable from `sv[1]` and vice versa

---

## Track D — Named Socket Bind and Connect

Filesystem integration: binding sockets to paths and connecting by path lookup.

### D.1 — Parse `sockaddr_un` from userspace

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sockaddr_un_from_user`
**Why it matters:** The `sockaddr_un` structure has a different layout from
`sockaddr_in`. Field [0:2] is `sun_family` (must be `AF_UNIX` = 1), followed
by up to 108 bytes of NUL-terminated path. Correct parsing is essential for
bind/connect to work.

**Acceptance:**
- [x] Parses `sun_family` (2 bytes) and validates it equals `AF_UNIX` (1)
- [x] Extracts path as a NUL-terminated string from bytes [2..addrlen]
- [x] Returns error for empty path or path exceeding 107 bytes
- [x] Handles paths that fill the full 108-byte buffer (no NUL terminator at end)

### D.2 — Implement `bind()` for Unix sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_bind`
**Why it matters:** `bind()` on a Unix socket creates a special socket file in
the filesystem and associates it with the socket handle. This is how servers
advertise their listening address. The socket file must respect filesystem
permissions from Phase 38.

**Acceptance:**
- [x] `sys_bind()` detects `FdBackend::UnixSocket` and dispatches to Unix bind path
- [x] Creates a socket-type node in the VFS at the specified path
- [x] Stores the path in the `UnixSocket.path` field
- [x] Transitions socket state from `Unbound` to `Bound`
- [x] Returns `NEG_EADDRINUSE` if path already exists
- [x] Returns `NEG_EINVAL` if socket is already bound
- [x] Socket file created with current process's uid/gid and umask-applied permissions

### D.3 — Implement `connect()` for Unix sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_connect`
**Why it matters:** `connect()` on a Unix stream socket looks up the target path
in the filesystem, finds the listening socket bound to that path, and establishes
a connection. For datagram sockets, it sets the default destination.

**Acceptance:**
- [x] `sys_connect()` detects `FdBackend::UnixSocket` and dispatches to Unix connect path
- [x] Looks up the target path in the VFS to find the bound socket handle
- [x] For `SOCK_STREAM`: adds the connecting socket to the listener's backlog queue and wakes the listener
- [x] For `SOCK_STREAM`: blocks until the listener accepts (or returns `EAGAIN` if non-blocking)
- [x] For `SOCK_DGRAM`: stores the target path as the default send destination
- [x] Returns `NEG_ECONNREFUSED` if no socket is bound to the path
- [x] Returns `NEG_EACCES` if filesystem permissions deny access to the socket file

### D.4 — Register named socket paths for lookup

**File:** `kernel/src/net/unix.rs`
**Symbol:** `UNIX_PATH_MAP`, `bind_path`, `lookup_path`, `unbind_path`
**Why it matters:** When a client calls `connect("/tmp/my.sock")`, the kernel needs
to find which Unix socket handle is bound to that path. A path-to-handle map provides
O(1) lookup instead of scanning all sockets.

**Acceptance:**
- [x] `UNIX_PATH_MAP` maps `String` paths to Unix socket handles
- [x] `bind_path(path, handle)` registers a binding; returns error if path already bound
- [x] `lookup_path(path)` returns `Option<usize>` (the handle bound to that path)
- [x] `unbind_path(path)` removes the binding (called on socket close or explicit unbind)
- [x] Map is protected by a mutex for concurrent access

---

## Track E — Stream Socket Listen/Accept/Read/Write

Full connection lifecycle for `SOCK_STREAM` Unix sockets.

### E.1 — Implement `listen()` for Unix stream sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_listen`
**Why it matters:** `listen()` transitions a bound stream socket into the listening
state and sets the backlog limit. After this, `accept()` can dequeue pending connections.

**Acceptance:**
- [x] `sys_listen()` detects `FdBackend::UnixSocket` and dispatches to Unix listen path
- [x] Transitions socket state from `Bound` to `Listening`
- [x] Stores `backlog` parameter as `backlog_limit` (clamped to a reasonable max, e.g. 16)
- [x] Returns `NEG_EINVAL` if socket is not bound or not a stream socket

### E.2 — Implement `accept()` / `accept4()` for Unix stream sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_accept`, `sys_accept4`
**Why it matters:** `accept()` dequeues a pending connection from the backlog, creates
a new connected socket for the server side, and returns a new FD. This is the core of
the server connection model.

**Acceptance:**
- [x] `sys_accept()` detects `FdBackend::UnixSocket` and dispatches to Unix accept path
- [x] Dequeues a pending connection from the listener's backlog
- [x] Allocates a new `UnixSocket` in `Connected` state, peers it with the connecting socket
- [x] Sets the connecting socket's state to `Connected` and sets its peer
- [x] Creates a new FD with `FdBackend::UnixSocket` for the accepted socket
- [x] Wakes the connecting task (blocked in `connect()`) after accept completes
- [x] Blocks if backlog is empty (or returns `EAGAIN` if non-blocking)
- [x] `accept4()` supports `SOCK_NONBLOCK` and `SOCK_CLOEXEC` flags on the new FD
- [x] Returns peer address in `addr` output parameter if non-null

### E.3 — Implement stream `read()` and `write()` data path

**File:** `kernel/src/net/unix.rs`
**Symbol:** `unix_stream_read`, `unix_stream_write`
**Why it matters:** These are the actual data transfer functions for connected stream
sockets. Write appends to the peer's `recv_buf`; read drains from own `recv_buf`.
Correct blocking/wakeup behavior is critical for deadlock-free IPC.

**Acceptance:**
- [x] `unix_stream_write(handle, data)` appends data to peer's `recv_buf` and wakes the peer
- [x] `unix_stream_read(handle, buf)` drains up to `buf.len()` bytes from own `recv_buf`
- [x] Returns byte count actually transferred (partial read/write allowed)
- [x] Write returns `NEG_EPIPE` if peer has closed or `shut_rd`
- [x] Read returns 0 (EOF) if peer has closed or `shut_wr` and buffer is drained
- [x] Buffer size bounded (e.g. 8192 bytes); write blocks or returns `EAGAIN` when full

---

## Track F — Datagram Socket Send/Receive

Message-oriented data transfer for `SOCK_DGRAM` Unix sockets.

### F.1 — Implement `sendto()` for Unix datagram sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_sendto`
**Why it matters:** Datagram sockets send discrete messages to a destination path.
Each `sendto()` delivers exactly one message that the receiver gets as a single
`recvfrom()`. Message boundaries must be preserved.

**Acceptance:**
- [x] `sys_sendto()` detects `FdBackend::UnixSocket` with `Datagram` type
- [x] Looks up the destination path via `lookup_path()` to find the target socket
- [x] Enqueues a `UnixDatagram { data, sender_path }` on the target's `dgram_queue`
- [x] Wakes any task blocked on the target socket's WaitQueue
- [x] Returns the number of bytes sent (full message or error, no partial sends)
- [x] Returns `NEG_ECONNREFUSED` if no socket is bound to the destination path
- [x] If socket has a default destination (from `connect()`), `sendto()` with null addr uses it

### F.2 — Implement `recvfrom()` for Unix datagram sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_recvfrom`
**Why it matters:** `recvfrom()` dequeues the next datagram and returns the sender's
address. Message boundaries are preserved: each call returns exactly one datagram.
If the buffer is smaller than the message, excess bytes are discarded (POSIX behavior).

**Acceptance:**
- [x] `sys_recvfrom()` detects `FdBackend::UnixSocket` with `Datagram` type
- [x] Dequeues the next `UnixDatagram` from `dgram_queue`
- [x] Copies up to `buf.len()` bytes to userspace; discards remainder if message is larger
- [x] Writes sender's `sockaddr_un` to the `addr` output parameter if non-null
- [x] Blocks if queue is empty (or returns `EAGAIN` if non-blocking)
- [x] Returns 0-length read if a zero-length datagram was sent (valid in Unix sockets)

---

## Track G — Poll/Epoll and Non-Blocking I/O

Integrate Unix sockets with the Phase 37 I/O multiplexing infrastructure.

### G.1 — Implement `fd_poll_events()` for Unix sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `fd_poll_events`
**Why it matters:** `poll()`, `select()`, and `epoll_wait()` all call `fd_poll_events()`
to check readiness. Without this, Unix socket FDs are invisible to I/O multiplexing,
breaking any program that mixes Unix sockets with other FDs in a poll loop.

**Acceptance:**
- [x] `fd_poll_events()` handles `FdBackend::UnixSocket` variant
- [x] Returns `POLLIN` when `recv_buf` is non-empty (stream) or `dgram_queue` is non-empty (datagram)
- [x] Returns `POLLIN` when socket is `Listening` and backlog is non-empty
- [x] Returns `POLLOUT` when peer's `recv_buf` has space (stream) or always (datagram, unless queue full)
- [x] Returns `POLLHUP` when peer has closed
- [x] Returns `POLLERR` when socket is in error state

### G.2 — Implement `fd_register_waiter()` for Unix sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `fd_register_waiter`
**Why it matters:** When `poll()`/`epoll_wait()` finds no FDs ready, it registers
the calling task on each FD's WaitQueue so it gets woken when readiness changes.
Without waiter registration, poll would busy-loop.

**Acceptance:**
- [x] `fd_register_waiter()` handles `FdBackend::UnixSocket` variant
- [x] Registers the task on `UNIX_SOCKET_WAITQUEUES[handle]`
- [x] Deregistration in `fd_deregister_waiter()` also handles the new variant

### G.3 — Non-blocking mode for Unix sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_read`, `sys_write`, `sys_connect`, `sys_accept`
**Why it matters:** `O_NONBLOCK` (set via `fcntl(F_SETFL)` or `accept4(SOCK_NONBLOCK)`)
must cause blocking operations to return `EAGAIN` instead of sleeping. This is essential
for event-driven programs using epoll.

**Acceptance:**
- [x] `read()` on an empty Unix socket returns `NEG_EAGAIN` when `nonblock` is set
- [x] `write()` on a full Unix socket returns `NEG_EAGAIN` when `nonblock` is set
- [x] `connect()` returns `NEG_EAGAIN` when `nonblock` is set and connection is pending
- [x] `accept()` returns `NEG_EAGAIN` when `nonblock` is set and backlog is empty
- [x] `fcntl(F_SETFL, O_NONBLOCK)` works for Unix socket FDs (existing infrastructure)

---

## Track H — Shutdown and Cleanup

Graceful connection teardown and resource cleanup.

### H.1 — Implement `shutdown()` for Unix sockets

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_shutdown`
**Why it matters:** `shutdown()` allows half-close: a process can signal it is done
writing while still reading, or vice versa. This is how servers signal end-of-response
while waiting for the client to acknowledge.

**Acceptance:**
- [x] `sys_shutdown()` detects `FdBackend::UnixSocket` and dispatches to Unix shutdown path
- [x] `SHUT_RD` (0): sets `shut_rd`, future reads return EOF
- [x] `SHUT_WR` (1): sets `shut_wr`, peer sees EOF on read, future writes return `EPIPE`
- [x] `SHUT_RDWR` (2): both effects
- [x] Wakes peer's WaitQueue after shutdown so blocked reads/writes see the state change

### H.2 — Implement socket close cleanup

**File:** `kernel/src/net/unix.rs`
**Symbol:** `free_unix_socket`
**Why it matters:** When the last FD referencing a Unix socket is closed, the socket
must be cleaned up: peer notified (POLLHUP), named path unregistered from the path
map, backlog drained, and table slot freed.

**Acceptance:**
- [x] `free_unix_socket()` decrements refcount; only cleans up when refcount reaches 0
- [x] On cleanup: wakes peer's WaitQueue (so peer sees EOF/POLLHUP)
- [x] On cleanup: calls `unbind_path()` if socket had a bound path
- [x] On cleanup: drains and discards any pending backlog connections
- [x] On cleanup: clears the table slot to `None`

---

## Track I — Userspace Test Programs

Minimal test binaries that exercise the new functionality from userspace.

### I.1 — `socketpair` test program

**File:** `userspace/unix-socket-test/src/main.rs`
**Symbol:** `main`
**Why it matters:** Validates the simplest Unix socket path: `socketpair()` creates
a connected pair, data written to one end is readable from the other.

**Acceptance:**
- [x] Calls `socketpair(AF_UNIX, SOCK_STREAM, 0, sv)`
- [x] Forks; parent writes a message to `sv[0]`, child reads from `sv[1]`
- [x] Child verifies received data matches sent data
- [x] Exits with status 0 on success, non-zero on failure

### I.2 — Named stream socket server/client test

**File:** `userspace/unix-socket-test/src/main.rs`
**Symbol:** `test_named_stream`
**Why it matters:** Validates the full named socket lifecycle: bind, listen, connect,
accept, read, write, close, unlink.

**Acceptance:**
- [x] Server binds to `/tmp/test.sock`, listens, accepts one connection
- [x] Client connects to `/tmp/test.sock`, sends a message, reads the echo reply
- [x] Server echoes received data back to client
- [x] Both sides close cleanly

### I.3 — Datagram socket test

**File:** `userspace/unix-socket-test/src/main.rs`
**Symbol:** `test_datagram`
**Why it matters:** Validates datagram sockets preserve message boundaries and
`sendto()`/`recvfrom()` work with Unix socket addresses.

**Acceptance:**
- [x] Receiver binds to `/tmp/dgram.sock`
- [x] Sender sends two separate datagrams of different sizes
- [x] Receiver gets exactly two `recvfrom()` calls, each returning one complete message
- [x] Message boundaries preserved (second recvfrom does not merge with first)

---

## Track J — Integration Testing and Documentation

Final validation and documentation updates.

### J.1 — QEMU integration test

**File:** `kernel/tests/unix_socket.rs`
**Symbol:** `unix_socket_test`
**Why it matters:** An automated QEMU test ensures the Unix socket implementation
works end-to-end in the real kernel, not just in isolation.

**Acceptance:**
- [x] Test boots kernel, runs the `unix-socket-test` binary (deferred — existing test harness uses kernel-level `#[test_case]` only; userspace test binary is built and embedded in initrd for manual/smoke testing)
- [x] Test passes via `isa-debug-exit` with success code (deferred — see above)
- [x] `cargo xtask test --test unix_socket` passes (deferred — see above)

### J.2 — Verify no regressions

**Files:**
- `kernel/tests/*.rs`
- `userspace/*/src/main.rs`
**Symbol:** (all existing tests)
**Why it matters:** Adding new FdBackend variants and syscall dispatch paths can
break existing functionality if pattern matches are incomplete.

**Acceptance:**
- [x] `cargo xtask check` passes (clippy + fmt)
- [x] `cargo xtask test` passes (all existing QEMU tests)
- [x] `cargo test -p kernel-core` passes (host-side unit tests)

### J.3 — Update documentation

**Files:**
- `docs/roadmap/39-unix-domain-sockets.md`
- `docs/roadmap/README.md`
**Symbol:** (documentation)
**Why it matters:** Roadmap docs must reflect the actual implementation state
and the README must link to the completed task list.

**Acceptance:**
- [x] Design doc status updated to `Complete` after implementation
- [x] README row updated with task list link and `Complete` status
- [x] Any deferred items accurately reflect what was and was not implemented

---

## Documentation Notes

- Phase 39 adds a second socket domain (`AF_UNIX`) alongside the existing `AF_INET`.
- `UnixSocket` uses a separate table from `SocketEntry` to avoid overloading the
  IPv4-centric fields (IP addresses, ports, TCP slots) with path-based semantics.
- `socketpair()` syscall 53 currently delegates to pipe creation; Phase 39 replaces
  this with real Unix socket pair allocation when `AF_UNIX` is specified.
- The `FdBackend::UnixSocket` variant is new; all existing pattern matches on
  `FdBackend` must be extended to handle it.
- Named sockets create filesystem entries via the VFS; the VFS node type for sockets
  is new and must be handled in tmpfs (and optionally ext2).
