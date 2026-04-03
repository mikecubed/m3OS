//! Asymmetric cryptography: Ed25519 signatures and X25519 key exchange.

use crate::CryptoError;
use crate::random::CsprngState;

// Re-export key types for callers.
pub use ed25519_dalek::{SigningKey, VerifyingKey};
pub use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};

/// Generate an Ed25519 keypair.
pub fn ed25519_keygen(rng: &mut CsprngState) -> (SigningKey, VerifyingKey) {
    let signing_key = SigningKey::generate(rng.rng());
    let verifying_key = signing_key.verifying_key();
    (signing_key, verifying_key)
}

/// Sign a message with an Ed25519 signing key.
pub fn ed25519_sign(key: &SigningKey, message: &[u8]) -> [u8; 64] {
    use ed25519_dalek::Signer;
    let sig = key.sign(message);
    sig.to_bytes()
}

/// Verify an Ed25519 signature.
pub fn ed25519_verify(key: &VerifyingKey, message: &[u8], signature: &[u8; 64]) -> bool {
    use ed25519_dalek::Verifier;
    let sig = ed25519_dalek::Signature::from_bytes(signature);
    key.verify(message, &sig).is_ok()
}

/// Export Ed25519 signing key as 32-byte seed.
pub fn ed25519_signing_key_to_bytes(key: &SigningKey) -> [u8; 32] {
    key.to_bytes()
}

/// Reconstruct Ed25519 signing key from 32-byte seed.
pub fn ed25519_signing_key_from_bytes(bytes: &[u8; 32]) -> SigningKey {
    SigningKey::from_bytes(bytes)
}

/// Export Ed25519 verifying (public) key as 32 bytes.
pub fn ed25519_verifying_key_to_bytes(key: &VerifyingKey) -> [u8; 32] {
    key.to_bytes()
}

/// Reconstruct Ed25519 verifying key from 32 bytes.
pub fn ed25519_verifying_key_from_bytes(bytes: &[u8; 32]) -> Result<VerifyingKey, CryptoError> {
    VerifyingKey::from_bytes(bytes).map_err(|_| CryptoError::InvalidKey)
}

/// Generate an X25519 keypair for Diffie-Hellman key exchange.
pub fn x25519_keygen(rng: &mut CsprngState) -> (X25519StaticSecret, X25519PublicKey) {
    let secret = X25519StaticSecret::random_from_rng(rng.rng());
    let public = X25519PublicKey::from(&secret);
    (secret, public)
}

