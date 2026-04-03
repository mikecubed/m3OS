# Phase 43 - SSH Server

**Status:** Complete
**Source Ref:** phase-43
**Depends on:** Phase 23 (Socket API) ✅, Phase 27 (User Accounts) ✅, Phase 29 (PTY) ✅, Phase 37 (I/O Multiplexing) ✅, Phase 42 (Crypto Primitives) ✅
**Builds on:** Uses the crypto-lib from Phase 42 (Ed25519, X25519, ChaCha20-Poly1305, SHA-256) for all cryptographic operations; reuses the PTY pair infrastructure from Phase 29 and the telnetd session architecture from Phase 30; authenticates against /etc/shadow from Phase 27; uses epoll from Phase 37 for multi-session I/O multiplexing
**Primary Components:** userspace sshd binary, sunset SSH library integration, host key management, authorized_keys support

## Milestone Goal

An SSH server runs inside the OS, providing encrypted remote shell access. Users can
connect from any standard SSH client (`ssh user@host`) with password or public key
authentication. This replaces the telnet server for untrusted networks and is a major
milestone: the OS is now a secure, networked, multi-user system.

## Why This Phase Exists

Phases 1–42 built a multi-user OS with networking, persistent storage, and
cryptographic primitives — but the only remote access is via telnet (Phase 30), which
transmits passwords and all session data in plaintext. Any observer on the network can
read everything. SSH is the universal standard for secure remote administration: it
encrypts the entire session, authenticates the server (preventing impersonation), and
authenticates the user (password or public key). Without SSH, the OS cannot be
considered secure for any network beyond localhost.

## Learning Goals

- Understand the SSH protocol: transport layer (encryption), user authentication layer,
  and connection layer (channels, sessions).
- Learn how key exchange (Diffie-Hellman / X25519) establishes a shared secret.
- See how public key authentication works without transmitting the private key.
- Understand why SSH is the universal standard for remote system administration.
- Learn how an IO-less protocol library integrates with an OS's socket and PTY layers.

## Feature Scope

### SSH Server (`sshd`)

**Transport Layer**
- SSH-2 protocol only (SSH-1 is obsolete and insecure).
- Key exchange: `curve25519-sha256` (using crypto from Phase 42).
- Host key: Ed25519 (generated on first boot, stored at `/etc/ssh/ssh_host_ed25519_key`).
- Encryption: `chacha20-poly1305@openssh.com` or `aes256-ctr`.
- MAC: `hmac-sha2-256` (if not using authenticated encryption).
- Compression: none (defer zlib).

**Authentication Layer**
- Password authentication (against `/etc/shadow` from Phase 27).
- Public key authentication (read `~/.ssh/authorized_keys`).
- Support `none` auth type for initial handshake.

**Connection Layer**
- Session channels with PTY allocation.
- Shell execution (via login or direct shell spawn).
- Window size changes (`window-change` channel request).
- Signal forwarding.
- Graceful channel close and session cleanup.

### Implementation Strategy

**Option A: sunset — Pure Rust SSH library (recommended)**

