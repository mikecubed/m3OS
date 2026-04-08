# Security Foundation

**Aligned Roadmap Phase:** Phase 48
**Status:** Complete
**Source Ref:** phase-48

## Overview

Phase 48 closes the trust-floor gaps in the m3OS multi-user and network-facing
security model. It replaces advisory, demo-quality defaults with real enforcement:
kernel-checked credential transitions, hardware-backed entropy, iterated password
hashing, and safe boot defaults.

## What This Doc Covers

- Kernel-enforced setuid/setgid privilege checks (replacing unconditional transitions)
- RDRAND-backed PRNG seeding (replacing TSC-only entropy)
- Iterated SHA-256 password hashing with random salts (replacing single-iteration username-salted hashing)
- Locked-account first-boot provisioning (replacing hardcoded default credentials)
- Telnet opt-in policy (replacing default-on plaintext remote access)
- Account file write hardening (single-write pattern, fsync)

## Core Implementation

### Credential Enforcement

Before Phase 48, `setuid()` and `setgid()` were unconditional — any process
could call `setuid(0)` and become root. The security boundary was entirely in
userspace password verification, which collapses if any process bypasses login.

Phase 48 adds POSIX-style privilege checks in the kernel:
- Root (euid 0) can set any UID/GID
- Non-root can only restore euid to their real UID
- `setreuid`/`setregid` enforce matching rules for real and effective IDs

The enforcement logic lives in `kernel-core::cred::Credentials`, making it
host-testable without QEMU.

### Entropy Pipeline

The kernel PRNG was seeded solely from `rdtsc` (CPU timestamp counter), which
is predictable on fast boot. Phase 48 adds RDRAND as the primary entropy source,
mixed with TSC to hedge against single-source failures. The PRNG reseeds from
RDRAND every 256 bytes to limit state-compromise damage.

The PRNG mixer is extracted into `kernel-core::prng::Prng` for host testing.

### Password Hashing

Passwords were hashed with a single SHA-256 iteration using the username as salt.
Phase 48 introduces an iterated scheme with 10,000 rounds and cryptographically
random 16-byte salts generated via `getrandom()`.

New format: `$sha256i$<rounds>$<hex_salt>$<hex_hash>`
Legacy format: `$sha256$<hex_salt>$<hex_hash>` (still verified for migration)

### First-Boot Provisioning

Default images no longer ship with valid password hashes. Shadow entries use the
locked-account marker `!`. On first login, the system detects the locked state and
prompts the user to set a password. After setup, normal authentication proceeds.

### Service Defaults

Telnetd is removed from the default image build. Operators who need plaintext
remote access for debugging can enable it with `cargo xtask image --enable-telnet`.
SSH, syslogd, and crond remain in the default boot path.

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/cred.rs` | Credential transition logic with POSIX privilege checks |
| `kernel-core/src/prng.rs` | Xorshift64-multiply PRNG, extracted for host testing |
| `kernel/src/arch/x86_64/syscall.rs` | Kernel syscall handlers for setuid/setgid/getrandom |
| `userspace/syscall-lib/src/sha256.rs` | Password hashing with iterated SHA-256 |
| `userspace/login/src/main.rs` | First-boot password setup for locked accounts |
| `userspace/passwd/src/main.rs` | Password change with random salts |
| `userspace/adduser/src/main.rs` | Account creation with single-write pattern and fsync |
| `xtask/src/main.rs` | Image build with locked accounts and telnet opt-in |

## How This Phase Differs From Later Security Work

- This phase establishes the **trust floor** — minimum enforcement for identity transitions, entropy, and credentials.
- Later phases add **sandboxing** (process isolation beyond UID checks), **privilege separation** (separating daemon logic from privileged operations), and **capability-based access control** (beyond simple UID/GID checks).
- This phase does NOT add setuid-bit-on-exec, supplementary groups, or filesystem ACLs.

## Related Roadmap Docs

- [Phase 48 roadmap doc](./roadmap/48-security-foundation.md)
- [Phase 48 task doc](./roadmap/tasks/48-security-foundation-tasks.md)
- [Phase 48 audit](./roadmap/48-security-foundation-audit.md)

## Deferred or Later-Phase Topics

- Setuid-bit on executables (requires exec-time capability checks)
- Supplementary groups and group-based access control
- Privilege separation for SSH and other network daemons
- Sandboxing and namespace isolation
- Proper key derivation functions (Argon2id, scrypt) — deferred for library availability in no_std