/// Perform X25519 Diffie-Hellman to compute a shared secret.
pub fn x25519_diffie_hellman(
    my_secret: &X25519StaticSecret,
    their_public: &X25519PublicKey,
) -> [u8; 32] {
    my_secret.diffie_hellman(their_public).to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_rng() -> CsprngState {
        CsprngState::from_seed([0x42u8; 32])
    }

    #[test]
    fn test_ed25519_sign_verify() {
        let mut rng = test_rng();
        let (signing_key, verifying_key) = ed25519_keygen(&mut rng);
        let message = b"test message";
        let signature = ed25519_sign(&signing_key, message);
        assert!(ed25519_verify(&verifying_key, message, &signature));
    }

    #[test]
    fn test_ed25519_tampered_message() {
        let mut rng = test_rng();
        let (signing_key, verifying_key) = ed25519_keygen(&mut rng);
        let message = b"test message";
        let signature = ed25519_sign(&signing_key, message);
        assert!(!ed25519_verify(
            &verifying_key,
            b"wrong message",
            &signature
        ));
    }

    #[test]
    fn test_ed25519_key_roundtrip() {
        let mut rng = test_rng();
        let (signing_key, verifying_key) = ed25519_keygen(&mut rng);
        let message = b"roundtrip test";
        let signature = ed25519_sign(&signing_key, message);

        // Export and reimport.
        let sk_bytes = ed25519_signing_key_to_bytes(&signing_key);
        let vk_bytes = ed25519_verifying_key_to_bytes(&verifying_key);

        let sk2 = ed25519_signing_key_from_bytes(&sk_bytes);
        let vk2 = ed25519_verifying_key_from_bytes(&vk_bytes).unwrap();

        // Verify with reimported key.
        assert!(ed25519_verify(&vk2, message, &signature));

        // Sign with reimported key, verify with original.
        let sig2 = ed25519_sign(&sk2, message);
        assert!(ed25519_verify(&verifying_key, message, &sig2));
    }

    #[test]
    fn test_x25519_mutual_dh() {
        let mut rng = test_rng();
        let (alice_secret, alice_public) = x25519_keygen(&mut rng);
        let (bob_secret, bob_public) = x25519_keygen(&mut rng);

        let alice_shared = x25519_diffie_hellman(&alice_secret, &bob_public);
        let bob_shared = x25519_diffie_hellman(&bob_secret, &alice_public);

        assert_eq!(alice_shared, bob_shared);
    }

    #[test]
    fn test_ed25519_rfc8032_test1() {
        // RFC 8032 Section 7.1, TEST 1: empty message
        // Reconstruct from seed and verify the signature matches.
        let sk_bytes: [u8; 32] = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];
        let expected_sig: [u8; 64] = [
            0xe5, 0x56, 0x43, 0x00, 0xc3, 0x60, 0xac, 0x72, 0x90, 0x86, 0xe2, 0xcc, 0x80, 0x6e,
            0x82, 0x8a, 0x84, 0x87, 0x7f, 0x1e, 0xb8, 0xe5, 0xd9, 0x74, 0xd8, 0x73, 0xe0, 0x65,
            0x22, 0x49, 0x01, 0x55, 0x5f, 0xb8, 0x82, 0x15, 0x90, 0xa3, 0x3b, 0xac, 0xc6, 0x1e,
            0x39, 0x70, 0x1c, 0xf9, 0xb4, 0x6b, 0xd2, 0x5b, 0xf5, 0xf0, 0x59, 0x5b, 0xbe, 0x24,
            0x65, 0x51, 0x41, 0x43, 0x8e, 0x7a, 0x10, 0x0b,
        ];

        let sk = ed25519_signing_key_from_bytes(&sk_bytes);
        let vk = sk.verifying_key();
        let sig = ed25519_sign(&sk, b"");

        // Verify the signature matches the RFC vector.
        assert_eq!(sig, expected_sig);
        // Verify the signature is valid.
        assert!(ed25519_verify(&vk, b"", &sig));
        // Tampered message should fail.
        assert!(!ed25519_verify(&vk, b"x", &sig));
    }

    #[test]
    fn test_ed25519_rfc8032_test2() {
        // RFC 8032 Section 7.1, TEST 2: 1-byte message 0x72
        let sk_bytes: [u8; 32] = [
            0x4c, 0xcd, 0x08, 0x9b, 0x28, 0xff, 0x96, 0xda, 0x9d, 0xb6, 0xc3, 0x46, 0xec, 0x11,
            0x4e, 0x0f, 0x5b, 0x8a, 0x31, 0x9f, 0x35, 0xab, 0xa6, 0x24, 0xda, 0x8c, 0xf6, 0xed,
            0x4f, 0xb8, 0xa6, 0xfb,
        ];
        let expected_sig: [u8; 64] = [
            0x92, 0xa0, 0x09, 0xa9, 0xf0, 0xd4, 0xca, 0xb8, 0x72, 0x0e, 0x82, 0x0b, 0x5f, 0x64,
            0x25, 0x40, 0xa2, 0xb2, 0x7b, 0x54, 0x16, 0x50, 0x3f, 0x8f, 0xb3, 0x76, 0x22, 0x23,
            0xeb, 0xdb, 0x69, 0xda, 0x08, 0x5a, 0xc1, 0xe4, 0x3e, 0x15, 0x99, 0x6e, 0x45, 0x8f,
            0x36, 0x13, 0xd0, 0xf1, 0x1d, 0x8c, 0x38, 0x7b, 0x2e, 0xae, 0xb4, 0x30, 0x2a, 0xee,
            0xb0, 0x0d, 0x29, 0x16, 0x12, 0xbb, 0x0c, 0x00,
        ];

        let sk = ed25519_signing_key_from_bytes(&sk_bytes);
        let sig = ed25519_sign(&sk, &[0x72]);
        assert_eq!(sig, expected_sig);
        assert!(ed25519_verify(&sk.verifying_key(), &[0x72], &sig));
    }

    #[test]
    fn test_x25519_rfc7748() {
        // RFC 7748 Section 6.1 test vector
        let alice_sk_bytes: [u8; 32] = [
            0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d, 0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2,
            0x66, 0x45, 0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a, 0xb1, 0x77, 0xfb, 0xa5,
            0x1d, 0xb9, 0x2c, 0x2a,
        ];
        let bob_sk_bytes: [u8; 32] = [
            0x5d, 0xab, 0x08, 0x7e, 0x62, 0x4a, 0x8a, 0x4b, 0x79, 0xe1, 0x7f, 0x8b, 0x83, 0x80,
            0x0e, 0xe6, 0x6f, 0x3b, 0xb1, 0x29, 0x26, 0x18, 0xb6, 0xfd, 0x1c, 0x2f, 0x8b, 0x27,
            0xff, 0x88, 0xe0, 0xeb,
        ];
        let expected_shared: [u8; 32] = [
            0x4a, 0x5d, 0x9d, 0x5b, 0xa4, 0xce, 0x2d, 0xe1, 0x72, 0x8e, 0x3b, 0xf4, 0x80, 0x35,
            0x0f, 0x25, 0xe0, 0x7e, 0x21, 0xc9, 0x47, 0xd1, 0x9e, 0x33, 0x76, 0xf0, 0x9b, 0x3c,
            0x1e, 0x16, 0x17, 0x42,
        ];

        let alice_secret = X25519StaticSecret::from(alice_sk_bytes);
        let bob_secret = X25519StaticSecret::from(bob_sk_bytes);
        let alice_public = X25519PublicKey::from(&alice_secret);
        let bob_public = X25519PublicKey::from(&bob_secret);

        let shared_ab = x25519_diffie_hellman(&alice_secret, &bob_public);
        let shared_ba = x25519_diffie_hellman(&bob_secret, &alice_public);

        assert_eq!(shared_ab, shared_ba);
        assert_eq!(shared_ab, expected_shared);
    }
}
