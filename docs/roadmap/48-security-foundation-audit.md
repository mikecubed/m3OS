# Phase 48 — Security Foundation: Trust-Floor Audit

**Status:** Complete
**Date:** 2026-04-07
**Scope:** Tasks A.1–A.4 (Track A — documentation-only audit)
**Source Ref:** `docs/roadmap/48-security-foundation.md`

---

## A.1 — Credential Transition Audit

### Current Behavior

Four syscalls govern UID/GID transitions:

| Syscall | Number | Location | Privilege Check |
|---|---|---|---|
| `sys_linux_setuid` | 105 | `syscall.rs:2663–2674` | **None** |
| `sys_linux_setgid` | 106 | `syscall.rs:2679–2690` | **None** |
| `sys_linux_setreuid` | 113 | `syscall.rs:2696–2725` | Partial |
| `sys_linux_setregid` | 114 | `syscall.rs:2728–2757` | Partial |

**`setuid` (105):** Sets both `proc.uid` and `proc.euid` unconditionally. No privilege check of any kind. Any unprivileged process can call `setuid(0)` and become root.

**`setgid` (106):** Sets both `proc.gid` and `proc.egid` unconditionally. Same complete absence of privilege checks as `setuid`.

**`setreuid` (113):** Has basic checks — ruid change requires `euid == 0` or new value matches current real/effective uid. Same logic for euid change. Does NOT consult saved UID, so a process that dropped privileges via `setreuid` cannot restore them even when it should be able to.

**`setregid` (114):** Mirrors `setreuid` logic for GID. Same partial checks, same missing saved-UID support.

### Call Sites

| Binary | File | Usage |
|---|---|---|
| login | `userspace/login/src/main.rs:80` | `setgid()` then `setuid()` after password verification. Runs as root (PID 1 child). |
| su | `userspace/su/src/main.rs` | `setgid()` then `setuid()` after password verification. Runs as root. |

### Required Changes

1. **`setuid`:** If `euid == 0`, set both `uid` and `euid`. If `euid != 0`, only allow setting `euid` back to real uid. Otherwise return `-EPERM`.
2. **`setgid`:** Mirror `setuid` logic for `gid`/`egid`.
3. **`setreuid`:** Add saved UID support — allow euid change if `new_euid` matches current real, effective, OR saved uid.
4. **`setregid`:** Mirror `setreuid` saved-UID logic for GID.

### Risk Assessment

**Severity: CRITICAL.** `setuid` and `setgid` are completely unguarded. Any ring-3 process can escalate to root with a single syscall. This is the highest-priority fix in the entire security audit. `setreuid`/`setregid` are lower severity (partial checks exist) but still incorrect due to missing saved-UID support.

---

## A.2 — Entropy Pipeline Audit

### Current Behavior

**Seeding (`seed_pseudorandom_state`, `syscall.rs:3624–3630`):**
- Seeds exclusively from `rdtsc()` (CPU timestamp counter).
- Falls back to the constant `0xDEAD_BEEF_CAFE_BABE` if `rdtsc` returns 0.
- No use of RDRAND or RDSEED despite both being available on QEMU's default CPU model (CPUID.01H:ECX.RDRAND bit 30 = 1).

**PRNG (`fill_pseudorandom_bytes`, `syscall.rs:3632–3639`):**
- xorshift64 mixer with `wrapping_mul(0x2545_F491_4F6C_DD1D)`.
- Outputs 1 byte per iteration (top byte of the product).
- No forward secrecy, no backtracking resistance, no reseeding.

**Syscall (`sys_getrandom`, `syscall.rs:10068–10084`, syscall 318):**
- Seeds fresh state on every call (from `rdtsc` again).
- Caps output at 256 bytes.
- Uses `copy_to_user` to write to userspace buffer.

### Call Sites

| Consumer | File | Usage |
|---|---|---|
| crypto-lib CSPRNG | `userspace/crypto-lib/src/random.rs` | `csprng_init()` reads 32 bytes via `getrandom` to seed ChaCha20Rng |
| sshd | via crypto-lib | Session key generation |
| genkey | `userspace/coreutils-rs/src/genkey.rs` | Cryptographic key generation |
| passwd | `userspace/passwd/src/main.rs` | Will need random salts (Track D) |
| adduser | `userspace/adduser/src/main.rs` | Will need random salts (Track D) |

### Required Changes

1. Probe CPUID for RDRAND/RDSEED availability at boot.
2. Use RDRAND (or RDSEED when available) as the primary entropy source, with `rdtsc` as a mixing supplement only.
3. Eliminate the `0xDEAD_BEEF_CAFE_BABE` constant fallback.
4. Consider maintaining a single kernel-wide CSPRNG state that reseeds periodically rather than creating fresh xorshift state on every `getrandom` call.

### Risk Assessment

**Severity: HIGH.** The current entropy source (`rdtsc`) is predictable to an attacker who can estimate boot timing. The xorshift64 PRNG is not cryptographically secure. All downstream consumers (SSH session keys, CSPRNG seeding, key generation) inherit this weakness. The fallback constant makes the output fully deterministic if `rdtsc` ever returns 0.

---

