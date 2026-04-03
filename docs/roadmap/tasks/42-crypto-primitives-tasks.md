# Phase 42 — Cryptography Primitives: Task List

**Status:** Complete
**Source Ref:** phase-42
**Depends on:** Phase 12 (POSIX Compat) ✅, Phase 24 (Persistent Storage) ✅, Phase 31 (Compiler Toolchain) ✅
**Goal:** Add a userspace cryptography library backed by RustCrypto crates providing
SHA-256, HMAC, HKDF, ChaCha20-Poly1305, AES-256, Ed25519, X25519, and a
ChaCha20-based CSPRNG seeded from `getrandom`. Build `sha256sum` and `genkey` utilities.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Workspace setup and crypto-lib crate scaffold | — | Complete |
| B | CSPRNG seeded from getrandom | A | Complete |
| C | Hash functions (SHA-256, HMAC-SHA-256, HKDF) | A | Complete |
| D | Symmetric encryption (ChaCha20-Poly1305, AES-256) | A | Complete |
| E | Asymmetric crypto (Ed25519, X25519) | A, B | Complete |
| F | Userspace utilities (sha256sum, genkey) | C, E | Complete |
| G | Integration testing and documentation | A–F | Complete |

---

## Track A — Workspace Setup and Crypto-Lib Crate Scaffold

Add RustCrypto dependencies to the workspace and create the crypto library crate
that userspace binaries will link against.

### A.1 — Add RustCrypto crate dependencies to workspace

**File:** `Cargo.toml`
**Symbol:** `[workspace.dependencies]`
**Why it matters:** All crypto primitives come from the RustCrypto ecosystem. Adding
them at the workspace level ensures consistent versions across crates and confirms
they compile for the `x86_64-m3os` target (no_std + alloc).

**Acceptance:**
- [x] `sha2`, `hmac`, `hkdf`, `chacha20poly1305`, `aes`, `ctr`, `ed25519-dalek`, `x25519-dalek`, `rand_chacha`, `rand_core` added to workspace dependencies
- [x] All crates configured with `default-features = false` and `alloc` feature where needed
- [x] `cargo xtask check` passes with the new dependencies

### A.2 — Create `userspace/crypto-lib/` crate

**File:** `userspace/crypto-lib/Cargo.toml`
**Symbol:** `crypto-lib`
**Why it matters:** A dedicated library crate provides a single entry point for all
crypto operations. Userspace binaries (`sha256sum`, `genkey`, SSH in Phase 43) depend
on this crate instead of importing RustCrypto crates individually, keeping the API
surface consistent and manageable.

**Acceptance:**
- [x] `userspace/crypto-lib/` exists as a `no_std` library crate with `#![no_std]`
- [x] Crate re-exports workspace RustCrypto dependencies
- [x] Crate is added to the workspace members list
- [x] `cargo xtask check` passes with the new crate

### A.3 — Define crypto-lib public API surface

**File:** `userspace/crypto-lib/src/lib.rs`
**Symbol:** `pub mod` declarations
**Why it matters:** A well-defined API boundary makes it easy for Phase 43 (SSH) and
future phases to consume crypto operations without understanding RustCrypto internals.
Each module maps to one category of cryptographic operation.

**Acceptance:**
- [x] `pub mod hash` — SHA-256, HMAC-SHA-256, HKDF
- [x] `pub mod symmetric` — ChaCha20-Poly1305, AES-256-CTR
- [x] `pub mod asymmetric` — Ed25519, X25519
- [x] `pub mod random` — CSPRNG
- [x] Each module compiles (functions can be stubs initially)

---

## Track B — CSPRNG Seeded from getrandom

Implement a cryptographically secure pseudorandom number generator that seeds
itself from the kernel's `getrandom` syscall.

### B.1 — Implement CSPRNG initialization from getrandom

