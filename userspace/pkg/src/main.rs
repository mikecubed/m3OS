//! Phase 85a Track C — `pkg`: offline `.m3pkg` installer for m3OS.
//!
//! **Offline only** — no network access.  The networked install path is a
//! Phase 86 task (`docs/roadmap/86-networking-and-github.md`).
//!
//! # Verbs
//!
//! - `pkg install <name>` — read `/usr/pkg/<name>.m3pkg`, verify, extract under
//!   `/`, record the DB.
//! - `pkg list` — print installed packages from `/var/lib/pkg/db`.
//! - `pkg verify <name>` — re-check installed files against the DB hashes.
//!
//! # Engineering notes
//!
//! All logic delegates to [`pkg_app`] (the `[lib]`).  This binary is thin:
//! parse argv → dispatch verb → print result.  No socket syscalls anywhere.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;

use alloc::collections::{BTreeMap, BTreeSet};
use pkg_app::{
    FileRecord, Meta, PkgRecord, db_parse, db_remove, db_serialize, db_upsert, install_path,
    parent_components, parse_meta, topo_install_order,
};
use pkg_format::{content_hash, to_hex};
use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, heap::BrkAllocator};

// ---------------------------------------------------------------------------
// Global allocator — required because we use Vec/String (pkg-format dep).
// ---------------------------------------------------------------------------

#[global_allocator]
static A: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDERR_FILENO, "pkg: alloc error\n");
    syscall_lib::exit(99)
}

// ---------------------------------------------------------------------------
// Entry point — uses entry_point! macro (same as m3ctl).
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

fn program_main(args: &[&str]) -> i32 {
    match args.get(1).copied() {
        Some("install") => match args.get(2).copied() {
            Some(name) => cmd_install(name),
            None => {
                eprint_str("pkg install: missing package name\n");
                print_usage();
                2
            }
        },
        Some("list") => cmd_list(),
        Some("verify") => match args.get(2).copied() {
            Some(name) => cmd_verify(name),
            None => {
                eprint_str("pkg verify: missing package name\n");
                print_usage();
                2
            }
        },
        Some("remove") => match args.get(2).copied() {
            Some(name) => cmd_remove(name),
            None => {
                eprint_str("pkg remove: missing package name\n");
                print_usage();
                2
            }
        },
        Some("upgrade") => match args.get(2).copied() {
            Some(name) => cmd_upgrade(name),
            None => {
                eprint_str("pkg upgrade: missing package name\n");
                print_usage();
                2
            }
        },
        _ => {
            print_usage();
            2
        }
    }
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDERR_FILENO, "pkg: PANIC\n");
    syscall_lib::exit(101)
}

// ---------------------------------------------------------------------------
// cmd_install — C.1 + dependency solver (Phase 85a follow-up)
// ---------------------------------------------------------------------------