[sunset](https://github.com/mkj/sunset) is a pure Rust SSH library with an IO-less,
`no_std`, no-alloc core. Created by Matt Johnston (the same author as Dropbear).
It provides both client and server support, uses RustCrypto crates internally
(same as Phase 42), and has been tested on embedded devices (RPi Pico W, ~13 KB per
session). The IO-less design means we feed it bytes and it gives us bytes back —
perfect for integration with m3OS sockets and PTY pairs.

See [Rust Crate Acceleration](../rust-crate-acceleration.md) for details.

**Option B: Port Dropbear SSH**

[Dropbear](https://matt.ucc.asn.au/dropbear/dropbear.html) is a lightweight SSH
server designed for embedded systems. It is:
- ~110 KB binary (static, with its own crypto)
- Written in portable C
- Supports all essential SSH features
- Widely deployed on routers, IoT devices, embedded Linux

Cross-compile Dropbear with musl. This is the fallback if sunset proves too immature.

**Option C: Write minimal SSH server from scratch**

Using the Phase 42 crypto primitives, implement the SSH-2 protocol directly. This is
the maximum learning path but is significantly more work. The SSH protocol has many
subtle requirements around packet framing, key re-exchange, and channel multiplexing.

**Option D: Port tinyssh**

[tinyssh](https://tinyssh.org/) is an even smaller SSH server (~30 KB) that only
supports modern crypto (Ed25519, ChaCha20). However, it uses a non-standard build
system and may be harder to port.

### Configuration

- `/etc/ssh/sshd_config` — minimal config file (port, authentication methods, host key path)
- `/etc/ssh/ssh_host_ed25519_key` — host private key (generated on first boot)
- `~/.ssh/authorized_keys` — per-user public keys for key-based auth

### Testing from the Host

```bash
# QEMU with port forwarding
qemu ... -nic user,hostfwd=tcp::2222-:22

# Connect from host
ssh -p 2222 user@localhost
```

## Important Components and How They Work

### sunset SSH Library Integration

The sunset crate provides an IO-less SSH-2 protocol engine. The m3OS adapter feeds
TCP socket bytes into sunset and writes sunset's output bytes back to the socket.
Separately, sunset emits decrypted channel data that the adapter routes to/from a PTY
pair. This separation means the protocol logic is independent of m3OS I/O — sunset
handles key exchange, encryption, authentication callbacks, and channel multiplexing,
while the adapter handles socket reads/writes and PTY relay.

### Host Key Management

On first boot (or when `/etc/ssh/ssh_host_ed25519_key` is missing), sshd generates an
Ed25519 keypair using the crypto-lib from Phase 42 and writes it to `/etc/ssh/`. On
subsequent boots, the existing key is loaded. The host key is used during key exchange
to prove the server's identity — clients cache the fingerprint and warn if it changes
(protecting against man-in-the-middle attacks).

### Session Lifecycle

Each accepted TCP connection spawns a child process that:
1. Performs SSH handshake (version exchange, key exchange, algorithm negotiation).
2. Authenticates the user (password or public key).
3. Allocates a PTY pair (reusing Phase 29 infrastructure).
4. Forks and execs `login` (or the user's shell directly) on the slave side.
5. Relays encrypted data between the TCP socket and the PTY master using epoll.
6. Handles window-change and signal requests from the SSH channel.
7. Cleans up on disconnect (close PTY, reap child, close socket).

This mirrors the telnetd architecture from Phase 30, but with encryption and proper
authentication wrapping the connection.

### Authentication Against /etc/shadow and authorized_keys

Password auth reads the user's hashed password from `/etc/shadow` (Phase 27), hashes
the provided password, and compares. Public key auth reads `~/.ssh/authorized_keys`,
parses each line for an Ed25519 public key, and verifies the client's signature over
the session ID — the private key never leaves the client.

## How This Builds on Earlier Phases

- Extends Phase 23 by using TCP server sockets to accept SSH connections on port 22
- Extends Phase 27 by authenticating SSH users against `/etc/shadow` and reading
  `~/.ssh/authorized_keys` with per-user file permissions
- Extends Phase 29 by allocating PTY pairs for SSH sessions (same mechanism as telnetd)
- Extends Phase 30 by replacing the telnet protocol with encrypted SSH — the session
  lifecycle (accept → auth → PTY → shell → relay → cleanup) is structurally identical
- Extends Phase 37 by using epoll for multiplexing socket and PTY I/O within each session
- Extends Phase 42 by consuming Ed25519, X25519, ChaCha20-Poly1305, HMAC-SHA-256, and
  HKDF through the crypto-lib crate for all SSH cryptographic operations

## Implementation Outline

1. Evaluate sunset crate: add dependency, verify it compiles for x86_64-m3os target.
2. Create `userspace/sshd/` crate with basic TCP accept loop.
3. Generate Ed25519 host key on first boot, store at `/etc/ssh/ssh_host_ed25519_key`.
4. Implement sunset adapter: feed TCP bytes into sunset, write output back to socket.
5. Implement password authentication callback against `/etc/shadow`.
6. Implement session channel handling: PTY allocation, shell exec, data relay.
7. Implement public key authentication with `~/.ssh/authorized_keys`.
8. Implement window-change and signal forwarding.
9. Add sshd to init's startup sequence and initrd.
10. Test from host with `ssh -p 2222 user@localhost`.
11. Test multiple simultaneous SSH sessions.
12. Verify encryption by inspecting traffic.

## Acceptance Criteria

- `sshd` starts at boot and listens on port 22.
- `ssh user@host` from a standard OpenSSH client connects successfully.
- Password authentication works.
- Public key authentication works with `authorized_keys`.
- The remote shell is fully functional (commands, pipes, editor, compiler).
- Multiple simultaneous SSH sessions work.
- Host key fingerprint is stable across reboots.
- Connection is encrypted (verified by inspecting traffic with tcpdump/wireshark).
- Closing the SSH client cleanly terminates the remote session.
- Invalid credentials are rejected.

## Companion Task List

- [Phase 43 Task List](./tasks/43-ssh-server-tasks.md)

## How Real OS Implementations Differ

- Production SSH servers (OpenSSH) support SFTP, SCP, port forwarding, tunneling,
  X11 forwarding, agent forwarding, ProxyJump, certificate auth, and GSSAPI.
- OpenSSH uses privilege separation (unprivileged child handles the network, privileged
  parent handles authentication) to limit the impact of vulnerabilities.
- Real systems support multiple host key types (RSA, ECDSA, Ed25519) and algorithm
  negotiation across dozens of ciphers and MACs.
- Dropbear intentionally omits many features for simplicity. Our deployment is further
  simplified: single-server, no forwarding, no SFTP (initially).
- Production systems implement connection rate limiting, fail2ban integration, and
  audit logging for brute-force protection.

## Deferred Until Later

- SFTP / SCP file transfer
- Port forwarding and tunneling
- SSH client (for connecting from the OS to other machines)
- Key agent
- Certificate authentication
- Connection rate limiting and brute-force protection
- Privilege separation (sshd runs as a single process per session)
- Multiple host key types (Ed25519 only)
- Key re-exchange during long sessions
