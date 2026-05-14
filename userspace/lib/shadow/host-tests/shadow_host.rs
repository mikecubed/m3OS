//! Integration tests for the atomic shadow-file write helper.
//!
//! These exercise the same `shadow_write_atomic_with` entry point as the
//! inline `#[cfg(test)]` cases in `src/lib.rs`, but operate as a separate
//! test target so they run under `cargo test -p shadow
//! --target x86_64-unknown-linux-gnu --features host-tests --test
//! shadow_host`.

use shadow::test_support::{MockFs, Op};
use shadow::{ShadowError, shadow_write_atomic_with};

fn nul(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

#[test]
fn happy_path_rewrites_shadow_atomically() {
    let mut fs = MockFs::new().with_file(b"/etc/shadow", b"old:hash\n");
    let result = shadow_write_atomic_with(&mut fs, "/etc/shadow", b"new:hash\n");
    assert_eq!(result, Ok(()));
    assert_eq!(
        fs.files.get(&b"/etc/shadow".to_vec()),
        Some(&b"new:hash\n".to_vec())
    );
    assert!(fs.files.get(&b"/etc/shadow.new".to_vec()).is_none());
}

#[test]
fn write_error_leaves_original_untouched() {
    let mut fs = MockFs::new().with_file(b"/etc/shadow", b"old:hash\n");
    fs.fail_write = Some(-5);
    let err = shadow_write_atomic_with(&mut fs, "/etc/shadow", b"new:hash\n");
    assert_eq!(err, Err(ShadowError::WriteFailed(-5)));
    assert_eq!(
        fs.files.get(&b"/etc/shadow".to_vec()),
        Some(&b"old:hash\n".to_vec())
    );
    assert!(
        fs.ops
            .iter()
            .any(|op| matches!(op, Op::Unlink(p) if p == &nul("/etc/shadow.new")))
    );
}
