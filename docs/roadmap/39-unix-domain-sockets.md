# Phase 39 - Unix Domain Sockets

## Milestone Goal

The OS supports `AF_UNIX` (Unix domain) sockets for high-performance local inter-process
communication. Programs can create stream and datagram sockets bound to filesystem paths
or abstract addresses. This is the primary IPC mechanism on Linux, used by D-Bus, X11,
systemd, PostgreSQL, Docker, and many other systems.

## Learning Goals

- Understand why Unix domain sockets exist: local IPC without network overhead.
- Learn the difference between named sockets (filesystem path) and abstract sockets
  (Linux-specific, no filesystem entry).
- See how `socketpair()` creates connected socket pairs for parent-child communication.
- Understand `SCM_RIGHTS` — passing file descriptors between processes over a socket.

## Feature Scope

### Socket Creation

- `socket(AF_UNIX, SOCK_STREAM, 0)` — stream socket (like TCP, connection-oriented).
- `socket(AF_UNIX, SOCK_DGRAM, 0)` — datagram socket (like UDP, connectionless).
- `socketpair(AF_UNIX, SOCK_STREAM, 0, sv)` — create a connected pair.

### Named Sockets (Filesystem-Bound)

- `bind(fd, "/tmp/my.sock")` — create a socket file in the filesystem.
- `connect(fd, "/tmp/my.sock")` — connect to a named socket.
- Socket files visible via `ls` and removable via `unlink()`.
- Permission checks on socket files (owner/group/other).

### Abstract Sockets (Stretch Goal)

- Bind to `\0name` (first byte NUL) — no filesystem entry.
- Lifecycle tied to the socket fd, not the filesystem.

### Stream Sockets (SOCK_STREAM)

- `listen()` + `accept()` — server accepts connections.
- Bidirectional byte stream (same semantics as TCP).
- `shutdown(SHUT_RD/SHUT_WR/SHUT_RDWR)` for half-close.
- Backlog queue for pending connections.

### Datagram Sockets (SOCK_DGRAM)

- Connectionless: `sendto()` / `recvfrom()` with socket addresses.
- Message boundaries preserved (unlike stream sockets).
- Optional `connect()` to set default destination.

### `socketpair()`

Syscall 53 — create a connected pair of Unix stream sockets:
```c
int sv[2];
socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
// sv[0] and sv[1] are connected; write to one, read from other
```

Used extensively for parent-child IPC after `fork()`.

### Ancillary Data: File Descriptor Passing (Stretch Goal)

Pass open file descriptors between processes via `sendmsg()`/`recvmsg()`:
- `SCM_RIGHTS` — attach fds to a message.
- Receiving process gets new fd numbers referencing the same underlying objects.
- Essential for privilege separation (pass a socket from a privileged process to
  an unprivileged one).

### Kernel Data Structures

```rust
struct UnixSocket {
    socket_type: UnixSocketType,  // Stream or Datagram
    state: UnixSocketState,       // Unbound, Bound, Listening, Connected
    path: Option<String>,         // Filesystem path (if named)
    peer: Option<UnixSocketId>,   // Connected peer
    recv_buffer: RingBuffer,      // Incoming data
    backlog: VecDeque<UnixSocketId>,  // Pending connections (if listening)
    wait_queue: WaitQueue,        // Tasks blocked on this socket
}
```

### Integration with I/O Multiplexing

Unix domain sockets must work with `poll()`, `select()`, and `epoll()` from Phase 37:
- `POLLIN` when data available or connection pending.
- `POLLOUT` when send buffer has space.
- `POLLHUP` when peer disconnected.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 23 (Socket API) | Socket syscall infrastructure |
| Phase 38 (Filesystem) | Socket files in the filesystem namespace |
| Phase 37 (I/O Multiplexing) | poll/epoll integration |

## Implementation Outline

1. Add `AF_UNIX` socket type to the socket syscall dispatcher.
2. Implement `UnixSocket` kernel data structure with ring buffers.
3. Implement `socketpair()` — simplest case (no filesystem, pre-connected).
4. Implement `bind()` for named sockets (create socket file in VFS).
5. Implement `connect()` to named sockets (find socket file, establish connection).
6. Implement `listen()` + `accept()` for stream sockets.
7. Implement `read()`/`write()` for connected stream sockets.
8. Implement `sendto()`/`recvfrom()` for datagram sockets.
9. Add poll/epoll support for Unix socket fds.
10. Test: syslog-style server accepting connections on `/dev/log`.
11. Test: `socketpair()` for parent-child communication.

## Acceptance Criteria

- `socketpair(AF_UNIX, SOCK_STREAM, 0, sv)` creates a working connected pair.
- A server can `bind` + `listen` + `accept` on a named socket path.
- A client can `connect` to the named socket and exchange data.
- `poll()` and `epoll` work with Unix domain socket fds.
- Datagram sockets preserve message boundaries.
- `unlink()` removes a named socket from the filesystem.
- Socket file permissions are enforced (non-root cannot connect to root-owned socket).
- All existing tests pass without regression.

## Companion Task List

- Phase 39 Task List — *not yet created*

## How Real OS Implementations Differ

Linux Unix domain sockets support:
- **SCM_RIGHTS** — pass file descriptors between processes.
- **SCM_CREDENTIALS** — pass sender's PID/UID/GID.
- **SOCK_SEQPACKET** — connection-oriented with message boundaries.
- **Abstract namespace** — sockets without filesystem entries.
- **Autobind** — kernel-assigned abstract addresses.
- **SO_PEERCRED** — query connected peer's credentials.
- **Ancillary data** — arbitrary control messages.

Our implementation covers the essential stream/datagram modes. SCM_RIGHTS is a
stretch goal that would enable privilege separation patterns.

## Deferred Until Later

- SCM_RIGHTS (file descriptor passing)
- SCM_CREDENTIALS (credential passing)
- SOCK_SEQPACKET
- Abstract namespace sockets
- SO_PEERCRED
- Autobind
