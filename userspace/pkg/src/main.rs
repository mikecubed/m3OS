//! Phase 85a Track C — `pkg`: the `.m3pkg` installer for m3OS.
//!
//! # Verbs
//!
//! - `pkg install <name>` — install `<name>` + its deps. A blob already at
//!   `/usr/pkg/<name>.m3pkg` installs offline (the Phase 85a path,
//!   unchanged); otherwise Phase 107's networked branch resolves it from
//!   the signature-verified index cached by `pkg update`, fetches
//!   `<key>.m3pkg` via `curl`, SHA-256-checks it against the index, and
//!   installs through the same offline code.
//! - `pkg update [url]` — fetch `index.m3idx` + `index.m3idx.sig` from each
//!   base URL in `/etc/pkg/repos.conf` (or the explicit `url` override) and
//!   ed25519-verify the index against `/etc/pkg/keys/m3os-pkgs.pub`.
//!   **Fail-closed**: a bad signature is rejected and the previously cached
//!   index is kept.
//! - `pkg list` — print installed packages from `/var/lib/pkg/db`.
//! - `pkg verify <name>` — re-check installed files against the DB hashes.
//!
//! # Engineering notes
//!
//! All pure logic delegates to [`pkg_app`] (the `[lib]`).  This binary
//! stays `no_std` and **never links a TLS stack** — HTTPS lives entirely
//! inside the spawned Phase 86c `curl` binary, reached through
//! `fork`/`execve`/`waitpid` (the same process boundary `git` uses).
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
// Tuning constants
// ---------------------------------------------------------------------------

/// Read the `.m3pkg` artifact in 256 KiB chunks. The dominant cost of an install
/// over m3OS's slow ring-3 VFS is the per-request round-trip, not the bytes
/// moved, so a large chunk cuts the read syscall/IPC count ~64x versus a 4 KiB
/// loop (a 21 MiB package drops from ~5400 reads to ~84). The deeper fix —
/// bulk transfer via page grants + VFS fairness so a big install does not starve
/// interactive clients — is the VFS bulk-I/O phase (docs/roadmap/92-vfs-bulk-io).
const READ_CHUNK: usize = 256 * 1024;

/// Files at or above this size get an individual progress line during install,
/// so a multi-MiB write (e.g. the ~15 MiB static `python3`) is visibly in
/// progress rather than a silent stall. Smaller files are covered by the count.
const LARGE_FILE: usize = 1024 * 1024;

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
        // Phase 107: fetch + verify the signed repo index. The optional URL
        // argument overrides /etc/pkg/repos.conf (manual runs + the
        // pkg-net-smoke tamper arm).
        Some("update") => cmd_update(args.get(2).copied()),
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
    // Phase 107: a blob already at /usr/pkg/<name>.m3pkg takes the Phase 85a
    // offline path unchanged (the bundled-repo contract pkg-smoke pins);
    // anything else resolves through the signature-verified index.
    let local = build_path(b"/usr/pkg/", name.as_bytes(), b".m3pkg\0");
    if !file_exists(&local) {
        return cmd_install_networked(name);
    }

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
// Phase 107 Track B — networked update + install
// ---------------------------------------------------------------------------

/// Cached, signature-verified index (written only by a successful
/// `pkg update`).
const INDEX_CACHE_PATH: &[u8] = b"/var/lib/pkg/index.m3idx\0";
/// Repo base URLs, one per line.
const REPOS_CONF_PATH: &[u8] = b"/etc/pkg/repos.conf\0";
/// The 32-byte ed25519 trust root baked into the image.
const PUBKEY_PATH: &[u8] = b"/etc/pkg/keys/m3os-pkgs.pub\0";
/// Unverified fetch landing spots — never read as trusted state.
const TMP_INDEX_PATH: &[u8] = b"/var/lib/pkg/index.m3idx.new\0";
const TMP_SIG_PATH: &[u8] = b"/var/lib/pkg/index.m3idx.sig.new\0";
/// The base URL the cached index was verified from. Blob fetches prefer
/// it so `U:` entries resolve against the repo that published them
/// (repos.conf bases remain the fallback) — and so a `pkg update <url>`
/// override coheres with the installs that follow it.
const INDEX_SRC_PATH: &[u8] = b"/var/lib/pkg/index.src\0";

