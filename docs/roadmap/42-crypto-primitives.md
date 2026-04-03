# Phase 42 - Cryptography Primitives

**Status:** Complete
**Source Ref:** phase-42
**Depends on:** Phase 12 (POSIX Compat) ✅, Phase 24 (Persistent Storage) ✅, Phase 31 (Compiler Toolchain) ✅
**Builds on:** Uses the `getrandom` syscall from Phase 12 as the entropy source; stores generated keys on the FAT32 filesystem from Phase 24; optionally compiles crypto code inside the OS using the TCC toolchain from Phase 31
**Primary Components:** userspace crypto-lib crate, sha256sum, genkey, crypto-test

## Milestone Goal

The OS has a cryptography library providing hash functions, symmetric encryption, and
asymmetric key operations. This is the foundation layer that SSH (Phase 43) and future
security features build upon.

## Why This Phase Exists

Phases 1-41 built a functional multi-user OS with networking, persistent storage, and a
compiler — but all network traffic is plaintext and there is no way to verify data
integrity or authenticate identities cryptographically. Without crypto primitives, the
OS cannot implement SSH (Phase 43), TLS, or any secure protocol. This phase provides
the building blocks that every security feature depends on.

## Learning Goals

- Understand the core cryptographic building blocks: hashing, symmetric encryption,
  asymmetric encryption, key exchange.
- Learn why you never implement your own crypto in production, but implementing it
  for learning is incredibly valuable.
- See how Diffie-Hellman key exchange enables two parties to agree on a shared secret
  over an insecure channel.
- Understand the difference between authentication (proving identity) and encryption
  (hiding content).

## Feature Scope

### Crypto Library

Port or implement the following primitives as a static library (`libcrypto.a`) that
userspace programs can link against:

**Hash Functions**
- SHA-256 — used for password hashing, key derivation, integrity checks
- HMAC-SHA-256 — keyed hashing for message authentication

**Symmetric Encryption**
- AES-128/256 — block cipher in CTR or CBC mode
- ChaCha20-Poly1305 — modern authenticated encryption (lighter than AES without
  hardware acceleration)

**Asymmetric Cryptography**
- Ed25519 — digital signatures (used for SSH host keys and user keys)
- Curve25519 / X25519 — Diffie-Hellman key exchange (used in SSH key exchange)
- RSA-2048 (stretch goal) — widely used but more complex to implement

**Key Derivation**
- HKDF (HMAC-based Key Derivation Function) — derive encryption keys from shared secrets

**Random Number Generation**
- Use the kernel's `getrandom` syscall (already implemented) as the entropy source.
- CSPRNG (Cryptographically Secure PRNG) — ChaCha20-based, seeded from `getrandom`.

### Implementation Strategy

**Option A: RustCrypto crates (recommended)**

Use the RustCrypto project's individual crates. They are pure Rust, `no_std`
compatible, actively maintained, and provide exactly the primitives this phase needs.
No C cross-compilation required. See [Rust Crate Acceleration](../rust-crate-acceleration.md).

| Primitive | Crate | `no_std` |
|---|---|---|
| SHA-256 | `sha2` | Yes |
| HMAC-SHA-256 | `hmac` | Yes |
| HKDF | `hkdf` | Yes |
| ChaCha20-Poly1305 | `chacha20poly1305` | Yes |
| AES-128/256 | `aes` + `ctr` | Yes |
| Ed25519 | `ed25519-dalek` | Yes (with `alloc`) |
| X25519 | `x25519-dalek` | Yes |
| CSPRNG | `rand_chacha` | Yes |

For TLS (userspace), add `rustls` + `webpki-roots` which embeds Mozilla CA
certificates directly in the binary — no cert files on disk needed.

**Option B: Port BearSSL**

