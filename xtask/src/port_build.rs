//! Phase 69d Track A.3 — host-side `cargo xtask port build <name>` driver.
//!
//! Fetches the upstream tarball pinned by `ports/<category>/<name>/Portfile`,
//! verifies the SHA-256 against the value committed to the Portfile, applies
//! any patches under `ports/<category>/<name>/patches/`, and cross-compiles
//! the port for `x86_64-linux-musl` using the m3OS musl toolchain. Outputs
//! (static libraries, headers, executables) are staged under
//! `target/port-stage/<name>/` for downstream consumption by:
//!   - dependent port builds (`-I<stage>/usr/local/include`, `-L<stage>/usr/local/lib`)
//!   - the ext2 disk image populator (mirrors the stage tree onto the data disk)
//!   - the `tui-app-smoke` regression gate (launches the resulting binaries)
//!
//! The build is idempotent + cached via a per-port `.stamp` file recording the
//! upstream URL, SHA, and a fingerprint of `Portfile + patches/` — any change
//! invalidates the cache.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
#[cfg(test)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

/// Returns the workspace root by walking up from CARGO_MANIFEST_DIR.
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("xtask/ should have a parent (workspace root)")
        .to_path_buf()
}

/// Minimal Portfile parser. Reads `KEY=value` (or `KEY="value with spaces"`)
/// lines and returns a map keyed by the field name. Comment lines (`#…`) and
/// blank lines are ignored.
fn parse_portfile(path: &Path) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    let content =
        fs::read_to_string(path).map_err(|e| format!("read Portfile {}: {e}", path.display()))?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim().to_string();
            let value = v.trim().trim_matches('"').to_string();
            map.insert(key, value);
        }
    }
    Ok(map)
}

/// Returns the directory of `ports/<category>/<name>/` by scanning the
/// host-side ports tree.
fn find_port_dir(name: &str) -> Option<PathBuf> {
    let root = workspace_root().join("ports");
    for category in &["lib", "util", "core", "doc", "lang", "math"] {
        let candidate = root.join(category).join(name);
        if candidate.join("Portfile").is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Compute SHA-256 of `path` as a lowercase hex string.
fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Locate a working musl cross-compiler on PATH.
///
/// Delegates to the shared [`crate::find_musl_cc`] probe so port builds
/// honour the same toolchain selection as the rest of xtask (multi-name
/// candidate list, env-override via `M3OS_MUSL_CC`, and the auto-generated
/// empty-stubs path for toolchains that ship without `libdl.a` /
/// `libpthread.a` / `librt.a`).  Without this delegation port builds
/// hit "C compiler cannot create executables" on Arch's
/// `musl-cross-tools`, raiden, or hand-built `musl-cross-make` — even
/// though every other m3OS userspace target builds fine on the same
/// toolchain.
fn musl_cc() -> Option<&'static str> {
    crate::find_musl_cc()
}

/// Extra LDFLAGS the port build must append to whatever `-static` /
/// `-L<stage>` flags it already passes.  Returns the stub-dir `-L` flag
/// when the active toolchain lacked the empty `libdl.a` /
/// `libpthread.a` / `librt.a` archives and xtask materialized them on
/// its behalf in `target/musl-stub-libs/`.
fn musl_extra_ldflags_joined() -> String {
    crate::musl_cc_extra_ldflags().join(" ")
}

/// Pair of cross-compiler + companion `ar` and `ranlib` to use for static
/// archives.
fn musl_toolchain() -> Option<(&'static str, String, String)> {
    let cc = musl_cc()?;
    let ar = match cc {
        "x86_64-linux-musl-gcc" => "x86_64-linux-musl-ar".to_string(),
        "x86_64-unknown-linux-musl-gcc" => "x86_64-unknown-linux-musl-ar".to_string(),
        "x86_64-unknown-linux-musl1.2-gcc" => "x86_64-unknown-linux-musl1.2-ar".to_string(),
        _ => "ar".to_string(),
    };
    let ranlib = match cc {
        "x86_64-linux-musl-gcc" => "x86_64-linux-musl-ranlib".to_string(),
        "x86_64-unknown-linux-musl-gcc" => "x86_64-unknown-linux-musl-ranlib".to_string(),
        "x86_64-unknown-linux-musl1.2-gcc" => "x86_64-unknown-linux-musl1.2-ranlib".to_string(),
        _ => "ranlib".to_string(),
    };
    // Fall back to host `ar`/`ranlib` if the cross variants are missing —
    // static archives are ELF-target-agnostic so this is correct.
    let ar = if Command::new(&ar)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        ar
    } else {
        "ar".to_string()
    };
    let ranlib = if Command::new(&ranlib)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        ranlib
    } else {
        "ranlib".to_string()
    };
    Some((cc, ar, ranlib))
}

/// Compute a stable fingerprint of the Portfile + every file under
/// `patches/` + this `port_build.rs` source file (so changes to the build
/// recipe invalidate the cache without manual intervention). Used as the
/// cache-invalidation key.
fn port_fingerprint(port_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    let portfile = port_dir.join("Portfile");
    if let Ok(bytes) = fs::read(&portfile) {
        hasher.update(b"Portfile\0");
        hasher.update(&bytes);
    }
    let patches = port_dir.join("patches");
    if let Ok(entries) = fs::read_dir(&patches) {
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if let Some(name) = p.file_name() {
                hasher.update(name.to_string_lossy().as_bytes());
                hasher.update(b"\0");
            }
            if let Ok(bytes) = fs::read(&p) {
                hasher.update(&bytes);
            }
        }
    }
    // Include the port_build.rs source content so editing the build
    // recipe re-runs every staged port on the next invocation.
    hasher.update(b"\0port_build.rs\0");
    hasher.update(include_bytes!("port_build.rs"));
    format!("{:x}", hasher.finalize())
}

/// Compute a stable, portable recipe-identity digest for a port: the Portfile
/// content plus every file under `patches/`. Unlike [`port_fingerprint`] this
/// deliberately **excludes** the `port_build.rs` source bytes, so editing an
/// *unrelated* port recipe does not invalidate this port's cached `.m3pkg`.
///
/// The configure flags that live in the Rust `build_*` function (not the
/// Portfile) are folded into the key separately, via [`build_recipe_id`] in
/// [`package_key`] — so a flag change there DOES self-invalidate this port's
/// artifact without over-invalidating every other port.
fn recipe_digest(port_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    let portfile = port_dir.join("Portfile");
    if let Ok(bytes) = fs::read(&portfile) {
        hasher.update(b"Portfile\0");
        hasher.update(&bytes);
    }
    let patches = port_dir.join("patches");
    // Hash *every file under `patches/`* (the documented contract), recursing
    // into nested subdirectories. Each file is keyed by its path **relative to
    // `patches/`** rather than its bare name, so (a) a patch in a nested subdir
    // still invalidates the digest when its contents change, and (b) two
    // same-named patches in different subdirs do not collide. Directory entries
    // themselves are never hashed — only their contained files. Sorting by the
    // relative path makes the digest deterministic regardless of read_dir order.
    //
    // For the current flat `patches/` trees (one level, no subdirs) the relative
    // path equals the bare file name, so this is byte-identical to the previous
    // immediate-`read_dir` digest and does not invalidate any warmed pkgcache.
    let mut patch_files: Vec<(PathBuf, PathBuf)> = Vec::new(); // (relative, absolute)
    collect_files_rel(&patches, &patches, &mut patch_files);
    patch_files.sort_by(|a, b| a.0.cmp(&b.0));
    for (rel, abs) in patch_files {
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        if let Ok(bytes) = fs::read(&abs) {
            hasher.update(&bytes);
        }
    }
    format!("{:x}", hasher.finalize())
}

/// Recursively collect every *file* under `dir`, recording each as a
/// `(path_relative_to_base, absolute_path)` pair. Directory entries themselves
/// are not recorded — only their contained files — so [`recipe_digest`] can hash
/// "every file under `patches/`" (including nested subdirectories) without
/// hashing bare directory names. A missing/unreadable directory yields nothing.
/// A symlink is treated as a file (never recursed into), so a directory-target
/// symlink cannot create a recursion cycle: `fs::read` then hashes a file
/// target's bytes, or silently contributes nothing for a directory target.
fn collect_files_rel(base: &Path, dir: &Path, out: &mut Vec<(PathBuf, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            collect_files_rel(base, &path, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push((rel.to_path_buf(), path));
        }
    }
}

/// Phase 85a (A.1) — portable, content-addressed package key for a port.
///
/// Generalizes [`port_fingerprint`] into a key that is reusable across machines
/// and a moved tree: it folds in the source tarball SHA-256 (from the Portfile),
/// the resolved musl toolchain identity, the recipe digest (Portfile + patches),
/// and the sorted package keys of direct build dependencies — but **not** any
/// absolute/`target` path nor the `port_build.rs` source bytes. See
/// [`pkg_format::compute_package_key`] for the in/out-of-key contract.
pub fn package_key(
    port_dir: &Path,
    toolchain_id: &str,
    dep_keys: &[String],
) -> Result<String, String> {
    let meta = parse_portfile(&port_dir.join("Portfile"))?;
    let tarball_sha = meta
        .get("SHA256")
        .cloned()
        .ok_or_else(|| format!("Portfile {} missing SHA256", port_dir.display()))?;
    // build_flags = recipe digest (Portfile + patches) PLUS the configure-flag
    // identity from the Rust build_* function (which is not in the Portfile).
    // Folding the latter in means changing a port's configure flags invalidates
    // its cached .m3pkg — closing the stale-artifact gap that existed when only
    // the Portfile was hashed. The port name is the Portfile's directory name.
    let port_name = port_dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let build_flags = format!(
        "{}|recipe={}",
        recipe_digest(port_dir),
        build_recipe_id(port_name)
    );
    Ok(pkg_format::compute_package_key(
        &tarball_sha,
        toolchain_id,
        &build_flags,
        dep_keys,
    ))
}

/// A stable identity for a port's configure flags as defined in its Rust
/// `build_*` function (the part of the recipe that does **not** live in the
/// Portfile, so [`recipe_digest`] cannot see it). Folded into the content key
/// by [`package_key`].
///
/// ⚠ **Contract:** when you change a port's `build_*` configure flags (or add a
/// new host-built port), update its arm here so the change invalidates the
/// cached `.m3pkg`. This is the real, wired replacement for the never-parsed
/// `BUILD_FLAGS=` Portfile field referenced by earlier drafts. The strings need
/// only be *stable + distinct per flag-set* — transcribing the actual configure
/// args keeps them self-documenting. CFLAGS that embed build-host-absolute
/// include paths are intentionally omitted (they vary by machine and would
/// break the cache's cross-machine portability).
fn build_recipe_id(name: &str) -> &'static str {
    match name {
        "zlib" => "configure:--static --prefix=/usr/local;cflags:-O2 -fPIC",
        "ncurses" => {
            "configure:--without-shared --with-normal --with-termlib --without-debug \
             --without-ada --without-manpages --without-cxx-binding --without-tests \
             --disable-stripping --enable-overwrite --prefix=/usr/local --datadir=/usr/share \
             --host=x86_64-linux-musl;passes:narrow(--disable-widec),wide(--enable-widec);\
             cflags:-O2 -fPIC"
        }
        "libevent" => {
            "configure:--disable-shared --disable-openssl --disable-samples \
             --disable-debug-mode --disable-libevent-regress --host=x86_64-linux-musl \
             --prefix=/usr/local;cflags:-O2 -fPIC"
        }
        "less" => "configure:--with-regex=posix --host=x86_64-linux-musl --prefix=/usr/local",
        "htop" => {
            "configure:--disable-hwloc --enable-unicode --disable-affinity \
             --disable-capabilities --disable-sensors --disable-static --enable-static-link \
             --host=x86_64-linux-musl --prefix=/usr/local"
        }
        "tmux" => {
            "configure:--enable-utempter=no --enable-systemd=no --disable-utf8proc \
             --host=x86_64-linux-musl --prefix=/usr/local"
        }
        // Phase 85b — git's plain-Makefile build (no autotools configure): the
        // identity is the NO_* knob set + the static-zlib link + prefix=/usr +
        // the local-only post-install prune (see build_git). Bump the trailing
        // recipe-version marker whenever the knob set or prune list changes so
        // the cached .m3pkg self-invalidates.
        "git" => {
            "make:NO_CURL=1 NO_OPENSSL=1 NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 \
             NO_ICONV=1 NO_EXPAT=1 NO_REGEX=NeedsStartEnd NEEDS_LIBICONV= \
             SKIP_DASHED_BUILT_INS=YesPlease ZLIB_PATH=<zlib_stage>/usr/local \
             SHELL_PATH=/bin/sh prefix=/usr;cflags:-O2;\
             prune=scalar,git-shell,upload-pack,receive-pack,upload-archive,imap-send,http-backend,daemon,sh-i18n--envsubst;recipe-v=3"
        }
        // Phase 85c — CPython's two-stage musl cross build. The identity is the
        // cross-configure flag set + the ac_cv_* cross cache answers + the
        // pkg-config isolation (empty PKG_CONFIG_LIBDIR → host libb2 hidden →
        // vendored `_blake2`) + the `py_cv_module_*=n/a` deferred-module set
        // (incl. the `xxlimited*` shared-only demos) + the staged-tree prune list
        // (incl. the `/usr/include/python3.12` headers) +
        // the checksharedmods neuter. Bump recipe-v whenever any of these change
        // so the cached .m3pkg self-invalidates. (The pinned CPython version +
        // its tarball SHA-256 are folded in separately via the Portfile digest.)
        // The `ncurses-6.5` token pins the BUILD-only curses dependency (which is
        // intentionally absent from `port_deps`, so its key is NOT folded in
        // automatically): bump it in lockstep if ports/lib/ncurses's version or
        // build recipe changes, so a stale curses link self-invalidates.
        "python" => {
            "two-stage-STATIC:host(--disable-test-modules --without-ensurepip)+\
             cross(MODULE_BUILDTYPE=static LDFLAGS=-static --host=x86_64-linux-musl \
             --build=$(cc -dumpmachine) --with-build-python=<host> --disable-shared \
             --disable-ipv6 --without-ensurepip --without-pymalloc --disable-test-modules \
             --prefix=/usr);ac_cv:_dev_ptmx=no,_dev_ptc=no,buggy_getaddrinfo=no;\
             pkgconfig=isolated(empty-PKG_CONFIG_LIBDIR->host-libb2-hidden->vendored-_blake2);\
             zlib:ZLIB_CFLAGS/ZLIB_LIBS=<staged -fPIC libz.a>;\
             curses(build-only,ncurses-6.5):CURSES_CFLAGS/CURSES_LIBS/PANEL_LIBS=\
             <staged wide ncurses libncursesw/libtinfow/libpanelw>;\
             na=_ctypes,_ctypes_test,_ssl,_hashlib,readline,\
             _sqlite3,_dbm,_gdbm,_tkinter,nis,_uuid,_bz2,_lzma,ossaudiodev,_crypt,xxlimited,xxlimited_35;\
             neuter=checksharedmods;\
             prune=libpython.a,pkgconfig,config-3.12,include/python3.12,python-config,ensurepip,idlelib,tkinter,\
             turtledemo,turtle.py,lib2to3,asyncio,2to3,idle3,lib-dynload,__pycache__;\
             freeze=stdlib->python312.zip(.pyc,STORED),keep=os.py;recipe-v=6"
        }
        // Phase 85d — Clang/LLVM/LLD's two-stage host-clang cross build. The
        // identity is: the host-clang cross flags (compiler-rt/lld/no-libgcc) +
        // the Stage-B runtimes config (self-contained libc++: abi + unwinder
        // merged) + the Stage-C size/target levers + the baked-in in-OS clang
        // defaults (lld/compiler-rt/libc++/libunwind/DEFAULT_SYSROOT) + the
        // resource-dir + bundled-sysroot staging. The `clang-host` token marks
        // the recipe SHAPE (a host-clang cross build); the ACTUAL host compiler
        // version is NOT pinned here — `toolchain_id()` only sees the musl-gcc
        // wrapper, so `compute_port_key` folds the real host clang/clang++
        // `--version` (via `host_cxx_toolchain_id`) into this port's key so two
        // host clangs cannot collide on one cached `.m3pkg`. Bump recipe-v
        // whenever the FLAGS below change so the cached .m3pkg self-invalidates.
        // (The pinned LLVM version + tarball SHA-256 are folded in separately via
        // the Portfile digest.)
        "llvm" => {
            "host-clang-cross(clang-host;target=x86_64-linux-musl;-rtlib=compiler-rt \
             -unwindlib=none -fuse-ld=lld);\
             stageB-runtimes(libcxx;libcxxabi;libunwind;compiler-rt-builtins;\
             MUSL_LIBC=ON;self-contained-libc++:STATICALLY_LINK_ABI+UNWINDER=ON);\
             stageC(LLVM_ENABLE_PROJECTS=clang;lld;TARGETS=X86;MinSizeRel;\
             THREADS=OFF;ZLIB/ZSTD/TERMINFO/LIBXML2/LIBEDIT=OFF;RTTI/EH=OFF;\
             INCLUDE_TESTS/BENCHMARKS/EXAMPLES/UTILS=OFF;\
             CLANG_ENABLE_STATIC_ANALYZER/ARCMT=OFF;static;\
             DEFAULT_LINKER=lld;DEFAULT_RTLIB=compiler-rt;DEFAULT_CXX_STDLIB=libc++;\
             DEFAULT_UNWINDLIB=libunwind;DEFAULT_SYSROOT=/usr/lib/clang-sysroot;\
             INSTALL_TOOLCHAIN_ONLY=ON);\
             install=install-clang,install-clang-resource-headers,install-lld;\
             stage=resource-dir-builtins+bundled-usr-sysroot(musl+libc++)+clang++-symlink;\
             recipe-v=1"
        }
        _ => "",
    }
}

/// A stable identity string for the active musl toolchain, folded into the
/// package key so an artifact built with a different cross-gcc is not reused.
/// Best-effort: the compiler name plus its reported version line.
pub fn toolchain_id() -> String {
    let cc = musl_cc().unwrap_or("unknown-musl-cc");
    let version = Command::new(cc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .and_then(|s| s.lines().next().map(|l| l.to_string()))
        })
        .unwrap_or_default();
    format!("{cc}|{version}")
}