/// `pkg update [url]` — fetch + ed25519-verify the repo index. Fail-closed:
/// every failure path leaves the previously cached index untouched.
fn cmd_update(url_override: Option<&str>) -> i32 {
    let bases: Vec<String> = match url_override {
        Some(u) => alloc::vec![u.trim_end_matches('/').into()],
        None => read_repo_bases(),
    };
    if bases.is_empty() {
        eprint_str("pkg update: no repos configured in /etc/pkg/repos.conf\n");
        return 1;
    }

    // The trust root must load before any fetch — no key, no update.
    let pubkey_bytes = match read_file_bytes(PUBKEY_PATH) {
        Ok(b) => b,
        Err(_) => {
            eprint_str("pkg update: trust key /etc/pkg/keys/m3os-pkgs.pub missing\n");
            return 1;
        }
    };
    if pubkey_bytes.len() != 32 {
        eprint_str("pkg update: trust key is not 32 bytes\n");
        return 1;
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&pubkey_bytes);
    let vk = match crypto_lib::asymmetric::ed25519_verifying_key_from_bytes(&pubkey) {
        Ok(k) => k,
        Err(_) => {
            eprint_str("pkg update: trust key is not a valid ed25519 public key\n");
            return 1;
        }
    };

    ensure_pkg_state_dirs();
    for base in &bases {
        let index_url = join_url(base, "index.m3idx");
        let sig_url = join_url(base, "index.m3idx.sig");
        print_str("pkg update: fetching ");
        print_str(&index_url);
        print_str("\n");
        if !fetch_url(&index_url, TMP_INDEX_PATH) || !fetch_url(&sig_url, TMP_SIG_PATH) {
            eprint_str("pkg update: fetch failed from ");
            eprint_str(base);
            eprint_str("\n");
            clean_tmp_fetches();
            continue;
        }
        let index_bytes = read_file_bytes(TMP_INDEX_PATH);
        let sig_bytes = read_file_bytes(TMP_SIG_PATH);
        clean_tmp_fetches();
        let (Ok(index_bytes), Ok(sig_bytes)) = (index_bytes, sig_bytes) else {
            eprint_str("pkg update: fetched files unreadable\n");
            continue;
        };
        if sig_bytes.len() != 64 {
            eprint_str("pkg update: signature is not 64 bytes - rejecting\n");
            continue;
        }
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&sig_bytes);
        if !crypto_lib::asymmetric::ed25519_verify(&vk, &index_bytes, &sig) {
            eprint_str("pkg update: signature verification FAILED - keeping previous index\n");
            continue;
        }
        // Signature is good; sanity-parse before caching so a signed-but-
        // malformed index can never wedge later installs.
        let entry_count = match core::str::from_utf8(&index_bytes)
            .ok()
            .and_then(|t| pkg_format::index::parse_index(t).ok())
        {
            Some(entries) => entries.len(),
            None => {
                eprint_str("pkg update: verified index does not parse - rejecting\n");
                continue;
            }
        };
        if !write_file(INDEX_CACHE_PATH, &index_bytes) {
            eprint_str("pkg update: cannot write /var/lib/pkg/index.m3idx\n");
            return 1;
        }
        let _ = write_file(INDEX_SRC_PATH, base.as_bytes());
        print_str("pkg update: index verified (");
        syscall_lib::write_u64(STDOUT_FILENO, entry_count as u64);
        print_str(" packages)\n");
        return 0;
    }
    1
}

