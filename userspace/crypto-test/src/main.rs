//! Crypto integration test — exercises all crypto-lib primitives inside m3OS.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write(1, b"crypto-test: out of memory\n");
    syscall_lib::exit(1)
}

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    let mut failures = 0;
    failures += test_sha256();
    failures += test_hmac();
    failures += test_hkdf();
    failures += test_chacha20poly1305();
    failures += test_aes256_ctr();
    failures += test_ed25519();
    failures += test_x25519();
    failures += test_csprng();

    if failures == 0 {
        syscall_lib::write(1, b"crypto-test: all tests PASSED\n");
        0
    } else {
        syscall_lib::write(1, b"crypto-test: some tests FAILED\n");
        1
    }
}

fn test_sha256() -> i32 {
    use crypto_lib::hash::sha256;

    let hash = sha256(b"");
    let expected: [u8; 32] = [
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
        0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
        0xb8, 0x55,
    ];
    if hash != expected {
        syscall_lib::write(1, b"  FAIL: sha256 empty\n");
        return 1;
    }

    let hash = sha256(b"abc");
    let expected: [u8; 32] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];
    if hash != expected {
        syscall_lib::write(1, b"  FAIL: sha256 abc\n");
        return 1;
    }

    // Incremental test.
    let mut hasher = crypto_lib::hash::Sha256Hasher::new();
    hasher.update(b"ab");
    hasher.update(b"c");
    if hasher.finalize() != sha256(b"abc") {
        syscall_lib::write(1, b"  FAIL: sha256 incremental\n");
        return 1;
    }

    syscall_lib::write(1, b"  PASS: SHA-256\n");
    0
}

fn test_hmac() -> i32 {
    use crypto_lib::hash::hmac_sha256;

    let key = [0x0bu8; 20];
    let expected: [u8; 32] = [
        0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1,
        0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32,
        0xcf, 0xf7,
    ];
    if hmac_sha256(&key, b"Hi There") != expected {
        syscall_lib::write(1, b"  FAIL: HMAC-SHA-256\n");
        return 1;
    }

    syscall_lib::write(1, b"  PASS: HMAC-SHA-256\n");
    0
}

fn test_hkdf() -> i32 {
    use crypto_lib::hash::{hkdf_expand, hkdf_extract};

    let ikm = [0x0bu8; 22];
    let salt: [u8; 13] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
    ];
    let info: [u8; 10] = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];

    let prk = hkdf_extract(&salt, &ikm);
    let expected_prk: [u8; 32] = [
        0x07, 0x77, 0x09, 0x36, 0x2c, 0x2e, 0x32, 0xdf, 0x0d, 0xdc, 0x3f, 0x0d, 0xc4, 0x7b, 0xba,
        0x63, 0x90, 0xb6, 0xc7, 0x3b, 0xb5, 0x0f, 0x9c, 0x31, 0x22, 0xec, 0x84, 0x4a, 0xd7, 0xc2,
        0xb3, 0xe5,
    ];
    if prk != expected_prk {
        syscall_lib::write(1, b"  FAIL: HKDF extract\n");
        return 1;
    }

    let mut okm = [0u8; 42];
    if hkdf_expand(&prk, &info, &mut okm).is_err() {
        syscall_lib::write(1, b"  FAIL: HKDF expand\n");
        return 1;
    }
    let expected_okm: [u8; 42] = [
        0x3c, 0xb2, 0x5f, 0x25, 0xfa, 0xac, 0xd5, 0x7a, 0x90, 0x43, 0x4f, 0x64, 0xd0, 0x36, 0x2f,
        0x2a, 0x2d, 0x2d, 0x0a, 0x90, 0xcf, 0x1a, 0x5a, 0x4c, 0x5d, 0xb0, 0x2d, 0x56, 0xec, 0xc4,
        0xc5, 0xbf, 0x34, 0x00, 0x72, 0x08, 0xd5, 0xb8, 0x87, 0x18, 0x58, 0x65,
    ];
    if okm != expected_okm {
        syscall_lib::write(1, b"  FAIL: HKDF expand values\n");
        return 1;
    }

    syscall_lib::write(1, b"  PASS: HKDF\n");
    0
}

fn test_chacha20poly1305() -> i32 {
    use crypto_lib::symmetric::{chacha20poly1305_open, chacha20poly1305_seal};

    let key = [0x42u8; 32];
    let nonce = [0x01u8; 12];
    let plaintext = b"Hello, m3OS crypto!";
    let aad = b"";

    let mut ct = [0u8; 128];
    let ct_len = match chacha20poly1305_seal(&key, &nonce, plaintext, aad, &mut ct) {
        Ok(n) => n,
        Err(_) => {
            syscall_lib::write(1, b"  FAIL: ChaCha20-Poly1305 seal\n");
            return 1;
        }
    };

    let mut pt = [0u8; 128];
    let pt_len = match chacha20poly1305_open(&key, &nonce, &ct[..ct_len], aad, &mut pt) {
        Ok(n) => n,
        Err(_) => {
            syscall_lib::write(1, b"  FAIL: ChaCha20-Poly1305 open\n");
            return 1;
        }
    };

    if &pt[..pt_len] != plaintext {
        syscall_lib::write(1, b"  FAIL: ChaCha20-Poly1305 roundtrip\n");
        return 1;
    }

    // Test tampered ciphertext.
    ct[0] ^= 0xff;
    if chacha20poly1305_open(&key, &nonce, &ct[..ct_len], aad, &mut pt).is_ok() {
        syscall_lib::write(1, b"  FAIL: ChaCha20-Poly1305 should reject tampered\n");
        return 1;
    }

    syscall_lib::write(1, b"  PASS: ChaCha20-Poly1305\n");
    0
}