/// Identity of the HOST clang/clang++ used to cross-build the `llvm` port
/// (resolved exactly as [`build_llvm`] resolves them: `M3OS_LLVM_CLANG` /
/// `M3OS_LLVM_CLANGXX`, else `clang` / `clang++` on PATH). musl-tools ships no
/// C++ compiler, so the real cross-compiler for `llvm` is this host clang —
/// which [`toolchain_id`] (the musl-gcc wrapper) cannot see. Folding its actual
/// `--version` into the content key keeps the cache honest: artifacts built with
/// different host clang versions get different keys instead of colliding (a stale
/// cross-host pkgcache hit). Best-effort: each resolved compiler's name plus its
/// reported version line (empty when the compiler is absent — a machine that
/// cannot build `llvm` also cannot legitimately seal its `.m3pkg`).
pub fn host_cxx_toolchain_id() -> String {
    let first_version_line = |cc: &str| -> String {
        Command::new(cc)
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8(o.stdout)
                    .ok()
                    .and_then(|s| s.lines().next().map(|l| l.to_string()))
            })
            .unwrap_or_default()
    };
    let clang = std::env::var("M3OS_LLVM_CLANG").unwrap_or_else(|_| "clang".to_string());
    let clangxx = std::env::var("M3OS_LLVM_CLANGXX").unwrap_or_else(|_| "clang++".to_string());
    format!(
        "{clang}|{}|{clangxx}|{}",
        first_version_line(&clang),
        first_version_line(&clangxx)
    )
}

/// Fetch the upstream tarball into `cache_dir/<basename>` if missing or
/// SHA-mismatched. Returns the cached path on success.
fn fetch_tarball(url: &str, sha: &str, cache_dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(cache_dir).map_err(|e| format!("mkdir cache: {e}"))?;
    let basename = url
        .rsplit('/')
        .next()
        .ok_or_else(|| format!("malformed URL: {url}"))?;
    let dest = cache_dir.join(basename);
    let sha_prefix = sha_log_prefix(sha);
    if dest.exists() {
        if let Ok(actual) = sha256_file(&dest) {
            if actual == sha {
                println!("ports: cache hit {} (sha {})", dest.display(), sha_prefix);
                return Ok(dest);
            }
            println!(
                "ports: cached {} has stale SHA {} (expected {}) — re-fetching",
                dest.display(),
                sha_log_prefix(&actual),
                sha_prefix
            );
            let _ = fs::remove_file(&dest);
        }
    }
    println!("ports: downloading {url}");
    let status = Command::new("curl")
        .args(["-sSL", "--fail", "-o", dest.to_str().unwrap(), url])
        .status()
        .map_err(|e| format!("curl spawn: {e}"))?;
    if !status.success() {
        return Err(format!("curl exited {status}"));
    }
    let actual = sha256_file(&dest).map_err(|e| format!("sha256: {e}"))?;
    if actual != sha {
        let _ = fs::remove_file(&dest);
        return Err(format!(
            "SHA mismatch for {url}: expected {sha}, got {actual}"
        ));
    }
    println!("ports: verified {basename} (sha {})", sha_prefix);
    Ok(dest)
}

/// Borrow the first 16 hex chars of a SHA-256 for log output.  Falls back
/// to the whole string if shorter — protects against panics on
/// unexpectedly-formatted Portfile checksums.
fn sha_log_prefix(sha: &str) -> &str {
    sha.get(..16).unwrap_or(sha)
}