**File:** `userspace/crypto-lib/src/random.rs`
**Symbol:** `CsprngState`, `csprng_init`
**Why it matters:** Every cryptographic operation that generates keys, nonces, or IVs
needs a reliable source of randomness. The CSPRNG bridges the kernel's `getrandom`
syscall (which may be slow or limited) and fast userspace random byte generation via
ChaCha20. Without proper seeding, generated keys are predictable.

**Acceptance:**
- [x] `csprng_init()` reads 32 bytes from `getrandom` syscall via `syscall-lib`
- [x] Seeds a `ChaCha20Rng` (from `rand_chacha`) with the 32-byte seed
- [x] Returns `CsprngState` that wraps the initialized `ChaCha20Rng`
- [x] Fails with an error if `getrandom` returns fewer than 32 bytes

### B.2 — Implement `csprng_fill()` for random byte generation

**File:** `userspace/crypto-lib/src/random.rs`
**Symbol:** `csprng_fill`
**Why it matters:** Callers need a simple function to fill a buffer with random bytes.
This is used by Ed25519 key generation, nonce generation for symmetric ciphers, and
any future protocol that needs randomness.

**Acceptance:**
- [x] `csprng_fill(state: &mut CsprngState, buf: &mut [u8])` fills `buf` with random bytes
- [x] Uses `RngCore::fill_bytes` from `rand_core` trait on the ChaCha20Rng
- [x] Calling `csprng_fill` twice with the same state produces different output
- [x] Works for buffers of any length (including 0)

---

## Track C — Hash Functions (SHA-256, HMAC-SHA-256, HKDF)

Implement hash function wrappers used for integrity checks, message
authentication, and key derivation.

### C.1 — Implement SHA-256 wrapper

**File:** `userspace/crypto-lib/src/hash.rs`
**Symbol:** `sha256`
**Why it matters:** SHA-256 is the most widely used hash function in the primitives
this phase provides. HMAC, HKDF, and Ed25519 all depend on it. The `sha256sum`
utility needs it directly. Getting test vectors right here validates the entire
RustCrypto integration path.

**Acceptance:**
- [x] `sha256(data: &[u8]) -> [u8; 32]` computes SHA-256 digest
- [x] Matches NIST test vectors: empty string → `e3b0c44298fc1c14...`
- [x] Matches NIST test vectors: `"abc"` → `ba7816bf8f01cfea...`
- [x] Supports incremental hashing via `Sha256Hasher` struct with `update()`/`finalize()`

### C.2 — Implement HMAC-SHA-256 wrapper

**File:** `userspace/crypto-lib/src/hash.rs`
**Symbol:** `hmac_sha256`
**Why it matters:** HMAC provides keyed message authentication. SSH uses HMAC to
verify that packets have not been tampered with in transit. HKDF (Track C.3) is
built on top of HMAC.

**Acceptance:**
- [x] `hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32]` computes HMAC-SHA-256
- [x] Matches RFC 4231 test vector 1 (key=0x0b*20, data="Hi There")
- [x] Matches RFC 4231 test vector 2 (key="Jefe", data="what do ya want...")
- [x] Supports incremental HMAC via `HmacSha256` struct with `update()`/`finalize()`

### C.3 — Implement HKDF wrapper

**File:** `userspace/crypto-lib/src/hash.rs`
**Symbol:** `hkdf_extract`, `hkdf_expand`
**Why it matters:** HKDF derives cryptographically strong keys from shared secrets.
SSH key exchange produces a shared secret via X25519, then uses HKDF to derive
session encryption keys. Without HKDF, key derivation would be ad-hoc and weak.

**Acceptance:**
- [x] `hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32]` extracts a pseudorandom key
- [x] `hkdf_expand(prk: &[u8], info: &[u8], len: usize) -> Vec<u8>` expands to `len` bytes
- [x] Matches RFC 5869 test case 1 (SHA-256, IKM=0x0b*22, salt=0x000102...0c)
- [x] Matches RFC 5869 test case 2 (SHA-256, longer inputs)

