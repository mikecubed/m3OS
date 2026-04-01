# Phase 42 - Cryptography Primitives

## Milestone Goal

The OS has a cryptography library providing hash functions, symmetric encryption, and
asymmetric key operations. This is the foundation layer that SSH (Phase 43) and future
security features build upon.

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

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 12 (POSIX Compat) | libc functions (memcpy, malloc) |
| Phase 31 (Compiler) | Optionally compile crypto code inside the OS |
| Phase 24 (Persistent Storage) | Store keys on disk |

## Implementation Outline

1. Choose implementation strategy (BearSSL recommended for balance of learning and reliability).
2. Cross-compile the crypto library with musl, targeting static linking.
3. Verify SHA-256 produces correct test vectors inside the OS.
4. Verify Ed25519 sign/verify with test vectors.
5. Verify X25519 key exchange with test vectors.
6. Verify AES or ChaCha20 with test vectors.
7. Build `sha256sum` and `genkey` utilities.
8. Document the library's API for use by the SSH server in Phase 43.

## Acceptance Criteria

- SHA-256 of known test vectors matches expected output.
- Ed25519 keypair generation, signing, and verification work correctly.
- X25519 key exchange produces matching shared secrets on both sides.
- AES-256 or ChaCha20-Poly1305 encrypt/decrypt round-trips correctly.
- `sha256sum /bin/tcc` produces a stable, correct hash.
- `genkey` creates an Ed25519 keypair saved to files.
- CSPRNG produces non-repeating output seeded from `getrandom`.

## Companion Task List

- Phase 42 Task List — *not yet created*

## How Real OS Implementations Differ

Real systems use:
- Hardware-accelerated crypto (AES-NI, SHA extensions) for performance
- OpenSSL or LibreSSL as the standard crypto library (huge, complex, battle-tested)
- Kernel-level crypto API (Linux crypto subsystem) for disk encryption, IPsec, etc.
- HSMs or TPMs for key storage

Our implementation prioritizes correctness and learning over performance. We use
software-only implementations and store keys as plain files.

## Security Note

The crypto implementations in this phase are for learning. They have not been audited
and should not be used to protect real secrets. BearSSL is the most trustworthy option
among our choices, as it was designed by a professional cryptographer.

## Deferred Until Later

- Hardware-accelerated crypto (AES-NI)
- TLS/SSL protocol implementation
- Certificate handling (X.509)
- RSA implementation
- Disk encryption
- Key management infrastructure (keyring, agent)
