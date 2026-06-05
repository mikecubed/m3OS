//! Phase 85a Track C — offline `.m3pkg` installer library.
//!
//! This library crate contains the pure logic for the `pkg` userspace tool:
//!
//! - **`db`**: `/var/lib/pkg/db` reader/writer (line-based text format, C.2).
//! - **`install`**: install-path mapping and parent-directory component splitting
//!   (pure logic, host-testable).
//!
//! The `[[bin]]` (`src/main.rs`) is thin — it delegates all logic here.  The
//! lib is intentionally `no_std`-compatible (but also compiles under `std` for
//! host tests) so the same code runs both on the host (test) and in the OS.
//!
//! ## Forward-compatible DB format
//!
//! ```text
//! # m3pkg-db v1
//! [pkg]
//! name=<name>
//! version=<version or empty>
//! key=<hex SHA-256 of the artifact bytes>
//! file=<install-path> <hash-hex>
//! file=<install-path> <hash-hex>
//! ...
//! [end]
//! ```
//!
//! Unknown `key=` lines are ignored on read (forward-compatible).
//! Re-installing the same package replaces the existing `[pkg]` block
//! (idempotent).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// DB format
// ---------------------------------------------------------------------------

/// DB file header line.
pub const DB_HEADER: &str = "# m3pkg-db v1";

/// A single installed-file record within a package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRecord {
    /// Absolute install path (e.g. `/usr/bin/foo`).
    pub path: String,
    /// Lowercase hex SHA-256 of the installed content.
    pub hash_hex: String,
}

/// A single package record in the DB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkgRecord {
    /// Package name (from the `pkg install <name>` CLI arg).
    pub name: String,
    /// Version string (from `/usr/pkg/<name>.meta` `VERSION=` line, or empty).
    pub version: String,
    /// Content key = `to_hex(content_hash(<artifact bytes>))`.
    pub key: String,
    /// Installed files and their SHA-256 hashes.
    pub files: Vec<FileRecord>,
}

// ---------------------------------------------------------------------------
// Serialisation
// ---------------------------------------------------------------------------

/// Serialise the full DB to a `String` (allocating).
///
/// Format:
/// ```text
/// # m3pkg-db v1
/// [pkg]
/// name=<…>
/// version=<…>
/// key=<…>
/// file=<path> <hash>
/// …
/// [end]
/// ```
///
/// Multiple `PkgRecord`s are concatenated (one `[pkg]…[end]` block each).
pub fn db_serialize(records: &[PkgRecord]) -> String {
    let mut out = String::new();
    out.push_str(DB_HEADER);
    out.push('\n');
    for rec in records {
        out.push_str("[pkg]\n");
        out.push_str("name=");
        out.push_str(&rec.name);
        out.push('\n');
        out.push_str("version=");
        out.push_str(&rec.version);
        out.push('\n');
        out.push_str("key=");
        out.push_str(&rec.key);
        out.push('\n');
        for f in &rec.files {
            out.push_str("file=");
            out.push_str(&f.path);
            out.push(' ');
            out.push_str(&f.hash_hex);
            out.push('\n');
        }
        out.push_str("[end]\n");
    }
    out
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse the DB text.  Unknown keys inside a `[pkg]` block are ignored for
/// forward-compatibility.  Returns `Err` only on a structurally invalid file
/// (e.g. a `[pkg]` block with no matching `[end]`).
pub fn db_parse(text: &str) -> Result<Vec<PkgRecord>, &'static str> {
    let mut records = Vec::new();
    let mut lines = text.lines().peekable();

    // Skip header + blank lines before the first block.
    while let Some(&line) = lines.peek() {
        if line.starts_with('#') || line.trim().is_empty() {
            lines.next();
        } else {
            break;
        }
    }

    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line != "[pkg]" {
            return Err("expected [pkg] block");
        }
        // Parse until [end]
        let mut name = String::new();
        let mut version = String::new();
        let mut key = String::new();
        let mut files = Vec::new();
        let mut ended = false;

        for inner in lines.by_ref() {
            let inner = inner.trim();
            if inner == "[end]" {
                ended = true;
                break;
            }
            if let Some(v) = inner.strip_prefix("name=") {
                name = v.into();
            } else if let Some(v) = inner.strip_prefix("version=") {
                version = v.into();
            } else if let Some(v) = inner.strip_prefix("key=") {
                key = v.into();
            } else if let Some(v) = inner.strip_prefix("file=") {
                // Format: `<path> <hash>`. A `file=` record with no
                // space-separated hash is malformed. The DB is written by the
                // installer itself, so this can only mean corruption/tampering —
                // reject it (fatal, like every other DB parse error) rather than
                // silently dropping it. A dropped record would shrink the `files`
                // list, making `pkg verify`/`remove`/`upgrade` ignore that path
                // (orphaned files, missing integrity checks) while still treating
                // the DB as successfully parsed. `rfind` splits on the *last*
                // space, so paths containing spaces are preserved.
                let Some(sp) = v.rfind(' ') else {
                    return Err("malformed file= record (missing hash)");
                };
                files.push(FileRecord {
                    path: v[..sp].into(),
                    hash_hex: v[sp + 1..].into(),
                });
            }
            // Unknown keys are silently ignored (forward-compat).
        }

        if !ended {
            return Err("unterminated [pkg] block");
        }
        if name.is_empty() {
            return Err("pkg block missing name=");
        }

        records.push(PkgRecord {
            name,
            version,
            key,
            files,
        });
    }

    Ok(records)
}

