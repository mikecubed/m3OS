# Phase 43 - SSH Server

## Milestone Goal

An SSH server runs inside the OS, providing encrypted remote shell access. Users can
connect from any standard SSH client (`ssh user@host`) with password or public key
authentication. This replaces the telnet server for untrusted networks and is a major
milestone: the OS is now a secure, networked, multi-user system.

## Learning Goals

- Understand the SSH protocol: transport layer (encryption), user authentication layer,
  and connection layer (channels, sessions).
- Learn how key exchange (Diffie-Hellman / X25519) establishes a shared secret.
- See how public key authentication works without transmitting the private key.
- Understand why SSH is the universal standard for remote system administration.

## Feature Scope

### SSH Server (`/sbin/sshd`)

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

**Option C: Port tinyssh**

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

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 23 (Socket API) | TCP server sockets |
| Phase 27 (User Accounts) | Authentication, UID/GID |
| Phase 29 (PTY) | Terminal sessions |
| Phase 42 (Crypto) | Ed25519, X25519, ChaCha20, SHA-256 |

## Implementation Outline

1. Cross-compile Dropbear with musl (or begin SSH protocol implementation).
2. Generate Ed25519 host key at first boot.
3. Implement SSH transport: version exchange, key exchange, encrypted packets.
4. Implement password authentication against `/etc/shadow`.
5. Implement session channel with PTY allocation.
6. Implement shell execution (fork + setsid + exec login).
7. Test from host with `ssh -p 2222 user@localhost`.
8. Implement public key authentication.
9. Add sshd to init's startup sequence.
10. Test multiple simultaneous SSH sessions.

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

- Phase 43 Task List — *not yet created*

## How Real OS Implementations Differ

Production SSH servers (OpenSSH) support:
- SFTP and SCP for file transfer
- Port forwarding and tunneling
- X11 forwarding
- Agent forwarding
- ProxyJump / ProxyCommand
- Match blocks for conditional configuration
- Kerberos/GSSAPI authentication
- Certificate-based authentication
- Audit logging

Dropbear intentionally omits many of these features for simplicity. Our deployment
is further simplified: single-server, no forwarding, no SFTP (initially).

## Deferred Until Later

- SFTP / SCP file transfer
- Port forwarding and tunneling
- SSH client (for connecting from the OS to other machines)
- Key agent
- Certificate authentication
- Connection rate limiting and brute-force protection