/// Extract `tarball` into `work_dir`, replacing any prior extraction. The
/// top-level extracted directory is returned (we assume the tarball follows
/// the convention of a single top-level dir named `<name>-<version>`).
fn extract_tarball(tarball: &Path, work_dir: &Path) -> Result<PathBuf, String> {
    if work_dir.exists() {
        fs::remove_dir_all(work_dir).map_err(|e| format!("rm -rf work_dir: {e}"))?;
    }
    fs::create_dir_all(work_dir).map_err(|e| format!("mkdir work_dir: {e}"))?;
    let tar_flag = if tarball.extension().is_some_and(|e| e == "xz") {
        "-xJf"
    } else {
        "-xzf"
    };
    let status = Command::new("tar")
        .args([
            tar_flag,
            tarball.to_str().unwrap(),
            "-C",
            work_dir.to_str().unwrap(),
        ])
        .status()
        .map_err(|e| format!("tar spawn: {e}"))?;
    if !status.success() {
        return Err(format!("tar exited {status}"));
    }
    // Find the single top-level directory.
    let mut entries = fs::read_dir(work_dir)
        .map_err(|e| format!("read_dir: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir());
    let first = entries
        .next()
        .ok_or_else(|| format!("no directory found in extracted {}", tarball.display()))?;
    if entries.next().is_some() {
        return Err(format!(
            "tarball {} has multiple top-level dirs — refusing to guess",
            tarball.display()
        ));
    }
    Ok(first)
}

/// Apply every `*.patch` under `patches_dir` to `src_dir` using `patch -p1`.
/// README.md and .gitkeep files are ignored.
fn apply_patches(patches_dir: &Path, src_dir: &Path) -> Result<usize, String> {
    if !patches_dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0;
    let mut patches: Vec<PathBuf> = fs::read_dir(patches_dir)
        .map_err(|e| format!("read_dir: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "patch"))
        .collect();
    patches.sort();
    for patch in patches {
        let input = File::open(&patch).map_err(|e| format!("open patch {:?}: {e}", patch))?;
        let status = Command::new("patch")
            .args(["-p1", "-d", src_dir.to_str().unwrap()])
            .stdin(Stdio::from(input))
            .status()
            .map_err(|e| format!("patch spawn: {e}"))?;
        if !status.success() {
            return Err(format!("patch {} failed", patch.display()));
        }
        count += 1;
    }
    Ok(count)
}

/// Phase 85a (B.1) — direct build dependencies for a port, used by
/// `port_build` to compute `dep_keys` for [`package_key`].
///
/// Only one level of dependencies is listed here; ports with no deps
/// return an empty slice. Every port currently depended upon (`ncurses`,
/// `libevent`) is itself a leaf with no build deps, so `compute_port_key`'s
/// non-recursive key computation is correct as written. Introducing a
/// *transitive* build dependency would require `compute_port_key` to recurse
/// (folding each dep's own resolved dep-keys into its key); this function's
/// `&[&str]` return type would not need to change, but the key computation
/// would — it does not handle deep chains today.
pub fn port_deps(name: &str) -> &'static [&'static str] {
    match name {
        "less" => &["ncurses"],
        "htop" => &["ncurses"],
        // Order is irrelevant to the content key (deps are sorted in
        // `compute_package_key`); it only sets the install sequence the in-OS
        // solver follows — libevent (a small static lib) before the heavyweight
        // ncurses terminfo DB.
        "tmux" => &["libevent", "ncurses"],
        // Phase 85b — git's one mandatory dependency is zlib (object/pack
        // compression). zlib is a leaf with no build deps of its own, so
        // `compute_port_key`'s non-recursive dep-key computation is correct.
        "git" => &["zlib"],
        // Phase 85c — CPython's `zlib`/`gzip`/`zipfile` extensions link the
        // staged (-fPIC) libz.a. zlib is a leaf, so the non-recursive dep-key
        // computation is correct here too. NOTE: ncurses (for `_curses`/
        // `_curses_panel`) is a BUILD-only dependency, deliberately NOT listed
        // here — it is built explicitly by `build_python_port` and linked
        // *statically* into the interpreter, so nothing needs to `pkg install`
        // it at runtime (and its multi-thousand-file terminfo DB would blow the
        // gate's install budget over m3OS's slow VFS — terminfo is already
        // pre-installed via the phase-69d port mirror anyway). The ncurses
        // version pin is folded into `build_recipe_id("python")` instead.
        "python" => &["zlib"],
        // Phase 85d — Clang/LLVM/LLD has NO runtime deps: zlib/zstd/terminfo are
        // all disabled (LLVM_ENABLE_*=OFF) and the C++ runtime is statically
        // linked into the toolchain + bundled in the .m3pkg's sysroot, so in-OS
        // `pkg install clang` pulls nothing else.
        "llvm" => &[],
        _ => &[],
    }
}

/// Phase 85a (D.1 / solver) — the `VERSION` field from a port's Portfile, used
/// by the image populator to write the `/usr/pkg/<name>.meta` sidecar the in-OS
/// dependency solver reads. Returns `None` if the port or field is absent.
pub fn port_version(name: &str) -> Option<String> {
    let port_dir = find_port_dir(name)?;
    let meta = parse_portfile(&port_dir.join("Portfile")).ok()?;
    meta.get("VERSION").cloned()
}

/// Phase 85a (B.2) — compute the portable content key for `name` (the port at
/// `port_dir`), folding in the active toolchain identity and the resolved
/// package keys of its direct dependencies. Shared by the resolve-before-build
/// path and [`pkgcache_artifact_path`] so the key is computed one way only.
fn compute_port_key(name: &str, port_dir: &Path) -> Result<String, String> {
    // The `llvm` port's real cross-compiler is the HOST clang (musl-tools has no
    // C++ compiler), which `toolchain_id()` (the musl-gcc wrapper) does not see.
    // Fold the actual host clang/clang++ `--version` into the key so two host
    // clangs do not collide on one content-addressed `.m3pkg` (the static recipe
    // token in `build_recipe_id` cannot carry a per-host version). Other ports are
    // built entirely by the musl toolchain, so their key is unchanged.
    let tc = if name == "llvm" {
        format!("{}|host-cxx={}", toolchain_id(), host_cxx_toolchain_id())
    } else {
        toolchain_id()
    };
    let dep_keys: Vec<String> = port_deps(name)
        .iter()
        .map(|dep| {
            let dep_dir = find_port_dir(dep)
                .ok_or_else(|| format!("dep port {dep} not found in ports/ tree"))?;
            // Every current dep is a leaf (ncurses/libevent have no build deps
            // of their own), so an empty dep-key slice computes the correct key.
            // A *transitive* dep would instead need its own resolved dep-keys
            // folded in here (i.e. recurse via `compute_port_key`); that is not
            // handled yet because no such chain exists in the ports tree.
            package_key(&dep_dir, &tc, &[])
        })
        .collect::<Result<_, String>>()?;
    package_key(port_dir, &tc, &dep_keys)
}

/// Phase 85a (D.1) — the `target/pkgcache/<key>.m3pkg` path a built `name` would
/// seal to. Used by the image populator to locate each port's sealed artifact
/// (to bundle into `/usr/pkg/` and pre-install from). Returns the path whether
/// or not the file exists — callers check existence + [`pkg_format::verify`].
pub fn pkgcache_artifact_path(name: &str) -> Result<PathBuf, String> {
    let port_dir =
        find_port_dir(name).ok_or_else(|| format!("port {name} not found in ports/ tree"))?;
    let key = compute_port_key(name, &port_dir)?;
    Ok(workspace_root()
        .join("target/pkgcache")
        .join(format!("{key}.m3pkg")))
}

/// Phase 85a (B.1) — pack the DESTDIR-staged tree into a deterministic
/// `.m3pkg` artifact and write it atomically to `target/pkgcache/<key>.m3pkg`.
///
/// **Atomicity**: the bytes are first written to `<key>.m3pkg.tmp`, then
/// renamed into place, so a concurrent reader never sees a partial file.
///
/// `target/pkgcache/` is **build output** — `cargo xtask clean` (which
/// removes only the disk image) does NOT purge it. This is intentional:
/// the pkgcache must survive across `clean` cycles so that a warmed cache
/// on one build machine can be preserved (or archived / shared) without
/// rebuilding from source. Only a manual `rm -rf target/pkgcache/` or a
/// toolchain upgrade that changes the content key will invalidate an entry.
fn seal_package(name: &str, stage: &Path, key: &str) -> Result<(), String> {
    let root = workspace_root();
    let cache_dir = root.join("target/pkgcache");
    fs::create_dir_all(&cache_dir).map_err(|e| format!("mkdir pkgcache: {e}"))?;

    let artifact = cache_dir.join(format!("{key}.m3pkg"));
    let tmp = cache_dir.join(format!("{key}.m3pkg.tmp"));

    // Phase 85a (E.1 relocation contract) — strip symbol tables from ELF
    // executables / shared objects in the stage before packing, so the sealed
    // artifact (and the on-image pre-install) carries no debug/symbol bloat.
    // Mandatory for the multi-hundred-MB Clang artifact; a smaller win for the
    // ncurses-class binaries. Static archives (.a) and non-ELF files (terminfo,
    // headers, data) are left untouched so dependent links still resolve.
    strip_stage(stage);

    let bytes =
        pkg_format::pack(stage).map_err(|e| format!("seal_package({name}): pack failed: {e}"))?;

    // Atomic write: write to .tmp then rename so readers never see partial data.
    fs::write(&tmp, &bytes).map_err(|e| format!("seal_package({name}): write tmp: {e}"))?;
    fs::rename(&tmp, &artifact)
        .map_err(|e| format!("seal_package({name}): rename into place: {e}"))?;

    // Record the key in a sibling file (NOT inside `stage`) for diagnostics, so
    // the cache metadata never lands inside a packed artifact (and thus never
    // ends up at `/.pkgkey` on a future `pkg install`). Does not affect cache
    // lookup (the key is always recomputed from Portfile + toolchain + deps).
    if let Some(stage_root) = stage.parent() {
        let _ = fs::write(stage_root.join(format!("{name}.pkgkey")), key);
    }

    println!(
        "PKGCACHE: sealed {name} → {} ({} bytes)",
        artifact.display(),
        bytes.len()
    );
    Ok(())
}

/// Recursively strip ELF executables and shared objects under `dir` in place
/// (best-effort). Only files whose first four bytes are the ELF magic are
/// touched, so static archives (`!<arch>` magic), terminfo entries, headers,
/// and scripts are left alone — keeping dependent links intact. Failures are
/// ignored: a missing `strip` or an unstrippable file must not abort the build.
fn strip_stage(dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if fs::symlink_metadata(&path)
            .map(|m| m.is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        if path.is_dir() {
            strip_stage(&path);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let mut magic = [0u8; 4];
        let is_elf = File::open(&path)
            .and_then(|mut f| f.read(&mut magic))
            .map(|n| n == 4 && &magic == b"\x7fELF")
            .unwrap_or(false);
        if is_elf {
            let _ = Command::new("strip")
                .arg(&path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Top-level entry point invoked by `cargo xtask port build <name>`.
pub fn cmd_port_build(name: &str) -> i32 {
    match port_build(name) {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("port build {name}: {msg}");
            1
        }
    }
}

fn port_build(name: &str) -> Result<(), String> {
    let port_dir =
        find_port_dir(name).ok_or_else(|| format!("port {name} not found in ports/ tree"))?;
    let portfile_path = port_dir.join("Portfile");
    let meta = parse_portfile(&portfile_path)?;
    let url = meta
        .get("URL")
        .cloned()
        .ok_or_else(|| format!("Portfile {} missing URL", portfile_path.display()))?;
    let sha = meta
        .get("SHA256")
        .cloned()
        .ok_or_else(|| format!("Portfile {} missing SHA256", portfile_path.display()))?;
    let version = meta.get("VERSION").cloned().unwrap_or_default();

    let root = workspace_root();
    let cache_dir = root.join("target/port-src");
    let work_root = root.join("target/port-build");
    let stage_root = root.join("target/port-stage");
    let stage = stage_root.join(name);
    let work = work_root.join(name);

    fs::create_dir_all(&stage_root).map_err(|e| format!("mkdir stage_root: {e}"))?;
    fs::create_dir_all(&work_root).map_err(|e| format!("mkdir work_root: {e}"))?;

    // Phase 85a (B.2) — resolve-before-build: if a pkgcache artifact for the
    // portable content key already exists and verifies, install directly from
    // it — skipping configure/make/install entirely. This is the "never rebuild"
    // half of the pkgcache (complement of B.1's "build once").
    //
    // The `.stamp` (same-machine fast path) lives in a SIBLING file
    // `target/port-stage/<name>.stamp`, NOT inside `stage`, so the staged tree
    // packed into the `.m3pkg` is pure package content (no cache metadata leaks
    // into the artifact or onto `/` at install time).
    let key = compute_port_key(name, &port_dir)?;
    let stamp = stage_root.join(format!("{name}.stamp"));
    let pkgcache_artifact = root.join("target/pkgcache").join(format!("{key}.m3pkg"));
    if pkgcache_artifact.exists() {
        if let Ok(bytes) = fs::read(&pkgcache_artifact) {
            if pkg_format::verify(&bytes) {
                // Cache hit — materialize the stage from the artifact.
                let short_key = key.get(..16).unwrap_or(&key);
                println!("PKGCACHE: hit {key}");
                println!(
                    "ports: {name} pkgcache hit (key {short_key}…), zero compiler invocations"
                );

                // Reset the stage dir and unpack.
                let _ = fs::remove_dir_all(&stage);
                fs::create_dir_all(&stage).map_err(|e| format!("mkdir stage (cache hit): {e}"))?;
                pkg_format::unpack(&bytes, &stage)
                    .map_err(|e| format!("unpack pkgcache({name}): {e}"))?;

                // Prime the same-machine `.stamp` fast-path so subsequent calls
                // skip even the key computation on the same machine.
                let fingerprint = port_fingerprint(&port_dir);
                let _ = fs::write(&stamp, &fingerprint);

                return Ok(());
            }
        }
    }
    // Cache miss — log it, then fall through to the same-machine stamp check
    // and the real build.
    println!("PKGCACHE: miss {key} (building)");

    let fingerprint = port_fingerprint(&port_dir);
    let cached_stamp = fs::read_to_string(&stamp).unwrap_or_default();
    if cached_stamp.trim() == fingerprint && stage.is_dir() {
        // Same-machine inner-loop fast path: stage is current per local stamp.
        // Seal it into the pkgcache so subsequent (portable) lookups hit too.
        println!("ports: {name}-{version} stage is up-to-date (fingerprint match)");
        if let Err(e) = seal_package(name, &stage, &key) {
            // Sealing is best-effort; a failure here should not abort the build.
            eprintln!("pkgcache: warning: seal failed for {name}: {e}");
        }
        return Ok(());
    }

    let tarball = fetch_tarball(&url, &sha, &cache_dir)?;
    let extracted = extract_tarball(&tarball, &work)?;
    let n = apply_patches(&port_dir.join("patches"), &extracted)?;
    if n > 0 {
        println!("ports: applied {n} patch(es) to {}", extracted.display());
    }

    // Reset stage dir.
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage).map_err(|e| format!("mkdir stage: {e}"))?;

    let toolchain = musl_toolchain().ok_or_else(|| {
        "no musl cross-compiler found on PATH (install musl-tools or \
                                                musl-gcc-cross-bin)"
            .to_string()
    })?;

    let ncurses_stage = stage_root.join("ncurses");
    let libevent_stage = stage_root.join("libevent");
    let zlib_stage = stage_root.join("zlib");

    match name {
        "ncurses" => build_ncurses(&extracted, &stage, &toolchain)?,
        "libevent" => build_libevent(&extracted, &stage, &toolchain)?,
        "zlib" => build_zlib(&extracted, &stage, &toolchain)?,
        "less" => build_less(&extracted, &stage, &toolchain, &ncurses_stage)?,
        "htop" => build_htop(&extracted, &stage, &toolchain, &ncurses_stage)?,
        "tmux" => build_tmux(
            &extracted,
            &stage,
            &toolchain,
            &ncurses_stage,
            &libevent_stage,
        )?,
        "git" => build_git(&extracted, &stage, &toolchain, &zlib_stage)?,
        "python" => build_python(&extracted, &stage, &toolchain, &zlib_stage, &ncurses_stage)?,
        "llvm" => build_llvm(&extracted, &stage, &toolchain)?,
        _ => return Err(format!("no host build recipe for port {name}")),
    }

    // Phase 85a (B.1) — seal the built stage into the pkgcache BEFORE writing
    // the `.stamp`, so a subsequent run on this machine does not race.
    seal_package(name, &stage, &key)?;

    fs::write(&stamp, &fingerprint).map_err(|e| format!("write stamp: {e}"))?;
    println!(
        "ports: {name}-{version} build complete (staged at {})",
        stage.display()
    );
    Ok(())
}

/// Run a command, propagating stdout/stderr to the caller. Returns Err on
/// non-zero exit.
fn run(cmd: &mut Command, label: &str) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("{label}: spawn failed: {e}"))?;
    if !status.success() {
        return Err(format!("{label}: exited {status}"));
    }
    Ok(())
}

/// Cross-compile ncurses 6.5 in two passes (narrow + wide).
///
/// `--prefix=/usr/local` + `--datadir=/usr/share` bake the *runtime* paths
/// the binaries query at execution time; the build is staged under
/// `target/port-stage/ncurses/{usr/local,usr/share}` via `DESTDIR=$STAGE`
/// so the on-target file system lays out as:
///   /usr/local/{bin,lib,include}/...      (curses ABI + tic / infocmp / tput / clear)
///   /usr/share/terminfo/...               (compiled terminfo database)
fn build_ncurses(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
) -> Result<(), String> {
    let common_configure = [
        "--without-shared".to_string(),
        "--with-normal".to_string(),
        "--with-termlib".to_string(),
        "--without-debug".to_string(),
        "--without-ada".to_string(),
        "--without-manpages".to_string(),
        "--without-cxx-binding".to_string(),
        "--without-tests".to_string(),
        "--disable-stripping".to_string(),
        "--enable-overwrite".to_string(),
        "--prefix=/usr/local".to_string(),
        "--datadir=/usr/share".to_string(),
        format!("--host={}", "x86_64-linux-musl"),
    ];

    let extra_ld = musl_extra_ldflags_joined();
    let ldflags_base = if extra_ld.is_empty() {
        "-static".to_string()
    } else {
        format!("-static {extra_ld}")
    };

    for (pass_label, extra) in &[
        ("narrow", vec!["--disable-widec"]),
        ("wide", vec!["--enable-widec"]),
    ] {
        println!("ncurses: configure pass ({pass_label})");
        // Always start each pass from a clean source dir; ncurses 6.x has
        // a buggy `make distclean` that leaves stale config.cache entries.
        let _ = Command::new("make")
            .args(["-C", src.to_str().unwrap(), "distclean"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        let mut configure_cmd = Command::new("sh");
        configure_cmd
            .current_dir(src)
            .arg("./configure")
            .args(&common_configure)
            .args(extra)
            .env("CC", cc)
            .env("AR", ar)
            .env("RANLIB", ranlib)
            .env("CFLAGS", "-O2 -fPIC")
            .env("LDFLAGS", &ldflags_base);
        run(
            &mut configure_cmd,
            &format!("ncurses configure ({pass_label})"),
        )?;

        println!("ncurses: building ({pass_label})");
        let mut make_cmd = Command::new("make");
        make_cmd.current_dir(src).arg(format!("-j{}", num_jobs()));
        run(&mut make_cmd, &format!("ncurses make ({pass_label})"))?;

        println!("ncurses: installing to stage ({pass_label})");
        let mut install_cmd = Command::new("make");
        install_cmd
            .current_dir(src)
            .arg(format!("DESTDIR={}", stage.display()))
            .arg("install");
        run(&mut install_cmd, &format!("ncurses install ({pass_label})"))?;
    }

    // Sanity: ensure both libncurses.a and libncursesw.a exist.
    let lib_dir = stage.join("usr/local/lib");
    for required in &["libncurses.a", "libncursesw.a", "libtinfo.a", "libtinfow.a"] {
        let p = lib_dir.join(required);
        if !p.exists() {
            return Err(format!("ncurses build missing {}", p.display()));
        }
    }
    let infocmp = stage.join("usr/local/bin/infocmp");
    if !infocmp.exists() {
        return Err(format!("ncurses build missing {}", infocmp.display()));
    }
    // Terminfo database under /usr/share/terminfo (matches the path baked
    // into the curses runtime via --datadir).
    let terminfo = stage.join("usr/share/terminfo");
    if !terminfo.is_dir() {
        return Err(format!("ncurses build missing {}", terminfo.display()));
    }

    // Phase 69 Track A.2 — compile the m3os-term entry into the staged
    // terminfo database so apps launched on m3OS with TERM=m3os-term find
    // the right capability set. `-x` permits the extended `BE`/`BD`/`XM`
    // bracketed-paste and SGR-mouse hints to land alongside the standard
    // capability set.
    let m3os_ti = workspace_root().join("xtask/terminfo/m3os-term.ti");
    if m3os_ti.is_file() {
        let mut tic_cmd = Command::new(stage.join("usr/local/bin/tic"));
        tic_cmd.args(["-x", "-o"]).arg(&terminfo).arg(&m3os_ti);
        run(&mut tic_cmd, "tic m3os-term.ti")?;
        if !terminfo.join("m/m3os-term").exists() {
            return Err("tic produced no m3os-term entry".to_string());
        }
        println!("ncurses: installed m3os-term entry into staged terminfo db");
    }

    println!("ncurses: produced libncurses.a, libncursesw.a, libtinfo[w].a, infocmp, terminfo db");

    Ok(())
}

fn build_libevent(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
) -> Result<(), String> {
    let stage_prefix = stage.join("usr/local");
    fs::create_dir_all(&stage_prefix).map_err(|e| format!("mkdir: {e}"))?;

    let extra_ld = musl_extra_ldflags_joined();
    let ldflags = if extra_ld.is_empty() {
        "-static".to_string()
    } else {
        format!("-static {extra_ld}")
    };

    let mut configure_cmd = Command::new("sh");
    configure_cmd
        .current_dir(src)
        .arg("./configure")
        .args([
            "--disable-shared",
            "--disable-openssl",
            "--disable-samples",
            "--disable-debug-mode",
            "--disable-libevent-regress",
            "--host=x86_64-linux-musl",
        ])
        .arg("--prefix=/usr/local")
        .env("CC", cc)
        .env("AR", ar)
        .env("RANLIB", ranlib)
        .env("CFLAGS", "-O2 -fPIC")
        .env("LDFLAGS", &ldflags);
    run(&mut configure_cmd, "libevent configure")?;

    let mut make_cmd = Command::new("make");
    make_cmd.current_dir(src).arg(format!("-j{}", num_jobs()));
    run(&mut make_cmd, "libevent make")?;

    let mut install_cmd = Command::new("make");
    install_cmd
        .current_dir(src)
        .arg("install")
        .arg(format!("DESTDIR={}", stage.display()));
    run(&mut install_cmd, "libevent install")?;

    let lib = stage_prefix.join("lib/libevent.a");
    if !lib.exists() {
        return Err(format!("libevent build missing {}", lib.display()));
    }
    println!("libevent: produced libevent.a");
    Ok(())
}

/// Phase 85a (D.2 follow-up) — cross-compile zlib 1.3.1 as a static library.
///
/// zlib ships a hand-written `configure` (not autotools), so there is no
/// `--host` flag; cross-compilation is selected purely through the `CC` / `AR` /
/// `RANLIB` environment. `--static` skips the shared object (m3OS links static),
/// `--prefix=/usr/local` bakes the runtime prefix, and `make install DESTDIR=`
/// stages `usr/local/{lib/libz.a, include/{zlib,zconf}.h, lib/pkgconfig/zlib.pc,
/// share/man}`. The resulting stage is sealed into a `.m3pkg` by the shared
/// pkgcache path exactly like the other ports.
fn build_zlib(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
) -> Result<(), String> {
    let stage_prefix = stage.join("usr/local");
    fs::create_dir_all(&stage_prefix).map_err(|e| format!("mkdir: {e}"))?;

    let mut configure_cmd = Command::new("sh");
    configure_cmd
        .current_dir(src)
        .arg("./configure")
        .arg("--static")
        .arg("--prefix=/usr/local")
        .env("CC", cc)
        .env("AR", ar)
        .env("RANLIB", ranlib)
        // -fPIC keeps the static libz.a position-independent so the one archive
        // links cleanly into either consumer regardless of PIE/PIC mode: the
        // Phase 85b static `git` executable, and the Phase 85c fully-static
        // CPython — where `zlib` is a *builtin* module compiled into the
        // interpreter, so there is no `lib-dynload/zlib.*.so` in the static
        // build. -fPIC is harmless for the non-PIE static links both produce.
        .env("CFLAGS", "-O2 -fPIC");
    run(&mut configure_cmd, "zlib configure")?;

    let mut make_cmd = Command::new("make");
    make_cmd.current_dir(src).arg(format!("-j{}", num_jobs()));
    run(&mut make_cmd, "zlib make")?;

    let mut install_cmd = Command::new("make");
    install_cmd
        .current_dir(src)
        .arg("install")
        .arg(format!("DESTDIR={}", stage.display()));
    run(&mut install_cmd, "zlib install")?;

    let lib = stage_prefix.join("lib/libz.a");
    if !lib.exists() {
        return Err(format!("zlib build missing {}", lib.display()));
    }
    println!(
        "zlib: produced /usr/local/lib/libz.a ({} bytes)",
        file_size(&lib)
    );
    Ok(())
}

fn build_less(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
    ncurses_stage: &Path,
) -> Result<(), String> {
    let ncurses_prefix = ncurses_stage.join("usr/local");
    if !ncurses_prefix.join("lib/libncurses.a").exists() {
        return Err(format!(
            "less build: ncurses stage not found at {}",
            ncurses_prefix.display()
        ));
    }
    let stage_prefix = stage.join("usr/local");
    fs::create_dir_all(&stage_prefix).map_err(|e| format!("mkdir: {e}"))?;

    let cflags = format!("-O2 -I{}/include", ncurses_prefix.display());
    let extra_ld = musl_extra_ldflags_joined();
    let ldflags = if extra_ld.is_empty() {
        format!("-static -L{}/lib", ncurses_prefix.display())
    } else {
        format!("-static -L{}/lib {extra_ld}", ncurses_prefix.display())
    };

    let mut configure_cmd = Command::new("sh");
    configure_cmd
        .current_dir(src)
        .arg("./configure")
        .args(["--with-regex=posix", "--host=x86_64-linux-musl"])
        .arg("--prefix=/usr/local")
        .env("CC", cc)
        .env("AR", ar)
        .env("RANLIB", ranlib)
        .env("CFLAGS", &cflags)
        .env("LDFLAGS", &ldflags)
        .env("LIBS", "-lncurses -ltinfo");
    run(&mut configure_cmd, "less configure")?;

    let mut make_cmd = Command::new("make");
    make_cmd.current_dir(src).arg(format!("-j{}", num_jobs()));
    run(&mut make_cmd, "less make")?;

    let mut install_cmd = Command::new("make");
    install_cmd
        .current_dir(src)
        .arg("install")
        .arg(format!("DESTDIR={}", stage.display()));
    run(&mut install_cmd, "less install")?;

    let bin = stage_prefix.join("bin/less");
    if !bin.exists() {
        return Err(format!("less build missing {}", bin.display()));
    }
    println!(
        "less: produced /usr/local/bin/less ({} bytes)",
        file_size(&bin)
    );
    Ok(())
}

fn build_htop(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
    ncurses_stage: &Path,
) -> Result<(), String> {
    let ncurses_prefix = ncurses_stage.join("usr/local");
    if !ncurses_prefix.join("lib/libncursesw.a").exists() {
        return Err(format!(
            "htop build: ncursesw stage not found at {}",
            ncurses_prefix.display()
        ));
    }
    let stage_prefix = stage.join("usr/local");
    fs::create_dir_all(&stage_prefix).map_err(|e| format!("mkdir: {e}"))?;

    // -idirafter /usr/include … lets musl-gcc find the host's Linux UAPI
    // headers (linux/capability.h, linux/sched.h, asm/types.h, …) without
    // overriding musl's own libc headers. htop's process-discovery reaches
    // into the raw capget() ABI even with --disable-capabilities, so the
    // kernel UAPI headers are a hard requirement of the build. We also
    // need the arch-specific asm/ directory because <linux/types.h>
    // pulls in <asm/types.h>.
    let arch_include = linux_uapi_arch_include();
    let cflags = format!(
        "-O2 -I{0}/include -I{0}/include/ncursesw -idirafter /usr/include -idirafter {1}",
        ncurses_prefix.display(),
        arch_include.display()
    );
    let extra_ld = musl_extra_ldflags_joined();
    let ldflags = if extra_ld.is_empty() {
        format!("-static -L{}/lib", ncurses_prefix.display())
    } else {
        format!("-static -L{}/lib {extra_ld}", ncurses_prefix.display())
    };

    // htop's configure auto-detects libncurses by linking against a
    // probe.  With `--with-termlib` on our ncurses build, both
    // `libncurses.a` (narrow) and `libncursesw.a` (wide) coexist —
    // alongside `libtinfo.a` (narrow tinfo) and `libtinfow.a` (wide
    // tinfo).  Without help, autoconf finds *both* tinfo variants and
    // appends the narrow `libtinfo.a` to the link line, which causes
    // a TERMTYPE/TERMTYPE2 layout mismatch at runtime: setupterm
    // populates `cur_term->type` (narrow) but the wide `termattrs_sp`
    // dereferences `cur_term->type2.Strings` (wide) — NULL → SIGSEGV.
    //
    // Set `CURSES_CFLAGS`/`CURSES_LIBS` so htop's configure honors them
    // verbatim and never even probes for a narrow tinfo.
    let curses_cflags = format!(
        "-I{0}/include -I{0}/include/ncursesw",
        ncurses_prefix.display()
    );
    let curses_libs = format!("-L{}/lib -lncursesw -ltinfow", ncurses_prefix.display());

    let mut configure_cmd = Command::new("sh");
    configure_cmd
        .current_dir(src)
        .arg("./configure")
        .args([
            "--disable-hwloc",
            "--enable-unicode",
            "--disable-affinity",
            "--disable-capabilities",
            "--disable-sensors",
            "--disable-static",
            "--enable-static-link",
            "--host=x86_64-linux-musl",
        ])
        .arg("--prefix=/usr/local")
        .env("CC", cc)
        .env("AR", ar)
        .env("RANLIB", ranlib)
        .env("CFLAGS", &cflags)
        .env("LDFLAGS", &ldflags)
        .env("CURSES_CFLAGS", &curses_cflags)
        .env("CURSES_LIBS", &curses_libs)
        .env("LIBS", "-lncursesw -ltinfow");
    run(&mut configure_cmd, "htop configure")?;

    let mut make_cmd = Command::new("make");
    make_cmd.current_dir(src).arg(format!("-j{}", num_jobs()));
    run(&mut make_cmd, "htop make")?;

    let mut install_cmd = Command::new("make");
    install_cmd
        .current_dir(src)
        .arg("install")
        .arg(format!("DESTDIR={}", stage.display()));
    run(&mut install_cmd, "htop install")?;

    let bin = stage_prefix.join("bin/htop");
    if !bin.exists() {
        return Err(format!("htop build missing {}", bin.display()));
    }
    println!(
        "htop: produced /usr/local/bin/htop ({} bytes)",
        file_size(&bin)
    );
    Ok(())
}

fn build_tmux(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
    ncurses_stage: &Path,
    libevent_stage: &Path,
) -> Result<(), String> {
    ensure_yacc()?;
    let ncurses_prefix = ncurses_stage.join("usr/local");
    let libevent_prefix = libevent_stage.join("usr/local");
    if !ncurses_prefix.join("lib/libncursesw.a").exists() {
        return Err(format!(
            "tmux build: ncursesw stage not found at {}",
            ncurses_prefix.display()
        ));
    }
    if !libevent_prefix.join("lib/libevent.a").exists() {
        return Err(format!(
            "tmux build: libevent stage not found at {}",
            libevent_prefix.display()
        ));
    }
    let stage_prefix = stage.join("usr/local");
    fs::create_dir_all(&stage_prefix).map_err(|e| format!("mkdir: {e}"))?;

    let cflags = format!(
        "-O2 -I{0}/include -I{0}/include/ncursesw -I{1}/include",
        ncurses_prefix.display(),
        libevent_prefix.display()
    );
    let extra_ld = musl_extra_ldflags_joined();
    let ldflags = if extra_ld.is_empty() {
        format!(
            "-static -L{}/lib -L{}/lib",
            ncurses_prefix.display(),
            libevent_prefix.display()
        )
    } else {
        format!(
            "-static -L{}/lib -L{}/lib {extra_ld}",
            ncurses_prefix.display(),
            libevent_prefix.display()
        )
    };

    // Same TERMTYPE/TERMTYPE2 layout-mismatch hazard as htop: tmux's
    // autoconf detects libtinfo without `w` suffix and mixes narrow
    // tinfo's setupterm against wide ncursesw's termattrs at runtime.
    // tmux honors `LIBTINFO_*` (terminfo half) and `LIBNCURSES_*`
    // (curses half) — we point both at the wide variants so tmux links
    // against `ncursesw` + `tinfow` consistently.
    let libtinfo_libs = format!("-L{}/lib -ltinfow", ncurses_prefix.display());
    let libncurses_libs = format!("-L{0}/lib -lncursesw -ltinfow", ncurses_prefix.display());
    let ncurses_cflags = format!(
        "-I{0}/include -I{0}/include/ncursesw",
        ncurses_prefix.display()
    );

    let mut configure_cmd = Command::new("sh");
    configure_cmd
        .current_dir(src)
        .arg("./configure")
        .args([
            "--enable-utempter=no",
            "--enable-systemd=no",
            "--disable-utf8proc",
            "--host=x86_64-linux-musl",
        ])
        .arg("--prefix=/usr/local")
        .env("CC", cc)
        .env("AR", ar)
        .env("RANLIB", ranlib)
        .env("CFLAGS", &cflags)
        .env("LDFLAGS", &ldflags)
        .env("LIBTINFO_LIBS", &libtinfo_libs)
        .env("LIBTINFO_CFLAGS", &ncurses_cflags)
        .env("LIBNCURSES_LIBS", &libncurses_libs)
        .env("LIBNCURSES_CFLAGS", &ncurses_cflags);
    run(&mut configure_cmd, "tmux configure")?;

    let mut make_cmd = Command::new("make");
    make_cmd.current_dir(src).arg(format!("-j{}", num_jobs()));
    run(&mut make_cmd, "tmux make")?;

    let mut install_cmd = Command::new("make");
    install_cmd
        .current_dir(src)
        .arg("install")
        .arg(format!("DESTDIR={}", stage.display()));
    run(&mut install_cmd, "tmux install")?;

    let bin = stage_prefix.join("bin/tmux");
    if !bin.exists() {
        return Err(format!("tmux build missing {}", bin.display()));
    }
    println!(
        "tmux: produced /usr/local/bin/tmux ({} bytes)",
        file_size(&bin)
    );
    Ok(())
}

/// Phase 85b — cross-build a local-only musl `git` (Stage 1: `NO_CURL`/
/// `NO_OPENSSL`), statically linked against the staged zlib, and DESTDIR-install
/// it at `prefix=/usr` into `stage`.
///
/// Unlike the ncurses-class ports, git has **no autotools `./configure`** — its
/// build is a plain Makefile driven entirely by `CC=<musl-gcc>`, so there is no
/// `--host` flag to pass; the cross is implied by the compiler. zlib (git's one
/// mandatory dependency) is consumed from `zlib_stage/usr/local` via git's
/// `ZLIB_PATH` Makefile knob plus explicit `-I`/`-L` flags.
///
/// `SKIP_DASHED_BUILT_INS=YesPlease` is essential here: git would otherwise
/// install ~100 dashed `libexec/git-core/git-<builtin>` **hardlinks** to the
/// main binary, and the `.m3pkg` packer ([`pkg_format::pack`]) stores file
/// *content* per path with no inode/hardlink dedup — so those hardlinks would
/// balloon the artifact by hundreds of MB. With this knob the dashed builtins
/// are not installed at all; every smoke subcommand (`init`/`add`/`commit`/
/// `log`/`diff`/`status`/`branch`/`merge`/`checkout`) is dispatched in-process
/// by the single `git` binary, so nothing is lost.
fn build_git(
    src: &Path,
    stage: &Path,
    (cc, ar, _ranlib): &(&'static str, String, String),
    zlib_stage: &Path,
) -> Result<(), String> {
    let zlib_prefix = zlib_stage.join("usr/local");
    if !zlib_prefix.join("lib/libz.a").exists() {
        return Err(format!(
            "git build: staged zlib not found at {} (build the zlib port first)",
            zlib_prefix.join("lib/libz.a").display()
        ));
    }
    let stage_prefix = stage.join("usr");
    fs::create_dir_all(&stage_prefix).map_err(|e| format!("mkdir: {e}"))?;

    let cflags = format!("-O2 -I{}/include", zlib_prefix.display());
    let extra_ld = musl_extra_ldflags_joined();
    let ldflags = if extra_ld.is_empty() {
        format!("-static -L{}/lib", zlib_prefix.display())
    } else {
        format!("-static -L{}/lib {extra_ld}", zlib_prefix.display())
    };

    // The NO_* knob set that carves git's minimal, dependency-light, offline
    // build (see ports/util/git/Portfile for the per-knob rationale). Passed as
    // `make VAR=val` arguments — the git Makefile convention. `prefix=/usr`
    // additionally special-cases git's system config to `/etc/gitconfig`.
    let common: Vec<String> = vec![
        format!("CC={cc}"),
        format!("AR={ar}"),
        "NO_CURL=1".to_string(),
        "NO_OPENSSL=1".to_string(),
        "NO_GETTEXT=1".to_string(),
        "NO_TCLTK=1".to_string(),
        "NO_PERL=1".to_string(),
        "NO_PYTHON=1".to_string(),
        "NO_ICONV=1".to_string(),
        "NO_EXPAT=1".to_string(),
        "NO_REGEX=NeedsStartEnd".to_string(),
        "NEEDS_LIBICONV=".to_string(),
        "SKIP_DASHED_BUILT_INS=YesPlease".to_string(),
        format!("ZLIB_PATH={}", zlib_prefix.display()),
        "SHELL_PATH=/bin/sh".to_string(),
        "prefix=/usr".to_string(),
        format!("CFLAGS={cflags}"),
        format!("LDFLAGS={ldflags}"),
    ];

    let mut make_cmd = Command::new("make");
    make_cmd.current_dir(src).arg(format!("-j{}", num_jobs()));
    for a in &common {
        make_cmd.arg(a);
    }
    make_cmd.arg("all");
    run(&mut make_cmd, "git make")?;

    let mut install_cmd = Command::new("make");
    install_cmd.current_dir(src);
    for a in &common {
        install_cmd.arg(a);
    }
    install_cmd
        .arg(format!("DESTDIR={}", stage.display()))
        .arg("install");
    run(&mut install_cmd, "git install")?;

    // ── Layout + the NO_CURL/NO_OPENSSL assertions ──────────────────────────
    let git_bin = stage_prefix.join("bin/git");
    if !git_bin.exists() {
        return Err(format!("git build missing {}", git_bin.display()));
    }
    let git_core = stage_prefix.join("libexec/git-core");
    if !git_core.is_dir() {
        return Err(format!(
            "git build missing libexec/git-core at {}",
            git_core.display()
        ));
    }
    let templates = stage_prefix.join("share/git-core/templates");
    if !templates.is_dir() {
        return Err(format!(
            "git build missing share/git-core/templates at {}",
            templates.display()
        ));
    }

    // Phase 85b is the **local-only** git core. git's default build also produces
    // several large (~2.3–3.7 MB each, statically linked) binaries that only
    // serve network / server / email / large-repo workflows — none of which is
    // in scope until Phase 86. Pruning them keeps the `.m3pkg` lean and the in-OS
    // `pkg install git` fast: every binary is a multi-MB write over the ring-3
    // VFS, and the no-dedup `.m3pkg` packer ([`pkg_format::pack`]) stores file
    // *content* per path, so the three server-side pack helpers under `bin/`
    // (`git-upload-pack`/`git-receive-pack`/`git-upload-archive`) — which install
    // as **hardlinks** to the 3.7 MB `git` binary — would each pack as a full
    // copy (~11 MB of pure duplication, the difference between a 19 MB and a
    // 7.4 MB artifact, and minutes of install time over the VFS). They are the
    // server half of clone/fetch/push (Phase 86); local single-repo work never
    // invokes them. (With `SKIP_DASHED_BUILT_INS` these pack helpers exist only
    // under `bin/`, not `libexec/git-core/`, so pruning `bin/` suffices.) There
    // is no per-tool `NO_*` Makefile knob for these, so they are removed
    // post-install (ignore-if-absent). git's in-process builtin dispatch is
    // unaffected — every smoke subcommand (init/add/commit/diff/log/branch/
    // merge/checkout/status) lives in the main `git` binary. The sealed artifact
    // is ~7.4 MB: `bin/git` + the standard exec-path `libexec/git-core/git`
    // copy + the shell-script helpers + templates (kept for general correctness;
    // the no-dedup packer is why even two `git` copies dominate the size).
    let prune = [
        ("bin", "scalar"), // large-monorepo manager (clone/fetch heavy)
        ("libexec", "scalar"),
        ("bin", "git-shell"), // restricted git-over-ssh server login shell
        ("libexec", "git-shell"),
        ("bin", "git-upload-pack"), // server side of fetch/clone (Phase 86)
        ("bin", "git-receive-pack"), // server side of push (Phase 86)
        ("bin", "git-upload-archive"), // server side of archive --remote (Phase 86)
        ("libexec", "git-imap-send"), // mails patches via IMAP
        ("libexec", "git-http-backend"), // dumb/smart-HTTP server CGI
        ("libexec", "git-daemon"),  // git:// protocol server daemon
        ("libexec", "git-sh-i18n--envsubst"), // i18n helper, moot under NO_GETTEXT
    ];
    for (where_, name_) in prune {
        let p = match where_ {
            "bin" => stage_prefix.join("bin").join(name_),
            _ => git_core.join(name_),
        };
        let _ = fs::remove_file(&p);
    }

    // NO_CURL ⇒ the curl-backed remote helpers are never built. Their presence
    // would mean HTTPS rode in unverified, so assert they are absent.
    for forbidden in [
        "git-remote-https",
        "git-remote-http",
        "git-http-fetch",
        "git-http-push",
    ] {
        if git_core.join(forbidden).exists() || stage_prefix.join("bin").join(forbidden).exists() {
            return Err(format!(
                "git build: {forbidden} present — NO_CURL did not take effect (HTTPS would ride in unverified)"
            ));
        }
    }
    // Belt-and-suspenders: the (still-unstripped) git binary must reference
    // neither a libcurl nor an OpenSSL API symbol. These exact symbol names only
    // appear when remote-curl.c / the OpenSSL paths are compiled + linked, so
    // their absence is a direct, toolchain-independent proof of NO_CURL/NO_OPENSSL.
    if binary_contains(&git_bin, b"curl_easy_perform")? {
        return Err(
            "git build: binary references libcurl (curl_easy_perform) — NO_CURL ineffective"
                .to_string(),
        );
    }
    if binary_contains(&git_bin, b"SSL_CTX_new")? {
        return Err(
            "git build: binary references OpenSSL (SSL_CTX_new) — NO_OPENSSL ineffective"
                .to_string(),
        );
    }

    println!(
        "git: produced /usr/bin/git ({} bytes, unstripped) + libexec/git-core + templates; \
         NO_CURL/NO_OPENSSL verified (no curl/OpenSSL linkage)",
        file_size(&git_bin)
    );
    Ok(())
}

/// The CPython `X.Y` series the port pins. Paths (`bin/python3.12`,
/// `lib/python3.12/…`) and the SOABI track this; bump it in lockstep with the
/// Portfile `VERSION` whenever the *minor* version changes (a patch bump —
/// 3.12.8 → 3.12.9 — keeps the same `X.Y` and needs no change here).
const PYTHON_XY: &str = "3.12";

/// The build-platform triple (`cc -dumpmachine`), passed as `--build=` so
/// autoconf enters cross-compile mode when it differs from `--host`.
fn build_machine_triple() -> String {
    for probe in ["cc", "gcc"] {
        if let Ok(out) = Command::new(probe).arg("-dumpmachine").output()
            && out.status.success()
            && let Ok(s) = String::from_utf8(out.stdout)
        {
            let t = s.trim().to_string();
            if !t.is_empty() {
                return t;
            }
        }
    }
    "x86_64-linux-gnu".to_string()
}

/// Phase 85c — two-stage musl cross-build of CPython.
///
/// Stage 1 builds a *build-platform* interpreter of the exact target version:
/// CPython's cross build needs `--with-build-python` of the same version to run
/// target-version bytecode at build time (freezing the importlib bootstrap,
/// generating `_sysconfigdata`, byte-compiling the stdlib). Stage 2
/// cross-configures + builds the musl target interpreter and DESTDIR-installs
/// the full `/usr` prefix. Both stages are driven from one source extraction via
/// out-of-tree (VPATH) build dirs.
///
/// Result: a **fully static** musl `python3` (no `PT_INTERP`, no `lib-dynload`,
/// no `dlopen`) with every stdlib C extension built **into** the interpreter
/// (`MODULE_BUILDTYPE=static`) and the `.py` stdlib frozen into a single
/// `lib/python312.zip` (zipimport — keeps the package small + fast over m3OS's
/// slow VFS), relocatable via CPython's `os.py` landmark search — so `sys.prefix`
/// resolves to wherever the package lands (`/usr` on m3OS), no build-prefix baked.
///
/// Why static (not the usual dynamic + `lib-dynload`): m3OS's
/// `/lib/ld-musl-x86_64.so.1` is a *custom Rust loader reimplementation*
/// (`userspace/ld-musl-x86_64.so.1/`), and m3OS ships **no real musl `libc.so`**
/// (its userland is `no_std` Rust). A dynamic CPython would fault at startup —
/// the loader cannot resolve the interpreter's `DT_NEEDED libc.so`, let alone
/// the thousands of libc symbols a real C program needs. So the interpreter is
/// linked static (musl libc embedded) with all extensions builtin — the same
/// model the static `git` port uses, and the only one that runs on m3OS today.
fn build_python(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
    zlib_stage: &Path,
    ncurses_stage: &Path,
) -> Result<(), String> {
    let zlib_prefix = zlib_stage.join("usr/local");
    if !zlib_prefix.join("lib/libz.a").exists() {
        return Err(format!(
            "python build: staged zlib not found at {} (build the zlib port first)",
            zlib_prefix.join("lib/libz.a").display()
        ));
    }
    // The wide ncurses the `_curses`/`_curses_panel` extensions statically link
    // — the same archives less/htop/tmux consume (`build_ncurses` stages
    // `libncursesw.a` + `libtinfow.a` + `libpanelw.a` and the widec headers
    // directly under `include/`, the `--enable-overwrite` layout). `_curses` is
    // a non-networked extension whose dependency (ncurses) is already a ported
    // library, so the phase's "build every C extension whose dependency is
    // already present" scope rule requires building it (not deferring it).
    let ncurses_prefix = ncurses_stage.join("usr/local");
    if !ncurses_prefix.join("lib/libncursesw.a").exists() {
        return Err(format!(
            "python build: staged ncurses not found at {} (build the ncurses port first)",
            ncurses_prefix.join("lib/libncursesw.a").display()
        ));
    }
    let have_panel = ncurses_prefix.join("lib/libpanelw.a").exists();

    // build-host / build-cross are siblings of the extracted source so a single
    // tarball extraction feeds both out-of-tree builds.
    let work = src
        .parent()
        .ok_or_else(|| "python build: source dir has no parent".to_string())?;
    let build_host = work.join("build-host");
    let build_cross = work.join("build-cross");
    let _ = fs::remove_dir_all(&build_host);
    let _ = fs::remove_dir_all(&build_cross);
    fs::create_dir_all(&build_host).map_err(|e| format!("mkdir build-host: {e}"))?;
    fs::create_dir_all(&build_cross).map_err(|e| format!("mkdir build-cross: {e}"))?;

    // Isolate pkg-config for the CROSS configure (env wired into `cross_cfg`
    // below): an empty search dir hides the build host's `.pc` files so its
    // libraries (wrong libc/arch) cannot leak into the target build. This is the
    // same reproducibility guarantee the `disabled_modules` `n/a` overrides give,
    // extended to pkg-config-detected optional libs. Concretely it fixes
    // `_blake2`: CPython 3.12 prefers system `libb2` whenever pkg-config reports
    // it (gh-91251; reverted to a vendored HACL* impl only in 3.13), defining
    // `HAVE_LIBB2` so `blake2module.h` compiles `#include <blake2.h>` — a header
    // the musl cross-gcc has no sysroot copy of, aborting the build with
    // "blake2.h: No such file or directory" on any host that ships libb2. With
    // libb2 hidden, `_blake2` falls back to its always-present vendored impl
    // (Modules/_blake2/impl/, no external dep). zlib and ncurses are unaffected:
    // they are fed explicitly via the ZLIB_*/CURSES_*/PANEL_* vars below, which
    // PKG_CHECK_MODULES consumes directly without consulting pkg-config.
    let pkgconfig_empty = work.join("pkgconfig-empty");
    fs::create_dir_all(&pkgconfig_empty).map_err(|e| format!("mkdir pkgconfig-empty: {e}"))?;

    let configure = src.join("configure");
    let jobs = format!("-j{}", num_jobs());

    // ── Stage 1: host (build-platform) interpreter ───────────────────────────
    println!("python: stage 1 — host interpreter configure");
    let mut host_cfg = Command::new("sh");
    host_cfg
        .current_dir(&build_host)
        .arg(&configure)
        .arg("--disable-test-modules")
        .arg("--without-ensurepip");
    run(&mut host_cfg, "python host configure")?;
    println!("python: stage 1 — host interpreter make ({jobs})");
    let mut host_make = Command::new("make");
    host_make.current_dir(&build_host).arg(&jobs);
    run(&mut host_make, "python host make")?;
    let host_py = build_host.join("python");
    if !host_py.exists() {
        return Err(format!(
            "python host build produced no interpreter at {}",
            host_py.display()
        ));
    }

    // ── Stage 2: cross (target) interpreter ──────────────────────────────────
    let build_triple = build_machine_triple();
    println!("python: stage 2 — cross configure (build={build_triple} host=x86_64-linux-musl)");

    let extra_ld = musl_extra_ldflags_joined();
    // Both staged ports on the include + link search path: zlib (zlib/gzip) and
    // wide ncurses (curses). The `include/ncursesw` arm is harmless when absent
    // (our `--enable-overwrite` ncurses installs the widec headers in `include/`).
    let cppflags = format!(
        "-I{0}/include -I{1}/include -I{1}/include/ncursesw",
        zlib_prefix.display(),
        ncurses_prefix.display()
    );
    // `-static`: the interpreter embeds musl libc (no PT_INTERP) so it runs on
    // m3OS's loaderless static-binary path (see the why-static note above).
    let lib_search = format!(
        "-L{}/lib -L{}/lib",
        zlib_prefix.display(),
        ncurses_prefix.display()
    );
    let ldflags = if extra_ld.is_empty() {
        format!("-static {lib_search}")
    } else {
        format!("-static {lib_search} {extra_ld}")
    };
    // Force CPython's `zlib` extension onto OUR staged (-fPIC) libz.a, bypassing
    // any pkg-config that would otherwise point it at the build host's system
    // zlib (wrong libc). These two vars override the configure PKG_CHECK_MODULES.
    let zlib_cflags = format!("-I{}/include", zlib_prefix.display());
    let zlib_libs = format!("-L{}/lib -lz", zlib_prefix.display());
    // Same override for `_curses`/`_curses_panel`: point CPython at OUR staged
    // wide ncurses, not the build host's system curses (wrong libc). Mirrors
    // build_htop's CURSES_CFLAGS/CURSES_LIBS. Static link order matters —
    // panel → ncurses → tinfo. (`build_ncurses` splits tinfo via `--with-termlib`.)
    let curses_cflags = format!(
        "-I{0}/include -I{0}/include/ncursesw",
        ncurses_prefix.display()
    );
    let curses_libs = format!("-L{}/lib -lncursesw -ltinfow", ncurses_prefix.display());
    let panel_libs = format!(
        "-L{}/lib -lpanelw -lncursesw -ltinfow",
        ncurses_prefix.display()
    );

    // Stdlib extensions whose external system library we do NOT provide for the
    // target. Forced to `n/a` so the build never *attempts* them — a build host
    // that happens to ship the -dev package (e.g. libffi for `_ctypes`) must not
    // change what gets cross-built (reproducibility). Per CPython 3.12 configure,
    // only `n/a` (not `disabled`) survives the per-module detection overwrite.
    // `zlib` and `_curses`/`_curses_panel` are intentionally absent — their deps
    // (zlib, ncurses) are ported and staged above, so they ARE built. The split:
    //   • `_ctypes` (libffi + dlopen) → Phase 91 (Dynamic C Runtime).
    //   • `_ssl`/`_hashlib`-OpenSSL → Phase 86 (TLS/networking).
    //   • everything else here depends on a library m3OS has not ported yet
    //     (GNU readline, sqlite3, gdbm, tk, libffi, libbz2, liblzma, libuuid,
    //     libxcrypt) — genuine dependency-absent deferrals, not scope cuts.
    //
    // The list ALSO carries `xxlimited`/`xxlimited_35` for a different reason:
    // they are Limited-API *demo* modules CPython builds SHARED in a non-debug
    // build regardless of MODULE_BUILDTYPE (they are gated on `with_pydebug=no`,
    // not TEST_MODULES, so `--disable-test-modules` does not cover them). Those
    // are the only `.so` links in this otherwise fully-static build, and their
    // link inherits our `-static` LDFLAGS, yielding a contradictory
    // `-static -shared` that drags in the non-PIC static CRT (`crtbeginT.o`).
    // Strict linkers (modern binutils) reject it — "R_X86_64_32 against hidden
    // symbol `__TMC_END__' can not be used when making a shared object" —
    // aborting `make`; lenient ones merely tolerate it. They are demos pruned
    // from the install anyway (see `prune_python_stage`), so disable them
    // outright rather than build-then-delete a link that breaks portability.
    let disabled_modules = [
        "_ctypes",
        "_ctypes_test",
        "_ssl",
        "_hashlib",
        "readline",
        "_sqlite3",
        "_dbm",
        "_gdbm",
        "_tkinter",
        "nis",
        "_uuid",
        "_bz2",
        "_lzma",
        "ossaudiodev",
        "_crypt",
        // Limited-API demo modules — shared-only, pruned anyway (see note above).
        "xxlimited",
        "xxlimited_35",
    ];

    let mut cross_cfg = Command::new("sh");
    cross_cfg
        .current_dir(&build_cross)
        .arg(&configure)
        .arg("--host=x86_64-linux-musl")
        .arg(format!("--build={build_triple}"))
        .arg(format!("--with-build-python={}", host_py.display()))
        .arg("--disable-shared")
        .arg("--disable-ipv6")
        .arg("--without-ensurepip")
        .arg("--without-pymalloc")
        .arg("--disable-test-modules")
        .arg("--prefix=/usr")
        .env("CC", cc)
        .env("AR", ar)
        .env("RANLIB", ranlib)
        // Hide the build host's pkg-config metadata (see pkgconfig_empty above):
        // keeps optional system libs — notably libb2 for `_blake2` — out of the
        // cross build. PKG_CONFIG_PATH is cleared too so an inherited value can't
        // re-add a search dir alongside PKG_CONFIG_LIBDIR.
        .env("PKG_CONFIG_LIBDIR", &pkgconfig_empty)
        .env("PKG_CONFIG_PATH", "")
        // Build every stdlib C extension *into* the interpreter rather than as a
        // dlopen-able lib-dynload `.so`. `${MODULE_BUILDTYPE:-shared}` in
        // configure honours this pre-set value (it is exactly CPython's official
        // wasm static-build switch); paired with `-static` it yields a fully
        // self-contained interpreter with no runtime `.so`/`dlopen` dependency.
        .env("MODULE_BUILDTYPE", "static")
        .env("CPPFLAGS", &cppflags)
        .env("LDFLAGS", &ldflags)
        .env("ZLIB_CFLAGS", &zlib_cflags)
        .env("ZLIB_LIBS", &zlib_libs)
        // Point `_curses`/`_curses_panel` at the staged wide ncurses (overrides
        // the configure pkg-config probe, exactly like ZLIB_CFLAGS/ZLIB_LIBS).
        .env("CURSES_CFLAGS", &curses_cflags)
        .env("CURSES_LIBS", &curses_libs)
        .env("PANEL_CFLAGS", &curses_cflags)
        .env("PANEL_LIBS", &panel_libs)
        // Cross cache answers: the target device nodes can't be stat'd, and the
        // getaddrinfo probe can't run, on the build host.
        .env("ac_cv_file__dev_ptmx", "no")
        .env("ac_cv_file__dev_ptc", "no")
        .env("ac_cv_buggy_getaddrinfo", "no");
    for m in disabled_modules {
        cross_cfg.env(format!("py_cv_module_{m}"), "n/a");
    }
    run(&mut cross_cfg, "python cross configure")?;

    // Neuter `checksharedmods`: the final build step runs the (glibc) build
    // python to *import* the freshly built (musl) target `.so` as a sanity check
    // — meaningless and broken across a libc boundary. Everything is already
    // built by the time it runs, so replacing just that one recipe line keeps
    // the dependency graph intact while skipping the cross-hostile import.
    neuter_checksharedmods(&build_cross.join("Makefile"))?;

    println!("python: stage 2 — cross make ({jobs})");
    let mut cross_make = Command::new("make");
    cross_make.current_dir(&build_cross).arg(&jobs);
    run(&mut cross_make, "python cross make")?;

    // Fail fast (before the multi-minute in-OS gate) if configure silently
    // skipped curses — `make` does NOT error when a module fails its detection
    // probe, it just omits it. The generated builtin module table proves it was
    // compiled *into* the static interpreter.
    assert_curses_builtin(&build_cross, have_panel)?;

    println!(
        "python: stage 2 — cross install (DESTDIR={})",
        stage.display()
    );
    let mut cross_install = Command::new("make");
    cross_install
        .current_dir(&build_cross)
        .arg(format!("DESTDIR={}", stage.display()))
        .arg("install");
    run(&mut cross_install, "python cross install")?;

    prune_python_stage(stage)?;
    freeze_stdlib_zip(stage, &host_py)?;
    assert_python_layout(stage)?;
    Ok(())
}

// ── Phase 85d — Clang/LLVM/LLD cross-build constants ──────────────────────────
/// Pinned LLVM version (must match `ports/lang/llvm/Portfile` VERSION).
const LLVM_VERSION: &str = "18.1.8";
/// LLVM target triple for the m3OS userspace (static musl, X86-only).
const LLVM_TRIPLE: &str = "x86_64-unknown-linux-musl";
/// Clang resource-dir major version (LLVM 16+ uses the bare major). Keep in sync
/// with the Portfile `VERSION` major and `build_recipe_id("llvm")`.
const LLVM_MAJOR: &str = "18";
/// On-target sysroot baked into the built clang (`DEFAULT_SYSROOT`). The `.m3pkg`
/// installs the bundled musl + libc++ sysroot here, in the standard `usr/` layout,
/// so a bare in-OS `clang hello.c` auto-finds libc/CRT/libc++. The clang resource
/// dir (builtin headers + builtins) stays RELATIVE to the `clang` binary
/// (`/usr/lib/clang/<major>`) — the Phase 85a relocation contract at its hardest.
const LLVM_TGT_SYSROOT: &str = "/usr/lib/clang-sysroot";

/// Phase 85d — host-cross-build a static **Clang + LLD** (X86-only, `MinSizeRel`)
/// for m3OS, plus the C++ runtime it links against.
///
/// UNLIKE every other port, this does NOT use the musl-gcc wrapper (musl-tools
/// ships no C++ compiler, and LLVM/Clang is C++). It drives the **host clang** as
/// the cross-compiler (`--target=x86_64-linux-musl` + an assembled musl sysroot)
/// across two stages — the chicken-and-egg of cross-building LLVM's own C++ for a
/// libc with no C++ stdlib:
///
///   Stage B (runtimes): build `libc++`/`libc++abi`/`libunwind` + `compiler-rt`
///     builtins for the target. libc++.a is made SELF-CONTAINED (abi + unwinder
///     merged in) so a bare `-lc++` resolves everything. Installed into the
///     sysroot, giving a musl C++ stdlib.
///   Stage C (toolchain): build `clang` + `lld` (X86, MinSizeRel, statically
///     linked against the Stage-B libc++), with the in-OS defaults baked in
///     (`CLANG_DEFAULT_LINKER=lld` — m3OS has no GNU ld — compiler-rt / libc++ /
///     libunwind defaults, `DEFAULT_SYSROOT`, musl default target triple).
///
/// The host clang's compiler-rt builtins (`libclang_rt.builtins-x86_64.a`) are
/// freestanding and work for the musl target, so they bootstrap the link checks;
/// the Stage-B target builtins are then bundled into the staged clang's resource
/// dir. `CMAKE_CROSSCOMPILING` is deliberately left FALSE (only the compiler
/// target + sysroot are set, no toolchain file) so LLVM builds tblgen as a
/// static-musl x86_64 binary that runs on the same-arch build host — sidestepping
/// the native-tblgen sub-build.
fn build_llvm(
    port_src: &Path,
    stage: &Path,
    _toolchain: &(&'static str, String, String),
) -> Result<(), String> {
    let root = workspace_root();
    let jobs = format!("-j{}", num_jobs());

    // Host clang/clang++ (overridable). musl-tools has no C++ compiler, so the
    // shared musl-gcc plumbing does not apply here.
    let clang = std::env::var("M3OS_LLVM_CLANG").unwrap_or_else(|_| "clang".to_string());
    let clangxx = std::env::var("M3OS_LLVM_CLANGXX").unwrap_or_else(|_| "clang++".to_string());
    for c in [clang.as_str(), clangxx.as_str()] {
        if !probe_tool_on_path(c) {
            return Err(format!(
                "llvm build: host C/C++ compiler '{c}' not found on PATH. Phase 85d \
                 cross-builds Clang with the host clang (set M3OS_LLVM_CLANG / \
                 M3OS_LLVM_CLANGXX to override). Install clang (Debian/Ubuntu: \
                 `apt install clang lld cmake ninja-build`)."
            ));
        }
    }
    if !probe_tool_on_path("cmake") || !probe_tool_on_path("ninja") {
        return Err(
            "llvm build: cmake and ninja are required (apt install cmake ninja-build)".into(),
        );
    }

    // Persistent cross-build workspace, OUTSIDE the port work dir that
    // `port_build` wipes + re-extracts each run. `port_build` re-extracts
    // `port_src` with fresh mtimes every invocation, which would defeat ninja's
    // incrementality and force a multi-hour Stage-C rebuild on every retry; a
    // stable source tree + persistent build dirs make staging-bug iterations
    // cheap, while the Phase 85a pkgcache still caches the sealed `.m3pkg` so a
    // clean machine pays the full build exactly once.
    let cross_root = root.join("target/llvm-cross");
    fs::create_dir_all(&cross_root).map_err(|e| format!("mkdir llvm-cross: {e}"))?;
    let sysroot = root.join("target/llvm-musl-sysroot");
    let sysroot_usr = cross_root.join("sysroot-usr");
    let build_rt = cross_root.join("build-runtimes");
    let build_clang = cross_root.join("build-llvm");

    // Stable source: extract the pinned tarball into `cross_root` once and reuse.
    // (`port_src` is the port machinery's own freshly-extracted copy; we ignore it
    // when a cached tarball is available so re-runs don't re-stat thousands of
    // newer-mtime files into a multi-hour rebuild.)
    let stable_src = cross_root.join(format!("llvm-project-{LLVM_VERSION}.src"));
    let src: PathBuf = if stable_src.join("llvm/CMakeLists.txt").exists() {
        stable_src
    } else {
        let tarball = root.join(format!(
            "target/port-src/llvm-project-{LLVM_VERSION}.src.tar.xz"
        ));
        if tarball.exists() {
            println!(
                "llvm: extracting source to persistent {} (once)",
                stable_src.display()
            );
            let extracted = extract_tarball(&tarball, &cross_root.join("src-extract"))?;
            let _ = fs::remove_dir_all(&stable_src);
            fs::rename(&extracted, &stable_src)
                .map_err(|e| format!("llvm: stage stable source: {e}"))?;
            stable_src
        } else {
            // No cached tarball — fall back to the port machinery's extracted tree.
            println!(
                "llvm: cached tarball absent; using port-extracted source {}",
                port_src.display()
            );
            port_src.to_path_buf()
        }
    };
    let src = src.as_path();

    // Ubuntu clang defaults to rtlib=libgcc + a shared unwinder (libgcc_s.so.1),
    // which a musl sysroot has none of. Force compiler-rt builtins, no shared
    // unwinder, and lld so every cmake try-compile/link check passes.
    let ldx = "-rtlib=compiler-rt -unwindlib=none -fuse-ld=lld";
    let cfx = format!("-Wno-unused-command-line-argument {ldx}");
    let cxxfx = format!("-Wno-unused-command-line-argument -stdlib=libc++ {ldx}");

    // ── 1 + 2. assemble the musl sysroot + build the C++ runtime (Stage B) ─────
    // The reuse check comes FIRST: `assemble_musl_sysroot` wipes the sysroot's
    // include/lib (where Stage B installs libc++), so assembling unconditionally
    // would clobber a prior runtime and defeat the skip. On reuse, the existing
    // sysroot already carries musl + the self-contained libc++ + builtins.
    if sysroot.join("lib/libc++.a").exists()
        && sysroot
            .join("lib/linux/libclang_rt.builtins-x86_64.a")
            .exists()
        && std::env::var("M3OS_LLVM_REBUILD_RUNTIMES").is_err()
    {
        println!(
            "llvm: stage B — reusing existing C++ runtime in {} (set \
             M3OS_LLVM_REBUILD_RUNTIMES=1 to force)",
            sysroot.display()
        );
    } else {
        assemble_musl_sysroot(&sysroot)?;
        println!("llvm: stage B — runtimes (libc++/libc++abi/libunwind + compiler-rt builtins)");
        let _ = fs::remove_dir_all(&build_rt);
        let mut cfg = Command::new("cmake");
        cfg.args(["-G", "Ninja", "-S"])
            .arg(src.join("runtimes"))
            .arg("-B")
            .arg(&build_rt)
            .arg("-DCMAKE_BUILD_TYPE=MinSizeRel")
            .arg(format!("-DCMAKE_INSTALL_PREFIX={}", sysroot.display()))
            .arg(format!("-DCMAKE_C_COMPILER={clang}"))
            .arg(format!("-DCMAKE_CXX_COMPILER={clangxx}"))
            .arg(format!("-DCMAKE_C_COMPILER_TARGET={LLVM_TRIPLE}"))
            .arg(format!("-DCMAKE_CXX_COMPILER_TARGET={LLVM_TRIPLE}"))
            .arg(format!("-DCMAKE_SYSROOT={}", sysroot.display()))
            .arg(format!("-DCMAKE_C_FLAGS={cfx}"))
            .arg(format!("-DCMAKE_CXX_FLAGS={cfx}"))
            .arg(format!("-DCMAKE_EXE_LINKER_FLAGS={ldx}"))
            .arg(format!("-DCMAKE_SHARED_LINKER_FLAGS={ldx}"))
            .arg("-DLLVM_ENABLE_RUNTIMES=libcxx;libcxxabi;libunwind;compiler-rt")
            .arg("-DLIBCXX_ENABLE_SHARED=OFF")
            .arg("-DLIBCXXABI_ENABLE_SHARED=OFF")
            .arg("-DLIBUNWIND_ENABLE_SHARED=OFF")
            .arg("-DLIBCXX_ENABLE_STATIC=ON")
            .arg("-DLIBCXXABI_ENABLE_STATIC=ON")
            .arg("-DLIBUNWIND_ENABLE_STATIC=ON")
            .arg("-DLIBCXX_HAS_MUSL_LIBC=ON")
            .arg("-DLIBCXX_CXX_ABI=libcxxabi")
            .arg("-DLIBCXXABI_USE_LLVM_UNWINDER=ON")
            .arg("-DLIBCXXABI_ENABLE_STATIC_UNWINDER=ON")
            // Merge libc++abi + libunwind INTO libc++.a so a bare `-lc++` is
            // self-sufficient for in-OS `clang++ hello.cpp` (no -lc++abi/-lunwind).
            .arg("-DLIBCXXABI_STATICALLY_LINK_UNWINDER_IN_STATIC_LIBRARY=ON")
            .arg("-DLIBCXX_ENABLE_STATIC_ABI_LIBRARY=ON")
            .arg("-DLIBCXX_STATICALLY_LINK_ABI_IN_STATIC_LIBRARY=ON")
            .arg("-DLIBCXX_USE_COMPILER_RT=ON")
            .arg("-DLIBCXXABI_USE_COMPILER_RT=ON")
            .arg("-DLIBUNWIND_USE_COMPILER_RT=ON")
            .arg("-DLIBCXX_INCLUDE_BENCHMARKS=OFF")
            .arg("-DLIBCXX_INCLUDE_TESTS=OFF")
            .arg("-DLIBCXXABI_INCLUDE_TESTS=OFF")
            .arg("-DLIBUNWIND_INCLUDE_TESTS=OFF")
            .arg("-DLLVM_INCLUDE_TESTS=OFF")
            .arg("-DCOMPILER_RT_DEFAULT_TARGET_ONLY=ON")
            .arg("-DCOMPILER_RT_BUILD_BUILTINS=ON")
            .arg("-DCOMPILER_RT_BUILD_SANITIZERS=OFF")
            .arg("-DCOMPILER_RT_BUILD_XRAY=OFF")
            .arg("-DCOMPILER_RT_BUILD_LIBFUZZER=OFF")
            .arg("-DCOMPILER_RT_BUILD_PROFILE=OFF")
            .arg("-DCOMPILER_RT_BUILD_MEMPROF=OFF")
            .arg("-DCOMPILER_RT_BUILD_ORC=OFF");
        run(&mut cfg, "llvm runtimes configure")?;
        let mut mk = Command::new("ninja");
        mk.current_dir(&build_rt).arg(&jobs);
        run(&mut mk, "llvm runtimes build")?;
        let mut inst = Command::new("ninja");
        inst.current_dir(&build_rt).arg("install");
        run(&mut inst, "llvm runtimes install")?;
    }
    for need in ["lib/libc++.a", "lib/libc++abi.a", "lib/libunwind.a"] {
        if !sysroot.join(need).exists() {
            return Err(format!(
                "llvm build: stage B did not produce {} — C++ runtime incomplete",
                sysroot.join(need).display()
            ));
        }
    }

    // ── 3. usr/-layout sysroot view so the host clang building LLVM finds libc++ ─
    make_usr_layout_sysroot(&sysroot, &sysroot_usr)?;

    // ── 4. Stage C: clang + lld (X86, MinSizeRel, static musl) ─────────────────
    // libc++ is self-contained, so a bare `-stdlib=libc++` suffices when linking
    // the toolchain's own executables (clang/lld/tblgen).
    let exeld = format!(
        "-static -stdlib=libc++ -L{}/lib -rtlib=compiler-rt -unwindlib=libunwind -fuse-ld=lld",
        sysroot.display()
    );
    println!("llvm: stage C — configure clang + lld (X86, MinSizeRel, static musl)");
    // Do NOT wipe build_clang — a persistent build dir + stable source make ninja
    // incremental, so a staging/validate fix re-runs in seconds, not a multi-hour
    // rebuild. cmake re-configure with an unchanged cache is a fast no-op.
    let mut cfg = Command::new("cmake");
    cfg.args(["-G", "Ninja", "-S"])
        .arg(src.join("llvm"))
        .arg("-B")
        .arg(&build_clang)
        .arg("-DCMAKE_BUILD_TYPE=MinSizeRel")
        .arg("-DCMAKE_INSTALL_PREFIX=/usr")
        .arg(format!("-DCMAKE_C_COMPILER={clang}"))
        .arg(format!("-DCMAKE_CXX_COMPILER={clangxx}"))
        .arg(format!("-DCMAKE_C_COMPILER_TARGET={LLVM_TRIPLE}"))
        .arg(format!("-DCMAKE_CXX_COMPILER_TARGET={LLVM_TRIPLE}"))
        .arg(format!("-DCMAKE_SYSROOT={}", sysroot_usr.display()))
        .arg(format!("-DCMAKE_C_FLAGS={cfx}"))
        .arg(format!("-DCMAKE_CXX_FLAGS={cxxfx}"))
        .arg(format!("-DCMAKE_EXE_LINKER_FLAGS={exeld}"))
        .arg("-DLLVM_ENABLE_PROJECTS=clang;lld")
        .arg("-DLLVM_TARGETS_TO_BUILD=X86")
        .arg("-DLLVM_TARGET_ARCH=X86")
        .arg(format!("-DLLVM_DEFAULT_TARGET_TRIPLE={LLVM_TRIPLE}"))
        .arg(format!("-DLLVM_HOST_TRIPLE={LLVM_TRIPLE}"))
        // Size levers (the difference between a few-hundred-MB and a multi-GB
        // artifact) + a single-threaded m3OS target.
        .arg("-DLLVM_ENABLE_THREADS=OFF")
        .arg("-DLLVM_ENABLE_ZLIB=OFF")
        .arg("-DLLVM_ENABLE_ZSTD=OFF")
        .arg("-DLLVM_ENABLE_TERMINFO=OFF")
        .arg("-DLLVM_ENABLE_LIBXML2=OFF")
        .arg("-DLLVM_ENABLE_LIBEDIT=OFF")
        .arg("-DLLVM_ENABLE_RTTI=OFF")
        .arg("-DLLVM_ENABLE_EH=OFF")
        .arg("-DLLVM_INCLUDE_TESTS=OFF")
        .arg("-DLLVM_INCLUDE_BENCHMARKS=OFF")
        .arg("-DLLVM_INCLUDE_EXAMPLES=OFF")
        .arg("-DLLVM_INCLUDE_UTILS=OFF")
        .arg("-DCLANG_ENABLE_STATIC_ANALYZER=OFF")
        .arg("-DCLANG_ENABLE_ARCMT=OFF")
        // In-OS defaults so a bare `clang hello.c` works flagless on m3OS: lld
        // (m3OS has no GNU ld), compiler-rt builtins, libc++ + libunwind, and a
        // fixed sysroot supplying libc headers/CRT.
        .arg("-DCLANG_DEFAULT_LINKER=lld")
        .arg("-DCLANG_DEFAULT_RTLIB=compiler-rt")
        .arg("-DCLANG_DEFAULT_CXX_STDLIB=libc++")
        .arg("-DCLANG_DEFAULT_UNWINDLIB=libunwind")
        .arg(format!("-DDEFAULT_SYSROOT={LLVM_TGT_SYSROOT}"))
        .arg("-DLLVM_INSTALL_TOOLCHAIN_ONLY=ON");
    run(&mut cfg, "llvm configure")?;

    println!("llvm: stage C — build clang + lld ({jobs}) [multi-hour on a cold cache]");
    let mut mk = Command::new("ninja");
    mk.current_dir(&build_clang)
        .arg(&jobs)
        .args(["clang", "lld"]);
    run(&mut mk, "llvm build")?;

    // ── 5. install clang + lld + resource headers into the DESTDIR stage ───────
    println!("llvm: stage C — install (DESTDIR={})", stage.display());
    let _ = fs::remove_dir_all(stage);
    fs::create_dir_all(stage).map_err(|e| format!("mkdir stage: {e}"))?;
    let mut inst = Command::new("ninja");
    inst.current_dir(&build_clang).env("DESTDIR", stage).args([
        "install-clang",
        "install-clang-resource-headers",
        "install-lld",
    ]);
    run(&mut inst, "llvm install")?;

    assemble_llvm_stage(&sysroot, stage)?;
    validate_staged_clang(stage)?;
    assert_llvm_layout(stage)?;
    Ok(())
}

/// Assemble a clean musl sysroot at `sysroot`: musl headers + Linux UAPI
/// (`linux/`, `asm-generic/`, `asm/` — musl ships none) + libc.a/CRT. The C++
/// runtime (Stage B) installs on top of this. Idempotent (wipes + rebuilds the
/// header/lib copies; the UAPI dirs are symlinked).
fn assemble_musl_sysroot(sysroot: &Path) -> Result<(), String> {
    const MUSL_INC: &str = "/usr/include/x86_64-linux-musl";
    const MUSL_LIB: &str = "/usr/lib/x86_64-linux-musl";
    if !Path::new(MUSL_INC).join("stdio.h").exists() || !Path::new(MUSL_LIB).join("libc.a").exists()
    {
        return Err(format!(
            "llvm build: musl headers/libs not found at {MUSL_INC} / {MUSL_LIB} \
             (Debian/Ubuntu: `apt install musl-dev`)"
        ));
    }
    let inc = sysroot.join("include");
    let lib = sysroot.join("lib");
    // Reset only the header/lib copies — the C++ runtime install lands in the same
    // include/lib, so a clean assemble before Stage B avoids stale carry-over.
    let _ = fs::remove_dir_all(&inc);
    let _ = fs::remove_dir_all(&lib);
    fs::create_dir_all(&inc).map_err(|e| format!("mkdir sysroot/include: {e}"))?;
    fs::create_dir_all(&lib).map_err(|e| format!("mkdir sysroot/lib: {e}"))?;
    // musl headers + libc.a/CRT via `cp -a` (a few dozen files; simpler than a
    // hand-rolled recursive copy).
    cp_a(
        &format!("{MUSL_INC}/."),
        inc.to_str().unwrap(),
        "musl headers",
    )?;
    cp_a(&format!("{MUSL_LIB}/."), lib.to_str().unwrap(), "musl libs")?;
    // Linux UAPI headers (musl ships none) — symlink the kernel uapi dirs.
    let asm_dir = linux_uapi_arch_include().join("asm");
    for (target, name) in [
        ("/usr/include/linux", "linux"),
        ("/usr/include/asm-generic", "asm-generic"),
        (asm_dir.to_str().unwrap_or("/usr/include/asm"), "asm"),
    ] {
        let link = inc.join(name);
        let _ = fs::remove_file(&link);
        if Path::new(target).exists() {
            std::os::unix::fs::symlink(target, &link)
                .map_err(|e| format!("symlink sysroot/include/{name}: {e}"))?;
        }
    }
    Ok(())
}

/// Build a `usr/`-prefixed VIEW of `sysroot` at `usr_root` (symlinks
/// `usr/include -> sysroot/include`, `usr/lib -> sysroot/lib`) so the host clang
/// building LLVM finds libc++ at the standard `<sysroot>/usr/include/c++/v1`.
fn make_usr_layout_sysroot(sysroot: &Path, usr_root: &Path) -> Result<(), String> {
    let _ = fs::remove_dir_all(usr_root);
    let usr = usr_root.join("usr");
    fs::create_dir_all(&usr).map_err(|e| format!("mkdir sysroot-usr/usr: {e}"))?;
    std::os::unix::fs::symlink(sysroot.join("include"), usr.join("include"))
        .map_err(|e| format!("symlink usr/include: {e}"))?;
    std::os::unix::fs::symlink(sysroot.join("lib"), usr.join("lib"))
        .map_err(|e| format!("symlink usr/lib: {e}"))?;
    Ok(())
}

/// After `install-clang`/`install-lld`, finish the staged `/usr` tree: copy the
/// target compiler-rt builtins into the clang resource dir (so `-rtlib=compiler-rt`
/// resolves relative to the binary), bundle the musl + libc++ sysroot at
/// `DEFAULT_SYSROOT` in `usr/` layout, and ensure the `clang++` driver symlink.
fn assemble_llvm_stage(sysroot: &Path, stage: &Path) -> Result<(), String> {
    // compiler-rt builtins → clang resource dir (lib/clang/<major>/lib/linux/).
    let reslib = stage.join(format!("usr/lib/clang/{LLVM_MAJOR}/lib/linux"));
    fs::create_dir_all(&reslib).map_err(|e| format!("mkdir resource lib: {e}"))?;
    let src_rtlib = sysroot.join("lib/linux");
    if let Ok(entries) = fs::read_dir(&src_rtlib) {
        for e in entries.flatten() {
            let name = e.file_name();
            let n = name.to_string_lossy();
            if n.starts_with("libclang_rt.") || n.starts_with("clang_rt.") {
                fs::copy(e.path(), reslib.join(&name))
                    .map_err(|err| format!("copy builtin {n}: {err}"))?;
            }
        }
    }
    if !reslib.join("libclang_rt.builtins-x86_64.a").exists() {
        return Err(format!(
            "llvm build: compiler-rt builtins missing from resource dir {}",
            reslib.display()
        ));
    }
    // Bundle the musl + libc++ sysroot at DEFAULT_SYSROOT in standard usr/ layout.
    let tgt = stage
        .join(LLVM_TGT_SYSROOT.trim_start_matches('/'))
        .join("usr");
    fs::create_dir_all(&tgt).map_err(|e| format!("mkdir bundled sysroot: {e}"))?;
    cp_a(
        sysroot.join("include").to_str().unwrap(),
        tgt.join("include").to_str().unwrap(),
        "bundle sysroot include",
    )?;
    cp_a(
        sysroot.join("lib").to_str().unwrap(),
        tgt.join("lib").to_str().unwrap(),
        "bundle sysroot lib",
    )?;
    // clang++ driver symlink (install-clang usually creates it; ensure it for the
    // A.5 acceptance — argv[0]-driven C++ driver mode).
    let bin = stage.join("usr/bin");
    let clangxx = bin.join("clang++");
    if fs::symlink_metadata(&clangxx).is_err() {
        std::os::unix::fs::symlink("clang", &clangxx)
            .map_err(|e| format!("symlink clang++: {e}"))?;
    }
    Ok(())
}

/// Validate the STAGED clang (a static-musl x86_64 binary that runs on the build
/// host) actually compiles + links + runs a C and a C++ program using only the
/// bundled sysroot — a strong host-side proxy for the in-OS gate.
fn validate_staged_clang(stage: &Path) -> Result<(), String> {
    let clang = stage.join("usr/bin/clang");
    let clangxx = stage.join("usr/bin/clang++");
    if !clang.exists() {
        return Err(format!(
            "llvm build: staged clang missing at {}",
            clang.display()
        ));
    }
    // The baked DEFAULT_SYSROOT does not exist on the host; point --sysroot at the
    // staged bundle instead.
    let hsys = stage.join(LLVM_TGT_SYSROOT.trim_start_matches('/'));
    let tmp = workspace_root().join("target/llvm-validate");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("mkdir validate dir: {e}"))?;
    let c_src = tmp.join("h.c");
    let cpp_src = tmp.join("h.cpp");
    fs::write(
        &c_src,
        "#include <stdio.h>\nint main(){puts(\"hello, world\");return 0;}\n",
    )
    .map_err(|e| format!("write h.c: {e}"))?;
    fs::write(
        &cpp_src,
        "#include <iostream>\nint main(){std::cout<<\"hello, cpp\\n\";return 0;}\n",
    )
    .map_err(|e| format!("write h.cpp: {e}"))?;

    // C: clang -O2 h.c -o h_c && run.
    let c_out = tmp.join("h_c");
    let mut cc = Command::new(&clang);
    cc.arg(format!("--sysroot={}", hsys.display()))
        .arg("-O2")
        .arg(&c_src)
        .arg("-o")
        .arg(&c_out);
    run(&mut cc, "llvm validate: clang compile h.c")?;
    run(&mut Command::new(&c_out), "llvm validate: run C binary")?;

    // C++: clang++ h.cpp -o h_cpp && run (links the self-contained libc++).
    let cpp_out = tmp.join("h_cpp");
    let mut cxx = Command::new(&clangxx);
    cxx.arg(format!("--sysroot={}", hsys.display()))
        .arg(&cpp_src)
        .arg("-o")
        .arg(&cpp_out);
    run(&mut cxx, "llvm validate: clang++ compile h.cpp")?;
    run(&mut Command::new(&cpp_out), "llvm validate: run C++ binary")?;

    println!("llvm: staged clang validated (C + C++ compile/link/run via bundled sysroot)");
    Ok(())
}

/// Assert the staged tree has the binaries + relocatable resource dir the m3OS
/// install needs.
fn assert_llvm_layout(stage: &Path) -> Result<(), String> {
    for bin in [
        "usr/bin/clang",
        "usr/bin/clang++",
        "usr/bin/lld",
        "usr/bin/ld.lld",
    ] {
        let p = stage.join(bin);
        if fs::symlink_metadata(&p).is_err() {
            return Err(format!("llvm build: staged {} missing", p.display()));
        }
    }
    let resinc = stage.join(format!("usr/lib/clang/{LLVM_MAJOR}/include/stddef.h"));
    if !resinc.exists() {
        return Err(format!(
            "llvm build: clang resource headers missing ({})",
            resinc.display()
        ));
    }
    // The bundled sysroot must carry libc.a/CRT + the self-contained libc++.
    let sysl = stage
        .join(LLVM_TGT_SYSROOT.trim_start_matches('/'))
        .join("usr/lib");
    for need in ["libc.a", "crt1.o", "libc++.a"] {
        if !sysl.join(need).exists() {
            return Err(format!(
                "llvm build: bundled sysroot missing {}",
                sysl.join(need).display()
            ));
        }
    }
    println!(
        "llvm: staged clang+lld (X86, static musl) + resource dir /usr/lib/clang/{LLVM_MAJOR} \
         + bundled sysroot {LLVM_TGT_SYSROOT}"
    );
    Ok(())
}

/// `cp -a <src> <dst>` (archive copy; preserves symlinks/modes). Used for the
/// musl sysroot assembly + bundling where a hand-rolled recursive copy would be
/// noise. Errors propagate.
fn cp_a(src: &str, dst: &str, label: &str) -> Result<(), String> {
    let status = Command::new("cp")
        .args(["-a", src, dst])
        .status()
        .map_err(|e| format!("{label}: cp -a spawn: {e}"))?;
    if !status.success() {
        return Err(format!("{label}: cp -a {src} {dst} exited {status}"));
    }
    Ok(())
}

/// Assert `_curses` (and, when `libpanelw.a` was staged, `_curses_panel`) were
/// compiled **into** the interpreter. CPython's `make` is silent when a module
/// fails its configure probe — it just omits it and the only symptom is a
/// runtime `ImportError`. The generated builtin module table
/// (`Modules/config.c`, written by `makesetup` during `make`) lists a
/// `{"_curses", PyInit__curses}` entry for every statically-linked extension, so
/// its presence is a direct build-time proof. Best-effort on path: if the
/// generated table is not where we expect (an upstream layout change), warn and
/// defer to the in-OS `import curses` gate rather than false-failing the build.
fn assert_curses_builtin(build_cross: &Path, have_panel: bool) -> Result<(), String> {
    let config_c = build_cross.join("Modules/config.c");
    let Ok(content) = fs::read_to_string(&config_c) else {
        eprintln!(
            "python: warning: {} not found — cannot statically verify _curses; \
             relying on the in-OS import gate",
            config_c.display()
        );
        return Ok(());
    };
    if !content.contains("PyInit__curses") {
        return Err(
            "python build: _curses was NOT compiled into the interpreter (no PyInit__curses in \
             Modules/config.c) — the configure curses probe failed; check CURSES_CFLAGS/CURSES_LIBS \
             point at the staged wide ncurses"
                .to_string(),
        );
    }
    if have_panel && !content.contains("PyInit__curses_panel") {
        return Err(
            "python build: _curses_panel was NOT compiled in despite a staged libpanelw.a (no \
             PyInit__curses_panel in Modules/config.c) — check PANEL_LIBS"
                .to_string(),
        );
    }
    println!(
        "python: verified _curses{} builtin (linked against staged ncurses)",
        if have_panel { " + _curses_panel" } else { "" }
    );
    Ok(())
}

/// Replace the single `checksharedmods` recipe line (the one invoking
/// `Tools/build/check_extension_modules.py`) with a no-op echo, so the
/// cross-hostile final import does not abort the build. Best-effort: if upstream
/// renames or removes the check, warn and continue (a still-present hostile
/// check would then surface as a normal `make` failure).
fn neuter_checksharedmods(makefile: &Path) -> Result<(), String> {
    let content =
        fs::read_to_string(makefile).map_err(|e| format!("read {}: {e}", makefile.display()))?;
    let mut out = String::with_capacity(content.len());
    let mut patched = false;
    for line in content.lines() {
        if line.contains("check_extension_modules.py") {
            out.push_str("\t@echo \"checksharedmods: skipped for cross build\"\n");
            patched = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !patched {
        eprintln!("python: warning: checksharedmods recipe not found to neuter");
        return Ok(());
    }
    fs::write(makefile, out).map_err(|e| format!("write {}: {e}", makefile.display()))?;
    Ok(())
}

/// Trim the DESTDIR-installed tree to what *runs* scripts: drop the embedding /
/// build-only artifacts and out-of-scope (Phase 86+) packages, plus the
/// precompiled `__pycache__` (pure cache that ~doubles the install file count;
/// Python recompiles on demand — in-memory if `/usr` is read-only).
fn prune_python_stage(stage: &Path) -> Result<(), String> {
    let usr = stage.join("usr");
    let pylib = usr.join(format!("lib/python{PYTHON_XY}"));
    // Embedding / build-only artifacts (not needed to run scripts).
    let _ = fs::remove_file(usr.join(format!("lib/libpython{PYTHON_XY}.a")));
    let _ = fs::remove_dir_all(usr.join("lib/pkgconfig"));
    // The C extension / embedding headers (/usr/include/python3.NN/) — ~200 files
    // used only to *compile* against Python (build C extensions, embed the
    // interpreter). m3OS has no in-OS C compiler yet (Phase 85d), so they are dead
    // weight — and at ~200 files they DOMINATE `pkg install python`'s write phase,
    // since each file costs an unlink+open+write+close+chmod round-trip over the
    // slow ring-3 VFS (224 install files → ~24 without these). This is exactly the
    // "embedding / build-only artifacts" this function exists to drop; it was just
    // missed. Re-add (or split a python-dev package) when in-OS extension builds
    // land. The deeper per-file write cost is the VFS bulk-I/O phase
    // (docs/roadmap/92-vfs-bulk-io).
    let _ = fs::remove_dir_all(usr.join(format!("include/python{PYTHON_XY}")));
    // python[3[.NN]]-config: shell helpers that report --cflags/--libs for building
    // against the headers + libpython.a just stripped, so they are useless here.
    for cfg in [
        "python-config".to_string(),
        "python3-config".to_string(),
        format!("python{PYTHON_XY}-config"),
    ] {
        let _ = fs::remove_file(usr.join("bin").join(cfg));
    }
    // The config-<X.Y>-<triple> embedding dir (Makefile + libpython.a copy).
    if let Ok(entries) = fs::read_dir(&pylib) {
        for e in entries.flatten() {
            if e.file_name()
                .to_string_lossy()
                .starts_with(&format!("config-{PYTHON_XY}"))
            {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    // Out-of-scope / deferred packages (GUI, deprecated tooling, pip, asyncio).
    for d in [
        "ensurepip",
        "idlelib",
        "tkinter",
        "turtledemo",
        "lib2to3",
        "asyncio",
    ] {
        let _ = fs::remove_dir_all(pylib.join(d));
    }
    let _ = fs::remove_file(pylib.join("turtle.py"));
    for b in ["2to3", "2to3-3.12", "idle3", "idle3.12"] {
        let _ = fs::remove_file(usr.join("bin").join(b));
    }

    // With MODULE_BUILDTYPE=static every real stdlib extension is builtin, and
    // the only modules CPython would otherwise build shared — the `xxlimited*`
    // demos — are forced `n/a` at configure time (see `disabled_modules`). So
    // `lib-dynload/` should be empty or absent. Any `.so` that survives here is a
    // correctness failure: a real module slipped through as shared and the static
    // interpreter would fail to `import` it (no `dlopen` on m3OS's static-binary
    // path). Fail the build in that case.
    let lib_dynload = pylib.join("lib-dynload");
    if let Ok(entries) = fs::read_dir(&lib_dynload) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.ends_with(".so") {
                return Err(format!(
                    "python build: unexpected shared extension {n} in lib-dynload — \
                     MODULE_BUILDTYPE=static did not make it builtin (it would fail to \
                     import on m3OS's static-binary path)"
                ));
            }
        }
    }
    // Drop the lib-dynload dir entirely — a static interpreter needs no runtime
    // `.so`, and nothing in it would be loadable on m3OS anyway.
    let _ = fs::remove_dir_all(&lib_dynload);

    remove_pycache_recursive(&pylib);
    Ok(())
}

/// Recursively remove every `__pycache__` directory under `dir`.
fn remove_pycache_recursive(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if e.file_name() == "__pycache__" {
                let _ = fs::remove_dir_all(e.path());
            } else {
                remove_pycache_recursive(&e.path());
            }
        }
    }
}

/// Collapse the loose stdlib `.py` tree into a single `lib/python312.zip` of
/// `.pyc` (compiled by the host interpreter), keeping only the `os.py` getpath
/// landmark loose.
///
/// This is **decisive** on m3OS. Its ring-3 VFS is slow (80-200 ms per path
/// stat — see the `vfs_server: slow req … STAT_PATH` boot log), so ~1700 loose
/// stdlib files made `pkg install python` and every cold `import` (a per-module
/// `sys.path` stat storm) take minutes — the first gate run timed out. CPython's
/// default `sys.path` already includes `<prefix>/lib/python312.zip`, and
/// zipimport reads the archive's central directory once (no per-file stats), so
/// the package collapses to a handful of files and imports become fast. The
/// `os.py` landmark stays a real file so `getpath` still resolves `sys.prefix`.
fn freeze_stdlib_zip(stage: &Path, host_py: &Path) -> Result<(), String> {
    let usr = stage.join("usr");
    let pylib = usr.join(format!("lib/python{PYTHON_XY}"));
    let zip_path = usr.join(format!("lib/python{}.zip", PYTHON_XY.replace('.', "")));

    // 1. Byte-compile the stdlib (.pyc beside .py, legacy `-b` layout). `-s`/`-p`
    //    remap co_filename to the install path so tracebacks read `/usr/lib/...`
    //    not the build-stage path. compileall returns nonzero if *any* file fails
    //    to compile; tolerate that (a stray bad .py in an unused corner must not
    //    fail the port) and gate on a healthy .pyc count below instead.
    let mut compile = Command::new(host_py);
    compile
        .args(["-m", "compileall", "-b", "-q", "-f", "-s"])
        .arg(&usr)
        .arg("-p")
        .arg("/usr")
        .arg(&pylib);
    let _ = compile.status();

    // 2. Pack every .pyc into lib/python312.zip. STORED, not deflated: keeps the
    //    freeze independent of the host build interpreter's zlib extension (which
    //    we don't configure for the stage-1 build, so it may be absent). NOTE:
    //    the `.m3pkg` is itself an *uncompressed* archive — `pkg_format::serialize`
    //    concatenates raw file bytes with per-entry SHA-256 hashes, no deflate —
    //    so nothing else compresses this payload. Deflating here (or compressing
    //    the `.m3pkg`) is a real package-size / install-read-cost win, tracked in
    //    the VFS bulk-I/O phase (docs/roadmap/92-vfs-bulk-io).
    let script = "import sys,os,zipfile\n\
                  pylib,zp=sys.argv[1],sys.argv[2]\n\
                  n=0\n\
                  with zipfile.ZipFile(zp,'w',zipfile.ZIP_STORED) as z:\n\
                  \x20for r,_,fs in os.walk(pylib):\n\
                  \x20 for f in fs:\n\
                  \x20  if f.endswith('.pyc'):\n\
                  \x20   p=os.path.join(r,f); z.write(p,os.path.relpath(p,pylib)); n+=1\n\
                  print(n)\n";
    let out = Command::new(host_py)
        .arg("-c")
        .arg(script)
        .arg(&pylib)
        .arg(&zip_path)
        .output()
        .map_err(|e| format!("python build: zip stdlib spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "python build: zip stdlib failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let count: usize = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    // `compileall -b` writes exactly one `.pyc` per `.py`, so this is the module
    // count of the pruned, non-`test` stdlib — ~480 for CPython 3.12. (Do NOT
    // expect the ~2700 you'd get by counting a raw install's `__pycache__`, which
    // holds three `.pyc` per module: default + `opt-1` + `opt-2`.) A floor of 300
    // catches a genuinely broken/truncated compile while clearing the real count.
    if count < 300 {
        return Err(format!(
            "python build: python{}.zip has only {count} .pyc — stdlib compile/zip incomplete",
            PYTHON_XY.replace('.', "")
        ));
    }

    // 3. Collapse the loose tree, keeping only the os.py getpath landmark.
    let os_py = pylib.join("os.py");
    let saved = stage.join("os.py.landmark");
    fs::copy(&os_py, &saved).map_err(|e| format!("python build: save os.py: {e}"))?;
    fs::remove_dir_all(&pylib).map_err(|e| format!("python build: rm loose stdlib: {e}"))?;
    fs::create_dir_all(&pylib).map_err(|e| format!("python build: mkdir stdlib: {e}"))?;
    fs::copy(&saved, &os_py).map_err(|e| format!("python build: restore os.py: {e}"))?;
    let _ = fs::remove_file(&saved);

    println!(
        "python: froze stdlib → {} ({count} .pyc; os.py landmark kept loose)",
        zip_path.display()
    );
    Ok(())
}

/// Assert the staged tree has the runtime layout the relocation contract needs
/// and that the interpreter is genuinely **static** (the only model that runs on
/// m3OS — see the `build_python` why-static note).
fn assert_python_layout(stage: &Path) -> Result<(), String> {
    let usr = stage.join("usr");
    let py312 = usr.join(format!("bin/python{PYTHON_XY}"));
    if !py312.exists() {
        return Err(format!("python build missing {}", py312.display()));
    }
    // bin/python3 → python3.12 symlink (the name scripts + the smoke gate use).
    let py3 = usr.join("bin/python3");
    if fs::symlink_metadata(&py3).is_err() {
        return Err(format!("python build missing {}", py3.display()));
    }
    // The stdlib landmark CPython searches upward for to resolve sys.prefix
    // (kept loose by `freeze_stdlib_zip`; the rest of the stdlib is in the zip).
    let os_py = usr.join(format!("lib/python{PYTHON_XY}/os.py"));
    if !os_py.exists() {
        return Err(format!(
            "python build missing stdlib landmark {}",
            os_py.display()
        ));
    }
    // The frozen stdlib zip (already on CPython's default sys.path) is where
    // every other stdlib module lives.
    let stdlib_zip = usr.join(format!("lib/python{}.zip", PYTHON_XY.replace('.', "")));
    if !stdlib_zip.exists() {
        return Err(format!(
            "python build missing frozen stdlib {}",
            stdlib_zip.display()
        ));
    }
    // The interpreter MUST be static: a dynamic build would carry a PT_INTERP
    // referencing `/lib/ld-musl-x86_64.so.1` and fault at startup on m3OS (no
    // real libc.so). A static musl ELF embeds libc and never names the loader,
    // so the absence of that interp string is a direct proof of `-static`.
    if binary_contains(&py312, b"/lib/ld-musl-x86_64.so.1")? {
        return Err(format!(
            "python build: {} references the dynamic loader — `-static` did not take effect \
             (a dynamic interpreter cannot resolve DT_NEEDED libc.so on m3OS)",
            py312.display()
        ));
    }
    // No lib-dynload dir should survive (prune removed the demo-only one): every
    // real extension is builtin. Its absence confirms the all-static build.
    let lib_dynload = usr.join(format!("lib/python{PYTHON_XY}/lib-dynload"));
    if lib_dynload.exists() {
        return Err(format!(
            "python build: {} still present after prune (a real shared extension leaked)",
            lib_dynload.display()
        ));
    }
    println!(
        "python: staged static /usr/bin/python3 + frozen stdlib (python{}.zip; all C \
         extensions builtin; zlib/gzip + curses (wide ncurses) + hashlib via built-in HACL \
         _md5/_sha*; _ssl/_hashlib/_ctypes/DNS absent)",
        PYTHON_XY.replace('.', "")
    );
    Ok(())
}

/// Substring search over a file's raw bytes. Used by [`build_git`] to assert
/// the *absence* of libcurl / OpenSSL API symbols in the built binary.
///
/// This guard **fails closed**: a read failure returns `Err`, which the caller
/// propagates as a build error. Returning `Ok(false)` ("symbol absent") on an
/// unreadable file would silently weaken the NO_CURL/NO_OPENSSL assertion — the
/// build would pass without ever proving the forbidden symbols are gone. The
/// caller already verified the binary exists, so a read failure here is a
/// genuine anomaly worth aborting on rather than skipping the check.
fn binary_contains(path: &Path, needle: &[u8]) -> Result<bool, String> {
    let bytes = fs::read(path).map_err(|e| {
        format!(
            "git build: cannot read {} to verify NO_CURL/NO_OPENSSL: {e}",
            path.display()
        )
    })?;
    if needle.is_empty() || needle.len() > bytes.len() {
        return Ok(false);
    }
    Ok(bytes.windows(needle.len()).any(|window| window == needle))
}

fn file_size(p: &Path) -> u64 {
    fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn num_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
}

/// Ensure a host-side `yacc` is reachable on PATH for tmux's
/// `cmd-parse.y` parser generation.  Autoconf-generated configure
/// scripts shell out to `yacc` by name, so a host that ships only
/// `bison` (Debian) or only `byacc` (without the `yacc` symlink) does
/// not satisfy the requirement on its own.
///
/// Resolution order:
///   1. If a `yacc` executable is already on PATH, do nothing.
///   2. If `bison` or `byacc` is on PATH, stage a `yacc` symlink under
///      `target/host-bin/` pointing at the discovered tool and prepend
///      that directory to PATH.
///   3. Otherwise, download and build Berkeley yacc (byacc) 20240109
///      from upstream — byacc is a one-file C program with no external
///      dependencies and builds in a few seconds — then stage the
///      `yacc` symlink as in (2).
fn ensure_yacc() -> Result<(), String> {
    if probe_tool_on_path("yacc") {
        return Ok(());
    }

    let root = workspace_root();
    let host_bin = root.join("target/host-bin");
    let yacc_bin = host_bin.join("yacc");

    if !yacc_bin.is_file() {
        fs::create_dir_all(&host_bin).map_err(|e| format!("mkdir host_bin: {e}"))?;
        if let Some(existing) = ["bison", "byacc"]
            .into_iter()
            .find(|t| probe_tool_on_path(t))
            .and_then(which_on_path)
        {
            std::os::unix::fs::symlink(&existing, &yacc_bin)
                .map_err(|e| format!("symlink yacc -> {}: {e}", existing.display()))?;
            println!(
                "yacc: staged symlink {} -> {}",
                yacc_bin.display(),
                existing.display()
            );
        } else {
            bootstrap_byacc(&host_bin, &yacc_bin)?;
        }
    }
    prepend_path(&host_bin);
    Ok(())
}

/// Check whether a tool responds to `--version` on PATH without panicking
/// on hosts where the binary is missing.
fn probe_tool_on_path(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Resolve `tool` on PATH to its absolute path via `command -v`.  Returns
/// None when the lookup fails or returns a relative path we can't anchor.
fn which_on_path(tool: &str) -> Option<PathBuf> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() { Some(path) } else { None }
}

/// Prepend `dir` to the current process's PATH so subsequent
/// `Command::new` invocations resolve through it.  Uses
/// `std::env::split_paths` to compare PATH entries by exact equality
/// — substring matching would let `/foo/host-bin2` shadow
/// `/foo/host-bin`.
fn prepend_path(dir: &Path) {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let already_present = std::env::split_paths(&existing).any(|p| p == dir);
    if already_present {
        return;
    }
    let mut entries: Vec<PathBuf> = vec![dir.to_path_buf()];
    entries.extend(std::env::split_paths(&existing));
    match std::env::join_paths(entries) {
        Ok(joined) => {
            // SAFETY: xtask is single-threaded at this point; child processes
            // inherit the updated env.  std::env::set_var is `unsafe` in
            // Rust edition 2024 because of cross-thread UB risks.
            unsafe {
                std::env::set_var("PATH", joined);
            }
        }
        Err(e) => {
            eprintln!("prepend_path: join_paths failed: {e}");
        }
    }
}

/// Download + build Berkeley yacc (byacc) 20240109 and stage a `yacc`
/// symlink in `host_bin/`.  Used as the fallback when the host ships
/// neither `yacc`, `bison`, nor `byacc`.
fn bootstrap_byacc(host_bin: &Path, yacc_bin: &Path) -> Result<(), String> {
    const BYACC_URL: &str = "https://invisible-mirror.net/archives/byacc/byacc-20240109.tgz";
    // DevSkim: ignore DS173237 -- SHA-256 of the public byacc-20240109 tarball,
    // a content-addressed integrity check consumed by fetch_tarball; not a secret.
    const BYACC_SHA: &str = "f2897779017189f1a94757705ef6f6e15dc9208ef079eea7f28abec577e08446";

    let root = workspace_root();
    let cache_dir = root.join("target/port-src");
    let tarball = fetch_tarball(BYACC_URL, BYACC_SHA, &cache_dir)?;
    let work = root.join("target/byacc-build");
    let extracted = extract_tarball(&tarball, &work)?;
    let prefix = host_bin.join("byacc-prefix");
    let _ = fs::remove_dir_all(&prefix);
    let mut configure_cmd = Command::new("sh");
    configure_cmd
        .current_dir(&extracted)
        .arg("./configure")
        .arg(format!("--prefix={}", prefix.display()))
        .arg("--program-prefix=")
        .env("CFLAGS", "-O2");
    run(&mut configure_cmd, "byacc configure")?;
    let mut make_cmd = Command::new("make");
    make_cmd
        .current_dir(&extracted)
        .arg(format!("-j{}", num_jobs()));
    run(&mut make_cmd, "byacc make")?;
    let mut install_cmd = Command::new("make");
    install_cmd.current_dir(&extracted).arg("install");
    run(&mut install_cmd, "byacc install")?;
    let installed = prefix.join("bin/yacc");
    if !installed.is_file() {
        return Err(format!("byacc build missing {}", installed.display()));
    }
    let _ = fs::remove_file(yacc_bin);
    std::os::unix::fs::symlink(&installed, yacc_bin).map_err(|e| format!("symlink yacc: {e}"))?;
    println!("byacc: bootstrapped {}", yacc_bin.display());
    Ok(())
}

/// Locate the arch-specific Linux UAPI directory containing `asm/types.h`.
/// On Debian/Ubuntu this is `/usr/include/x86_64-linux-gnu`; on Arch and
/// musl-only systems it is bundled with the host kernel headers under
/// `/usr/include`. Returns the first candidate that exists, or falls back
/// to `/usr/include` so the caller's `-idirafter` still resolves to a
/// real path.
fn linux_uapi_arch_include() -> PathBuf {
    for cand in &[
        "/usr/include/x86_64-linux-gnu",
        "/usr/include/x86_64-linux-musl",
    ] {
        let p = PathBuf::from(cand);
        if p.join("asm/types.h").is_file() {
            return p;
        }
    }
    PathBuf::from("/usr/include")
}

/// Build every port in the Phase 69d set in dependency order. Used by the
/// image-build pipeline to pre-stage everything before the ext2 disk is
/// populated.
///
/// Returns `Ok` if every port either builds successfully or was already
/// staged with a current fingerprint.  Errors short-circuit with a message
/// the caller can surface.
pub fn build_phase_69d_ports() -> Result<(), String> {
    if musl_cc().is_none() {
        return Err(
            "no musl cross-compiler on PATH (install musl-tools or musl-gcc-cross-bin)".to_string(),
        );
    }
    for name in &["zlib", "ncurses", "libevent", "less", "htop", "tmux"] {
        port_build(name).map_err(|e| format!("port {name}: {e}"))?;
    }
    Ok(())
}

/// Phase 85b — build the local-only `git` toolchain and its single dependency
/// (zlib) into their `.m3pkg` artifacts. Separate from [`build_phase_69d_ports`]
/// so a routine image build does not pay git's multi-minute cross-compile; the
/// `git-local-smoke` gate (and any explicit `cargo xtask port build git`) drives
/// this. zlib is built first so [`build_git`] finds the staged `libz.a`.
pub fn build_git_port() -> Result<(), String> {
    if musl_cc().is_none() {
        return Err(
            "no musl cross-compiler on PATH (install musl-tools or musl-gcc-cross-bin)".to_string(),
        );
    }
    port_build("zlib").map_err(|e| format!("port zlib: {e}"))?;
    port_build("git").map_err(|e| format!("port git: {e}"))?;
    Ok(())
}

/// Phase 85c — build the zlib + ncurses + python ports so their sealed `.m3pkg`
/// artifacts exist for the image populator to bundle into `/usr/pkg/`. The first
/// build two-stage cross-compiles CPython (several minutes); a warm pkgcache
/// makes this a zero-compiler hit. zlib + ncurses are built first so
/// [`build_python`] finds the staged `libz.a` (zlib/gzip) and the staged wide
/// `libncursesw.a`/`libtinfow.a`/`libpanelw.a` (`_curses`/`_curses_panel`).
/// ncurses is a **build-only** dependency: it is linked *statically* into the
/// interpreter, so python's runtime `DEPS` is just `zlib` — nothing `pkg
/// install`s ncurses (its terminfo DB is already pre-installed via the
/// phase-69d port mirror, and re-installing thousands of terminfo files over
/// m3OS's slow VFS would blow the gate's install budget).
pub fn build_python_port() -> Result<(), String> {
    if musl_cc().is_none() {
        return Err(
            "no musl cross-compiler on PATH (install musl-tools or musl-gcc-cross-bin)".to_string(),
        );
    }
    port_build("zlib").map_err(|e| format!("port zlib: {e}"))?;
    port_build("ncurses").map_err(|e| format!("port ncurses: {e}"))?;
    port_build("python").map_err(|e| format!("port python: {e}"))?;
    Ok(())
}

/// Phase 85d — build the Clang/LLVM/LLD toolchain into its `.m3pkg`. Separate
/// from the routine ports (it is the heaviest artifact, multi-GB-RAM / multi-hour
/// on a cold cache) so only the opt-in image feature + the `clang-smoke` gate +
/// an explicit `cargo xtask port build llvm` drive it. Unlike the other ports it
/// has no musl-gcc dependency (it cross-builds with the host clang); it has no
/// runtime `DEPS`, so nothing else is built first. A warm pkgcache makes a repeat
/// build a zero-compiler hit (the headline Phase 85a payoff, proven on the
/// heaviest artifact).
pub fn build_llvm_port() -> Result<(), String> {
    port_build("llvm").map_err(|e| format!("port llvm: {e}"))?;
    Ok(())
}

/// Mirror the staged `usr/local/{bin,lib,include}` tree for one port back to
/// `dest_dir/usr/local/...`. Used by the ext2 populator to flatten every
/// Phase 69d port into the data disk.
#[allow(dead_code)]
pub fn collect_stage_files(
    name: &str,
    dest_dir: &mut Vec<(String, PathBuf)>,
    dest_dirs: &mut Vec<String>,
) {
    let root = workspace_root();
    let stage = root.join("target/port-stage").join(name).join("usr/local");
    if !stage.is_dir() {
        return;
    }
    walk_stage(&stage, "usr/local", dest_dir, dest_dirs);
}

#[allow(dead_code)]
fn walk_stage(
    src: &Path,
    prefix: &str,
    files: &mut Vec<(String, PathBuf)>,
    dirs: &mut Vec<String>,
) {
    dirs.push(prefix.to_string());
    let entries = match fs::read_dir(src) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let child_prefix = format!("{prefix}/{name_str}");
        if path.is_dir() {
            walk_stage(&path, &child_prefix, files, dirs);
        } else if path.is_file() {
            files.push((child_prefix, path));
        }
    }
}

/// Read a port's installed binary as bytes (used by initrd-flat embedding
/// fallback if the data-disk path is unavailable for a given smoke).
#[allow(dead_code)]
pub fn read_staged_binary(name: &str, exe: &str) -> Option<Vec<u8>> {
    let root = workspace_root();
    let path = root
        .join("target/port-stage")
        .join(name)
        .join("usr/local/bin")
        .join(exe);
    fs::read(&path).ok()
}

/// Test hooks.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portfile_parse_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("Portfile");
        let mut f = File::create(&p).unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f, "NAME=foo").unwrap();
        writeln!(f, "VERSION=1.2.3").unwrap();
        writeln!(f, "URL=https://example/foo-1.2.3.tar.gz").unwrap();
        writeln!(f, "SHA256=deadbeef").unwrap();
        drop(f);
        let m = parse_portfile(&p).expect("parse Portfile");
        assert_eq!(m.get("NAME").unwrap(), "foo");
        assert_eq!(m.get("SHA256").unwrap(), "deadbeef");
    }

    #[test]
    fn find_ncurses_port() {
        // Sanity: the Phase 69d Portfile must be discoverable.
        let p = find_port_dir("ncurses").expect("ncurses Portfile staged in repo");
        assert!(p.join("Portfile").exists());
    }

    // ── A.1 — recipe_digest hashes every file under patches/, recursively ──

    /// Regression: `recipe_digest` documents that it folds in *every file under
    /// `patches/`*. A patch placed in a **nested** subdirectory must therefore
    /// invalidate the digest when its contents change — otherwise a hierarchical
    /// patches tree could let a patch edit ride a stale cached `.m3pkg`.
    #[test]
    fn recipe_digest_includes_nested_patch_files() {
        let tmp = tempfile::tempdir().unwrap();
        let port = tmp.path();
        fs::write(port.join("Portfile"), b"NAME=demo\nSHA256=abc\n").unwrap();
        let nested = port.join("patches").join("series");
        fs::create_dir_all(&nested).unwrap();
        let patch = nested.join("0001-fix.patch");
        fs::write(&patch, b"--- a\n+++ b\n@@ original @@\n").unwrap();

        let before = recipe_digest(port);

        // Editing the *nested* patch's contents must change the digest.
        fs::write(&patch, b"--- a\n+++ b\n@@ edited @@\n").unwrap();
        let after = recipe_digest(port);

        assert_ne!(
            before, after,
            "a change to a nested patch file must invalidate the recipe digest"
        );
    }

    /// Same-named patch files in different subdirectories must not collide:
    /// the digest keys each file by its path **relative to `patches/`**, so
    /// swapping two equally-named files' contents across subdirs changes the
    /// digest. The previous immediate-`read_dir` digest never recursed into
    /// subdirs at all, so it would have missed this swap entirely.
    #[test]
    fn recipe_digest_keys_patches_by_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let port = tmp.path();
        fs::write(port.join("Portfile"), b"NAME=demo\nSHA256=abc\n").unwrap();
        let a = port.join("patches").join("a");
        let b = port.join("patches").join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("p.patch"), b"AAA").unwrap();
        fs::write(b.join("p.patch"), b"BBB").unwrap();
        let before = recipe_digest(port);

        // Swap the two same-named files' contents across subdirs.
        fs::write(a.join("p.patch"), b"BBB").unwrap();
        fs::write(b.join("p.patch"), b"AAA").unwrap();
        let after = recipe_digest(port);

        assert_ne!(
            before, after,
            "relative-path keying must distinguish same-named patches in different subdirs"
        );
    }

    // ── B.1 — seal_package ───────────────────────────────────────────────

    /// Exercise `seal_package`'s constituent steps — `pkg_format::pack` plus the
    /// atomic temp-write/rename into `target/pkgcache/<key>.m3pkg` — on a minimal
    /// fake stage, and assert that:
    ///   - `target/pkgcache/<key>.m3pkg` is created
    ///   - `pkg_format::verify` passes on the resulting bytes
    ///
    /// The steps are reproduced inline rather than calling `seal_package`
    /// directly because that function resolves `workspace_root()` internally
    /// (xtask-specific). The `.pkgkey` diagnostic sidecar that the real
    /// `seal_package` writes alongside the artifact is therefore out of scope
    /// for this unit test. No compiler is invoked — this is pure filesystem logic.
    #[test]
    fn seal_package_creates_valid_m3pkg() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().join("stage");
        fs::create_dir_all(stage.join("usr/local/bin")).unwrap();
        fs::write(stage.join("usr/local/bin/foo"), b"fake-elf-bytes").unwrap();
        fs::write(stage.join("usr/local/bin/bar"), b"another-binary").unwrap();

        // Use a temp directory as the workspace-root substitute: redirect
        // target/pkgcache/ inside tmp so we do not pollute the real build tree.
        let fake_root = tmp.path().join("fake-root");
        let pkgcache = fake_root.join("target/pkgcache");
        fs::create_dir_all(&pkgcache).unwrap();

        // We test seal_package's constituent steps directly (pack + atomic write)
        // rather than calling the private function through the crate, because
        // `seal_package` calls `workspace_root()` internally (xtask-specific).
        // Instead, reproduce the logic verbatim so the test is self-contained.
        let key = "0000000000000000000000000000000000000000000000000000000000000001";
        let artifact = pkgcache.join(format!("{key}.m3pkg"));
        let tmp_file = pkgcache.join(format!("{key}.m3pkg.tmp"));

        let bytes = pkg_format::pack(&stage).expect("pack should succeed");
        fs::write(&tmp_file, &bytes).unwrap();
        fs::rename(&tmp_file, &artifact).unwrap();

        // Artifact exists.
        assert!(artifact.exists(), "artifact must be created by seal");

        // Content is valid.
        let read_back = fs::read(&artifact).unwrap();
        assert!(
            pkg_format::verify(&read_back),
            "verify must pass on the sealed artifact"
        );
    }

    /// `pack` then `unpack` round-trips the stage tree: every file that went in
    /// comes back out with identical bytes.  This test exercises the
    /// seal→resolve pipeline without a musl cross-compiler.
    #[test]
    fn seal_then_resolve_round_trips_stage() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().join("stage-original");
        fs::create_dir_all(stage.join("usr/local/lib")).unwrap();
        fs::create_dir_all(stage.join("usr/local/bin")).unwrap();
        fs::write(
            stage.join("usr/local/lib/libfoo.a"),
            b"\x7fELF-fake-archive",
        )
        .unwrap();
        fs::write(stage.join("usr/local/bin/mytool"), b"#!/bin/sh\necho hi\n").unwrap();

        // Seal.
        let bytes = pkg_format::pack(&stage).expect("pack");
        assert!(pkg_format::verify(&bytes), "verify after pack");

        // Resolve into a fresh dest.
        let dest = tmp.path().join("stage-restored");
        fs::create_dir_all(&dest).unwrap();
        let n = pkg_format::unpack(&bytes, &dest).expect("unpack");
        assert_eq!(n, 2, "two files should round-trip");

        // Content matches.
        assert_eq!(
            fs::read(dest.join("usr/local/lib/libfoo.a")).unwrap(),
            b"\x7fELF-fake-archive"
        );
        assert_eq!(
            fs::read(dest.join("usr/local/bin/mytool")).unwrap(),
            b"#!/bin/sh\necho hi\n"
        );
    }

    /// A single flipped byte must cause `verify` to return `false` (integrity
    /// protection check for the pkgcache — prevents silent stage corruption).
    #[test]
    fn verify_detects_single_flipped_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().join("stage");
        fs::create_dir_all(stage.join("bin")).unwrap();
        fs::write(stage.join("bin/x"), b"original content").unwrap();

        let mut bytes = pkg_format::pack(&stage).expect("pack");
        assert!(pkg_format::verify(&bytes), "baseline must verify");

        // Flip the last byte of the data blob.
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        assert!(
            !pkg_format::verify(&bytes),
            "verify must fail after bit flip"
        );
    }

    // ── B.2 — port_deps ──────────────────────────────────────────────────

    #[test]
    fn port_deps_known_ports() {
        assert_eq!(port_deps("less"), &["ncurses"]);
        assert_eq!(port_deps("htop"), &["ncurses"]);
        // tmux depends on both ncurses and libevent (order matters for
        // sorted dep key computation — sorted internally by compute_package_key).
        let tmux_deps = port_deps("tmux");
        assert!(
            tmux_deps.contains(&"ncurses") && tmux_deps.contains(&"libevent"),
            "tmux must list both ncurses and libevent as deps"
        );
        assert_eq!(port_deps("ncurses"), &[] as &[&str]);
        assert_eq!(port_deps("libevent"), &[] as &[&str]);
        // Phase 85b — git's one mandatory dependency is zlib.
        assert_eq!(port_deps("git"), &["zlib"]);
        // Phase 85c — python's one *runtime* dependency is zlib (zlib/gzip).
        // ncurses (for the statically-linked _curses/_curses_panel) is a
        // BUILD-only dep, deliberately absent here so the in-OS solver does not
        // re-install its terminfo DB; its pin is folded via build_recipe_id.
        assert_eq!(port_deps("python"), &["zlib"]);
        // Unknown port returns empty.
        assert_eq!(port_deps("clang"), &[] as &[&str]);
    }

    // ── M5 — build_recipe_id (configure-flag identity folded into the key) ──

    #[test]
    fn build_recipe_id_is_distinct_and_nonempty_per_host_port() {
        // Every host-built port must have a non-empty, distinct configure-flag
        // identity so a flag change self-invalidates its cached .m3pkg and two
        // ports never collide on the recipe component of the key.
        let ports = [
            "zlib", "ncurses", "libevent", "less", "htop", "tmux", "git", "python",
        ];
        let ids: Vec<&str> = ports.iter().map(|p| build_recipe_id(p)).collect();
        for (p, id) in ports.iter().zip(&ids) {
            assert!(!id.is_empty(), "{p} must have a build_recipe_id");
        }
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "build_recipe_ids must be distinct");
        // Unknown ports yield the empty identity (no recipe contribution).
        assert_eq!(build_recipe_id("clang"), "");
    }

    // ── Phase 85b — binary_contains fails closed (NO_CURL/NO_OPENSSL guard) ──

    /// The NO_CURL/NO_OPENSSL guard must **fail closed**: if the built `git`
    /// binary cannot be read, [`binary_contains`] must return `Err` (aborting
    /// the build) rather than `Ok(false)` — otherwise the security assertion
    /// would silently pass without ever proving the forbidden symbols are gone.
    #[test]
    fn binary_contains_fails_closed_on_unreadable_path() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.bin");
        let r = binary_contains(&missing, b"curl_easy_perform");
        assert!(
            r.is_err(),
            "an unreadable binary must error (fail closed), got {r:?}"
        );
    }

    /// Positive/negative substring detection on a readable file.
    #[test]
    fn binary_contains_detects_present_and_absent_needle() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("blob.bin");
        fs::write(&f, b"....SSL_CTX_new....").unwrap();
        assert_eq!(
            binary_contains(&f, b"SSL_CTX_new").unwrap(),
            true,
            "present symbol must be detected"
        );
        assert_eq!(
            binary_contains(&f, b"curl_easy_perform").unwrap(),
            false,
            "absent symbol must report false on a readable file"
        );
    }

    /// An empty needle or a needle longer than the file is reported as absent
    /// (`Ok(false)`), not an error — only an I/O failure is fail-closed.
    #[test]
    fn binary_contains_edge_cases_report_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("small.bin");
        fs::write(&f, b"hi").unwrap();
        assert_eq!(binary_contains(&f, b"").unwrap(), false);
        assert_eq!(
            binary_contains(&f, b"this-needle-is-longer").unwrap(),
            false
        );
    }
}
