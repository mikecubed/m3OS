# Phase 39 - Unix Domain Sockets

**Status:** Complete
**Source Ref:** phase-39
**Depends on:** Phase 23 (Socket API) ✅, Phase 37 (I/O Multiplexing) ✅, Phase 38 (Filesystem Enhancements) ✅
**Builds on:** Extends Phase 23 socket infrastructure with `AF_UNIX` domain; reuses Phase 37 poll/epoll/WaitQueue integration; leverages Phase 38 filesystem namespace for named sockets
**Primary Components:** kernel/src/net/unix.rs, kernel/src/net/mod.rs, kernel/src/arch/x86_64/syscall.rs, kernel/src/process/mod.rs

## Milestone Goal

The OS supports `AF_UNIX` (Unix domain) sockets for high-performance local inter-process
communication. Programs can create stream and datagram sockets bound to filesystem paths.
`socketpair()` creates connected pairs for parent-child IPC after `fork()`. This is the
primary IPC mechanism on Linux, used by D-Bus, X11, systemd, PostgreSQL, Docker, and
many other systems.

## Why This Phase Exists

The kernel already has network sockets (`AF_INET`) for TCP/UDP, but real Unix systems
rely heavily on local-only IPC through Unix domain sockets. These are faster than network
sockets (no protocol overhead, no checksums, no routing) and offer filesystem-based
naming and permission enforcement. Many standard tools and daemons assume `AF_UNIX`
exists. Without it, the OS cannot run syslog, D-Bus-style services, or any software
that uses `socketpair()` for parent-child communication.

## Learning Goals

- Understand why Unix domain sockets exist: local IPC without network overhead.
- Learn the difference between named sockets (filesystem path) and abstract sockets
  (Linux-specific, no filesystem entry).
- See how `socketpair()` creates connected socket pairs for parent-child communication.
- Understand the connection lifecycle: bind, listen, accept, connect for stream sockets.
- Learn how datagram sockets preserve message boundaries vs. stream sockets.

## Feature Scope

### Socket Creation

- `socket(AF_UNIX, SOCK_STREAM, 0)` — stream socket (connection-oriented).
- `socket(AF_UNIX, SOCK_DGRAM, 0)` — datagram socket (connectionless).
- `socketpair(AF_UNIX, SOCK_STREAM, 0, sv)` — create a connected pair.

### Named Sockets (Filesystem-Bound)

- `bind(fd, "/tmp/my.sock")` — create a socket file in the filesystem.
- `connect(fd, "/tmp/my.sock")` — connect to a named socket.
- Socket files visible via `ls` and removable via `unlink()`.
- Permission checks on socket files (owner/group/other).

### Stream Sockets (SOCK_STREAM)

- `listen()` + `accept()` — server accepts connections.
- Bidirectional byte stream (same semantics as TCP).
- `shutdown(SHUT_RD/SHUT_WR/SHUT_RDWR)` for half-close.
- Backlog queue for pending connections.

### Datagram Sockets (SOCK_DGRAM)

- Connectionless: `sendto()` / `recvfrom()` with socket addresses.
- Message boundaries preserved (unlike stream sockets).
- Optional `connect()` to set default destination.

### Integration with I/O Multiplexing

Unix domain sockets must work with `poll()`, `select()`, and `epoll()` from Phase 37:
- `POLLIN` when data available or connection pending.
- `POLLOUT` when send buffer has space.
- `POLLHUP` when peer disconnected.

## Important Components and How They Work

### UnixSocket kernel data structure

New struct in `kernel/src/net/unix.rs` managing the state machine for each Unix domain
socket. Stores socket type (stream/datagram), state (unbound/bound/listening/connected),
optional filesystem path, peer reference, ring buffers for data, and a backlog queue for
pending connections. Each socket gets a WaitQueue for blocking I/O and poll integration.

```rust
struct UnixSocket {
    socket_type: UnixSocketType,       // Stream or Datagram
    state: UnixSocketState,            // Unbound, Bound, Listening, Connected
    path: Option<String>,              // Filesystem path (if named)
    peer: Option<UnixSocketId>,        // Connected peer
    recv_buf: VecDeque<u8>,            // Incoming data (stream)
    dgram_queue: VecDeque<Datagram>,   // Incoming datagrams (dgram)
    backlog: VecDeque<UnixSocketId>,   // Pending connections (if listening)
    backlog_limit: usize,              // Max pending connections
    shut_rd: bool,
    shut_wr: bool,
    refcount: u32,
}
```

### Socket table extension

