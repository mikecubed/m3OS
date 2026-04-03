//! Cryptography primitives library for m3OS userspace (Phase 42).
//!
//! Provides SHA-256, HMAC-SHA-256, HKDF, ChaCha20-Poly1305, AES-256-CTR,
//! Ed25519, X25519, and a CSPRNG seeded from `getrandom`.
#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod asymmetric;
pub mod hash;
pub mod random;
pub mod symmetric;

/// Error type for cryptographic operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// AEAD authentication tag verification failed (decryption).
    AuthenticationFailed,
    /// Encryption operation failed.
    EncryptionFailed,
    /// Invalid key or parameter length.
    InvalidLength,
    /// CSPRNG seeding failed (getrandom returned insufficient bytes).
    SeedingFailed,
    /// Invalid key bytes (e.g., not on curve).
    InvalidKey,
}