## A.3 — Password Hashing and Default Credentials Audit

### Current Behavior

**Hash format:** `$sha256$<hex_salt>$<hex_hash>`

**Algorithm:** Single SHA-256 iteration of `salt || password`. Work factor is effectively zero — a modern GPU can compute billions of SHA-256 hashes per second.

**Salt derivation:** Username bytes encoded as hex (e.g., "root" becomes `726f6f74`). The salt is deterministic and publicly derivable from the username, defeating the purpose of salting.

**Hardcoded credentials in `xtask/src/main.rs` (`populate_ext2_files`):**

| Account | Salt (hex-encoded string) | Hash |
|---|---|---|
| root | `726f6f7473616c74` ("rootsalt") | `e95f58b3cda26426125bb223a690ddfde7444ac5d859e260fade5e515b91e7be` |
| user | `7573657273616c74` ("usersalt") | `9df26fef99d129060bdc8b3c35db9cdffd52cfc58361c4045ce3d37eb46160fe` |

### Call Sites

| Binary | File | Function |
|---|---|---|
| login | `userspace/login/src/main.rs` | `verify_password` via `verify_shadow()` |
| su | `userspace/su/src/main.rs` | `verify_password` |
| passwd | `userspace/passwd/src/main.rs:66` | `hash_password` |
| adduser | `userspace/adduser/src/main.rs:77` | `hash_password` |

### Required Changes

1. Replace single-iteration SHA-256 with a proper password hashing scheme (e.g., PBKDF2-SHA256 with a minimum of 100,000 iterations, or scrypt/Argon2 if feasible in `no_std`).
2. Generate salts from `getrandom` (after A.2 fixes), not from the username.
3. Update `hash_password` and `verify_password` in all consumers.
4. Regenerate default credentials in `xtask` with proper random salts.

### Risk Assessment

**Severity: HIGH.** Single-iteration SHA-256 with deterministic salts provides negligible resistance to offline brute-force attacks. The hardcoded password hashes in the xtask source are trivially reversible. Any attacker with access to `/etc/shadow` can crack all passwords near-instantly.

---

## A.4 — Service Defaults and Telnet Exposure Audit

### Current Behavior

**Service configurations written to ext2 image by xtask (`populate_ext2_files`):**

| Service | Config Path | Restart Policy | Depends On |
|---|---|---|---|
| sshd | `/etc/services.d/sshd.conf` | `restart=always, max_restart=10` | syslogd |
| telnetd | `/etc/services.d/telnetd.conf` | `restart=always, max_restart=10` | syslogd |
| syslogd | `/etc/services.d/syslogd.conf` | `restart=always, max_restart=10` | (none) |
| crond | `/etc/services.d/crond.conf` | `restart=always, max_restart=10` | syslogd |

**Init's `KNOWN_CONFIGS` array (`userspace/init/src/main.rs:47–56`):**
`sshd.conf`, `telnetd.conf`, `syslogd.conf`, `crond.conf`, `httpd.conf`, `dhcpd.conf`, `ntpd.conf`, `ftpd.conf`

All four services with config files auto-start on every boot. There is no mechanism to disable a service without deleting its config file.

### Call Sites

Init reads `/etc/services.d/*.conf` at boot and starts all matching services in dependency order.

### Required Changes

1. Add an `enabled=yes|no` field to service configs. Default telnetd to `enabled=no`.
2. Alternatively, remove `telnetd.conf` from the default image and require explicit opt-in.
3. Add a `service enable/disable` subcommand to the service management tool.
4. Document that telnetd transmits credentials in plaintext and should only be used on trusted networks or for debugging.

### Risk Assessment

**Severity: MEDIUM.** Telnetd listens on a TCP port, accepts plaintext connections, and feeds credentials through the login binary in cleartext. Combined with the weak password hashing (A.3), an attacker on the same network can trivially capture credentials. The `restart=always` policy ensures telnetd remains available even after crashes. SSH is available as a secure alternative, making telnetd's default-on status an unnecessary exposure.

---

## Summary of Findings

| ID | Finding | Severity | Category |
|---|---|---|---|
| A.1a | `setuid`/`setgid` have zero privilege checks — any process can become root | **CRITICAL** | Privilege escalation |
| A.1b | `setreuid`/`setregid` lack saved-UID support | MEDIUM | Incorrect POSIX semantics |
| A.2a | Entropy seeded from `rdtsc` only; RDRAND/RDSEED unused | HIGH | Weak entropy |
| A.2b | xorshift64 PRNG is not cryptographically secure | HIGH | Weak PRNG |
| A.2c | Fallback to constant `0xDEAD_BEEF_CAFE_BABE` if rdtsc returns 0 | HIGH | Deterministic output |
| A.3a | Single-iteration SHA-256 password hashing (zero work factor) | HIGH | Password security |
| A.3b | Salts derived deterministically from username | HIGH | Password security |
| A.3c | Hardcoded password hashes in xtask source | LOW | Credential hygiene |
| A.4a | Telnetd auto-starts by default, transmits plaintext credentials | MEDIUM | Network exposure |
| A.4b | No `enabled` toggle for services — deletion is the only disable mechanism | LOW | Configuration |