// ---------------------------------------------------------------------------
// Idempotent upsert
// ---------------------------------------------------------------------------

/// Insert or replace the record for `rec.name` in `records`.
///
/// If a record with the same name exists, it is replaced in-place.  Otherwise
/// the new record is appended.  This makes re-install idempotent — no duplicate
/// blocks accumulate.
pub fn db_upsert(records: &mut Vec<PkgRecord>, rec: PkgRecord) {
    for existing in records.iter_mut() {
        if existing.name == rec.name {
            *existing = rec;
            return;
        }
    }
    records.push(rec);
}

// ---------------------------------------------------------------------------
// Install-path mapping
// ---------------------------------------------------------------------------

/// Map a packed entry path to its absolute install path on the OS filesystem.
///
/// `.m3pkg` entry paths are relative and begin with the install-prefix
/// component (e.g. `usr/bin/foo`, `usr/lib/libfoo.so`).  Prepend `/` to get
/// the absolute on-disk path: `usr/bin/foo` → `/usr/bin/foo`.
///
/// Empty paths or paths that start with `/` are returned unchanged (unusual but
/// handled defensively).
pub fn install_path(entry_path: &str) -> String {
    if entry_path.is_empty() || entry_path.starts_with('/') {
        return entry_path.into();
    }
    let mut s = String::with_capacity(1 + entry_path.len());
    s.push('/');
    s.push_str(entry_path);
    s
}

/// Split an absolute path into the parent directory components that must exist
/// before the file can be created.
///
/// For example, `/usr/local/bin/foo` yields `["/usr", "/usr/local",
/// "/usr/local/bin"]`.  The file component itself is not included.
///
/// Returns an empty `Vec` for paths with no parent directories (e.g. `/foo`).
pub fn parent_components(abs_path: &str) -> Vec<String> {
    let mut components = Vec::new();
    // Walk from 1 to skip the leading '/'.
    let bytes = abs_path.as_bytes();
    let mut i = 1usize;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            // bytes[0..i] is the current prefix, e.g. "/usr/local"
            if i > 1 {
                components.push(abs_path[..i].into());
            }
        }
        i += 1;
    }
    components
}

/// Remove the record named `name` from `records`, returning it (with its file
/// list) if it was present. Makes `pkg remove` idempotent and gives the caller
/// the installed paths to unlink.
pub fn db_remove(records: &mut Vec<PkgRecord>, name: &str) -> Option<PkgRecord> {
    records
        .iter()
        .position(|r| r.name == name)
        .map(|idx| records.remove(idx))
}

// ---------------------------------------------------------------------------
// Package metadata sidecar (`/usr/pkg/<name>.meta`) — version + dependencies
// ---------------------------------------------------------------------------