---

## Track D — Symmetric Encryption (ChaCha20-Poly1305, AES-256)

Implement authenticated encryption and block cipher wrappers for data
confidentiality.

### D.1 — Implement ChaCha20-Poly1305 wrapper

**File:** `userspace/crypto-lib/src/symmetric.rs`
**Symbol:** `chacha20poly1305_seal`, `chacha20poly1305_open`
**Why it matters:** ChaCha20-Poly1305 is the preferred cipher for SSH when AES-NI
hardware is not available (which is the case in our QEMU setup). It provides
authenticated encryption — both confidentiality and integrity in a single operation.

**Acceptance:**
- [x] `chacha20poly1305_seal(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8], aad: &[u8]) -> Vec<u8>` encrypts and appends 16-byte auth tag
- [x] `chacha20poly1305_open(key: &[u8; 32], nonce: &[u8; 12], ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError>` decrypts and verifies tag
- [x] Encrypt then decrypt round-trips produce the original plaintext
- [x] Matches RFC 8439 Section 2.8.2 test vector
- [x] Tampered ciphertext returns `Err(CryptoError::AuthenticationFailed)`

### D.2 — Implement AES-256-CTR wrapper

**File:** `userspace/crypto-lib/src/symmetric.rs`
**Symbol:** `aes256_ctr_encrypt`, `aes256_ctr_decrypt`
**Why it matters:** AES is the most widely deployed symmetric cipher. While
ChaCha20 is preferred in this project, AES support is needed for interoperability
with systems that require it and for completeness of the crypto library.

**Acceptance:**
- [x] `aes256_ctr_encrypt(key: &[u8; 32], nonce: &[u8; 16], plaintext: &[u8]) -> Vec<u8>` encrypts
- [x] `aes256_ctr_decrypt(key: &[u8; 32], nonce: &[u8; 16], ciphertext: &[u8]) -> Vec<u8>` decrypts
- [x] CTR mode: encrypt and decrypt are the same operation (XOR with keystream)
- [x] Encrypt then decrypt round-trips produce the original plaintext
- [x] Matches NIST SP 800-38A F.5.5 AES-256-CTR test vector

---

## Track E — Asymmetric Cryptography (Ed25519, X25519)

Implement digital signatures and key exchange for authentication and secure
channel establishment.

### E.1 — Implement Ed25519 key generation, signing, and verification

**File:** `userspace/crypto-lib/src/asymmetric.rs`
**Symbol:** `ed25519_keygen`, `ed25519_sign`, `ed25519_verify`
**Why it matters:** Ed25519 is used for SSH host key authentication and user key
authentication. The server proves its identity by signing a challenge with its host
key, and users authenticate by signing with their private key. Key generation is
needed for the `genkey` utility.

**Acceptance:**
- [x] `ed25519_keygen(rng: &mut CsprngState) -> (SigningKey, VerifyingKey)` generates a keypair
- [x] `ed25519_sign(key: &SigningKey, message: &[u8]) -> [u8; 64]` produces a 64-byte signature
- [x] `ed25519_verify(key: &VerifyingKey, message: &[u8], signature: &[u8; 64]) -> bool` verifies
- [x] Matches RFC 8032 Section 7.1 test vector (TEST 1: empty message)
- [x] Matches RFC 8032 Section 7.1 test vector (TEST 2: 1-byte message 0x72)
- [x] Verification of a tampered message returns `false`

### E.2 — Implement X25519 Diffie-Hellman key exchange

**File:** `userspace/crypto-lib/src/asymmetric.rs`
**Symbol:** `x25519_keygen`, `x25519_diffie_hellman`
**Why it matters:** X25519 enables two parties to agree on a shared secret over an
insecure channel. SSH uses this during key exchange so that the client and server
derive the same session keys without ever transmitting them.