[BearSSL](https://bearssl.org/) is a minimal, portable TLS library in C with no
dependencies beyond libc. It includes all the primitives listed above and is designed
for constrained environments. Cross-compile with musl. This is the fallback if
RustCrypto integration proves problematic.

**Option C: Write from scratch (maximum learning)**

Implement SHA-256, ChaCha20, and Ed25519 from their specifications. This teaches the
most but takes the longest. Reserve for stretch goals.

### Userspace Utilities

- **`sha256sum`** — hash files (like `sha256sum` on Linux)
- **`genkey`** — generate an Ed25519 keypair, write to files

## Important Components and How They Work

### Crypto Library Crate (`userspace/crypto-lib/`)

A `no_std`-compatible Rust library crate that re-exports RustCrypto primitives with a
thin wrapper API. Userspace binaries depend on this crate. It provides: `sha256()`,
`hmac_sha256()`, `hkdf_extract()`/`hkdf_expand()`, `aes256_ctr_encrypt()`/`decrypt()`,
`chacha20poly1305_seal()`/`open()`, `ed25519_keygen()`/`sign()`/`verify()`,
`x25519_keygen()`/`x25519_diffie_hellman()`, and `csprng_init()`/`csprng_fill()`.
Note: functions use output slices rather than `Vec<u8>` returns to avoid requiring alloc.

### CSPRNG Seeding Path

The kernel's `getrandom` syscall currently provides entropy from a TSC-seeded PRNG
(not cryptographically secure — hardening to RDRAND/RDSEED is deferred). The userspace
CSPRNG reads a 32-byte seed via `getrandom()`, initializes a ChaCha20 stream, and
generates random bytes on demand. The CSPRNG is per-process (not shared across fork).

### Key Storage

Ed25519 keypairs are stored as raw files on the filesystem. `genkey` writes
`id_ed25519` (private key seed, 32 bytes) and `id_ed25519.pub` (public key, 32 bytes)
in the current directory (or a directory specified with `-o <dir>`).
File permissions are enforced by the existing multi-user system from Phase 27.

## How This Builds on Earlier Phases

- Extends Phase 12 by consuming `getrandom()` syscall output as CSPRNG seed material
- Extends Phase 24 by storing generated keypairs on the persistent FAT32 filesystem
- Extends Phase 27 by using file permissions to protect private key files
- Extends Phase 31 by optionally allowing crypto test programs to be compiled inside the OS via TCC

## Implementation Outline

1. Add RustCrypto crate dependencies to the workspace (`sha2`, `hmac`, `hkdf`, `chacha20poly1305`, `aes`, `ctr`, `ed25519-dalek`, `x25519-dalek`, `rand_chacha`).
2. Create `userspace/crypto-lib/` crate wrapping the RustCrypto APIs.
3. Implement CSPRNG seeded from `getrandom` syscall.
4. Verify SHA-256 produces correct test vectors inside the OS.
5. Verify HMAC-SHA-256 and HKDF with test vectors.
6. Verify ChaCha20-Poly1305 and AES-256 with test vectors.
7. Verify Ed25519 sign/verify with test vectors.
8. Verify X25519 key exchange with test vectors.
9. Build `sha256sum` utility.
10. Build `genkey` utility.
11. Run full integration test inside QEMU.
12. Document the library's API for use by the SSH server in Phase 43.

## Acceptance Criteria

- SHA-256 of known test vectors matches expected output.
- HMAC-SHA-256 of known test vectors matches expected output.
- HKDF key derivation produces expected output for RFC 5869 test vectors.
- Ed25519 keypair generation, signing, and verification work correctly.
- X25519 key exchange produces matching shared secrets on both sides.
- ChaCha20-Poly1305 encrypt/decrypt round-trips correctly with RFC 8439 test vectors.
- AES-256-CTR encrypt/decrypt round-trips correctly.
- `sha256sum /bin/tcc` produces a stable, correct hash.
- `genkey` creates an Ed25519 keypair saved to files.
- CSPRNG produces non-repeating output seeded from `getrandom`.

## Companion Task List

- [Phase 42 Task List](./tasks/42-crypto-primitives-tasks.md)

## How Real OS Implementations Differ

- Real systems use hardware-accelerated crypto (AES-NI, SHA extensions) for performance
- OpenSSL or LibreSSL serve as the standard crypto library (huge, complex, battle-tested)
- Linux has a kernel-level crypto API (crypto subsystem) for disk encryption, IPsec, etc.
- HSMs or TPMs handle secure key storage
- Certificate authorities and X.509 chains provide identity verification at scale

Our implementation prioritizes correctness and learning over performance. We use
software-only implementations and store keys as plain files.

## Security Note

The crypto implementations in this phase are for learning. They have not been audited
and should not be used to protect real secrets. The RustCrypto crates are the most
trustworthy option among our choices, as they are actively maintained and widely
reviewed by the Rust security community.

## Deferred Until Later

- Hardware-accelerated crypto (AES-NI)
- TLS/SSL protocol implementation (Phase 43+ / future)
- Certificate handling (X.509)
- RSA implementation
- Disk encryption
- Key management infrastructure (keyring, agent)