/// Parsed `/usr/pkg/<name>.meta` sidecar. Forward-compatible: unknown keys are
/// ignored. Format (one `KEY=value` per line):
/// ```text
/// VERSION=1.3.1
/// DEPS=ncurses libevent
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// Package version (empty if absent).
    pub version: String,
    /// Direct dependency package names (space-separated in the file).
    pub deps: Vec<String>,
}

/// Parse a `.meta` sidecar. Missing file → caller passes `""` → empty `Meta`.
pub fn parse_meta(text: &str) -> Meta {
    let mut meta = Meta::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("VERSION=") {
            meta.version = v.trim().into();
        } else if let Some(v) = line.strip_prefix("DEPS=") {
            meta.deps = v.split_whitespace().map(|s| s.into()).collect();
        }
        // Unknown keys ignored (forward-compat).
    }
    meta
}

/// Serialize a `.meta` sidecar (used by the image builder to stage deps).
pub fn meta_serialize(version: &str, deps: &[&str]) -> String {
    let mut s = String::new();
    s.push_str("VERSION=");
    s.push_str(version);
    s.push('\n');
    s.push_str("DEPS=");
    s.push_str(&deps.join(" "));
    s.push('\n');
    s
}

// ---------------------------------------------------------------------------
// Dependency solver — topological install order
// ---------------------------------------------------------------------------

/// Compute the install order for `target` given a dependency map (each name →
/// its direct deps) and the set of already-installed package names. Returns the
/// names to install in dependency-first order (a dep always precedes anything
/// that needs it), omitting anything already installed. Detects cycles.
///
/// Pure logic: the in-OS installer builds `deps` by reading `.meta` sidecars,
/// then installs each returned name in order. Host-tested.
pub fn topo_install_order(
    target: &str,
    deps: &alloc::collections::BTreeMap<String, Vec<String>>,
    installed: &alloc::collections::BTreeSet<String>,
) -> Result<Vec<String>, &'static str> {
    let mut order = Vec::new();
    // visiting = on the current DFS stack (cycle detection);
    // done = fully emitted.
    let mut done = alloc::collections::BTreeSet::new();
    let mut visiting = alloc::collections::BTreeSet::new();
    fn visit(
        name: &str,
        deps: &alloc::collections::BTreeMap<String, Vec<String>>,
        installed: &alloc::collections::BTreeSet<String>,
        order: &mut Vec<String>,
        done: &mut alloc::collections::BTreeSet<String>,
        visiting: &mut alloc::collections::BTreeSet<String>,
    ) -> Result<(), &'static str> {
        if done.contains(name) || installed.contains(name) {
            return Ok(());
        }
        if visiting.contains(name) {
            return Err("dependency cycle detected");
        }
        visiting.insert(name.into());
        if let Some(children) = deps.get(name) {
            for child in children {
                visit(child, deps, installed, order, done, visiting)?;
            }
        }
        visiting.remove(name);
        done.insert(name.into());
        order.push(name.into());
        Ok(())
    }
    visit(
        target,
        deps,
        installed,
        &mut order,
        &mut done,
        &mut visiting,
    )?;
    Ok(order)
}