/// Networked install: resolve `name` from the cached verified index, fetch
/// every missing blob, SHA-256-check each against the index, and install
/// through the unchanged offline path.
fn cmd_install_networked(name: &str) -> i32 {
    let index_bytes = match read_file_bytes(INDEX_CACHE_PATH) {
        Ok(b) => b,
        Err(_) => {
            eprint_str("pkg install: ");
            eprint_str(name);
            eprint_str(": no local package and no cached index - run `pkg update` first\n");
            return 1;
        }
    };
    let entries = match core::str::from_utf8(&index_bytes)
        .ok()
        .and_then(|t| pkg_format::index::parse_index(t).ok())
    {
        Some(e) => e,
        None => {
            eprint_str("pkg install: cached index corrupt - run `pkg update`\n");
            return 1;
        }
    };
    if find_index_entry(&entries, name).is_none() {
        eprint_str("pkg install: ");
        eprint_str(name);
        eprint_str(": not in the repo index\n");
        return 1;
    }

    let deps = pkg_app::index_dep_map(&entries);
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

    // Fetch blobs from the base that served the verified index first;
    // repos.conf bases follow as fallback mirrors.
    let mut bases: Vec<String> = Vec::new();
    if let Ok(src) = read_file_bytes(INDEX_SRC_PATH)
        && let Ok(src) = core::str::from_utf8(&src)
    {
        let src = src.trim();
        if !src.is_empty() {
            bases.push(src.into());
        }
    }
    for b in read_repo_bases() {
        if !bases.contains(&b) {
            bases.push(b);
        }
    }
    if bases.is_empty() {
        eprint_str("pkg install: no repos configured in /etc/pkg/repos.conf\n");
        return 1;
    }

    for pkg in &order {
        let local = build_path(b"/usr/pkg/", pkg.as_bytes(), b".m3pkg\0");
        if !file_exists(&local) {
            let Some(entry) = find_index_entry(&entries, pkg) else {
                eprint_str("pkg install: dependency ");
                eprint_str(pkg);
                eprint_str(": not in the repo index\n");
                return 1;
            };
            let rc = fetch_and_check_blob(&bases, entry, &local);
            if rc != 0 {
                return rc;
            }
            // Stage the .meta sidecar from the trusted index so upgrade /
            // future offline resolution see the same version + deps.
            let meta_path = build_path(b"/usr/pkg/", pkg.as_bytes(), b".meta\0");
            let dep_refs: Vec<&str> = entry.deps.iter().map(|d| d.as_str()).collect();
            let meta_text = pkg_app::meta_serialize(&entry.version, &dep_refs);
            let _ = write_file(&meta_path, meta_text.as_bytes());
        }
        let rc = install_one(pkg);
        if rc != 0 {
            return rc;
        }
    }
    0
}

/// Fetch one blob into `dest` and require its SHA-256 to match the signed
/// index. A hash mismatch is a hard failure (potential tamper), not a
/// try-the-next-mirror condition; the poisoned file is removed.
fn fetch_and_check_blob(
    bases: &[String],
    entry: &pkg_format::index::IndexEntry,
    dest: &[u8],
) -> i32 {
    for base in bases {
        let url = join_url(base, &entry.url);
        print_str("pkg install: ");
        print_str(&entry.name);
        print_str(": fetching ");
        syscall_lib::write_u64(STDOUT_FILENO, entry.size);
        print_str(" bytes\n");
        if !fetch_url(&url, dest) {
            let _ = syscall_lib::unlink(dest);
            continue;
        }
        let Ok(bytes) = read_file_bytes(dest) else {
            let _ = syscall_lib::unlink(dest);
            continue;
        };
        let got = to_hex(&content_hash(&bytes));
        if got == entry.sha256 {
            return 0;
        }
        eprint_str("pkg install: ");
        eprint_str(&entry.name);
        eprint_str(": SHA-256 MISMATCH vs signed index - rejecting blob\n");
        let _ = syscall_lib::unlink(dest);
        return 1;
    }
    eprint_str("pkg install: ");
    eprint_str(&entry.name);
    eprint_str(": fetch failed from every configured repo\n");
    1
}