The existing `SOCKET_TABLE` in `kernel/src/net/mod.rs` and `FdBackend::Socket` handle
network sockets. Unix sockets use a separate `UNIX_SOCKET_TABLE` with its own handle
space to avoid conflating IPv4 and Unix socket semantics. A new `FdBackend::UnixSocket`
variant links FDs to Unix socket handles.

### Syscall dispatch extension

The socket syscall dispatcher in `syscall.rs` currently rejects `AF_UNIX` (domain 1)
with `EAFNOSUPPORT`. Phase 39 adds `AF_UNIX` handling to `sys_socket()`, `sys_bind()`,
`sys_connect()`, `sys_listen()`, `sys_accept()`, `sys_sendto()`, `sys_recvfrom()`,
`sys_shutdown()`, and `sys_socketpair()`. The existing `socketpair` syscall (53)
currently delegates to pipe creation — it will be replaced with a real Unix socket
pair implementation.

### Named socket filesystem integration

`bind()` on a Unix socket creates a socket-type file in the VFS (tmpfs or ext2).
`connect()` looks up the path in the VFS to find the listening socket. `unlink()`
removes the filesystem entry but does not close existing connections. Permission
checks from Phase 38 apply to socket files.

## How This Builds on Earlier Phases

- Extends Phase 23 by adding `AF_UNIX` as a second socket domain alongside `AF_INET`.
- Reuses Phase 37 WaitQueue, poll/epoll integration, and `O_NONBLOCK` infrastructure.
- Leverages Phase 38 filesystem permission enforcement for named socket access control.
- Follows the same `FdBackend` + handle pattern from Phase 23 for FD-to-socket mapping.
- Uses ring buffer patterns from pipes (`kernel-core/src/pipe.rs`) for socket data buffers.

## Implementation Outline

1. Add `UnixSocket` struct and `UNIX_SOCKET_TABLE` in `kernel/src/net/unix.rs`.
2. Add `FdBackend::UnixSocket` variant to process FD infrastructure.
3. Extend `sys_socket()` to accept `AF_UNIX` and allocate Unix sockets.
4. Implement `sys_socketpair()` with real Unix socket pairs (replace pipe delegation).
5. Implement `bind()` for named sockets (create socket file in VFS).
6. Implement `connect()` to named sockets (find socket file, establish connection).
7. Implement `listen()` + `accept()` for stream sockets.
8. Implement `read()`/`write()` for connected stream sockets.
9. Implement `sendto()`/`recvfrom()` for datagram sockets.
10. Add poll/epoll support for Unix socket fds.
11. Add `shutdown()` support for Unix sockets.
12. Test: `socketpair()` for parent-child communication.
13. Test: named stream socket server/client.
14. Test: datagram socket message boundary preservation.

## Acceptance Criteria

- `socketpair(AF_UNIX, SOCK_STREAM, 0, sv)` creates a working connected pair.
- A server can `bind` + `listen` + `accept` on a named socket path.
- A client can `connect` to the named socket and exchange data bidirectionally.
- `poll()` and `epoll` work with Unix domain socket fds.
- Datagram sockets preserve message boundaries.
- `unlink()` removes a named socket from the filesystem.
- Socket file permissions are enforced (non-root cannot connect to root-owned socket).
- `shutdown()` half-close works (SHUT_RD, SHUT_WR, SHUT_RDWR).
- Non-blocking mode (`O_NONBLOCK`) works for Unix sockets.
- All existing tests pass without regression.

## Companion Task List

- [Phase 39 Task List](./tasks/39-unix-domain-sockets-tasks.md)

## How Real OS Implementations Differ

- **SCM_RIGHTS** — Linux passes file descriptors between processes over Unix sockets.
- **SCM_CREDENTIALS** — Linux passes sender's PID/UID/GID as ancillary data.
- **SOCK_SEQPACKET** — Connection-oriented with message boundaries (combines stream
  reliability with datagram framing).
- **Abstract namespace** — Linux-specific sockets without filesystem entries (`\0name`).
- **Autobind** — Kernel assigns abstract addresses automatically.
- **SO_PEERCRED** — Query connected peer's credentials via `getsockopt()`.
- **sendmsg/recvmsg** — Scatter-gather I/O with ancillary data (control messages).

## Deferred Until Later

- SCM_RIGHTS (file descriptor passing)
- SCM_CREDENTIALS (credential passing)
- SOCK_SEQPACKET
- Abstract namespace sockets
- SO_PEERCRED
- Autobind
- sendmsg/recvmsg with ancillary data
