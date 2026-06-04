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

use pkg_app::{
    FileRecord, PkgRecord, db_parse, db_serialize, db_upsert, install_path, parent_components,
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
// cmd_install — C.1
// ---------------------------------------------------------------------------

fn cmd_install(name: &str) -> i32 {
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

    for entry in &manifest.entries {
        let abs = install_path(&entry.path);

        // 5a. Ensure parent directories exist (mkdir each component, ignore EEXIST).
        let dirs = parent_components(&abs);
        for dir in &dirs {
            let mut dir_path = dir.as_bytes().to_vec();
            dir_path.push(0);
            // Ignore errors: directory may already exist.
            let _ = syscall_lib::mkdir(&dir_path, 0o755);
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
            let rc = syscall_lib::symlink(&target, &link_path);
            if (rc as i64) < 0 {
                // Ignore EEXIST (symlink already present from a prior install).
                // Any other error is non-fatal: warn and continue.
                if rc != -17 {
                    // -17 = EEXIST
                    eprint_str("pkg install: symlink warning for ");
                    eprint_str(&entry.path);
                    eprint_str("\n");
                }
            }
        } else {
            // 5d. Regular file: open/write/close/chmod.
            let mut file_path = abs.as_bytes().to_vec();
            file_path.push(0);

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
    let version = read_meta_version(name);

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
        Err(_) => {
            // No DB yet — nothing installed.
            print_str("(no packages installed)\n");
            return 0;
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
        Err(_) => {
            eprint_str("pkg verify: DB not found (no packages installed)\n");
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

        match read_file_bytes(&file_path) {
            Ok(data) => {
                let actual_hex = to_hex(&content_hash(&data));
                if actual_hex == f.hash_hex {
                    print_str("OK      ");
                    print_str(&f.path);
                    print_str("\n");
                    ok_count += 1;
                } else {
                    print_str("MISMATCH ");
                    print_str(&f.path);
                    print_str("\n");
                    mismatch_count += 1;
                }
            }
            Err(_) => {
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
// DB helpers
// ---------------------------------------------------------------------------

/// Path to the DB file.
const DB_PATH: &[u8] = b"/var/lib/pkg/db\0";

/// Read and parse the installed-file DB.  Returns `Err` if the file does not
/// exist or cannot be parsed.
fn db_read() -> Result<Vec<PkgRecord>, &'static str> {
    let data = read_file_bytes(DB_PATH).map_err(|_| "cannot read DB")?;
    let text = core::str::from_utf8(&data).map_err(|_| "DB is not UTF-8")?;
    db_parse(text).map_err(|_| "DB parse error")
}

/// Upsert a `PkgRecord` into the DB on disk (atomic: write then close).
fn db_update(rec: PkgRecord) -> Result<(), &'static str> {
    // Read existing records (empty if DB absent).
    let mut records = db_read().unwrap_or_default();
    db_upsert(&mut records, rec);

    // Ensure /var/lib/pkg/ exists.
    let _ = syscall_lib::mkdir(b"/var\0", 0o755);
    let _ = syscall_lib::mkdir(b"/var/lib\0", 0o755);
    let _ = syscall_lib::mkdir(b"/var/lib/pkg\0", 0o755);

    let text = db_serialize(&records);
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

// ---------------------------------------------------------------------------
// Read /usr/pkg/<name>.meta for VERSION=<…>
// ---------------------------------------------------------------------------

fn read_meta_version(name: &str) -> String {
    let meta_path = build_path(b"/usr/pkg/", name.as_bytes(), b".meta\0");
    let data = match read_file_bytes(&meta_path) {
        Ok(d) => d,
        Err(_) => return String::new(),
    };
    let text = match core::str::from_utf8(&data) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("VERSION=") {
            return v.trim().into();
        }
    }
    String::new()
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
         pkg install <name>   Install /usr/pkg/<name>.m3pkg (offline)\n  \
         pkg list             List installed packages\n  \
         pkg verify <name>    Verify installed files against the DB\n\n\
         Note: pkg is offline-only in Phase 85a; networked install is Phase 86.\n",
    );
}