/// Spawn `curl -fsSL -o <dest> <url>` and wait. TLS (when the URL is
/// https) lives entirely inside curl — this binary links no TLS stack.
/// `dest` must be NUL-terminated.
fn fetch_url(url: &str, dest: &[u8]) -> bool {
    let mut url_buf = Vec::with_capacity(url.len() + 1);
    url_buf.extend_from_slice(url.as_bytes());
    url_buf.push(0);

    let pid = syscall_lib::fork();
    if pid < 0 {
        return false;
    }
    if pid == 0 {
        // Child: argv/envp are NUL-terminated strings + null-terminated
        // pointer arrays (the shell's execve convention).
        //
        // Deliberately NO `--connect-timeout`/`--max-time`: with a
        // signal-resolver curl build those arm the alarm()/SIGALRM +
        // sigsetjmp timeout path, which wedges pre-connect on m3OS
        // (pkg-net-smoke bisected this — a shell-run curl with those two
        // flags hangs before its first SYN; without them it completes).
        // The envp mirrors the shell's so curl sees the environment it is
        // validated under.
        let argv: [*const u8; 6] = [
            c"curl".as_ptr().cast::<u8>(),
            c"-fsSL".as_ptr().cast::<u8>(),
            c"-o".as_ptr().cast::<u8>(),
            dest.as_ptr(),
            url_buf.as_ptr(),
            core::ptr::null(),
        ];
        let envp: [*const u8; 4] = [
            c"PATH=/usr/local/bin:/usr/bin:/bin".as_ptr().cast::<u8>(),
            c"HOME=/root".as_ptr().cast::<u8>(),
            c"TERM=m3os-term".as_ptr().cast::<u8>(),
            core::ptr::null(),
        ];
        // Try the known install locations; execve only returns on failure.
        for path in [
            &b"/usr/local/bin/curl\0"[..],
            &b"/usr/bin/curl\0"[..],
            &b"/bin/curl\0"[..],
        ] {
            let _ = syscall_lib::execve(path, &argv, &envp);
        }
        syscall_lib::write_str(
            STDERR_FILENO,
            "pkg: curl not found - `pkg install curl` first\n",
        );
        syscall_lib::exit(127)
    }
    let mut status = 0i32;
    if syscall_lib::waitpid(pid as i32, &mut status, 0) < 0 {
        eprint_str("pkg: waitpid for curl failed\n");
        return false;
    }
    // Normal exit with code 0 (the shell's status decode).
    let ok = status & 0x7f == 0 && (status >> 8) & 0xff == 0;
    if !ok {
        eprint_str("pkg: curl exited with status ");
        syscall_lib::write_u64(STDERR_FILENO, ((status >> 8) & 0xff) as u64);
        eprint_str(" (raw ");
        syscall_lib::write_u64(STDERR_FILENO, status as u64);
        eprint_str(")\n");
    }
    ok
}

