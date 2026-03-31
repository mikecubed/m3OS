# Phase 30 -- Telnet Server

## Overview

Phase 30 adds `telnetd`, a telnet server that listens on TCP port 23, accepts
remote connections, allocates a PTY pair per session, and relays data between
the TCP socket and the terminal.  This is the first demonstration of m3OS as a
networked, multi-user system -- a machine you can log into from another
computer.

## Architecture

```
Host                          Guest (m3OS)
                              +--------------------------------------------+
telnet client                 |  telnetd (main, PID N)                     |
  |                           |    listen(port 23)                         |
  |  TCP via QEMU port fwd    |    accept() -> fork child                 |
  +-------------------------->|                                            |
                              |  child (relay, PID N+1)                    |
                              |    open /dev/ptmx -> master FD             |
                              |    fork grandchild                         |
                              |    poll(socket, pty_master)                |
                              |    socket -> IAC parse -> PTY master       |
                              |    PTY master -> CRLF escape -> socket     |
                              |                                            |
                              |  grandchild (session, PID N+2)             |
                              |    setsid() + TIOCSCTTY                    |
                              |    dup2 PTY slave -> stdin/stdout/stderr   |
                              |    exec /bin/login                         |
                              +--------------------------------------------+
```

### Connection lifecycle

1. `telnetd` starts at boot (spawned by init) and binds to TCP port 23.
2. On `accept()`, the main process forks a **child** (relay process).
3. The child allocates a PTY pair (`/dev/ptmx`), unlocks the slave, sends
   telnet option negotiation, and forks a **grandchild**.
4. The grandchild calls `setsid()`, opens the PTY slave as controlling
   terminal, redirects stdio, and `exec`s `/bin/login`.
5. The child enters a `poll()`-based relay loop between the TCP socket and
   the PTY master.
6. When either side closes (client disconnect or shell exit), the relay
   cleans up and exits.

## Telnet Protocol

### IAC sequences

The telnet protocol uses IAC (Interpret As Command, byte 0xFF) as an escape:

| Sequence | Meaning |
|---|---|
| IAC WILL opt | Server offers to perform option |
| IAC WONT opt | Server refuses option |
| IAC DO opt | Server requests client enable option |
| IAC DONT opt | Server requests client disable option |
| IAC SB opt ... IAC SE | Subnegotiation |
| IAC IAC | Literal 0xFF byte |

### Option negotiation

On connection, telnetd sends:
- `IAC WILL ECHO` -- server handles echo (disables client-side echo)
- `IAC WILL SGA` -- suppress go-ahead (character-at-a-time mode)
- `IAC DO SGA` -- request client to suppress go-ahead
- `IAC DO NAWS` -- request client to send window size

### NAWS (Negotiate About Window Size)

When the client supports NAWS, it sends `IAC SB NAWS <width-hi> <width-lo>
<height-hi> <height-lo> IAC SE`.  telnetd extracts the dimensions and calls
`ioctl(master_fd, TIOCSWINSZ, &winsize)` to update the PTY, which delivers
SIGWINCH to the shell.

### CR/LF translation

- **Socket to PTY:** CR NUL -> CR, CR LF -> LF (NVT to Unix)
- **PTY to socket:** bare LF -> CR LF (Unix to NVT)
- **IAC escaping:** literal 0xFF in PTY output -> IAC IAC to socket

## Kernel Changes

### Socket reference counting (P30-T004)

Added `refcount` field to `SocketEntry`.  `alloc_socket()` sets it to 1.
`add_fd_refs()` (called on fork) increments it.  `free_socket()` decrements
it and only frees the socket when refcount reaches zero.  This prevents a
forked child from destroying the parent's listening socket when it closes
the inherited FD.

### poll() for PTY master FDs (P30-T001)

Added explicit `FdBackend::PtyMaster` handling in `sys_poll()`:
- POLLIN when `s2m` ring buffer has data or slave refcount is zero
- POLLOUT when `m2s` ring buffer has space
- POLLHUP when slave side is fully closed

Previously, PTY master FDs fell through to the optimistic fallback that
always reported all events as ready, causing busy-waiting.

### poll() for TCP listening sockets (P30-T003)

When a socket is in `Listening` state, poll now checks whether the
underlying TCP slot has transitioned to `Established` (a connection is
ready to accept), rather than checking `has_recv_data()` which only
applies to connected sockets.

### TCP connection limit (P30-T002)

`MAX_TCP_CONNECTIONS` increased from 4 to 8, allowing 1 listening slot
plus at least 4 concurrent client connections (with headroom).

## QEMU Port Forwarding

The xtask build system adds `-netdev user,id=net0,hostfwd=tcp::2323-:23`
to QEMU arguments, forwarding host port 2323 to guest port 23.  Connect
from the host with:

```bash
telnet localhost 2323
```

## Differences from Production Telnet Servers

- No inetd/xinetd super-server -- telnetd is a standalone daemon
- No TCP wrappers or IP-based access control
- No Kerberos authentication -- uses simple password auth via `/bin/login`
- No LINEMODE option -- character-at-a-time with server echo
- No ENVIRON option -- environment variables are not passed from client
- No keep-alive or idle timeout -- connections persist until explicitly closed
- No encryption -- all traffic is plaintext (SSH planned for Phase 35)
- No send()/recv() syscalls -- uses sendto()/recvfrom() with NULL addr

## Files

| File | Purpose |
|---|---|
| `userspace/telnetd/telnetd.c` | Telnet server implementation |
| `kernel/src/net/mod.rs` | Socket reference counting |
| `kernel/src/net/tcp.rs` | TCP connection limit increase |
| `kernel/src/arch/x86_64/syscall.rs` | poll() for PTY master + listening sockets |
| `kernel/src/process/mod.rs` | Socket refcount in fork/close/cloexec |
| `kernel/src/fs/ramdisk.rs` | telnetd.elf in initrd |
| `userspace/init/src/main.rs` | telnetd daemon spawn at boot |
| `xtask/src/main.rs` | Build + QEMU port forwarding |
