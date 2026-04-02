# Phase 23: Socket API

**Aligned Roadmap Phase:** Phase 23
**Status:** Complete
**Source Ref:** phase-23

This document describes how m3OS exposes its kernel TCP/IP stack to
userspace via standard Linux socket syscalls. After this phase, userspace
programs can open TCP and UDP connections, send ICMP pings, and use
`poll` to multiplex across sockets and files -- all from ring-3 code
with no kernel modifications required per network program.

## Why Sockets Matter

Before Phase 23, all network I/O lived inside the kernel. The `ping`
command was a shell builtin that called kernel functions directly. Every
new network program would have required adding more kernel code -- an
unsustainable model that violates the microkernel philosophy.

Socket syscalls solve this by exposing the network stack through the same
fd abstraction used for files and pipes. From userspace, a socket fd is
indistinguishable from a file fd: `read`, `write`, `close`, and `poll`
all work on it via the existing fd dispatch path.

## Architecture

```
userspace                        kernel                        hardware
---------                        ------                        --------
ping ELF                         syscall gate
  socket(AF_INET,SOCK_DGRAM,     sys_socket()
         IPPROTO_ICMP)              alloc_socket() -> handle
                                    fd_table[n] = Socket{handle}
                                    return n
  sendto(fd, packet, addr)        sys_sendto()
                                    ICMP echo request
                                    -> IPv4 -> virtio-net TX    -> QEMU
  poll([fd], POLLIN, timeout)     sys_poll()
                                    check PING_REPLY_RECEIVED
                                    block until reply or timeout
  recv(fd, buf)                   sys_recvfrom()               <- QEMU
                                    net dispatch -> ICMP reply
                                    copy to user buffer
```

## Socket Table

The kernel maintains a global `SOCKET_TABLE` with 32 slots, each
holding an `Option<SocketEntry>`. A `SocketHandle` is a `u32` index
into this table.

```rust
// kernel/src/net/mod.rs
pub struct SocketEntry {
    pub kind: SocketKind,       // Stream (TCP), Dgram (UDP), or DgramIcmp
    pub protocol: SocketProtocol,
    pub local_addr: u32,
    pub local_port: u16,
    pub remote_addr: u32,
    pub remote_port: u16,
    pub state: SocketState,     // Unbound, Bound, Connected, Listening, Closed
    pub options: SocketOptions,
    pub ref_count: u32,
}
```

`alloc_socket()` scans for the first `None` slot, initializes it, and
returns the handle. `free_socket()` decrements the reference count and
clears the slot when it reaches zero. Reference counting is needed
because `fork` and `dup2` can create multiple fds pointing at the same
socket.

## FD Table Integration

Phase 12 established `FdBackend` with variants for files, pipes, and
device TTYs. Phase 23 adds `FdBackend::Socket { handle: SocketHandle }`.

The syscall dispatcher routes `read`/`write`/`close` calls on socket
fds through the same match arm as other fd types:

- `read(fd)` on a socket fd calls `sys_recvfrom` internally
- `write(fd)` on a socket fd calls `sys_sendto` internally
- `close(fd)` on a socket fd calls `free_socket(handle)` which sends
  TCP FIN/RST as appropriate

## Socket Syscalls

All socket syscalls use Linux ABI numbers (41-55) with `AF_INET` only:

| Syscall | Linux # | What it does |
|---|---|---|
| `socket` | 41 | Create TCP, UDP, or ICMP socket; allocate fd |
| `connect` | 42 | TCP 3-way handshake (blocking); UDP/ICMP stores remote addr |
| `accept` | 43 | Block until incoming TCP connection; create new socket+fd |
| `sendto` | 44 | TCP send, UDP sendto, ICMP echo request |
| `recvfrom` | 47 | TCP recv (blocking), UDP recv (blocking), ICMP reply wait |
| `shutdown` | 48 | SHUT_RD/SHUT_WR/SHUT_RDWR with TCP FIN |
| `bind` | 49 | UDP port binding, TCP/ICMP address storage |
| `listen` | 50 | TCP passive open |
| `getsockname` | 51 | Read local address |
| `getpeername` | 52 | Read remote address |
| `setsockopt` | 54 | SO_REUSEADDR, SO_KEEPALIVE, SO_RCVBUF, SO_SNDBUF, TCP_NODELAY |
| `getsockopt` | 55 | Read back option values |

### `sockaddr_in` ABI

The `SockaddrIn` struct matches the Linux layout exactly (16 bytes,
network byte order):

```rust
// userspace/syscall-lib/src/lib.rs
#[repr(C)]
pub struct SockaddrIn {
    pub sin_family: u16,    // AF_INET = 2
    pub sin_port: u16,      // network byte order
    pub sin_addr: u32,      // network byte order
    pub sin_zero: [u8; 8],  // padding
}
```

The kernel validates and copies this structure from userspace memory
on every socket syscall that takes an address argument.

## Poll Extension for Sockets

Phase 23 extends `sys_poll` to handle socket fds alongside files and
pipes. For each socket fd in the pollfd array:

- **TCP**: checks the receive buffer and connection state for `POLLIN`,
  always reports `POLLOUT` when connected, reports `POLLHUP` on close
- **UDP**: uses `has_data()` peek on the UDP binding for `POLLIN`
- **ICMP**: checks the `PING_REPLY_RECEIVED` atomic flag for `POLLIN`

This allows network programs to multiplex across multiple sockets and
file descriptors in a single blocking call.

## ICMP DGRAM Sockets

Rather than requiring raw socket privileges for ping,
`socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP)` creates a socket that:

- On `sendto`: constructs an ICMP echo request with the kernel's ICMP
  layer and sends it through the IPv4 stack
- On `recvfrom`: blocks until a matching ICMP echo reply arrives

This is the same approach Linux uses for unprivileged ICMP
(`net.ipv4.ping_group_range`).

## Userspace Ping

`userspace/ping/` is a standalone `#![no_std]` Rust ELF binary that
demonstrates the socket API end-to-end:

1. Opens `socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP)`
2. Sends echo requests in a loop via `sendto`
3. Uses `poll` to wait for replies with a timeout
4. Reads reply ticks via `read` and prints round-trip time
5. Prints statistics on exit

The kernel's `icmp::ping()` builtin function was removed after this
binary proved the socket layer works.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/net/mod.rs` | SocketHandle, SocketEntry, SOCKET_TABLE, alloc/free/with helpers |
| `kernel/src/arch/x86_64/syscall.rs` | All sys_socket/bind/connect/listen/accept/send/recv/shutdown/poll dispatch |
| `kernel/src/process/mod.rs` | FdBackend::Socket variant, ref counting on fork/close |
| `userspace/syscall-lib/src/lib.rs` | SockaddrIn, socket constants, syscall wrappers |
| `userspace/ping/src/main.rs` | Userspace ping ELF binary |
| `kernel-core/src/net/mod.rs` | SockaddrIn layout tests (host-testable) |

## How This Phase Differs From Later Network Work

- This phase implements blocking socket I/O only. Phase 37 (I/O
  Multiplexing) adds `epoll`, non-blocking sockets, and `O_NONBLOCK`.
- Only `AF_INET` is supported. Phase 39 (Unix Domain Sockets) adds
  `AF_UNIX`.
- No TLS or encryption. Phase 42 (Crypto and TLS) adds `rustls`.
- No DNS resolution. Phase 51 (Networking and GitHub) adds DNS.

## Related Roadmap Docs

- [Phase 23 roadmap doc](./roadmap/23-socket-api.md)
- [Phase 23 task doc](./roadmap/tasks/23-socket-api-tasks.md)
