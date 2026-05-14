use passwd::{
    HASH_FORMAT_PREFIX, HASH_ROUNDS, ShadowRewriteError, find_username_by_uid, requested_username,
    rewrite_shadow_file,
};

#[test]
fn hash_format_prefix_embeds_hash_rounds() {
    // HASH_FORMAT_PREFIX is `$sha256i$<rounds>$`. Parse the middle field
    // and assert it matches HASH_ROUNDS, so the prefix and the in-code
    // iteration count can never drift apart silently.
    let prefix = core::str::from_utf8(HASH_FORMAT_PREFIX).expect("prefix is ASCII");
    let mut parts = prefix.split('$');
    assert_eq!(parts.next(), Some(""), "prefix should start with `$`");
    assert_eq!(
        parts.next(),
        Some("sha256i"),
        "algorithm tag must be sha256i"
    );
    let rounds_str = parts.next().expect("rounds field present");
    assert_eq!(
        parts.next(),
        Some(""),
        "prefix should end with trailing `$`"
    );
    let embedded: u32 = rounds_str
        .parse()
        .expect("rounds field is a decimal integer");
    assert_eq!(
        embedded, HASH_ROUNDS,
        "HASH_FORMAT_PREFIX rounds field ({embedded}) must match HASH_ROUNDS ({HASH_ROUNDS})"
    );
}

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

#[test]
fn find_username_by_uid_skips_overflowed_uid_fields() {
    let passwd = b"evil:x:4294967296:0:evil:/root:/bin/sh\nroot:x:0:0:root:/root:/bin/sh\n";

    assert_eq!(find_username_by_uid(passwd, 0), Some("root".as_bytes()));
}