/// Parse `/etc/pkg/repos.conf` (absent file → empty list).
fn read_repo_bases() -> Vec<String> {
    match read_file_bytes(REPOS_CONF_PATH) {
        Ok(bytes) => match core::str::from_utf8(&bytes) {
            Ok(t) => pkg_app::parse_repos_conf(t),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

fn find_index_entry<'e>(
    entries: &'e [pkg_format::index::IndexEntry],
    name: &str,
) -> Option<&'e pkg_format::index::IndexEntry> {
    entries.iter().find(|e| e.name == name)
}

fn join_url(base: &str, file: &str) -> String {
    let mut s = String::with_capacity(base.len() + 1 + file.len());
    s.push_str(base);
    s.push('/');
    s.push_str(file);
    s
}

/// Does `path` (NUL-terminated) exist and open readably?
fn file_exists(path: &[u8]) -> bool {
    let fd = syscall_lib::open(path, syscall_lib::O_RDONLY, 0);
    if (fd as i64) < 0 {
        return false;
    }
    let _ = syscall_lib::close(fd as i32);
    true
}

/// Create-or-truncate `path` (NUL-terminated) with `data`.
fn write_file(path: &[u8], data: &[u8]) -> bool {
    let flags = syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC;
    let fd = syscall_lib::open(path, flags, 0o644);
    if (fd as i64) < 0 {
        return false;
    }
    let fd = fd as i32;
    let ok = write_all(fd, data);
    let _ = syscall_lib::close(fd);
    ok
}

/// `mkdir -p /var/lib/pkg` (each level EEXIST-tolerant — the db_write
/// convention).
fn ensure_pkg_state_dirs() {
    let _ = syscall_lib::mkdir(b"/var\0", 0o755);
    let _ = syscall_lib::mkdir(b"/var/lib\0", 0o755);
    let _ = syscall_lib::mkdir(b"/var/lib/pkg\0", 0o755);
}

fn clean_tmp_fetches() {
    let _ = syscall_lib::unlink(TMP_INDEX_PATH);
    let _ = syscall_lib::unlink(TMP_SIG_PATH);
}

// ---------------------------------------------------------------------------
// install_one — single-package install (C.1 core)
// ---------------------------------------------------------------------------

fn install_one(name: &str) -> i32 {
    // ---- 1. Build the path to the local package file. ----
    // /usr/pkg/<name>.m3pkg  (NUL-terminated for syscall)
    let pkg_path = build_path(b"/usr/pkg/", name.as_bytes(), b".m3pkg\0");

    print_str("pkg install: ");
    print_str(name);
    print_str(": reading package\n");

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

    print_str("pkg install: ");
    print_str(name);
    print_str(": ");
    let read_mib = pkg_bytes.len() / (1024 * 1024);
    if read_mib > 0 {
        print_str("read ");
        syscall_lib::write_u64(STDOUT_FILENO, read_mib as u64);
        print_str(" MiB, ");
    }
    print_str("verifying\n");

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

    print_str("pkg install: ");
    print_str(name);
    print_str(": installing ");
    syscall_lib::write_u64(STDOUT_FILENO, manifest.entries.len() as u64);
    print_str(" files\n");

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

        // Show a per-file line for large writes so a multi-MiB file (e.g. the
        // static `python3`) is visibly in progress rather than a silent stall.
        if content.len() >= LARGE_FILE {
            print_str("  ");
            print_str(&entry.path);
            print_str(" (");
            syscall_lib::write_u64(STDOUT_FILENO, (content.len() / (1024 * 1024)) as u64);
            print_str(" MiB)\n");
        }

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
    // Pre-size the buffer from the file's stat so a multi-MiB package (the
    // ~21 MiB python.m3pkg) lands in one allocation. Without this the Vec doubles
    // repeatedly, and m3OS's musl has no `mremap`, so every grow is an
    // alloc-copy-free of the whole buffer — wasted work that scales with size.
    // `prealloc_hint` clamps the raw `st_size` (PR #223): a corrupted/malicious
    // size must not drive an unbounded `Vec::with_capacity`, which would abort
    // `pkg` via the allocator error path. The hint is a ceiling only — the read
    // loop below still grows `buf` to fit a genuinely larger file.
    let mut st = syscall_lib::Stat::zeroed();
    let hint = if syscall_lib::fstat(fd, &mut st) >= 0 {
        pkg_app::prealloc_hint(st.st_size)
    } else {
        0
    };
    let mut buf = Vec::with_capacity(hint);
    // Large read chunk: see READ_CHUNK — far fewer VFS round-trips than 4 KiB.
    let mut chunk = alloc::vec![0u8; READ_CHUNK];
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
         pkg update [url]     Fetch + ed25519-verify the repo index (fail-closed)\n  \
         pkg install <name>   Install <name> + deps (local /usr/pkg blob, else\n                       \
         fetch from the verified index + SHA-256 check)\n  \
         pkg remove <name>    Remove an installed package's files + DB entry\n  \
         pkg upgrade <name>   Reinstall <name>, pruning orphaned files\n  \
         pkg list             List installed packages\n  \
         pkg verify <name>    Verify installed files against the DB\n",
    );
}