// ---------------------------------------------------------------------------
// Host-only tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::{BTreeMap, BTreeSet};

    fn make_pkg(name: &str, version: &str, key: &str, files: &[(&str, &str)]) -> PkgRecord {
        PkgRecord {
            name: name.into(),
            version: version.into(),
            key: key.into(),
            files: files
                .iter()
                .map(|(p, h)| FileRecord {
                    path: (*p).into(),
                    hash_hex: (*h).into(),
                })
                .collect(),
        }
    }

    // -----------------------------------------------------------------------
    // C.2 — DB round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn db_roundtrip() {
        let orig = vec![
            make_pkg(
                "foo",
                "1.0",
                "aabbcc",
                &[
                    ("/usr/bin/foo", "deadbeef"),
                    ("/usr/lib/foo.so", "cafebabe"),
                ],
            ),
            make_pkg("bar", "", "112233", &[("/usr/bin/bar", "feedface")]),
        ];
        let text = db_serialize(&orig);
        let parsed = db_parse(&text).expect("parse failed");
        assert_eq!(parsed, orig);
    }

    // -----------------------------------------------------------------------
    // C.2 — idempotent replace: insert foo, insert foo again → one block
    // -----------------------------------------------------------------------

    #[test]
    fn db_upsert_idempotent() {
        let mut records = Vec::new();
        db_upsert(
            &mut records,
            make_pkg("foo", "1.0", "aaa", &[("/usr/bin/foo", "aaa")]),
        );
        db_upsert(
            &mut records,
            make_pkg("foo", "2.0", "bbb", &[("/usr/bin/foo", "bbb")]),
        );
        assert_eq!(
            records.len(),
            1,
            "should have exactly one block after upsert"
        );
        assert_eq!(records[0].version, "2.0");
        assert_eq!(records[0].key, "bbb");
    }

    // -----------------------------------------------------------------------
    // C.2 — unknown key is ignored on read (forward-compat)
    // -----------------------------------------------------------------------

    #[test]
    fn db_tolerates_unknown_key() {
        let text = "# m3pkg-db v1\n\
                    [pkg]\n\
                    name=foo\n\
                    version=1.0\n\
                    key=aaa\n\
                    future_key=some_future_value\n\
                    file=/usr/bin/foo aaa\n\
                    [end]\n";
        let records = db_parse(text).expect("should not fail on unknown key");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "foo");
        assert_eq!(records[0].files.len(), 1);
    }

    #[test]
    fn db_rejects_malformed_file_record() {
        // A `file=` line with no space-separated hash is corruption: parsing
        // must fail fatally rather than silently dropping the record (which
        // would desync the DB's file list from disk and let `pkg verify`/
        // `remove`/`upgrade` ignore that path).
        let text = "# m3pkg-db v1\n\
                    [pkg]\n\
                    name=foo\n\
                    version=1.0\n\
                    key=aaa\n\
                    file=/usr/bin/foo\n\
                    [end]\n";
        assert!(
            db_parse(text).is_err(),
            "a file= record without a hash must be rejected, not silently dropped"
        );

        // A path containing spaces (split on the *last* space) is still valid.
        let ok = "# m3pkg-db v1\n\
                  [pkg]\n\
                  name=foo\n\
                  version=1.0\n\
                  key=aaa\n\
                  file=/usr/share/my docs/foo aaa\n\
                  [end]\n";
        let records = db_parse(ok).expect("path with spaces must parse");
        assert_eq!(records[0].files.len(), 1);
        assert_eq!(records[0].files[0].path, "/usr/share/my docs/foo");
        assert_eq!(records[0].files[0].hash_hex, "aaa");
    }

    // -----------------------------------------------------------------------
    // C.1 — install-path mapping
    // -----------------------------------------------------------------------

    #[test]
    fn install_path_mapping() {
        // Typical packed path → absolute install path.
        assert_eq!(install_path("usr/local/bin/x"), "/usr/local/bin/x");
        assert_eq!(install_path("usr/bin/foo"), "/usr/bin/foo");
        assert_eq!(install_path("usr/lib/libfoo.so"), "/usr/lib/libfoo.so");
        // Already-absolute paths are returned unchanged (defensive).
        assert_eq!(install_path("/absolute/path"), "/absolute/path");
        // Empty path.
        assert_eq!(install_path(""), "");
    }

    // -----------------------------------------------------------------------
    // C.1 — parent-directory component splitting
    // -----------------------------------------------------------------------

    #[test]
    fn parent_components_split() {
        let comps = parent_components("/usr/local/bin/foo");
        assert_eq!(comps, vec!["/usr", "/usr/local", "/usr/local/bin"]);
    }

    #[test]
    fn parent_components_top_level() {
        // /foo has no parent components to mkdir (/ always exists).
        let comps = parent_components("/foo");
        assert!(comps.is_empty());
    }

    #[test]
    fn parent_components_two_levels() {
        let comps = parent_components("/usr/bin/x");
        assert_eq!(comps, vec!["/usr", "/usr/bin"]);
    }

    // -----------------------------------------------------------------------
    // Multi-package DB: upsert does not disturb unrelated records
    // -----------------------------------------------------------------------

    #[test]
    fn db_upsert_preserves_other_records() {
        let mut records = Vec::new();
        db_upsert(
            &mut records,
            make_pkg("foo", "1.0", "aaa", &[("/usr/bin/foo", "aaa")]),
        );
        db_upsert(
            &mut records,
            make_pkg("bar", "2.0", "bbb", &[("/usr/bin/bar", "bbb")]),
        );
        db_upsert(
            &mut records,
            make_pkg("foo", "1.1", "ccc", &[("/usr/bin/foo", "ccc")]),
        );
        assert_eq!(records.len(), 2);
        let foo = records.iter().find(|r| r.name == "foo").unwrap();
        let bar = records.iter().find(|r| r.name == "bar").unwrap();
        assert_eq!(foo.version, "1.1");
        assert_eq!(bar.version, "2.0");
    }

    // -----------------------------------------------------------------------
    // pkg remove — DB removal returns the record (with its file list)
    // -----------------------------------------------------------------------

    #[test]
    fn db_remove_returns_and_drops_record() {
        let mut records = vec![
            make_pkg("foo", "1.0", "a", &[("/usr/bin/foo", "h1")]),
            make_pkg("bar", "2.0", "b", &[("/usr/bin/bar", "h2")]),
        ];
        let removed = db_remove(&mut records, "foo").expect("foo present");
        assert_eq!(removed.name, "foo");
        assert_eq!(removed.files.len(), 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "bar");
        // Removing a missing package is a no-op returning None (idempotent).
        assert!(db_remove(&mut records, "foo").is_none());
        assert_eq!(records.len(), 1);
    }

    // -----------------------------------------------------------------------
    // .meta sidecar parse / serialize round-trip + forward-compat
    // -----------------------------------------------------------------------

    #[test]
    fn meta_roundtrip_and_forward_compat() {
        let text = meta_serialize("1.3.1", &["ncurses", "libevent"]);
        let m = parse_meta(&text);
        assert_eq!(m.version, "1.3.1");
        assert_eq!(m.deps, vec!["ncurses".to_string(), "libevent".to_string()]);
        // Empty deps.
        let m0 = parse_meta(&meta_serialize("2.0", &[]));
        assert_eq!(m0.version, "2.0");
        assert!(m0.deps.is_empty());
        // Unknown key ignored; missing file (empty) → default.
        let m1 = parse_meta("VERSION=9\nFUTURE=x\nDEPS=a b\n");
        assert_eq!(m1.version, "9");
        assert_eq!(m1.deps, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(parse_meta(""), Meta::default());
    }

    // -----------------------------------------------------------------------
    // dependency solver — topological install order
    // -----------------------------------------------------------------------

    fn deps_map(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(n, ds)| ((*n).into(), ds.iter().map(|d| (*d).into()).collect()))
            .collect()
    }

    #[test]
    fn solver_orders_deps_before_dependents() {
        // tmux → ncurses, libevent ; less → ncurses
        let deps = deps_map(&[
            ("tmux", &["ncurses", "libevent"]),
            ("less", &["ncurses"]),
            ("ncurses", &[]),
            ("libevent", &[]),
        ]);
        let order = topo_install_order("tmux", &deps, &BTreeSet::new()).unwrap();
        // tmux must be last; both deps must precede it.
        assert_eq!(order.last().unwrap(), "tmux");
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("ncurses") < pos("tmux"));
        assert!(pos("libevent") < pos("tmux"));
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn solver_skips_already_installed() {
        let deps = deps_map(&[("tmux", &["ncurses", "libevent"])]);
        let mut installed = BTreeSet::new();
        installed.insert("ncurses".to_string());
        let order = topo_install_order("tmux", &deps, &installed).unwrap();
        assert!(!order.contains(&"ncurses".to_string()));
        assert_eq!(order, vec!["libevent".to_string(), "tmux".to_string()]);
    }

    #[test]
    fn solver_detects_cycles() {
        let deps = deps_map(&[("a", &["b"]), ("b", &["a"])]);
        assert!(topo_install_order("a", &deps, &BTreeSet::new()).is_err());
    }

    #[test]
    fn solver_no_deps_is_singleton() {
        let deps = deps_map(&[("ncurses", &[])]);
        let order = topo_install_order("ncurses", &deps, &BTreeSet::new()).unwrap();
        assert_eq!(order, vec!["ncurses".to_string()]);
    }
}