fn test_aes256_ctr() -> i32 {
    use crypto_lib::symmetric::{aes256_ctr_decrypt, aes256_ctr_encrypt};

    let key = [0x42u8; 32];
    let nonce = [0x01u8; 16];
    let plaintext = b"AES-256-CTR test";

    let mut ct = [0u8; 64];
    if aes256_ctr_encrypt(&key, &nonce, plaintext, &mut ct).is_err() {
        syscall_lib::write(1, b"  FAIL: AES-256-CTR encrypt\n");
        return 1;
    }

    let mut pt = [0u8; 64];
    if aes256_ctr_decrypt(&key, &nonce, &ct[..plaintext.len()], &mut pt).is_err() {
        syscall_lib::write(1, b"  FAIL: AES-256-CTR decrypt\n");
        return 1;
    }

    if &pt[..plaintext.len()] != plaintext {
        syscall_lib::write(1, b"  FAIL: AES-256-CTR roundtrip\n");
        return 1;
    }

    syscall_lib::write(1, b"  PASS: AES-256-CTR\n");
    0
}

fn test_ed25519() -> i32 {
    use crypto_lib::asymmetric::*;

    let mut rng = match crypto_lib::random::csprng_init() {
        Ok(r) => r,
        Err(_) => {
            syscall_lib::write(1, b"  FAIL: CSPRNG init for Ed25519\n");
            return 1;
        }
    };

    let (sk, vk) = ed25519_keygen(&mut rng);
    let message = b"ed25519 test message";
    let sig = ed25519_sign(&sk, message);

    if !ed25519_verify(&vk, message, &sig) {
        syscall_lib::write(1, b"  FAIL: Ed25519 sign/verify\n");
        return 1;
    }

    if ed25519_verify(&vk, b"wrong", &sig) {
        syscall_lib::write(1, b"  FAIL: Ed25519 should reject wrong message\n");
        return 1;
    }

    // Key roundtrip.
    let sk_bytes = ed25519_signing_key_to_bytes(&sk);
    let vk_bytes = ed25519_verifying_key_to_bytes(&vk);
    let sk2 = ed25519_signing_key_from_bytes(&sk_bytes);
    let vk2 = match ed25519_verifying_key_from_bytes(&vk_bytes) {
        Ok(k) => k,
        Err(_) => {
            syscall_lib::write(1, b"  FAIL: Ed25519 key deserialize\n");
            return 1;
        }
    };

    let sig2 = ed25519_sign(&sk2, message);
    if !ed25519_verify(&vk2, message, &sig2) {
        syscall_lib::write(1, b"  FAIL: Ed25519 key roundtrip\n");
        return 1;
    }

    syscall_lib::write(1, b"  PASS: Ed25519\n");
    0
}

fn test_x25519() -> i32 {
    use crypto_lib::asymmetric::*;

    let mut rng = match crypto_lib::random::csprng_init() {
        Ok(r) => r,
        Err(_) => {
            syscall_lib::write(1, b"  FAIL: CSPRNG init for X25519\n");
            return 1;
        }
    };

    let (alice_secret, alice_public) = x25519_keygen(&mut rng);
    let (bob_secret, bob_public) = x25519_keygen(&mut rng);

    let shared_a = x25519_diffie_hellman(&alice_secret, &bob_public);
    let shared_b = x25519_diffie_hellman(&bob_secret, &alice_public);

    if shared_a != shared_b {
        syscall_lib::write(1, b"  FAIL: X25519 mutual DH\n");
        return 1;
    }

    syscall_lib::write(1, b"  PASS: X25519\n");
    0
}

fn test_csprng() -> i32 {
    use crypto_lib::random::{csprng_fill, csprng_init};

    let mut rng = match csprng_init() {
        Ok(r) => r,
        Err(_) => {
            syscall_lib::write(1, b"  FAIL: CSPRNG init\n");
            return 1;
        }
    };

    let mut buf1 = [0u8; 32];
    let mut buf2 = [0u8; 32];
    csprng_fill(&mut rng, &mut buf1);
    csprng_fill(&mut rng, &mut buf2);

    if buf1 == buf2 {
        syscall_lib::write(1, b"  FAIL: CSPRNG produced identical blocks\n");
        return 1;
    }

    // Also test zero-length fill (should not panic).
    let mut empty = [0u8; 0];
    csprng_fill(&mut rng, &mut empty);

    syscall_lib::write(1, b"  PASS: CSPRNG\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write(1, b"crypto-test: PANIC\n");
    syscall_lib::exit(101)
}
