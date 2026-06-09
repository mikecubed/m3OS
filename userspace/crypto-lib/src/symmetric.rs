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

// ── AES Key-Wrap / RFC 3394 (AES-128-KW) ────────────────────────────────────
//
// no software AES-CCM — CCMP is chipset-offloaded; the host only key-wraps/unwraps the GTK.
//
// Reuses the `aes` crate already in the dependency tree (for `Aes128`).
// The `alloc` feature is required because the output length is dynamic.

#[cfg(feature = "alloc")]
pub use kw::aes_key_unwrap;
#[cfg(feature = "alloc")]
pub use kw::aes_key_wrap;

#[cfg(feature = "alloc")]
mod kw {
    use aes::Aes128;
    use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};

    use crate::CryptoError;

    /// RFC 3394 §2.2.1 default IV.
    const IV: [u8; 8] = [0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6];

    /// Wrap `key` with the 128-bit key-encryption key `kek` (RFC 3394 §2.2.1).
    ///
    /// `key.len()` must be a multiple of 8 and at least 16; otherwise
    /// `Err(CryptoError::InvalidLength)` is returned. A malformed KDE (or a
    /// future caller bug) must not panic the whole driver/process, so this
    /// returns a `CryptoError` rather than asserting — mirroring
    /// [`aes_key_unwrap`] and the rest of `crypto-lib`.
    /// On success returns a `Vec<u8>` of length `key.len() + 8`.
    pub fn aes_key_wrap(kek: &[u8; 16], key: &[u8]) -> Result<alloc::vec::Vec<u8>, CryptoError> {
        if key.len() < 16 || !key.len().is_multiple_of(8) {
            return Err(CryptoError::InvalidLength);
        }

        let n = key.len() / 8; // number of 64-bit blocks
        let cipher = Aes128::new(GenericArray::from_slice(kek));

        // Initialise: A = IV, R[1..n] = key split into 8-byte blocks.
        let mut a = u64::from_be_bytes(IV);
        let mut r: alloc::vec::Vec<u64> = (0..n)
            .map(|i| u64::from_be_bytes(key[i * 8..(i + 1) * 8].try_into().unwrap()))
            .collect();

        // 6 wrapping rounds.
        for j in 0..6u64 {
            for (i, ri) in r.iter_mut().enumerate() {
                // B = AES(A || R[i])
                let mut block = [0u8; 16];
                block[..8].copy_from_slice(&a.to_be_bytes());
                block[8..].copy_from_slice(&ri.to_be_bytes());
                let mut ga = GenericArray::from(block);
                cipher.encrypt_block(&mut ga);

                // A = MSB(64, B) XOR t  where  t = n*j + (i+1)
                let t = (n as u64) * j + (i as u64 + 1);
                a = u64::from_be_bytes(ga[..8].try_into().unwrap()) ^ t;
                *ri = u64::from_be_bytes(ga[8..].try_into().unwrap());
            }
        }

        // Output: A || R[1] || … || R[n]
        let mut out = alloc::vec::Vec::with_capacity(8 + key.len());
        out.extend_from_slice(&a.to_be_bytes());
        for ri in r {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        Ok(out)
    }

    /// Unwrap a wrapped key produced by [`aes_key_wrap`] (RFC 3394 §2.2.2).
    ///
    /// Returns `Err(CryptoError::InvalidLength)` if `wrapped.len()` is not a
    /// multiple of 8, less than 24, or otherwise malformed.
    /// Returns `Err(CryptoError::AuthenticationFailed)` if the integrity check
    /// value does not match `A6A6A6A6A6A6A6A6`.
    pub fn aes_key_unwrap(
        kek: &[u8; 16],
        wrapped: &[u8],
    ) -> Result<alloc::vec::Vec<u8>, CryptoError> {
        if wrapped.len() < 24 || !wrapped.len().is_multiple_of(8) {
            return Err(CryptoError::InvalidLength);
        }

        let n = (wrapped.len() / 8) - 1; // number of plaintext 64-bit blocks
        let cipher = Aes128::new(GenericArray::from_slice(kek));

        // Initialise: A = wrapped[0], R[1..n] = rest.
        let mut a = u64::from_be_bytes(wrapped[..8].try_into().unwrap());
        let mut r: alloc::vec::Vec<u64> = (0..n)
            .map(|i| u64::from_be_bytes(wrapped[(i + 1) * 8..(i + 2) * 8].try_into().unwrap()))
            .collect();

        // 6 unwrapping rounds (reverse order).
        for j in (0..6u64).rev() {
            for (rev_i, ri) in r.iter_mut().rev().enumerate() {
                // Original forward index: i = n - 1 - rev_i
                let i = n - 1 - rev_i;
                let t = (n as u64) * j + (i as u64 + 1);
                // B = AES_inv((A XOR t) || R[i])
                let mut block = [0u8; 16];
                block[..8].copy_from_slice(&(a ^ t).to_be_bytes());
                block[8..].copy_from_slice(&ri.to_be_bytes());
                let mut ga = GenericArray::from(block);
                cipher.decrypt_block(&mut ga);

                a = u64::from_be_bytes(ga[..8].try_into().unwrap());
                *ri = u64::from_be_bytes(ga[8..].try_into().unwrap());
            }
        }

        // Verify the integrity check value.
        if a.to_be_bytes() != IV {
            return Err(CryptoError::AuthenticationFailed);
        }

        // Serialise output.
        let mut out = alloc::vec::Vec::with_capacity(n * 8);
        for ri in r {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(test)]
    use std::{println, vec};

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

    // ── AES Key-Wrap (RFC 3394) tests — require alloc feature ──────────────

    #[cfg(feature = "alloc")]
    #[test]
    fn aes_kw_rfc3394() {
        // RFC 3394 §4.1 — 128-bit KEK, 128-bit key data.
        // KEK  = 000102030405060708090A0B0C0D0E0F
        // PT   = 00112233445566778899AABBCCDDEEFF
        // CT   = 1FA68B0A8112B447AEF34BD8FB5A7B829D3E862371D2CFE5
        let kek: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F,
        ];
        let key_data: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ];
        let expected_wrapped: [u8; 24] = [
            0x1F, 0xA6, 0x8B, 0x0A, 0x81, 0x12, 0xB4, 0x47, 0xAE, 0xF3, 0x4B, 0xD8, 0xFB, 0x5A,
            0x7B, 0x82, 0x9D, 0x3E, 0x86, 0x23, 0x71, 0xD2, 0xCF, 0xE5,
        ];

        let wrapped = aes_key_wrap(&kek, &key_data).expect("16-byte key wraps");
        assert_eq!(
            wrapped.as_slice(),
            &expected_wrapped[..],
            "RFC 3394 §4.1 wrap"
        );

        let unwrapped = aes_key_unwrap(&kek, &wrapped).unwrap();
        assert_eq!(
            unwrapped.as_slice(),
            &key_data[..],
            "RFC 3394 §4.1 unwrap round-trip"
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn aes_kw_rejects_tampered() {
        let kek: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F,
        ];
        let key_data: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ];
        let mut wrapped = aes_key_wrap(&kek, &key_data).expect("16-byte key wraps");
        // Flip one byte of the wrapped blob.
        wrapped[5] ^= 0xFF;
        let result = aes_key_unwrap(&kek, &wrapped);
        assert_eq!(
            result,
            Err(CryptoError::AuthenticationFailed),
            "tampered blob must be rejected"
        );
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

    /// NIST SP 800-38A F.5.5: CTR-AES256.Encrypt — all four blocks.
    ///
    /// This is a stronger conformance test than the single-block variant above:
    /// it exercises the counter increment path (blocks 2-4) and is the primary
    /// AES-NI correctness anchor.  Hardware AES-NI and the fixsliced software
    /// backend must produce identical ciphertext for these published vectors.
    #[test]
    fn test_aes256_ctr_nist_sp800_38a_f55_all_blocks() {
        // NIST SP 800-38A §F.5.5 key and initial counter block.
        let key: [u8; 32] = [
            0x60, 0x3d, 0xeb, 0x10, 0x15, 0xca, 0x71, 0xbe, 0x2b, 0x73, 0xae, 0xf0, 0x85, 0x7d,
            0x77, 0x81, 0x1f, 0x35, 0x2c, 0x07, 0x3b, 0x61, 0x08, 0xd7, 0x2d, 0x98, 0x10, 0xa3,
            0x09, 0x14, 0xdf, 0xf4,
        ];
        let nonce: [u8; 16] = [
            0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
            0xfe, 0xff,
        ];
        // Four consecutive plaintext blocks (64 bytes total).
        let plaintext: [u8; 64] = [
            // Block 1
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a, // Block 2
            0xae, 0x2d, 0x8a, 0x57, 0x1e, 0x03, 0xac, 0x9c, 0x9e, 0xb7, 0x6f, 0xac, 0x45, 0xaf,
            0x8e, 0x51, // Block 3
            0x30, 0xc8, 0x1c, 0x46, 0xa3, 0x5c, 0xe4, 0x11, 0xe5, 0xfb, 0xc1, 0x19, 0x1a, 0x0a,
            0x52, 0xef, // Block 4
            0xf6, 0x9f, 0x24, 0x45, 0xdf, 0x4f, 0x9b, 0x17, 0xad, 0x2b, 0x41, 0x7b, 0xe6, 0x6c,
            0x37, 0x10,
        ];
        // Expected ciphertext from NIST SP 800-38A §F.5.6.
        let expected_ct: [u8; 64] = [
            // Block 1
            0x60, 0x1e, 0xc3, 0x13, 0x77, 0x57, 0x89, 0xa5, 0xb7, 0xa7, 0xf5, 0x04, 0xbb, 0xf3,
            0xd2, 0x28, // Block 2
            0xf4, 0x43, 0xe3, 0xca, 0x4d, 0x62, 0xb5, 0x9a, 0xca, 0x84, 0xe9, 0x90, 0xca, 0xca,
            0xf5, 0xc5, // Block 3
            0x2b, 0x09, 0x30, 0xda, 0xa2, 0x3d, 0xe9, 0x4c, 0xe8, 0x70, 0x17, 0xba, 0x2d, 0x84,
            0x98, 0x8d, // Block 4
            0xdf, 0xc9, 0xc5, 0x8d, 0xb6, 0x7a, 0xad, 0xa6, 0x13, 0xc2, 0xdd, 0x08, 0x45, 0x79,
            0x41, 0xa6,
        ];

        let mut ct = [0u8; 64];
        aes256_ctr_encrypt(&key, &nonce, &plaintext, &mut ct).unwrap();
        assert_eq!(
            ct, expected_ct,
            "CTR-AES256 4-block ciphertext mismatch (AES-NI vs expected NIST vector)"
        );

        // Decrypt must recover the original plaintext.
        let mut pt = [0u8; 64];
        aes256_ctr_decrypt(&key, &nonce, &ct, &mut pt).unwrap();
        assert_eq!(pt, plaintext, "CTR-AES256 decrypt round-trip failed");
    }

    // ── Host-side A/B microbenchmark (AES-CTR hardware vs. forced-soft) ────────
    //
    // Run the hardware benchmark:
    //   cargo test -p crypto-lib --target x86_64-unknown-linux-gnu \
    //     --features alloc --release -- bench_aes_ctr --nocapture
    //
    // Run the forced-software benchmark (aes_force_soft cfg forces fixsliced soft):
    //   RUSTFLAGS='--cfg aes_force_soft' \
    //   cargo test -p crypto-lib --target x86_64-unknown-linux-gnu \
    //     --features alloc --release -- bench_aes_ctr --nocapture
    //
    // The ratio (hardware / soft) must be ≥ 2× on any x86_64 host with AES-NI.
    // Under QEMU/TCG (no KVM) AES-NI is emulated and the ratio may be < 2×
    // — the host A/B run is the authoritative comparison.
    #[test]
    fn bench_aes_ctr() {
        use std::time::Instant;

        const PAYLOAD_BYTES: usize = 1024 * 1024; // 1 MiB per iteration
        const ITERATIONS: usize = 32; // 32 MiB total per run

        let key = [0x60u8; 32];
        let nonce = [0xf0u8; 16];

        // Allocate a reusable heap buffer so we don't measure allocation.
        let plaintext = vec![0x5au8; PAYLOAD_BYTES];
        let mut ct = vec![0u8; PAYLOAD_BYTES];

        // Warm-up: one full pass to prime caches and AES-NI cpufeatures detection.
        aes256_ctr_encrypt(&key, &nonce, &plaintext, &mut ct).unwrap();

        let start = Instant::now();
        for _ in 0..ITERATIONS {
            aes256_ctr_encrypt(&key, &nonce, &plaintext, &mut ct).unwrap();
        }
        let elapsed = start.elapsed();

        let total_mib = (PAYLOAD_BYTES * ITERATIONS) as f64 / (1024.0 * 1024.0);
        let mib_per_sec = total_mib / elapsed.as_secs_f64();

        #[cfg(aes_force_soft)]
        let backend = "soft";
        #[cfg(not(aes_force_soft))]
        let backend = "hw";

        println!(
            "BENCH:aes-ctr-{backend}: {:.1} MiB/s  ({total_mib:.0} MiB in {:.3}s)",
            mib_per_sec,
            elapsed.as_secs_f64()
        );

        // Sanity: output must differ from input (cipher actually ran).
        assert_ne!(&ct[..32], &plaintext[..32], "AES-CTR produced no-op output");
    }

    /// ChaCha20-Poly1305 throughput reference (no A/B — ChaCha is always
    /// software; printed alongside AES-CTR for comparison).
    #[test]
    fn bench_chacha20poly1305() {
        use std::time::Instant;

        const PAYLOAD_BYTES: usize = 1024 * 1024; // 1 MiB per iteration
        const ITERATIONS: usize = 32;

        let key = [0x42u8; 32];
        let nonce = [0x01u8; 12];
        let aad = b"";

        let plaintext = vec![0x5au8; PAYLOAD_BYTES];
        let mut ct = vec![0u8; PAYLOAD_BYTES + 16];

        // Warm-up.
        chacha20poly1305_seal(&key, &nonce, &plaintext, aad, &mut ct).unwrap();

        let start = Instant::now();
        for _ in 0..ITERATIONS {
            chacha20poly1305_seal(&key, &nonce, &plaintext, aad, &mut ct).unwrap();
        }
        let elapsed = start.elapsed();

        let total_mib = (PAYLOAD_BYTES * ITERATIONS) as f64 / (1024.0 * 1024.0);
        let mib_per_sec = total_mib / elapsed.as_secs_f64();

        println!(
            "BENCH:chacha20poly1305: {:.1} MiB/s  ({total_mib:.0} MiB in {:.3}s)",
            mib_per_sec,
            elapsed.as_secs_f64()
        );

        // Sanity: output is valid ciphertext+tag (decrypt succeeds).
        let ct_len = PAYLOAD_BYTES + 16;
        let mut pt_out = vec![0u8; PAYLOAD_BYTES];
        chacha20poly1305_open(&key, &nonce, &ct[..ct_len], aad, &mut pt_out).unwrap();
        assert_eq!(
            pt_out, plaintext,
            "ChaCha20-Poly1305 bench round-trip failed"
        );
    }
}
