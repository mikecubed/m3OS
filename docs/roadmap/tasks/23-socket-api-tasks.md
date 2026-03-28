# Phase 23 — Socket API: Task List

**Depends on:** Phase 16 (Network) ✅, Phase 22 (TTY) ✅
**Goal:** Expose the kernel's existing TCP/IP stack to userspace via standard
Linux socket syscalls. At the end of this phase a userspace program can open a
TCP connection, send an HTTP request, and receive the response from ring-3
code. The `ping` shell builtin moves out of the kernel into a standalone
userspace ELF.

## Track Layout

| Track | Scope | Status |
|---|---|---|
| A | Socket handle type, FD backend, kernel socket table | ✅ Done |
| B | Syscall library constants and wrappers | ✅ Done |
| C | Core socket syscalls (socket, bind, connect, listen, accept) | ✅ Done |
| D | Data transfer syscalls (send, sendto, recv, recvfrom, shutdown) | ✅ Done |
| E | Socket info and options (getsockname, getpeername, setsockopt, getsockopt) | ✅ Done |
| F | Poll extension for sockets | ✅ Done |
| G | ICMP DGRAM socket and userspace ping | ✅ Done |
| H | Cleanup and validation | ✅ Done |

## Implementation Summary

### Track A — Socket Handle and FD Backend
- `SocketHandle`, `SocketKind`, `SocketProtocol`, `SocketState`, `SocketEntry` types in `kernel/src/net/mod.rs`
- Global `SOCKET_TABLE` (32 slots) with `alloc_socket()`, `free_socket()`, `with_socket()`, `with_socket_mut()`
- `FdBackend::Socket { handle }` variant added to process FD table
- Close, read, write dispatch wired for socket FDs

### Track B — Syscall Library
- Socket syscall numbers (41-55) in `userspace/syscall-lib/src/lib.rs`
- `AF_INET`, `SOCK_STREAM`, `SOCK_DGRAM`, `IPPROTO_*` constants
- `SockaddrIn` struct matching Linux ABI (16 bytes, network byte order)
- Wrapper functions: `socket`, `bind`, `connect`, `listen`, `accept`, `send`, `sendto`, `recv`, `recvfrom`, `shutdown`, `getsockname`, `getpeername`, `setsockopt`, `getsockopt`

### Track C — Core Socket Syscalls
- `sys_socket(AF_INET, SOCK_STREAM/SOCK_DGRAM, protocol)` — allocates socket + fd
- `sys_bind` — UDP port binding, TCP/ICMP address storage
- `sys_connect` — TCP 3-way handshake with blocking yield-loop, UDP/ICMP stores remote addr
- `sys_listen` — TCP passive open
- `sys_accept` — blocks until incoming TCP connection, creates new socket + fd

### Track D — Data Transfer
- `sys_sendto` — TCP send, UDP sendto, ICMP echo request via DGRAM socket
- `sys_recvfrom_socket` — TCP recv with blocking, UDP recv with blocking, ICMP reply wait
- `sys_shutdown_sock` — SHUT_RD/SHUT_WR/SHUT_RDWR with TCP FIN
- `read()`/`write()` on socket FDs delegates to recvfrom/sendto

### Track E — Socket Info and Options
- `sys_getsockname`, `sys_getpeername` — read local/remote address
- `sys_setsockopt` — SO_REUSEADDR, SO_KEEPALIVE, SO_RCVBUF, SO_SNDBUF, TCP_NODELAY
- `sys_getsockopt` — read back option values

### Track F — Poll Extension
- Socket readiness in `sys_poll`: TCP checks recv buffer + connection state, UDP uses `has_data()` peek, ICMP checks `PING_REPLY_RECEIVED` atomic
- Reports POLLIN/POLLOUT/POLLHUP

### Track G — Userspace Ping
- `userspace/ping/` — standalone `#![no_std]` ELF binary
- Opens `socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP)`, sends echo requests via `sendto`, reads reply ticks via `read`
- Prints RTT and statistics
- Kernel `icmp::ping()` function removed

### Track H — Cleanup
- Removed `#[allow(dead_code)]` from net modules now reachable through socket syscalls
- Added kernel-core unit tests: `SockaddrIn` layout (size=16, field offsets), network byte order
- QEMU test suite passes, `cargo xtask check` clean

## Related

- [Phase 23 Design Doc](../23-socket-api.md)
