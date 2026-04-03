//! Symmetric encryption: ChaCha20-Poly1305 (AEAD) and AES-256-CTR.

use crate::CryptoError;

/// Encrypt with ChaCha20-Poly1305 (AEAD).
///
/// `output` must be at least `plaintext.len() + 16` bytes (ciphertext + 16-byte auth tag).
/// Returns the number of bytes written to `output` (plaintext.len() + 16).
pub fn chacha20poly1305_seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
    aad: &[u8],
    output: &mut [u8],
) -> Result<usize, CryptoError> {
    use chacha20poly1305::aead::AeadInPlace;
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit};

    let needed = plaintext.len() + 16;
    if output.len() < needed {
        return Err(CryptoError::InvalidLength);
    }

    // Copy plaintext into output buffer, encrypt in place.
    output[..plaintext.len()].copy_from_slice(plaintext);
    let cipher = ChaCha20Poly1305::new(key.into());
    let tag = cipher
        .encrypt_in_place_detached(nonce.into(), aad, &mut output[..plaintext.len()])
        .map_err(|_| CryptoError::EncryptionFailed)?;
    output[plaintext.len()..needed].copy_from_slice(&tag);
    Ok(needed)
}

/// Decrypt with ChaCha20-Poly1305 (AEAD).
///
/// `ciphertext` includes the 16-byte auth tag at the end.
/// `output` must be at least `ciphertext.len() - 16` bytes.
/// Returns the number of plaintext bytes written.
pub fn chacha20poly1305_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    aad: &[u8],
    output: &mut [u8],
) -> Result<usize, CryptoError> {
    use chacha20poly1305::aead::AeadInPlace;
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit};

    if ciphertext.len() < 16 {
        return Err(CryptoError::InvalidLength);
    }
    let pt_len = ciphertext.len() - 16;
    if output.len() < pt_len {
        return Err(CryptoError::InvalidLength);
    }

    // Split ciphertext and tag.
    let (ct, tag_bytes) = ciphertext.split_at(pt_len);
    let tag = chacha20poly1305::Tag::from_slice(tag_bytes);

    // Copy ciphertext to output, decrypt in place.
    output[..pt_len].copy_from_slice(ct);
    let cipher = ChaCha20Poly1305::new(key.into());
    cipher
        .decrypt_in_place_detached(nonce.into(), aad, &mut output[..pt_len], tag)
        .map_err(|_| CryptoError::AuthenticationFailed)?;
    Ok(pt_len)
}

/// Encrypt with AES-256-CTR.
///
/// `output` must be at least `plaintext.len()` bytes.
pub fn aes256_ctr_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 16],
    plaintext: &[u8],
    output: &mut [u8],
) -> Result<(), CryptoError> {
    use aes::Aes256;
    use ctr::cipher::{KeyIvInit, StreamCipher};
    type Aes256Ctr = ctr::Ctr128BE<Aes256>;

    if output.len() < plaintext.len() {
        return Err(CryptoError::InvalidLength);
    }

    output[..plaintext.len()].copy_from_slice(plaintext);
    let mut cipher = Aes256Ctr::new(key.into(), nonce.into());
    cipher.apply_keystream(&mut output[..plaintext.len()]);
    Ok(())
}

