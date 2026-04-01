# Phase 30 — Telnet Server

**Status:** Complete
**Source Ref:** phase-30
**Depends on:** Phase 23 (Socket API) ✅, Phase 27 (User Accounts) ✅, Phase 29 (PTY Subsystem) ✅
**Builds on:** Combines TCP sockets from Phase 23, PTY pairs from Phase 29, and login authentication from Phase 27 into the first networked multi-user service
**Primary Components:** userspace/telnetd/

## Milestone Goal

A telnet server runs inside the OS, allowing remote users to log in over the network
and get an interactive shell session. This is the first demonstration of the OS as a
networked, multi-user system — a machine you can connect to from another computer (or
another terminal on the host) and do real work.

## Why This Phase Exists

Phases 23, 27, and 29 each built independent capabilities: TCP sockets, user
authentication, and virtual terminals. Individually, none of them delivers the experience
of a real multi-user networked OS. A telnet server is the simplest protocol that ties
all three together — it accepts a TCP connection, allocates a PTY, authenticates the
user, and provides a full interactive shell. Building this proves the OS subsystems
integrate correctly and gives a tangible remote-access demonstration.

## Learning Goals

- Understand the telnet protocol: IAC (Interpret As Command) sequences, option
  negotiation, and NVT (Network Virtual Terminal) mapping.
- Learn how a network server forks per-connection processes, each with its own PTY.
- See how login, PTY, and networking come together to provide remote shell access.
- Understand why telnet was replaced by SSH (no encryption) and why it is still useful
  for learning and trusted networks.

## Feature Scope

### Telnet Server (`/sbin/telnetd`)

- Listen on TCP port 23 (configurable).
- On each incoming connection:
  1. Allocate a PTY pair.
  2. Fork a child process.
  3. In the child: `setsid()`, open PTY slave as controlling terminal, `exec` `/bin/login`.
  4. In the parent: relay data between the TCP socket and the PTY master.
- Handle telnet IAC sequences:
  - `WILL`/`WONT`/`DO`/`DONT` option negotiation
  - `ECHO` suppression (let the server-side terminal handle echo)
  - `SUPPRESS-GO-AHEAD` (character-at-a-time mode)
  - `NAWS` (Negotiate About Window Size) — forward to PTY via `TIOCSWINSZ`
- Strip or process IAC sequences from the data stream before passing to the PTY.
- Clean up PTY and child process when the connection closes.

### Integration with Existing Infrastructure

- Uses TCP sockets from Phase 23.
- Uses PTY pairs from Phase 29.
- Uses login from Phase 27.
- Launched by init at boot time.

### Testing from the Host

QEMU's network setup (user-mode or tap) should allow `telnet localhost <port>` from
the host machine to connect to the OS's telnet server. Document the QEMU port
forwarding configuration.

## Important Components and How They Work

### IAC Parser

The telnet protocol uses IAC (0xFF) as an escape byte. The server must parse incoming
data to distinguish telnet commands from user input. IAC sequences include option
negotiation (`WILL`/`WONT`/`DO`/`DONT`) and sub-negotiation for options like NAWS.

### Per-Connection Architecture

Each accepted connection forks a child process. The parent relays data between the TCP
socket and the PTY master fd using `poll()`. The child calls `setsid()`, opens the PTY
slave as its controlling terminal, and execs `/bin/login` to authenticate the user.

### Connection Lifecycle

When the TCP connection closes or the remote client disconnects, the server sends
`SIGHUP` to the session (via PTY master close), cleans up the PTY pair, and reaps the
child process.

## How This Builds on Earlier Phases

- **Extends Phase 23 (Socket API):** Uses TCP server sockets (`bind`, `listen`, `accept`) for network connections.
- **Extends Phase 27 (User Accounts):** Delegates authentication to the `login` binary from Phase 27.
- **Extends Phase 29 (PTY Subsystem):** Allocates PTY pairs for each remote session; relies on `setsid()` and `TIOCSCTTY`.
- **Reuses Phase 26 (Text Editor):** The text editor works over telnet connections, proving full terminal compatibility.

## Implementation Outline

1. Write a minimal telnet server in C (cross-compiled with musl).
2. Implement IAC parsing: strip telnet commands from the data stream.
3. Implement option negotiation for ECHO, SGA, and NAWS.
4. On accept: fork, allocate PTY, exec login on the slave side.
5. Main loop: `poll()` on both the socket fd and the PTY master fd, relay data.
6. Handle connection close: send `SIGHUP` to the session, clean up.
7. Add telnetd to init's startup sequence.
8. Configure QEMU with port forwarding: `-nic user,hostfwd=tcp::2323-:23`.
9. Test from host: `telnet localhost 2323`.

## Acceptance Criteria

- `telnetd` starts at boot and listens on port 23.
- Connecting via `telnet` from the host reaches a `login:` prompt.
- After login, the remote shell is fully functional (commands, pipes, editing).
- The text editor from Phase 26 works over the telnet connection.
- Multiple simultaneous telnet sessions work (at least 4).
- Closing the telnet client cleanly terminates the remote session.
- `who` or `w` (stretch) shows active telnet sessions.

## Companion Task List

- [Phase 30 Task List](./tasks/30-telnet-server-tasks.md)

## How Real OS Implementations Differ

Production telnet servers (like those in inetd/xinetd) support:
- inetd-style super-server launching (one daemon for all network services)
- TCP wrappers for access control
- Kerberos authentication
- Environment variable passing
- Full telnet option negotiation (dozens of options)

Modern systems have largely replaced telnet with SSH. We implement telnet first because:
1. The protocol is dramatically simpler (no crypto).
2. It demonstrates all the same OS concepts (sockets + PTY + login + fork).
3. It provides a working remote access prototype that SSH will later replace.

### Security Note

Telnet transmits everything in plaintext, including passwords. This is acceptable for:
- QEMU virtual networking (never leaves the host)
- Trusted local networks during development
- Learning the protocol concepts

Phase 43 (SSH) will provide encrypted remote access for untrusted networks.

## Deferred Until Later

- Encryption (that's SSH, Phase 43)
- inetd/xinetd super-server
- TCP wrappers or IP-based access control
- Telnet LINEMODE option
- Environment variable passing (ENVIRON option)
- Keep-alive / idle timeout