/// Install `name` and its (transitive) dependencies. Reads each package's
/// `/usr/pkg/<n>.meta` sidecar to build a dependency graph, computes a
/// dependency-first install order skipping packages already in the DB, and
/// installs each in turn. With no `.meta` (or empty `DEPS=`) this degrades to a
/// single-package install — preserving the original flat behaviour.
fn cmd_install(name: &str) -> i32 {
    // Build the dependency map by reading .meta sidecars transitively.
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    collect_deps(name, &mut deps);

    // Already-installed set (names) from the DB. A corrupt DB is fatal here
    // (rather than treated as empty) so we never proceed to write files +
    // overwrite a DB we could not read; DB-absent is `Ok(empty)`.
    let installed: BTreeSet<String> = match db_read() {
        Ok(recs) => recs.into_iter().map(|r| r.name).collect(),
        Err(e) => {
            eprint_str("pkg install: ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };

    let order = match topo_install_order(name, &deps, &installed) {
        Ok(o) => o,
        Err(e) => {
            eprint_str("pkg install: ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };

    if order.is_empty() {
        print_str("pkg install: ");
        print_str(name);
        print_str(": already installed\n");
        return 0;
    }
    if order.len() > 1 {
        print_str("pkg install: resolving ");
        print_str(name);
        print_str(" + dependencies\n");
    }

    for pkg in &order {
        let rc = install_one(pkg);
        if rc != 0 {
            return rc;
        }
    }
    0
}

/// Recursively read `<name>.meta` (DEPS=) into `deps`, terminating on
/// already-visited names (which also breaks cycles for the collection phase;
/// `topo_install_order` reports the cycle as an error).
fn collect_deps(name: &str, deps: &mut BTreeMap<String, Vec<String>>) {
    if deps.contains_key(name) {
        return;
    }
    let meta = read_meta(name);
    deps.insert(name.into(), meta.deps.clone());
    for d in &meta.deps {
        collect_deps(d, deps);
    }
}

// ---------------------------------------------------------------------------
// install_one — single-package install (C.1 core)
// ---------------------------------------------------------------------------

fn install_one(name: &str) -> i32 {
    // ---- 1. Build the path to the local package file. ----
    // /usr/pkg/<name>.m3pkg  (NUL-terminated for syscall)
    let mut pkg_path = build_path(b"/usr/pkg/", name.as_bytes(), b".m3pkg\0");

    // ---- 2. Read the .m3pkg artifact into memory. ----
    let pkg_bytes = match read_file_bytes(&pkg_path) {
        Ok(b) => b,
        Err(e) => {
            eprint_str("pkg install: cannot read ");
            eprint_str(name);
            eprint_str(": ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };
    // Drop the NUL before using as a display path string.
    pkg_path.pop();

    // ---- 3. Integrity check. ----
    if !pkg_format::verify(&pkg_bytes) {
        eprint_str("pkg install: integrity check FAILED for ");
        eprint_str(name);
        eprint_str("\n");
        return 1;
    }

    // ---- 4. Parse the manifest. ----
    let manifest = match pkg_format::parse(&pkg_bytes) {
        Ok(m) => m,
        Err(e) => {
            eprint_str("pkg install: parse error for ");
            eprint_str(name);
            eprint_str(": ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };

    // ---- 5. Extract entries. ----
    let mut installed_files: Vec<FileRecord> = Vec::new();
    // Track directories already created during this install so a package with
    // many files in the same tree (e.g. ncurses' ~1833-entry terminfo DB) does
    // not re-issue a `mkdir` per file — that mkdir storm dominated the install
    // time over the ring-3 VFS. Each unique component is created at most once.
    let mut created_dirs: BTreeSet<String> = BTreeSet::new();

    for entry in &manifest.entries {
        let abs = install_path(&entry.path);

        // 5a. Ensure parent directories exist (mkdir each *new* component once).
        for dir in parent_components(&abs) {
            if created_dirs.contains(&dir) {
                continue;
            }
            let mut dir_path = dir.as_bytes().to_vec();
            dir_path.push(0);
            // Ignore errors: directory may already exist on disk.
            let _ = syscall_lib::mkdir(&dir_path, 0o755);
            created_dirs.insert(dir);
        }

        // 5b. Borrow the entry content from the artifact buffer.
        let content = match manifest.entry_content(&pkg_bytes, entry) {
            Ok(c) => c,
            Err(e) => {
                eprint_str("pkg install: entry content error (");
                eprint_str(e);
                eprint_str("): ");
                eprint_str(&entry.path);
                eprint_str("\n");
                return 1;
            }
        };

        if entry.is_symlink() {
            // 5c. Symlink: content = link target bytes.
            let mut target = content.to_vec();
            target.push(0);
            let mut link_path = abs.as_bytes().to_vec();
            link_path.push(0);
            // Remove any node already at the link path first, so a reinstall or
            // upgrade refreshes a *changed* target instead of silently keeping
            // the old one: symlink(2) returns EEXIST when a node is present, and
            // ignoring it would leave the on-disk link stale while the DB below
            // records the new content hash — a guaranteed `pkg verify` MISMATCH.
            // A missing-path unlink error is fine (nothing to remove); the
            // symlink call is the real gate.
            let _ = syscall_lib::unlink(&link_path);
            let rc = syscall_lib::symlink(&target, &link_path);
            if (rc as i64) < 0 {
                // The DB is about to record this path's hash, so a link we could
                // not create would surface as a phantom MISSING/MISMATCH on
                // `pkg verify`. Fail the install, mirroring the regular-file
                // open/write error handling below.
                eprint_str("pkg install: cannot create symlink ");
                eprint_str(&entry.path);
                eprint_str("\n");
                return 1;
            }
        } else {
            // 5d. Regular file: open/write/close/chmod.
            let mut file_path = abs.as_bytes().to_vec();
            file_path.push(0);

            // Remove any node already at this path first (mirrors the symlink
            // branch). `O_TRUNC` truncates an existing *regular* file in place,
            // but if a prior version installed this path as a *symlink* the open
            // would follow the link and write through to its target, leaving the
            // symlink in place while the DB records this path's hash — the same
            // FS/DB divergence the symlink branch guards against. Unlinking first
            // turns an upgrade type-change into a clean fresh create.
            let _ = syscall_lib::unlink(&file_path);

            let flags = syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC;
            let fd = syscall_lib::open(&file_path, flags, 0o644);
            if (fd as i64) < 0 {
                eprint_str("pkg install: cannot open ");
                eprint_str(&entry.path);
                eprint_str(" for writing\n");
                return 1;
            }
            let fd = fd as i32;

            if !write_all(fd, content) {
                eprint_str("pkg install: write error for ");
                eprint_str(&entry.path);
                eprint_str("\n");
                let _ = syscall_lib::close(fd);
                return 1;
            }
            let _ = syscall_lib::close(fd);

            let perm = entry.perm_bits() as u16;
            let _ = syscall_lib::chmod(&file_path, perm);
        }

        // 5e. Record this file for the DB.
        installed_files.push(FileRecord {
            path: abs,
            hash_hex: to_hex(&entry.content_hash),
        });
    }

    // ---- 6. Read optional sidecar .meta for VERSION=. ----
    let version = read_meta(name).version;

    // ---- 7. Compute the artifact content key. ----
    let artifact_key = to_hex(&content_hash(&pkg_bytes));

    // ---- 8. Update the DB (idempotent upsert). ----
    let new_rec = PkgRecord {
        name: name.into(),
        version,
        key: artifact_key,
        files: installed_files,
    };

    if let Err(e) = db_update(new_rec) {
        eprint_str("pkg install: DB update error: ");
        eprint_str(e);
        eprint_str("\n");
        return 1;
    }

    print_str("pkg install: ");
    print_str(name);
    print_str(": OK\n");
    0
}

// ---------------------------------------------------------------------------
// cmd_list — C.1
// ---------------------------------------------------------------------------

fn cmd_list() -> i32 {
    let records = match db_read() {
        Ok(r) => r,
        Err(e) => {
            // DB-absent is `Ok(empty)`; a real `Err` is a corrupt/unreadable DB.
            eprint_str("pkg list: ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };

    if records.is_empty() {
        print_str("(no packages installed)\n");
        return 0;
    }

    for rec in &records {
        print_str(&rec.name);
        if !rec.version.is_empty() {
            print_str(" ");
            print_str(&rec.version);
        }
        print_str("\n");
    }
    0
}

// ---------------------------------------------------------------------------
// cmd_verify — C.1
// ---------------------------------------------------------------------------

fn cmd_verify(name: &str) -> i32 {
    let records = match db_read() {
        Ok(r) => r,
        Err(e) => {
            eprint_str("pkg verify: ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };

    let rec = match records.iter().find(|r| r.name == name) {
        Some(r) => r,
        None => {
            eprint_str("pkg verify: ");
            eprint_str(name);
            eprint_str(": not installed\n");
            return 1;
        }
    };

    let mut ok_count = 0u32;
    let mut mismatch_count = 0u32;

    for f in &rec.files {
        let mut file_path = f.path.as_bytes().to_vec();
        file_path.push(0);

        // A symlink entry records SHA-256(link-target-bytes) — NOT the content
        // of the file it points at (see `install_one`/`pkg_format::pack`).
        // `readlink` returns the target for a symlink and `EINVAL` (<0) for a
        // regular file, so probe it first: opening a symlink `O_RDONLY` would
        // follow it and hash the wrong bytes, spuriously reporting MISMATCH for
        // every link (e.g. all of ncurses' alias links).
        // `readlink` returns the byte count written, with no NUL and no
        // truncation flag: a return equal to the buffer length means the target
        // may have been truncated, which would hash to a false MISMATCH. Start
        // with a small buffer and, only on a full read, retry once with a
        // PATH_MAX-class buffer. A negative return means it is not a symlink
        // (regular file) — fall back to hashing the file content.
        let actual_hex: Option<String> = {
            let mut link_buf = [0u8; 1024];
            let ln = syscall_lib::readlink(&file_path, &mut link_buf);
            if ln < 0 {
                match read_file_bytes(&file_path) {
                    Ok(data) => Some(to_hex(&content_hash(&data))),
                    Err(_) => None,
                }
            } else if (ln as usize) < link_buf.len() {
                Some(to_hex(&content_hash(&link_buf[..ln as usize])))
            } else {
                // Buffer filled exactly — the target may be longer; re-read with
                // a 4096-byte (PATH_MAX) buffer.
                let mut big = [0u8; 4096];
                let ln2 = syscall_lib::readlink(&file_path, &mut big);
                if ln2 >= 0 {
                    Some(to_hex(&content_hash(&big[..ln2 as usize])))
                } else {
                    // Unexpected retry failure: hash what the first read gave us.
                    Some(to_hex(&content_hash(&link_buf[..ln as usize])))
                }
            }
        };

        match actual_hex {
            Some(hex) if hex == f.hash_hex => {
                print_str("OK      ");
                print_str(&f.path);
                print_str("\n");
                ok_count += 1;
            }
            Some(_) => {
                print_str("MISMATCH ");
                print_str(&f.path);
                print_str("\n");
                mismatch_count += 1;
            }
            None => {
                print_str("MISSING  ");
                print_str(&f.path);
                print_str("\n");
                mismatch_count += 1;
            }
        }
    }

    print_str("summary: ");
    syscall_lib::write_u64(STDOUT_FILENO, ok_count as u64);
    print_str(" OK, ");
    syscall_lib::write_u64(STDOUT_FILENO, mismatch_count as u64);
    print_str(" MISMATCH\n");

    if mismatch_count > 0 { 1 } else { 0 }
}

// ---------------------------------------------------------------------------
// cmd_remove — delete a package's recorded files + DB entry
// ---------------------------------------------------------------------------

fn cmd_remove(name: &str) -> i32 {
    let mut records = match db_read() {
        Ok(r) => r,
        Err(e) => {
            // DB-absent is `Ok(empty)` (→ "not installed" below); a real `Err`
            // is a corrupt/unreadable DB — surface it rather than wiping.
            eprint_str("pkg remove: ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };
    let removed = match db_remove(&mut records, name) {
        Some(r) => r,
        None => {
            eprint_str("pkg remove: ");
            eprint_str(name);
            eprint_str(": not installed\n");
            return 1;
        }
    };

    let mut unlinked = 0u32;
    for f in &removed.files {
        let mut p = f.path.as_bytes().to_vec();
        p.push(0);
        if syscall_lib::unlink(&p) >= 0 {
            unlinked += 1;
        }
        // Missing files are tolerated (already gone) — remove stays idempotent.
    }

    if let Err(e) = db_write(&records) {
        eprint_str("pkg remove: DB write error: ");
        eprint_str(e);
        eprint_str("\n");
        return 1;
    }

    print_str("pkg remove: ");
    print_str(name);
    print_str(": removed ");
    syscall_lib::write_u64(STDOUT_FILENO, unlinked as u64);
    print_str(" file(s)\n");
    0
}

// ---------------------------------------------------------------------------
// cmd_upgrade — full update: prune orphaned files, then (re)install
// ---------------------------------------------------------------------------

fn cmd_upgrade(name: &str) -> i32 {
    // A corrupt DB is fatal (don't treat it as empty); DB-absent is `Ok(empty)`.
    let records = match db_read() {
        Ok(r) => r,
        Err(e) => {
            eprint_str("pkg upgrade: ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };
    let old = match records.iter().find(|r| r.name == name).cloned() {
        Some(r) => r,
        None => {
            // Not installed yet — upgrade degrades to a fresh (solver) install.
            print_str("pkg upgrade: ");
            print_str(name);
            print_str(" not installed — installing\n");
            return cmd_install(name);
        }
    };

    // Validate the NEW artifact and compute its file set up front, so a corrupt
    // replacement package aborts the upgrade before anything is touched.
    let pkg_path = build_path(b"/usr/pkg/", name.as_bytes(), b".m3pkg\0");
    let bytes = match read_file_bytes(&pkg_path) {
        Ok(b) => b,
        Err(e) => {
            eprint_str("pkg upgrade: cannot read ");
            eprint_str(name);
            eprint_str(": ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };
    if !pkg_format::verify(&bytes) {
        eprint_str("pkg upgrade: integrity check FAILED for ");
        eprint_str(name);
        eprint_str("\n");
        return 1;
    }
    let manifest = match pkg_format::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            eprint_str("pkg upgrade: parse error: ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };
    let mut new_paths: BTreeSet<String> = BTreeSet::new();
    for e in &manifest.entries {
        new_paths.insert(install_path(&e.path));
    }

    // Reinstall the target (forced) plus any newly-introduced dependencies
    // FIRST: build the dep graph, treat `name` as NOT installed so it is
    // reinstalled, and let the solver pull in any missing deps. `install_one`
    // upserts the new DB record.
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    collect_deps(name, &mut deps);
    let mut installed: BTreeSet<String> = records.iter().map(|r| r.name.clone()).collect();
    installed.remove(name);
    let order = match topo_install_order(name, &deps, &installed) {
        Ok(o) => o,
        Err(e) => {
            eprint_str("pkg upgrade: ");
            eprint_str(e);
            eprint_str("\n");
            return 1;
        }
    };
    for pkg in &order {
        let rc = install_one(pkg);
        if rc != 0 {
            return rc;
        }
    }

    // Only AFTER a successful reinstall + DB upsert do we prune files the old
    // version owned but the new one does not. Doing this last means a
    // mid-reinstall failure never leaves files deleted on disk while the DB
    // still lists them (the reorder that fixes that state-corruption window;
    // full atomic rollback remains a Phase 86 concern).
    let mut pruned = 0u32;
    for f in &old.files {
        if !new_paths.contains(&f.path) {
            let mut p = f.path.as_bytes().to_vec();
            p.push(0);
            if syscall_lib::unlink(&p) >= 0 {
                pruned += 1;
            }
        }
    }

    print_str("pkg upgrade: ");
    print_str(name);
    print_str(": OK (pruned ");
    syscall_lib::write_u64(STDOUT_FILENO, pruned as u64);
    print_str(" orphan(s))\n");
    0
}

// ---------------------------------------------------------------------------
// DB helpers
// ---------------------------------------------------------------------------

/// Path to the DB file.
const DB_PATH: &[u8] = b"/var/lib/pkg/db\0";

/// Read and parse the installed-file DB.
///
/// Distinguishes **DB-absent** from **DB-corrupt**, which matters because a
/// mutating caller (`install`/`upgrade`/`db_update`) rewrites the file with
/// `O_TRUNC`: collapsing both cases to an empty record set (the old
/// `unwrap_or_default()`) would silently wipe every other package's record the
/// first time the DB failed to parse. So:
///   - the DB file not existing (`ENOENT`) returns `Ok(empty)` — the correct,
///     non-destructive interpretation of "nothing installed yet";
///   - any other open/read failure, a non-UTF-8 body, or a parse error returns
///     `Err`, so the caller refuses to overwrite a DB it could not read.
fn db_read() -> Result<Vec<PkgRecord>, &'static str> {
    const ENOENT: isize = -2;
    let fd = syscall_lib::open(DB_PATH, syscall_lib::O_RDONLY, 0);
    if (fd as i64) < 0 {
        if (fd as isize) == ENOENT {
            return Ok(Vec::new());
        }
        return Err("cannot open DB");
    }
    let fd = fd as i32;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = syscall_lib::read(fd, &mut chunk);
        if n < 0 {
            let _ = syscall_lib::close(fd);
            return Err("DB read error");
        }
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
    }
    let _ = syscall_lib::close(fd);
    let text = core::str::from_utf8(&buf).map_err(|_| "DB is not UTF-8")?;
    db_parse(text).map_err(|_| "DB parse error")
}

/// Write the full record set to the DB on disk (creates `/var/lib/pkg/`).
fn db_write(records: &[PkgRecord]) -> Result<(), &'static str> {
    let _ = syscall_lib::mkdir(b"/var\0", 0o755);
    let _ = syscall_lib::mkdir(b"/var/lib\0", 0o755);
    let _ = syscall_lib::mkdir(b"/var/lib/pkg\0", 0o755);

    let text = db_serialize(records);
    let flags = syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC;
    let fd = syscall_lib::open(DB_PATH, flags, 0o644);
    if (fd as i64) < 0 {
        return Err("cannot open DB for writing");
    }
    let fd = fd as i32;
    let ok = write_all(fd, text.as_bytes());
    let _ = syscall_lib::close(fd);
    if ok { Ok(()) } else { Err("DB write error") }
}

/// Upsert a `PkgRecord` into the DB on disk.
fn db_update(rec: PkgRecord) -> Result<(), &'static str> {
    // Never fall back to an empty DB on a read failure (that would discard
    // every other package's record on the next `O_TRUNC` write). `db_read`
    // already maps DB-absent to `Ok(empty)`, so a real `Err` here is a corrupt
    // DB and must abort the update.
    let mut records = db_read()?;
    db_upsert(&mut records, rec);
    db_write(&records)
}

// ---------------------------------------------------------------------------
// Read /usr/pkg/<name>.meta (VERSION= + DEPS=)
// ---------------------------------------------------------------------------

fn read_meta(name: &str) -> Meta {
    let meta_path = build_path(b"/usr/pkg/", name.as_bytes(), b".meta\0");
    let data = match read_file_bytes(&meta_path) {
        Ok(d) => d,
        Err(_) => return Meta::default(),
    };
    match core::str::from_utf8(&data) {
        Ok(t) => parse_meta(t),
        Err(_) => Meta::default(),
    }
}

// ---------------------------------------------------------------------------
// I/O helpers (no network, only filesystem syscalls)
// ---------------------------------------------------------------------------

/// Read an entire file into a `Vec<u8>`.  `path` must be NUL-terminated.
fn read_file_bytes(path: &[u8]) -> Result<Vec<u8>, &'static str> {
    let fd = syscall_lib::open(path, syscall_lib::O_RDONLY, 0);
    if (fd as i64) < 0 {
        return Err("open failed");
    }
    let fd = fd as i32;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = syscall_lib::read(fd, &mut chunk);
        if n < 0 {
            let _ = syscall_lib::close(fd);
            return Err("read failed");
        }
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
    }
    let _ = syscall_lib::close(fd);
    Ok(buf)
}

/// Write all bytes to `fd`.  Returns `false` on error.
fn write_all(fd: i32, data: &[u8]) -> bool {
    let mut written = 0usize;
    while written < data.len() {
        let n = syscall_lib::write(fd, &data[written..]);
        if n <= 0 {
            return false;
        }
        written += n as usize;
    }
    true
}

/// Build a NUL-terminated path from prefix + name + suffix byte slices.
/// The returned `Vec<u8>` ends with a `\0` byte.
fn build_path(prefix: &[u8], name: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(prefix.len() + name.len() + suffix.len());
    v.extend_from_slice(prefix);
    v.extend_from_slice(name);
    v.extend_from_slice(suffix);
    v
}

// ---------------------------------------------------------------------------
// Output helpers
// ---------------------------------------------------------------------------

fn print_str(s: &str) {
    syscall_lib::write_str(STDOUT_FILENO, s);
}

fn eprint_str(s: &str) {
    syscall_lib::write_str(STDERR_FILENO, s);
}

fn print_usage() {
    print_str(
        "Usage:\n  \
         pkg install <name>   Install /usr/pkg/<name>.m3pkg + its deps (offline)\n  \
         pkg remove <name>    Remove an installed package's files + DB entry\n  \
         pkg upgrade <name>   Reinstall <name>, pruning orphaned files\n  \
         pkg list             List installed packages\n  \
         pkg verify <name>    Verify installed files against the DB\n\n\
         Note: pkg is offline-only in Phase 85a; networked install is Phase 86.\n",
    );
}
