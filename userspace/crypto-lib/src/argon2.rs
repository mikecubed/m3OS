//! Argon2id (RFC 9106) re-export (Phase 110 Track C).
//!
//! The implementation lives in [`syscall_lib::argon2`] — not here — because
//! `crypto-lib` depends on `syscall-lib` (for `getrandom`), so the password
//! path's `verify_password` (in `syscall-lib`) cannot call *up* into
//! `crypto-lib` without a dependency cycle. Placing the hash in `syscall-lib`
//! keeps that dispatch cycle-free; `crypto-lib` re-exports the public surface
//! so callers already depending on `crypto-lib` get the charter's
//! `crypto_lib::argon2::*` API, and the RFC 9106 conformance vector runs here
//! (this crate is in the `cargo xtask check` host-test set).
//!
//! Requires the `alloc` feature (argon2id needs a heap memory matrix).

pub use syscall_lib::argon2::{
    DEFAULT_PARAMS, Params, SHADOW_PREFIX, argon2id_hash, argon2id_raw, argon2id_verify,
    build_shadow_field, verify_shadow_field,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 9106 §5.3 Argon2id reference test vector — the authoritative
    /// conformance check, run in the merge gate via `crypto-lib`'s host tests.
    #[test]
    fn argon2id_rfc9106_vector() {
        let password = [0x01u8; 32];
        let salt = [0x02u8; 16];
        let secret = [0x03u8; 8];
        let ad = [0x04u8; 12];
        let params = Params {
            m_kib: 32,
            t: 3,
            p: 4,
            tag_len: 32,
        };
        let mut out = [0u8; 32];
        assert!(argon2id_raw(
            &password, &salt, &secret, &ad, &params, &mut out
        ));
        let expected: [u8; 32] = [
            0x0d, 0x64, 0x0d, 0xf5, 0x8d, 0x78, 0x76, 0x6c, 0x08, 0xc0, 0x37, 0xa3, 0x4a, 0x8b,
            0x53, 0xc9, 0xd0, 0x1e, 0xf0, 0x45, 0x2d, 0x75, 0xb6, 0x5e, 0xb5, 0x25, 0x20, 0xe9,
            0x6b, 0x01, 0xe6, 0x59,
        ];
        assert_eq!(out, expected);
    }

    /// The password-hashing convenience path (`DEFAULT_PARAMS`, empty
    /// secret/AD) round-trips and rejects a wrong password.
    #[test]
    fn default_params_hash_verify_roundtrip() {
        // Reduced memory keeps the host test brisk; the vector above pins the
        // algorithm and `DEFAULT_PARAMS` is exercised end-to-end by the gate.
        let params = Params {
            m_kib: 256,
            t: 2,
            p: 1,
            tag_len: 32,
        };
        let salt = *b"0123456789abcdef";
        let mut tag = [0u8; 32];
        assert!(argon2id_hash(
            b"s3cr3t-passphrase",
            &salt,
            &params,
            &mut tag
        ));
        assert!(argon2id_verify(b"s3cr3t-passphrase", &salt, &params, &tag));
        assert!(!argon2id_verify(b"s3cr3t-passphras3", &salt, &params, &tag));
    }

    /// The full `$argon2id$…` shadow field builds and verifies through the
    /// re-exported format helpers.
    #[test]
    fn shadow_field_via_reexport() {
        let params = Params {
            m_kib: 256,
            t: 2,
            p: 1,
            tag_len: 32,
        };
        let salt = *b"fedcba9876543210";
        let mut buf = [0u8; 200];
        let n = build_shadow_field(b"login-pw", &salt, &params, &mut buf).unwrap();
        assert!(buf[..n].starts_with(SHADOW_PREFIX));
        assert!(verify_shadow_field(b"login-pw", &buf[..n]));
        assert!(!verify_shadow_field(b"login-pX", &buf[..n]));
        let _ = DEFAULT_PARAMS;
    }
}
