use passwd::{ShadowRewriteError, requested_username, rewrite_shadow_file};

#[test]
fn requested_username_uses_cli_target_when_present() {
    assert_eq!(
        requested_username(&["passwd", "user"]),
        Some("user".as_bytes())
    );
    assert_eq!(requested_username(&["passwd"]), None);
}

#[test]
fn rewrite_shadow_file_updates_only_requested_user() {
    let shadow = b"root:$sha256i$10000$oldsalt$oldroot::::::\nuser:$sha256i$10000$oldsalt$olduser:17000:0:99999:7:::\n";
    let mut updated = [0u8; 256];
    let len = rewrite_shadow_file(
        shadow,
        b"user",
        b"$sha256i$10000$newsalt$newhash",
        &mut updated,
    )
    .unwrap();

    let updated = &updated[..len];
    assert_eq!(
        updated,
        b"root:$sha256i$10000$oldsalt$oldroot::::::\nuser:$sha256i$10000$newsalt$newhash:17000:0:99999:7:::\n"
    );
}

#[test]
fn rewrite_shadow_file_errors_for_missing_user() {
    let shadow = b"root:$sha256i$10000$oldsalt$oldroot::::::\n";
    let mut updated = [0u8; 128];
    assert_eq!(
        rewrite_shadow_file(
            shadow,
            b"user",
            b"$sha256i$10000$newsalt$newhash",
            &mut updated,
        ),
        Err(ShadowRewriteError::UserNotFound)
    );
}