/// Decrypt with AES-256-CTR (same operation as encrypt — XOR with keystream).
pub fn aes256_ctr_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 16],
    ciphertext: &[u8],
    output: &mut [u8],
) -> Result<(), CryptoError> {
    aes256_ctr_encrypt(key, nonce, ciphertext, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chacha20poly1305_roundtrip() {
        let key = [0x42u8; 32];
        let nonce = [0x01u8; 12];
        let plaintext = b"Hello, m3OS crypto!";
        let aad = b"";

        let mut ct = [0u8; 128];
        let ct_len = chacha20poly1305_seal(&key, &nonce, plaintext, aad, &mut ct).unwrap();
        assert_eq!(ct_len, plaintext.len() + 16);

        let mut pt = [0u8; 128];
        let pt_len = chacha20poly1305_open(&key, &nonce, &ct[..ct_len], aad, &mut pt).unwrap();
        assert_eq!(pt_len, plaintext.len());
        assert_eq!(&pt[..pt_len], plaintext);
    }

    #[test]
    fn test_chacha20poly1305_tampered() {
        let key = [0x42u8; 32];
        let nonce = [0x01u8; 12];
        let plaintext = b"Hello";
        let aad = b"";

        let mut ct = [0u8; 64];
        let ct_len = chacha20poly1305_seal(&key, &nonce, plaintext, aad, &mut ct).unwrap();

        // Tamper with ciphertext.
        ct[0] ^= 0xff;
        let mut pt = [0u8; 64];
        let result = chacha20poly1305_open(&key, &nonce, &ct[..ct_len], aad, &mut pt);
        assert_eq!(result, Err(CryptoError::AuthenticationFailed));
    }

    #[test]
    fn test_chacha20poly1305_rfc8439() {
        // RFC 8439 Section 2.8.2 test vector
        let key: [u8; 32] = [
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d,
            0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b,
            0x9c, 0x9d, 0x9e, 0x9f,
        ];
        let nonce: [u8; 12] = [
            0x07, 0x00, 0x00, 0x00, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
        ];
        let aad: [u8; 12] = [
            0x50, 0x51, 0x52, 0x53, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7,
        ];
        let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";

        let expected_ct: &[u8] = &[
            0xd3, 0x1a, 0x8d, 0x34, 0x64, 0x8e, 0x60, 0xdb, 0x7b, 0x86, 0xaf, 0xbc, 0x53, 0xef,
            0x7e, 0xc2, 0xa4, 0xad, 0xed, 0x51, 0x29, 0x6e, 0x08, 0xfe, 0xa9, 0xe2, 0xb5, 0xa7,
            0x36, 0xee, 0x62, 0xd6, 0x3d, 0xbe, 0xa4, 0x5e, 0x8c, 0xa9, 0x67, 0x12, 0x82, 0xfa,
            0xfb, 0x69, 0xda, 0x92, 0x72, 0x8b, 0x1a, 0x71, 0xde, 0x0a, 0x9e, 0x06, 0x0b, 0x29,
            0x05, 0xd6, 0xa5, 0xb6, 0x7e, 0xcd, 0x3b, 0x36, 0x92, 0xdd, 0xbd, 0x7f, 0x2d, 0x77,
            0x8b, 0x8c, 0x98, 0x03, 0xae, 0xe3, 0x28, 0x09, 0x1b, 0x58, 0xfa, 0xb3, 0x24, 0xe4,
            0xfa, 0xd6, 0x75, 0x94, 0x55, 0x85, 0x80, 0x8b, 0x48, 0x31, 0xd7, 0xbc, 0x3f, 0xf4,
            0xde, 0xf0, 0x8e, 0x4b, 0x7a, 0x9d, 0xe5, 0x76, 0xd2, 0x65, 0x86, 0xce, 0xc6, 0x4b,
            0x61, 0x16,
        ];
        let expected_tag: &[u8] = &[
            0x1a, 0xe1, 0x0b, 0x59, 0x4f, 0x09, 0xe2, 0x6a, 0x7e, 0x90, 0x2e, 0xcb, 0xd0, 0x60,
            0x06, 0x91,
        ];

        let mut output = [0u8; 256];
        let ct_len = chacha20poly1305_seal(&key, &nonce, plaintext, &aad, &mut output).unwrap();

        assert_eq!(&output[..plaintext.len()], expected_ct);
        assert_eq!(&output[plaintext.len()..ct_len], expected_tag);

        // Decrypt round-trip
        let mut pt_out = [0u8; 256];
        let pt_len =
            chacha20poly1305_open(&key, &nonce, &output[..ct_len], &aad, &mut pt_out).unwrap();
        assert_eq!(&pt_out[..pt_len], plaintext);
    }

    #[test]
    fn test_aes256_ctr_roundtrip() {
        let key = [0x42u8; 32];
        let nonce = [0x01u8; 16];
        let plaintext = b"AES-256-CTR test data";

        let mut ct = [0u8; 64];
        aes256_ctr_encrypt(&key, &nonce, plaintext, &mut ct).unwrap();
        // Ciphertext should differ from plaintext.
        assert_ne!(&ct[..plaintext.len()], plaintext);

        let mut pt = [0u8; 64];
        aes256_ctr_decrypt(&key, &nonce, &ct[..plaintext.len()], &mut pt).unwrap();
        assert_eq!(&pt[..plaintext.len()], plaintext);
    }

    #[test]
    fn test_aes256_ctr_nist_sp800_38a_f55() {
        // NIST SP 800-38A F.5.5: CTR-AES256.Encrypt (first block)
        let key: [u8; 32] = [
            0x60, 0x3d, 0xeb, 0x10, 0x15, 0xca, 0x71, 0xbe, 0x2b, 0x73, 0xae, 0xf0, 0x85, 0x7d,
            0x77, 0x81, 0x1f, 0x35, 0x2c, 0x07, 0x3b, 0x61, 0x08, 0xd7, 0x2d, 0x98, 0x10, 0xa3,
            0x09, 0x14, 0xdf, 0xf4,
        ];
        // Initial counter block
        let nonce: [u8; 16] = [
            0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
            0xfe, 0xff,
        ];
        // Block 1 plaintext
        let plaintext: [u8; 16] = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a,
        ];
        // Block 1 expected ciphertext
        let expected_ct: [u8; 16] = [
            0x60, 0x1e, 0xc3, 0x13, 0x77, 0x57, 0x89, 0xa5, 0xb7, 0xa7, 0xf5, 0x04, 0xbb, 0xf3,
            0xd2, 0x28,
        ];

        let mut ct = [0u8; 16];
        aes256_ctr_encrypt(&key, &nonce, &plaintext, &mut ct).unwrap();
        assert_eq!(ct, expected_ct);

        // Verify decrypt round-trips.
        let mut pt = [0u8; 16];
        aes256_ctr_decrypt(&key, &nonce, &ct, &mut pt).unwrap();
        assert_eq!(pt, plaintext);
    }
}