**Acceptance:**
- [x] `x25519_keygen(rng: &mut CsprngState) -> (StaticSecret, PublicKey)` generates a keypair
- [x] `x25519_diffie_hellman(my_secret: &StaticSecret, their_public: &PublicKey) -> [u8; 32]` computes shared secret
- [x] Two keypairs performing mutual DH produce the same shared secret
- [x] Matches RFC 7748 Section 6.1 test vector (Alice and Bob's shared secret)

### E.3 — Implement key serialization for file storage

**File:** `userspace/crypto-lib/src/asymmetric.rs`
**Symbol:** `ed25519_to_bytes`, `ed25519_from_bytes`
**Why it matters:** The `genkey` utility (Track F) needs to write keypairs to files
and the SSH server (Phase 43) needs to read them back. Serialization must be
deterministic so that keys round-trip correctly through the filesystem.

**Acceptance:**
- [x] `ed25519_signing_key_to_bytes(key: &SigningKey) -> [u8; 32]` exports private key seed
- [x] `ed25519_signing_key_from_bytes(bytes: &[u8; 32]) -> SigningKey` reconstructs from seed
- [x] `ed25519_verifying_key_to_bytes(key: &VerifyingKey) -> [u8; 32]` exports public key
- [x] `ed25519_verifying_key_from_bytes(bytes: &[u8; 32]) -> Result<VerifyingKey, CryptoError>` reconstructs
- [x] Round-trip: generate key → export → import → sign → verify succeeds

---

## Track F — Userspace Utilities (sha256sum, genkey)

Build command-line tools that exercise the crypto library and are useful on
their own.

### F.1 — Build `sha256sum` utility

**Files:**
- `userspace/coreutils-rs/src/bin/sha256sum.rs`
- `userspace/coreutils-rs/Cargo.toml`

**Symbol:** `main` (sha256sum binary)
**Why it matters:** `sha256sum` is a standard Unix utility that hashes files. It
validates the entire SHA-256 path end-to-end (file I/O → incremental hashing →
hex output) and provides a practical tool for verifying file integrity inside the OS.

**Acceptance:**
- [x] `sha256sum <file>` prints `<hex-hash>  <filename>` matching the GNU coreutils format
- [x] `sha256sum /bin/tcc` produces a stable, correct hash across reboots
- [x] Supports multiple file arguments: `sha256sum file1 file2`
- [x] Reads stdin when no file argument is given
- [x] Uses incremental hashing (does not load entire file into memory)

### F.2 — Build `genkey` utility

**Files:**
- `userspace/coreutils-rs/src/bin/genkey.rs`
- `userspace/coreutils-rs/Cargo.toml`

**Symbol:** `main` (genkey binary)
**Why it matters:** `genkey` generates Ed25519 keypairs and writes them to disk. SSH
(Phase 43) needs host keys and user keys. This utility lets users create their own
keypairs and validates the full path from CSPRNG → key generation → file I/O.

**Acceptance:**
- [x] `genkey` generates an Ed25519 keypair
- [x] Writes private key to `id_ed25519` (32-byte seed) in the current directory
- [x] Writes public key to `id_ed25519.pub` (32-byte public key) in the current directory
- [x] Prints the public key in hex to stdout
- [x] Accepts optional `-o <path>` flag to specify output directory
- [x] CSPRNG is properly seeded from `getrandom` (keys differ across invocations)

### F.3 — Add sha256sum and genkey to initrd

**File:** `kernel/build.rs`
**Symbol:** `INITRD_BINARIES` or equivalent initrd build list
**Why it matters:** Userspace binaries must be embedded in the initial ramdisk to
be available at boot. Without adding them to the initrd build, the utilities exist
as compiled artifacts but cannot be executed inside the OS.

**Acceptance:**
- [x] `sha256sum` binary is included in the initrd
- [x] `genkey` binary is included in the initrd
- [x] Both are accessible at `/bin/sha256sum` and `/bin/genkey` after boot
- [x] `cargo xtask image` produces a bootable image with both utilities

---

## Track G — Integration Testing and Documentation

Validate all crypto primitives work inside the running OS and update project
documentation.

### G.1 — Create crypto-test userspace program

**File:** `userspace/crypto-test/src/main.rs`
**Symbol:** `main`
**Why it matters:** A dedicated test binary exercises every crypto primitive inside
the actual OS environment (no_std, custom allocator, real getrandom syscall). This
catches issues that host-side unit tests cannot: wrong syscall numbers, allocator
incompatibilities, or missing entropy.

**Acceptance:**
- [x] Runs SHA-256, HMAC-SHA-256, HKDF test vectors and prints PASS/FAIL
- [x] Runs ChaCha20-Poly1305 and AES-256-CTR encrypt/decrypt round-trip tests
- [x] Runs Ed25519 keygen/sign/verify test
- [x] Runs X25519 mutual key exchange test
- [x] Runs CSPRNG non-repetition test (generate two 32-byte blocks, verify they differ)
- [x] Exits with 0 if all tests pass, non-zero on any failure

### G.2 — Add host-side unit tests to kernel-core or crypto-lib

**Files:**
- `userspace/crypto-lib/src/hash.rs`
- `userspace/crypto-lib/src/symmetric.rs`
- `userspace/crypto-lib/src/asymmetric.rs`

**Symbol:** `#[cfg(test)] mod tests`
**Why it matters:** Host-side tests run fast via `cargo test` and catch regressions
without booting QEMU. They verify that the wrapper functions correctly delegate to
the underlying RustCrypto crates and produce expected outputs.

**Acceptance:**
- [x] `cargo test -p crypto-lib` runs all test vector checks on the host
- [x] At least one test per public function in the crypto-lib API
- [x] Tests cover error cases (tampered ciphertext, invalid key bytes)

### G.3 — Verify no regressions in existing tests

**Files:**
- `kernel/tests/*.rs`
- `userspace/*/src/main.rs`

**Symbol:** (all existing tests)
**Why it matters:** Adding new crate dependencies and a new userspace crate could
introduce build issues or increase binary sizes beyond limits. All existing tests
must continue to pass.

**Acceptance:**
- [x] `cargo xtask check` passes (clippy + fmt)
- [x] `cargo xtask test` passes (all existing QEMU tests)
- [x] `cargo test -p kernel-core` passes (host-side unit tests)

### G.4 — Update documentation

**Files:**
- `docs/roadmap/42-crypto-primitives.md`
- `docs/roadmap/README.md`
- `docs/roadmap/tasks/README.md`

**Symbol:** (documentation)
**Why it matters:** Roadmap docs must reflect the actual implementation state and
link to the completed task list. The README tables must be updated so future phases
can find the crypto library.

**Acceptance:**
- [x] Design doc status updated to `Complete` after implementation
- [x] README row updated with task list link and `Complete` status
- [x] Tasks README row updated with link and `Complete` status
- [x] Any deferred items accurately reflect what was and was not implemented

---

## Documentation Notes

- Phase 42 introduces the first cryptographic capability in m3OS. Previously, all
  data handling was plaintext with no integrity verification or authentication.
- The `crypto-lib` crate is a userspace library, not a kernel component. The kernel's
  only role is providing entropy via `getrandom`.
- RustCrypto crates are used as-is with thin wrappers. No custom cryptographic
  algorithms are implemented (Option A from the design doc).
- Ed25519 keys are stored as raw 32-byte seeds/public keys, not in PEM or OpenSSH
  format. Phase 43 (SSH) will add format conversion if needed.
- The CSPRNG is per-process. After `fork()`, parent and child have independent CSPRNG
  states (because they have separate memory). This is correct behavior — shared CSPRNG
  state across processes would be a security bug.
- `sha256sum` is added to `coreutils-rs` rather than as a standalone binary, following
  the established pattern for Rust userspace utilities.
