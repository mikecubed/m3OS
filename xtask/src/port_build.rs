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
    // Phase 86d Track D — also hash `src/` (the in-repo program built by ports
    // like `go`), recursively + sorted, so editing `src/runtime_probe.go`
    // invalidates the same-machine stamp. Mirrors the portable `recipe_digest`.
    let src = port_dir.join("src");
    let mut src_files: Vec<(PathBuf, PathBuf)> = Vec::new();
    collect_files_rel(&src, &src, &mut src_files);
    src_files.sort_by(|a, b| a.0.cmp(&b.0));
    if !src_files.is_empty() {
        hasher.update(b"\0src\0");
    }
    for (rel, abs) in src_files {
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        if let Ok(bytes) = fs::read(&abs) {
            hasher.update(&bytes);
        }
    }
    // Include the port_build.rs source content so editing the build
    // recipe re-runs every staged port on the next invocation.
    hasher.update(b"\0port_build.rs\0");
    hasher.update(include_bytes!("port_build.rs"));
    format!("{:x}", hasher.finalize())
}

/// Compute a stable, portable recipe-identity digest for a port: the Portfile
/// content plus every file under `patches/` **and** `src/`. Unlike
/// [`port_fingerprint`] this deliberately **excludes** the `port_build.rs`
/// source bytes, so editing an *unrelated* port recipe does not invalidate this
/// port's cached `.m3pkg`. `src/` is included because a port (notably `go`) may
/// build an in-repo program rather than the downloaded tarball.
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
    // Phase 86d Track D — also hash every file under `src/`. For the `go` port
    // the built artifact IS the in-repo source (`src/runtime_probe.go`), not the
    // downloaded tarball, so without this a probe edit would leave the content
    // key unchanged and serve a stale `.m3pkg`. Hashed identically to `patches/`
    // (relative-path keyed, recursive, sorted). Ports with no `src/` dir are
    // unaffected; ports that DO have one (go, and a few vestigial trees) simply
    // re-cache once. Keyed under a distinct `src\0` prefix so a file cannot
    // collide with a same-named patch.
    let src = port_dir.join("src");
    let mut src_files: Vec<(PathBuf, PathBuf)> = Vec::new();
    collect_files_rel(&src, &src, &mut src_files);
    src_files.sort_by(|a, b| a.0.cmp(&b.0));
    if !src_files.is_empty() {
        hasher.update(b"src\0");
    }
    for (rel, abs) in src_files {
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
        // Phase 86c Track B — git REBUILT WITH curl (NO_CURL removed; NO_OPENSSL
        // kept since the TLS backend is mbedTLS-via-curl, not OpenSSL). The curl
        // link is wired through CURL_CFLAGS/CURL_LDFLAGS (the static libcurl +
        // mbedtls + zlib link line) + CURL_CONFIG=true (neutralizes the host
        // curl-config --vernum probe). The 85b absence-assertions are INVERTED to
        // presence: git-remote-https/git-remote-http/git-http-fetch must exist and
        // the remote-http helper must reference curl_multi_perform + a mbedTLS TLS
        // symbol; SSL_CTX_new must stay ABSENT (NO_OPENSSL). The server-side
        // pack-helper prune is unchanged. Bump recipe-v on any knob/assertion
        // change. (curl/mbedtls dep keys fold in via compute_port_key.)
        "git" => {
            "make:NO_OPENSSL=1 NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 \
             NO_ICONV=1 NO_EXPAT=1 NO_REGEX=NeedsStartEnd NEEDS_LIBICONV= \
             SKIP_DASHED_BUILT_INS=YesPlease ZLIB_PATH=<zlib_stage>/usr/local \
             CURL_CONFIG=true CURL_CFLAGS=<curl_stage include> \
             CURL_LDFLAGS=<static libcurl+mbedtls+zlib link line> \
             SHELL_PATH=/bin/sh prefix=/usr;cflags:-O2;\
             curl=ENABLED(git-remote-https+git-http-fetch present,curl_multi_perform+mbedtls_ssl symbol required,SSL_CTX_new absent);\
             prune=scalar,git-shell,upload-pack,receive-pack,upload-archive,imap-send,http-backend,daemon,sh-i18n--envsubst;recipe-v=4"
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
             CLANG_CONFIG_FILE_SYSTEM_DIR=.;INSTALL_TOOLCHAIN_ONLY=ON);\
             install=install-clang,install-clang-resource-headers,install-lld;\
             stage=resource-dir-builtins+bundled-usr-sysroot(musl+libc++)+clang++-symlink\
             +static-clang.cfg(flagless -static default);\
             seal-strip=skip-ET_REL(crt objects keep _start);\
             recipe-v=2"
        }
        // Phase 86b — dropbear's static client-only build. The identity is the
        // `PROGRAMS=dbclient` single-program build + the no-zlib/no-harden
        // configure flag set (the static link is forced by `-static` LDFLAGS +
        // `--disable-harden`) + the bundled-libtom software crypto + the
        // post-install stage shape (prune the manpage, add the `ssh` copy of
        // `dbclient` so both names land on PATH). Bump recipe-v whenever any of
        // these change so the cached `.m3pkg` self-invalidates. (The pinned
        // dropbear version + its tarball SHA-256 are folded in via the Portfile
        // digest.)
        "dropbear" => {
            "client-only(make PROGRAMS=dbclient);\
             configure(--disable-zlib --disable-harden \
             --enable-bundled-libtom --disable-syslog --disable-lastlog \
             --disable-utmp --disable-utmpx --disable-wtmp --disable-wtmpx \
             --host=x86_64-linux-musl --prefix=/usr);cflags:-O2;ldflags:-static;\
             stage=prune(share/man)+ssh-copy(/usr/bin/ssh<-dbclient);recipe-v=2"
        }
        // Phase 86c Track A — mbedTLS static archives (curl --with-mbedtls
        // backend). Identity: `make lib` (no programs/tests) + the config.py
        // trim from the shipped default (client-only: SSL_SRV_C/DTLS off, NET_C
        // off; TLS1.3 on) + the sys_getrandom entropy swap (NO_PLATFORM_ENTROPY +
        // ENTROPY_HARDWARE_ALT + the mbedtls_hardware_poll shim) + the manual
        // headers(mbedtls,psa)+3-archive install. Bump recipe-v whenever any of
        // these change so the cached .m3pkg self-invalidates. (The pinned mbedTLS
        // version + its tarball SHA-256 are folded in via the Portfile digest.)
        "mbedtls" => {
            "make:lib(static archives only,CC/AR cross,CFLAGS=-O2);\
             config.py(from-default unset:SSL_SRV_C,SSL_PROTO_DTLS,SSL_DTLS_ANTI_REPLAY,\
             SSL_DTLS_HELLO_VERIFY,SSL_DTLS_SRTP,SSL_DTLS_CONNECTION_ID,NET_C \
             set:SSL_PROTO_TLS1_3,NO_PLATFORM_ENTROPY,ENTROPY_HARDWARE_ALT);\
             entropy:mbedtls_hardware_poll->getrandom(318)+failclosed-on-0,ar-into-libmbedcrypto;\
             verify:config-on/off+entropy-self-test+no-/dev/urandom;\
             install:headers(mbedtls,psa,idempotent)+libmbed{crypto,x509,tls}.a->/usr/local;recipe-v=2"
        }
        // Phase 86c Track B — curl static HTTP/HTTPS client (mbedTLS backend).
        // Identity: the autotools configure flag set (static, --with-mbedtls +
        // --with-ca-bundle to the Phase 86a path, HTTP/HTTPS only, sync resolver,
        // every optional dep dropped) + the staged zlib/mbedtls link + the
        // build-time verification (static, mbedTLS-backend, HTTPS, embedded
        // CAINFO). Bump recipe-v whenever the flags change so the cached .m3pkg
        // self-invalidates. (curl version + tarball SHA-256 fold in via the
        // Portfile digest; the mbedtls/zlib dep keys fold in via compute_port_key.)
        "curl" => {
            "configure(--host=x86_64-linux-musl --prefix=/usr/local --disable-shared \
             --enable-static --with-mbedtls=<staged> --with-zlib=<staged> \
             --with-ca-bundle=/etc/ssl/certs/ca-certificates.crt --disable-threaded-resolver \
             --disable-{ldap,ldaps,ftp,file,dict,gopher,imap,pop3,smtp,telnet,tftp,rtsp,smb,mqtt} \
             --without-{libpsl,libidn2,brotli,zstd,nghttp2,nghttp3,ngtcp2,libssh2,libssh,librtmp,gssapi} \
             --disable-manual);cflags:-O2;configure-ldflags:-static;make-ldflags:-all-static(libtool fully-static CLI);\
             verify:static+mbedTLS-backend+HTTPS+embedded-CAINFO;recipe-v=2"
        }
        // Phase 86d Track D — the static Go runtime probe. Unlike every other
        // port the built artifact is NOT the downloaded tarball (that is the Go
        // *toolchain*, folded in via the Portfile SHA-256); it is the in-repo
        // program `src/runtime_probe.go`, whose bytes are folded in separately
        // because `recipe_digest` now also hashes `src/`. This arm pins the
        // cross-build knob set so a flag change self-invalidates the cached
        // `.m3pkg`. The Go toolchain identity IS now auto-folded into the content
        // key via `go_toolchain_id()` (see `compute_port_key_inner`), so a Go
        // version bump auto-invalidates this cache; bump recipe-v only when the
        // `build_go` flags below change.
        "go" => {
            "go-build:GOOS=linux GOARCH=amd64 CGO_ENABLED=0 GOTOOLCHAIN=local \
             GOPROXY=off GOFLAGS=-mod=mod;build:-trimpath -ldflags='-s -w';\
             stage=/usr/bin/go-runtime-probe+/usr/src/runtime_probe.go;recipe-v=1"
        }
        // Phase 86e — the static GitHub CLI. Like `go`, the built artifact is NOT
        // the downloaded tarball (that is the gh SOURCE, folded in via the
        // Portfile SHA-256); it is the compiled `gh` binary. This arm pins the
        // cross-build knob set so a flag change self-invalidates the cached
        // `.m3pkg`. The Go toolchain identity IS now auto-folded into the content
        // key via `go_toolchain_id()` (see `compute_port_key_inner`), so a Go
        // version bump auto-invalidates this cache without needing a recipe-v bump;
        // `toolchain=go1.24.6` here is human-readable documentation only.
        "gh" => {
            "go-build:GOOS=linux GOARCH=amd64 CGO_ENABLED=0 GOTOOLCHAIN=local \
             GOPROXY=https://proxy.golang.org,direct GOFLAGS=-mod=mod;\
             build:-trimpath -ldflags='-s -w -X github.com/cli/cli/v2/internal/build.Version=<version>';\
             toolchain=go1.24.6;stage=/usr/bin/gh;recipe-v=1"
        }
        // Phase 89 — Node.js. The built artifact is `node`+`npm` (the tarball is
        // the SOURCE, folded in via the Portfile SHA-256). This arm pins the V8/
        // configure knob set (jitless for W^X, static musl, full-icu) so a flag
        // change self-invalidates the cached `.m3pkg`; the host clang identity is
        // folded separately via `host_cxx_toolchain_id()` in `compute_port_key`.
        //
        // Phase 90a (D.2) — the JIT/jitless VARIANT is NOT pinned here (this arm is
        // `&'static str` and cannot read `M3OS_NODE_JIT`); it is folded into node's
        // content key via the per-port `tc` string in `compute_port_key_inner`
        // (`variant=jit|jitless` via `node_variant_id()`), so the two variants get
        // distinct keys regardless of this arm. The JIT variant additionally drops
        // `--v8-options=--jitless` + applies the three A.1 V8/Node PKU patches +
        // links the `pkey_*` shim (all gated on `node_jit_enabled()` in build_node).
        // recipe-v bumped to 5: build_node's recipe changed (variant branching), so
        // invalidate the stale Phase 89 cached jitless `.m3pkg`.
        // recipe-v 6: the JIT variant gained patch_pkey_disable_macros — define
        // PKEY_DISABLE_ACCESS/WRITE in V8's allocator + memory-protection-key TUs
        // (V8's gyp build doesn't inherit our CXXFLAGS -D, so PkeyAlloc() was a
        // `return -1` stub and PKU never engaged). Invalidate the pre-fix `.m3pkg`.
        // recipe-v 7: the `pkey_*` shim is now compiled as C++ (staged as
        // `m3os-pkey-shim.cc`, built with `-x c++`, no `extern "C"`) so its strong
        // defs mangle to V8's weak-ref names (`_Z10pkey_allocjj` …). The prior C
        // shim defined unmangled C symbols that did NOT bind V8's mangled weak
        // refs on musl, so `PkeyAlloc()`'s `if (!pkey_alloc) return -1;` saw a null
        // weak symbol and PKU stayed disabled (plain mprotect(RWX)). Invalidate the
        // pre-fix `.m3pkg`.
        // recipe-v 8: switched `--with-intl=small-icu` → `--with-intl=full-icu`
        // (+ `--download=all`). small-icu omits the ICU break-iterator data, so
        // `Intl.Segmenter.segment()` null-derefs in V8's `JSSegments::Create` —
        // which makes Claude Code's interactive TUI (Unicode grapheme segmentation)
        // unusable. full-icu compiles the complete ICU data into the binary.
        // Invalidate the small-icu `.m3pkg`.
        "node" => {
            "node-configure:--fully-static --enable-static --dest-cpu=x64 \
             --dest-os=linux --with-intl=full-icu --download=all --v8-options=--jitless \
             --openssl-no-asm --without-corepack --without-node-snapshot \
             --without-inspector;single-toolset;make-generator;wasm-in-jitless;\
             cxxstdlib=libc++-musl;jit-variant=M3OS_NODE_JIT(key-folded);\
             pkey-disable-macros-in-v8-tu;pkey-shim-cxx-linkage;recipe-v=8"
        }
        // Phase 90b — Claude Code. A FETCH-AND-STAGE port (no compiler): the
        // built artifact is the pinned npm tarball's payload, staged under
        // /usr/lib/claude-code/ + a /usr/bin/claude launcher. The pinned version
        // + tarball SHA-256 are folded in via the Portfile digest, and the node
        // runtime's full (JIT-variant-aware) key folds in via the DEPS=node
        // recursion in compute_port_key. This arm pins the STAGING recipe (layout,
        // prune list, launcher env contract) so a recipe change self-invalidates
        // the cached `.m3pkg`. Bump recipe-v whenever build_claude_code's staged
        // layout, prune set, or launcher env lines change.
        "claude-code" => {
            "fetch-stage:npm-tarball(package/);\
             stage=/usr/lib/claude-code(cli.js[embeds-yoga.wasm]+package.json+LICENSE.md\
             +vendor/ripgrep/x64-linux/rg)+/usr/bin/claude(launcher,0755);\
             launcher=node-import(#!/usr/bin/env node;NODE_EXTRA_CA_CERTS=/etc/ssl/certs/ca-certificates.crt,\
             DISABLE_AUTOUPDATER=1,CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1;\
             import(/usr/lib/claude-code/cli.js)single-process);\
             prune=vendor/{audio-capture,seccomp},vendor/ripgrep/{arm64-*,x64-darwin,x64-win32};\
             recipe-v=3"
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

/// Identity of the DOWNLOADED Go toolchain used to cross-build the `go` and
/// `gh` ports. Unlike every other port, `go` and `gh` are built with
/// `CGO_ENABLED=0` using the official Go binary toolchain downloaded from
/// `ports/lang/go/Portfile` — not the musl cross-compiler. Therefore
/// [`toolchain_id`] (the musl-gcc wrapper) is irrelevant to their artifacts:
/// folding the Go toolchain's Portfile VERSION+SHA into the content key instead
/// keeps the content-addressed pkgcache honest — bumping `ports/lang/go/Portfile`
/// auto-invalidates `go` and `gh` `.m3pkg`s, and a musl-gcc change does NOT
/// spuriously invalidate them. This is the per-port toolchain identity analogous
/// to how `llvm` folds `host_cxx_toolchain_id()`. Best-effort: on read/parse
/// failure or missing fields, returns a stable sentinel so the calling key
/// computation never panics (a machine without the Portfile cannot build `go`/`gh`
/// either, so the sentinel makes the cache miss deterministic).
pub fn go_toolchain_id() -> String {
    let portfile = workspace_root().join("ports/lang/go/Portfile");
    let meta = match parse_portfile(&portfile) {
        Ok(m) => m,
        Err(_) => return "go-toolchain|unknown".to_string(),
    };
    let version = meta.get("VERSION").cloned().unwrap_or_default();
    let sha = meta.get("SHA256").cloned().unwrap_or_default();
    if version.is_empty() || sha.is_empty() {
        return "go-toolchain|unknown".to_string();
    }
    format!("go-toolchain|{version}|{sha}")
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
    host_cxx_toolchain_id_for("M3OS_LLVM_CLANG", "M3OS_LLVM_CLANGXX")
}

/// Generalized [`host_cxx_toolchain_id`] that reads the host clang/clang++ names
/// from the GIVEN override env vars. `node` honors `M3OS_NODE_CLANG` /
/// `M3OS_NODE_CLANGXX` (a knob distinct from llvm's), so its content key must
/// fold the identity of the compiler it ACTUALLY uses — otherwise a node build
/// run with `M3OS_NODE_CLANG=clang-18` would key on the default `clang` and serve
/// a stale cross-host pkgcache hit.
pub fn host_cxx_toolchain_id_for(clang_var: &str, clangxx_var: &str) -> String {
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
    let clang = std::env::var(clang_var).unwrap_or_else(|_| "clang".to_string());
    let clangxx = std::env::var(clangxx_var).unwrap_or_else(|_| "clang++".to_string());
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
    } else if tarball.extension().is_some_and(|e| e == "bz2") {
        // Phase 86b — dropbear ships its releases as `.tar.bz2`; teach the
        // shared extractor the bzip2 flag (`-j`) so its port routes through the
        // same plumbing as the `.xz`/`.gz` ports rather than needing a bespoke
        // unpack. Harmless for every existing port (none is `.bz2`).
        "-xjf"
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
        // Phase 86c Track B — git now also depends on curl (the HTTPS smart-HTTP
        // transport), which transitively pulls mbedtls + ca-certificates. zlib
        // (object/pack compression) stays a direct dep too. The in-OS solver reads
        // these from the `.meta` sidecars; `compute_port_key` folds curl's full
        // (transitive) key in via recursion. The resolved install order is
        // zlib -> mbedtls -> ca-certificates -> curl -> git (ca-certificates is
        // curl's runtime trust store, so it precedes curl).
        "git" => &["zlib", "curl"],
        // Phase 86c Track B — curl links the staged static mbedtls (TLS backend)
        // + zlib (transfer decompression) at BUILD time, and needs the
        // ca-certificates trust store (the Mozilla CA bundle at
        // /etc/ssl/certs/ca-certificates.crt) at RUNTIME to validate certs. All
        // three are deps so the in-OS solver installs the CA bundle when git pulls
        // curl; their keys fold into curl's content key via compute_port_key's
        // recursion. (ca-certificates is a data-only package — no compiler.)
        "curl" => &["zlib", "mbedtls", "ca-certificates"],
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
        // Phase 89 — Node bundles OpenSSL/zlib/c-ares/nghttp2/ICU into the static
        // binary, so it has no runtime `.m3pkg` deps (the musl libc++ sysroot is a
        // build-time prerequisite, not a DEP).
        "node" => &[],
        // Phase 86a Track C.2 — the Mozilla CA bundle is a self-contained data
        // file (no libraries required at build or install time).
        "ca-certificates" => &[],
        // Phase 86c Track A — mbedTLS is a leaf: its static archives are linked
        // into curl/git at build time, so it has no build or runtime deps of its
        // own (and nothing `pkg install`s it directly — curl carries the code).
        "mbedtls" => &[],
        // Phase 86b — dropbear is built `--disable-zlib` (SSH compression is
        // optional; GitHub accepts `none`) and links its bundled libtomcrypt/
        // libtommath statically, so the client has NO runtime dependency: in-OS
        // `pkg install ssh` pulls nothing else.
        "dropbear" => &[],
        // Phase 90b — Claude Code's `cli.js` runs under the Node runtime, so
        // `node` is a real RUNTIME dependency: the in-OS solver installs node
        // first (dependency-first), and node's full (JIT-variant-aware) content
        // key folds into claude-code's key via compute_port_key's recursion. The
        // bundled yoga.wasm TUI requires the Phase 90a JIT node variant.
        // `ca-certificates` lays down /etc/ssl/certs/ca-certificates.crt, the CA
        // bundle the `/usr/bin/claude` launcher points NODE_EXTRA_CA_CERTS at so
        // Node's OpenSSL validates api.anthropic.com — without it the launcher
        // referenced a missing path ("Cannot open directory /etc/ssl/certs").
        "claude-code" => &["node", "ca-certificates"],
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
    // The dep recursion in `compute_port_key_inner` assumes `port_deps` forms a
    // DAG. Guard against an accidentally-introduced cycle (which would otherwise
    // recurse until a stack overflow) by threading the active dependency chain and
    // failing fast with a readable "dependency cycle" error.
    compute_port_key_inner(name, port_dir, &mut Vec::new())
}

/// Inner worker for [`compute_port_key`]. `chain` holds the in-progress
/// dependency path (ancestors only — pushed on entry, popped on the way back
/// up), so a port that re-appears as its own ancestor is a true cycle, while a
/// diamond dependency (a shared leaf reached by two distinct paths) is recomputed
/// exactly as the old un-guarded recursion did — content keys are unchanged.
fn compute_port_key_inner(
    name: &str,
    port_dir: &Path,
    chain: &mut Vec<String>,
) -> Result<String, String> {
    if chain.iter().any(|n| n == name) {
        return Err(format!(
            "ports dependency cycle detected: {} -> {name}",
            chain.join(" -> ")
        ));
    }
    chain.push(name.to_string());
    // Each port's "toolchain" component of the content key must reflect the
    // compiler that actually produced the artifact — not the generic musl-gcc:
    //
    //   llvm  — built by the HOST clang/clang++ (musl-tools has no C++ compiler).
    //           Fold the actual `--version` so two host clangs don't collide on
    //           one `.m3pkg`; musl-gcc is irrelevant.
    //
    //   go/gh — cross-built with the DOWNLOADED Go toolchain (CGO_ENABLED=0).
    //           Phase 86e: fold the Go Portfile VERSION+SHA instead of musl-gcc so
    //           a Go bump auto-invalidates go/gh caches and a musl change does NOT
    //           spuriously invalidate them. Analogous to how `llvm` folds host-cxx.
    //
    //   *     — all other ports are built entirely by the musl cross-compiler, so
    //           `toolchain_id()` (musl-gcc name + `--version`) is the right key.
    let tc = match name {
        // Phase 89 — node joins llvm: both cross-build C++ with the host clang, so
        // fold the host clang/clang++ identity so two host-clang versions don't
        // collide on one `.m3pkg`. They read DIFFERENT override env vars
        // (M3OS_LLVM_* vs M3OS_NODE_*), so each folds the identity of the compiler
        // it actually invokes.
        "llvm" => format!("{}|host-cxx={}", toolchain_id(), host_cxx_toolchain_id()),
        // Phase 90a (D.2) — fold the JIT/jitless VARIANT into node's key in
        // addition to the host-clang identity. `node_variant_id()` reads
        // `M3OS_NODE_JIT`; a JIT build (`variant=jit`) and a jitless build
        // (`variant=jitless`) therefore get DISTINCT content keys, so one is a pure
        // pkgcache MISS for the other (and a HIT for its own kind) — the two sealed
        // `.m3pkg`s never serve each other's cache entries. The jitless artifact
        // stays the Phase 89 default; the JIT variant is additive under its own key.
        "node" => format!(
            "{}|host-cxx={}|variant={}",
            toolchain_id(),
            host_cxx_toolchain_id_for("M3OS_NODE_CLANG", "M3OS_NODE_CLANGXX"),
            node_variant_id()
        ),
        // Phase 86e — go/gh cross-build with the downloaded Go toolchain, not the
        // musl cross-compiler; fold the Go toolchain identity (not musl) so a Go
        // bump auto-invalidates and a musl change does not spuriously invalidate.
        "go" | "gh" => go_toolchain_id(),
        _ => toolchain_id(),
    };
    let dep_keys: Vec<String> = port_deps(name)
        .iter()
        .map(|dep| {
            let dep_dir = find_port_dir(dep)
                .ok_or_else(|| format!("dep port {dep} not found in ports/ tree"))?;
            // Recurse so a TRANSITIVE dep's own resolved dep-keys are folded in:
            // Phase 86c introduces `curl -> {zlib, mbedtls, ca-certificates}` and
            // `git -> curl`, so git's key must reflect curl's full (transitive)
            // identity, not just a bare curl Portfile digest. For the pre-86c leaf
            // deps (zlib, ncurses,
            // libevent) this recursion is byte-identical to the old empty-slice
            // computation — `compute_port_key(leaf) == package_key(leaf_dir, tc,
            // &[])` — so warmed leaf-dep caches stay valid. (The recursion is
            // acyclic: the ports tree's DEPS form a DAG — now enforced above.)
            compute_port_key_inner(dep, &dep_dir, chain)
        })
        .collect::<Result<_, String>>()?;
    chain.pop();
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

    // Phase 88 follow-up — guard the crt-strip regression by construction. The
    // clang toolchain's relocatable crt objects (crt1.o etc.) must NEVER be
    // stripped: `strip --strip-all` deletes their symbol table, removing `_start`
    // from crt1.o, after which every clang link fails `cannot find entry symbol
    // _start` and produces an unrunnable binary. `strip_stage` skips ET_REL
    // objects to prevent this; verify the bundled sysroot's crt1.o still carries
    // its symbols after the strip so a regression fails the seal, not the
    // multi-hour in-OS gate.
    if name == "llvm" {
        let crt1 = stage
            .join(LLVM_TGT_SYSROOT.trim_start_matches('/'))
            .join("usr/lib/crt1.o");
        if crt1.exists() && !binary_contains(&crt1, b"_start")? {
            return Err(format!(
                "seal_package(llvm): {} lost its `_start` symbol after strip — a crt \
                 object was stripped (strip_stage must skip ET_REL relocatable objects)",
                crt1.display()
            ));
        }
    }

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
        // Read enough of the ELF header to check both the magic and e_type
        // (offset 16, u16, little-endian on x86_64). `strip` defaults to
        // `--strip-all`, which removes the *symbol table*. That is fine for final
        // executables / shared objects (ET_EXEC / ET_DYN) but CATASTROPHIC for
        // relocatable objects (ET_REL = 1, e.g. the musl/compiler-rt crt objects
        // crt1.o / crti.o / crtbegin.o): stripping them deletes the symbols the
        // LINKER needs — most importantly `_start` from crt1.o — so every
        // subsequent link fails with `ld.lld: cannot find entry symbol _start`
        // and produces an unrunnable binary. (.a archives are skipped already:
        // their `!<arch>` magic is not ELF.) Never strip ET_REL.
        let mut hdr = [0u8; 18];
        let n = File::open(&path)
            .and_then(|mut f| f.read(&mut hdr))
            .unwrap_or(0);
        let is_elf = n >= 18 && &hdr[0..4] == b"\x7fELF";
        const ET_REL: u16 = 1;
        let e_type = if is_elf {
            u16::from_le_bytes([hdr[16], hdr[17]])
        } else {
            0
        };
        if is_elf && e_type != ET_REL {
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

/// Ports with a host build recipe (the `port_build` dispatch arms + the
/// `go`/`gh` early-return branches). Other Portfiles in the tree (`bc`, `sbase`,
/// `mandoc`, `lua`, `minizip`, `ca-certificates`) are built via the legacy
/// Phase-69d path or staged differently and are not driven by `port build`.
pub const BUILDABLE_PORTS: &[&str] = &[
    "zlib",
    "ncurses",
    "libevent",
    "mbedtls",
    "dropbear",
    "llvm",
    "go",
    "gh",
    "python",
    "less",
    "htop",
    "tmux",
    "curl",
    "git",
    "node",
    "claude-code",
];

/// A port's declared `DEPS=` (whitespace-separated), restricted to `within` —
/// the set we're ordering. Returns empty on a missing/unreadable Portfile.
fn port_deps_within(name: &str, within: &[&str]) -> Vec<String> {
    let Some(dir) = find_port_dir(name) else {
        return Vec::new();
    };
    let Ok(meta) = parse_portfile(&dir.join("Portfile")) else {
        return Vec::new();
    };
    meta.get("DEPS")
        .map(|d| {
            d.split_whitespace()
                .filter(|dep| within.contains(dep))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Topologically order `ports` so each port's (in-set) dependencies precede it.
/// Depth-first; duplicates and out-of-set deps are ignored.
fn topo_order(ports: &[&str]) -> Vec<String> {
    fn visit(n: &str, ports: &[&str], out: &mut Vec<String>) {
        if out.iter().any(|x| x == n) {
            return;
        }
        for d in port_deps_within(n, ports) {
            visit(&d, ports, out);
        }
        if !out.iter().any(|x| x == n) {
            out.push(n.to_string());
        }
    }
    let mut out = Vec::new();
    for p in ports {
        visit(p, ports, &mut out);
    }
    out
}

/// `cargo xtask port list` — every Portfile in the tree with its version, deps,
/// whether `port build` can build it, and whether it is built on this machine.
pub fn cmd_port_list() -> i32 {
    let root = workspace_root();
    let stage_root = root.join("target/port-stage");
    let ports_dir = root.join("ports");
    let mut entries: Vec<(String, String, String, bool, bool)> = Vec::new();
    for cat in ["lib", "util", "core", "doc", "lang", "math"] {
        let Ok(rd) = fs::read_dir(ports_dir.join(cat)) else {
            continue;
        };
        for e in rd.flatten() {
            let pf = e.path().join("Portfile");
            if !pf.exists() {
                continue;
            }
            let Ok(meta) = parse_portfile(&pf) else {
                continue;
            };
            let name = meta
                .get("NAME")
                .cloned()
                .unwrap_or_else(|| e.file_name().to_string_lossy().into_owned());
            let ver = meta.get("VERSION").cloned().unwrap_or_default();
            let deps = meta.get("DEPS").cloned().unwrap_or_default();
            let recipe = BUILDABLE_PORTS.contains(&name.as_str());
            let built = stage_root.join(format!("{name}.stamp")).exists();
            entries.push((name, ver, deps, recipe, built));
        }
    }
    entries.sort();
    println!(
        "{:<16} {:<10} {:<7} {:<6} DEPS",
        "PORT", "VERSION", "RECIPE", "BUILT"
    );
    for (name, ver, deps, recipe, built) in &entries {
        println!(
            "{:<16} {:<10} {:<7} {:<6} {}",
            name,
            ver,
            if *recipe { "yes" } else { "no" },
            if *built { "yes" } else { "-" },
            if deps.is_empty() { "-" } else { deps }
        );
    }
    println!(
        "\n{} Portfiles, {} with a host build recipe. \
         `cargo xtask port build all` builds the recipe set in dependency order; \
         `cargo xtask port build <name>` builds one (deps must be built first — \
         or use `build all`).",
        entries.len(),
        BUILDABLE_PORTS.len()
    );
    0
}

/// `cargo xtask port build all` — build every recipe port in dependency order,
/// skipping pkgcache hits. A failed port causes its (in-set) dependents to be
/// skipped; the run continues and prints a PASS/FAIL/SKIP summary. Exit code is
/// nonzero if any port failed. Note: the heavy ports (`go`/`gh`/`llvm`/`python`)
/// need their own toolchains (Go download, host clang/cmake/ninja, musl cross);
/// missing prerequisites surface as a FAIL for that port without aborting the rest.
pub fn cmd_port_build_all() -> i32 {
    let order = topo_order(BUILDABLE_PORTS);
    println!(
        "ports: build all — dependency order: {}",
        order.join(" -> ")
    );
    let mut passed: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for name in &order {
        let blocked: Vec<String> = port_deps_within(name, BUILDABLE_PORTS)
            .into_iter()
            .filter(|d| failed.contains(d) || skipped.contains(d))
            .collect();
        if !blocked.is_empty() {
            println!(
                "ports: SKIP {name} (dependency not built: {})",
                blocked.join(", ")
            );
            skipped.push(name.clone());
            continue;
        }
        println!("\nports: ===== building {name} =====");
        match port_build(name) {
            Ok(()) => passed.push(name.clone()),
            Err(msg) => {
                eprintln!("ports: FAIL {name}: {msg}");
                failed.push(name.clone());
            }
        }
    }
    println!(
        "\nports: build all summary — {} passed, {} failed, {} skipped",
        passed.len(),
        failed.len(),
        skipped.len()
    );
    if !failed.is_empty() {
        println!("  failed:  {}", failed.join(", "));
    }
    if !skipped.is_empty() {
        println!("  skipped: {}", skipped.join(", "));
    }
    if failed.is_empty() { 0 } else { 1 }
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

    let blob = fetch_tarball(&url, &sha, &cache_dir)?;

    // Phase 86a Track C.2 — bundle-only (data-blob) ports: no tarball to
    // extract, no compiler needed. Fetch + SHA-256-verify the single file
    // (done above by `fetch_tarball`), stage it, seal, stamp, and return
    // before the extract / toolchain / configure paths run.
    if name == "ca-certificates" {
        // Reset stage dir.
        let _ = fs::remove_dir_all(&stage);
        fs::create_dir_all(&stage).map_err(|e| format!("mkdir stage: {e}"))?;
        build_ca_certificates(&blob, &stage)?;
        seal_package(name, &stage, &key)?;
        fs::write(&stamp, &fingerprint).map_err(|e| format!("write stamp: {e}"))?;
        println!(
            "ports: {name}-{version} build complete (staged at {})",
            stage.display()
        );
        return Ok(());
    }

    let extracted = extract_tarball(&blob, &work)?;
    let n = apply_patches(&port_dir.join("patches"), &extracted)?;
    if n > 0 {
        println!("ports: applied {n} patch(es) to {}", extracted.display());
    }

    // Reset stage dir.
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage).map_err(|e| format!("mkdir stage: {e}"))?;

    // Phase 86d Track D — `go` cross-builds with the *downloaded* Go toolchain
    // (CGO_ENABLED=0), not the musl cross-compiler, so branch before the
    // musl_toolchain() requirement below — the port has no musl dependency.
    if name == "go" {
        build_go(&extracted, &stage, &port_dir)?;
        seal_package(name, &stage, &key)?;
        fs::write(&stamp, &fingerprint).map_err(|e| format!("write stamp: {e}"))?;
        println!(
            "ports: {name}-{version} build complete (staged at {})",
            stage.display()
        );
        return Ok(());
    }

    // Phase 86e Track A — `gh` cross-builds with the *downloaded* Go toolchain
    // (CGO_ENABLED=0), not the musl cross-compiler, so branch before the
    // musl_toolchain() requirement below — the port has no musl dependency.
    if name == "gh" {
        build_gh(&extracted, &stage, &port_dir)?;
        seal_package(name, &stage, &key)?;
        fs::write(&stamp, &fingerprint).map_err(|e| format!("write stamp: {e}"))?;
        println!(
            "ports: {name}-{version} build complete (staged at {})",
            stage.display()
        );
        return Ok(());
    }

    // Phase 89 — `node` cross-builds C++ with the host clang/clang++ targeting
    // musl (+ the build_llvm Stage-B libc++ sysroot), NOT the musl-gcc C path, so
    // it branches before the musl_toolchain() requirement (it needs musl-dev
    // headers/libs via the llvm sysroot, not a musl gcc).
    if name == "node" {
        build_node(&extracted, &stage, &port_dir)?;
        seal_package(name, &stage, &key)?;
        fs::write(&stamp, &fingerprint).map_err(|e| format!("write stamp: {e}"))?;
        println!(
            "ports: {name}-{version} build complete (staged at {})",
            stage.display()
        );
        return Ok(());
    }

    // Phase 90b — Claude Code is a FETCH-AND-STAGE port (no compiler at all): the
    // npm tarball's `package/` payload is staged + a launcher written, so it
    // branches before the musl_toolchain() requirement (like the go/gh/node
    // download-based ports). `DEPS=node` is resolved at install time by the in-OS
    // solver, not at build time.
    if name == "claude-code" {
        build_claude_code(&extracted, &stage, &port_dir)?;
        seal_package(name, &stage, &key)?;
        fs::write(&stamp, &fingerprint).map_err(|e| format!("write stamp: {e}"))?;
        println!(
            "ports: {name}-{version} build complete (staged at {})",
            stage.display()
        );
        return Ok(());
    }

    let toolchain = musl_toolchain().ok_or_else(|| {
        "no musl cross-compiler found on PATH (install musl-tools or \
                                                musl-gcc-cross-bin)"
            .to_string()
    })?;

    let ncurses_stage = stage_root.join("ncurses");
    let libevent_stage = stage_root.join("libevent");
    let zlib_stage = stage_root.join("zlib");
    let mbedtls_stage = stage_root.join("mbedtls");
    let curl_stage = stage_root.join("curl");

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
        "git" => build_git(
            &extracted,
            &stage,
            &toolchain,
            &zlib_stage,
            &curl_stage,
            &mbedtls_stage,
        )?,
        "python" => build_python(&extracted, &stage, &toolchain, &zlib_stage, &ncurses_stage)?,
        "llvm" => build_llvm(&extracted, &stage, &toolchain)?,
        "dropbear" => build_dropbear(&extracted, &stage, &toolchain)?,
        "mbedtls" => build_mbedtls(&extracted, &stage, &toolchain)?,
        "curl" => build_curl(&extracted, &stage, &toolchain, &zlib_stage, &mbedtls_stage)?,
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

/// Phase 86d Track D — cross-compile the static Go runtime probe.
///
/// `extracted` is the unpacked official Go binary toolchain (the tarball's
/// top-level `go/` directory). `port_dir` is `ports/lang/go`, holding the
/// program source under `src/`. We invoke the downloaded `go` to build a fully
/// static (`CGO_ENABLED=0`) linux/amd64 binary and stage it at
/// `/usr/bin/go-runtime-probe`, with the source copied to `/usr/src` for
/// reference. No musl toolchain is used — the binary embeds the Go runtime and
/// makes Linux syscalls directly (the same static-binary class as CPython /
/// Clang, since m3OS's `ld-musl` has no real `libc.so`).
fn build_go(extracted: &Path, stage: &Path, port_dir: &Path) -> Result<(), String> {
    // Locate the `go` binary inside the unpacked toolchain. `extract_tarball`
    // returns the single top-level directory; the official tarball's is `go/`,
    // so the binary is `<extracted>/bin/go` — but tolerate a nested `go/go/`.
    let go_bin = {
        let direct = extracted.join("bin/go");
        let nested = extracted.join("go/bin/go");
        if direct.is_file() {
            direct
        } else if nested.is_file() {
            nested
        } else {
            return Err(format!(
                "go toolchain binary not found under {} (looked for bin/go, go/bin/go)",
                extracted.display()
            ));
        }
    };
    let goroot = go_bin
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| "cannot derive GOROOT from go binary path".to_string())?
        .to_path_buf();

    let src = port_dir.join("src/runtime_probe.go");
    if !src.is_file() {
        return Err(format!("go program source missing: {}", src.display()));
    }

    // A self-contained module dir so `go build` never needs the network or a
    // GOPATH layout (the program imports only the standard library).
    let root = workspace_root();
    let scratch = root.join("target/port-build/go-scratch");
    let builddir = scratch.join("probe");
    let gocache = scratch.join("gocache");
    let home = scratch.join("home");
    let _ = fs::remove_dir_all(&builddir);
    for d in [&builddir, &gocache, &home] {
        fs::create_dir_all(d).map_err(|e| format!("mkdir {}: {e}", d.display()))?;
    }
    fs::copy(&src, builddir.join("main.go")).map_err(|e| format!("stage go source: {e}"))?;
    fs::write(
        builddir.join("go.mod"),
        "module m3os.local/go-runtime-probe\n\ngo 1.24\n",
    )
    .map_err(|e| format!("write go.mod: {e}"))?;

    // Stage layout: /usr/bin/<probe> + /usr/src/runtime_probe.go.
    let bindir = stage.join("usr/bin");
    let srcdir = stage.join("usr/src");
    fs::create_dir_all(&bindir).map_err(|e| format!("mkdir {}: {e}", bindir.display()))?;
    fs::create_dir_all(&srcdir).map_err(|e| format!("mkdir {}: {e}", srcdir.display()))?;
    let out_bin = bindir.join("go-runtime-probe");

    println!(
        "ports: go — cross-building static runtime probe (GOROOT={}, CGO_ENABLED=0)",
        goroot.display()
    );
    run(
        Command::new(&go_bin)
            .current_dir(&builddir)
            .env("GOROOT", &goroot)
            .env("GOOS", "linux")
            .env("GOARCH", "amd64")
            .env("CGO_ENABLED", "0")
            .env("GOTOOLCHAIN", "local")
            .env("GOCACHE", &gocache)
            .env("GOPATH", scratch.join("gopath"))
            .env("HOME", &home)
            .env("GOPROXY", "off")
            .env("GOFLAGS", "-mod=mod")
            .arg("build")
            .arg("-trimpath")
            .arg("-ldflags=-s -w")
            .arg("-o")
            .arg(&out_bin)
            .arg("."),
        "go build runtime_probe",
    )?;

    if !out_bin.is_file() {
        return Err(format!(
            "go build produced no binary at {}",
            out_bin.display()
        ));
    }
    fs::copy(&src, srcdir.join("runtime_probe.go")).map_err(|e| format!("copy go source: {e}"))?;
    let len = fs::metadata(&out_bin).map(|m| m.len()).unwrap_or(0);
    println!(
        "ports: go — built static probe {} ({len} bytes)",
        out_bin.display()
    );
    Ok(())
}

/// Phase 90b — stage the pinned Claude Code npm bundle into a `.m3pkg`.
///
/// `extracted` is the unpacked npm tarball's single top-level dir (`package/`),
/// `port_dir` is `ports/util/claude-code`. There is NO compiler: Claude Code is
/// the `cli.js` JavaScript bundle run under the Node runtime (`DEPS=node`). We
/// stage the package payload under `/usr/lib/claude-code/`, keep only the
/// static-pie `vendor/ripgrep/x64-linux/rg` search tool (pruning the
/// other-platform binaries, the dynamic `audio-capture.node` addon, and the
/// `seccomp` helper m3OS cannot use), and write the `/usr/bin/claude` launcher
/// that pins the supported environment + imports `cli.js` in-process under node.
///
/// The pin is `2.1.112` — the last version shipping this `cli.js`-under-Node
/// model; `2.1.113+` repackaged into a per-platform native Bun binary that does
/// not use Node at all. The `yoga.wasm` TUI layout engine is embedded INSIDE
/// `cli.js` (not a separate file as in the 1.x/2.0 bundles), so staging `cli.js`
/// carries it; it still requires the Phase 90a JIT Node variant to instantiate.
fn build_claude_code(extracted: &Path, stage: &Path, port_dir: &Path) -> Result<(), String> {
    // Function-local trait import so `Permissions::from_mode` is available for
    // chmod'ing the launcher + the vendored rg (npm tar perms / host umask vary).
    use std::os::unix::fs::PermissionsExt;
    let set_exec = |p: &Path| -> Result<(), String> {
        fs::set_permissions(p, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod 0755 {}: {e}", p.display()))
    };

    let version = parse_portfile(&port_dir.join("Portfile"))?
        .get("VERSION")
        .cloned()
        .unwrap_or_default();

    // The npm tarball's single top-level dir is `package/`; `extract_tarball`
    // returns it directly as `extracted`. The entry point is `cli.js`.
    let cli_js = extracted.join("cli.js");
    if !cli_js.is_file() {
        return Err(format!(
            "claude-code bundle missing cli.js at {} — is the pinned tarball the \
             cli.js bundle (<= 2.1.112) and not the native-binary wrapper (>= 2.1.113)?",
            cli_js.display()
        ));
    }

    // package.json is mandatory too: the launcher / `.meta` machinery and cli.js
    // itself read its `version`/metadata at runtime. Require it explicitly
    // (mirroring the cli.js check above) so a tarball-layout change can't silently
    // produce an incomplete staged package — the prior any-of-three guard would
    // have accepted a bundle missing package.json as long as cli.js was present.
    let pkg_json = extracted.join("package.json");
    if !pkg_json.is_file() {
        return Err(format!(
            "claude-code bundle missing package.json at {} — incomplete/changed tarball layout?",
            pkg_json.display()
        ));
    }

    // Stage the package payload under /usr/lib/claude-code/. cli.js resolves its
    // vendored tooling relative to its own __dirname, so preserve that layout.
    let libdir = stage.join("usr/lib/claude-code");
    fs::create_dir_all(&libdir).map_err(|e| format!("mkdir {}: {e}", libdir.display()))?;

    // cli.js (embeds the yoga.wasm TUI engine) + package.json are mandatory and
    // were verified present above; LICENSE.md is optional provenance. The ~134 KB
    // `sdk-tools.d.ts` (TypeScript types, runtime-irrelevant) and `README.md` are
    // intentionally pruned to keep the install lean over the slow ring-3 VFS.
    for f in ["cli.js", "package.json", "LICENSE.md"] {
        let src = extracted.join(f);
        if src.is_file() {
            fs::copy(&src, libdir.join(f)).map_err(|e| format!("stage claude-code {f}: {e}"))?;
        }
    }

    // Stage ONLY the x64-linux ripgrep (Claude Code's search tool). It is a
    // fully-static `static-pie` binary (no PT_INTERP) — m3OS's ELF loader handles
    // ET_DYN static-PIE, so it runs directly. Prune every other-platform `rg`, the
    // dynamic `audio-capture.node` addon (no dlopen on m3OS), and the `seccomp`
    // helper (m3OS has no seccomp) by simply not copying them.
    let rg_src = extracted.join("vendor/ripgrep/x64-linux/rg");
    if !rg_src.is_file() {
        return Err(format!(
            "claude-code bundle missing vendored ripgrep at {}",
            rg_src.display()
        ));
    }
    let rg_dir = libdir.join("vendor/ripgrep/x64-linux");
    fs::create_dir_all(&rg_dir).map_err(|e| format!("mkdir {}: {e}", rg_dir.display()))?;
    let rg_dst = rg_dir.join("rg");
    fs::copy(&rg_src, &rg_dst).map_err(|e| format!("stage ripgrep rg: {e}"))?;
    set_exec(&rg_dst)?;
    let rg_copying = extracted.join("vendor/ripgrep/COPYING");
    if rg_copying.is_file() {
        let _ = fs::copy(&rg_copying, libdir.join("vendor/ripgrep/COPYING"));
    }

    // The /usr/bin/claude launcher — the pinned supported environment (Track C.1).
    //
    // It is a NODE script, NOT a `#!/bin/sh` script: m3OS's `/bin/sh` is `ion`,
    // which (unlike POSIX `sh`) does not run a shebang script file with flag args
    // — `ion /usr/bin/claude --version` is intercepted by ion's own `--version`
    // handler (it prints the ion banner and never runs the script body), and the
    // built-in `sh0` ignores argv entirely. The one script interpreter m3OS runs
    // correctly with args is `node` itself (the `#!/usr/bin/env node` path npm
    // rides). So the launcher is a tiny CJS node program that pins the supported
    // env and runs `cli.js` IN-PROCESS via dynamic `import()` (cli.js is ESM —
    // `package.json` has `"type":"module"`). A single node process (no node→node
    // fork) keeps the cold start to ONE binary load over the slow VFS. Each env
    // line is a documented support-boundary decision (docs/90b-claude-code.md):
    //   NODE_EXTRA_CA_CERTS — Node's OpenSSL validates api.anthropic.com against the
    //     Phase 86a CA bundle (m3OS has no system trust-store discovery). NOTE: node
    //     reads this lazily when the root store is first built, so the in-process set
    //     below covers the opt-in TLS arm; a user can also export it in the shell.
    //   DISABLE_AUTOUPDATER — the sealed .m3pkg is the only supported delivery.
    //   CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC — no telemetry/Statsig/Sentry egress.
    let bindir = stage.join("usr/bin");
    fs::create_dir_all(&bindir).map_err(|e| format!("mkdir {}: {e}", bindir.display()))?;
    let launcher = bindir.join("claude");
    let launcher_body = "#!/usr/bin/env node\n\
        // Phase 90b — Claude Code launcher (pinned supported environment).\n\
        // ion (m3OS /bin/sh) can't run shebang scripts with flag args, so this is a\n\
        // node wrapper that imports cli.js in-process (single node, no fork).\n\
        'use strict';\n\
        process.env.DISABLE_AUTOUPDATER = process.env.DISABLE_AUTOUPDATER || '1';\n\
        process.env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC =\n\
        \x20 process.env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC || '1';\n\
        if (!process.env.NODE_EXTRA_CA_CERTS)\n\
        \x20 process.env.NODE_EXTRA_CA_CERTS = '/etc/ssl/certs/ca-certificates.crt';\n\
        process.argv.splice(1, 1, '/usr/lib/claude-code/cli.js');\n\
        import('/usr/lib/claude-code/cli.js').catch((e) => {\n\
        \x20 console.error('claude launcher: failed to start cli.js:', e && e.message);\n\
        \x20 process.exit(1);\n\
        });\n";
    fs::write(&launcher, launcher_body).map_err(|e| format!("write claude launcher: {e}"))?;
    set_exec(&launcher)?;

    let cli_len = fs::metadata(&cli_js).map(|m| m.len()).unwrap_or(0);
    let rg_len = fs::metadata(&rg_dst).map(|m| m.len()).unwrap_or(0);
    println!(
        "ports: claude-code — staged v{version} bundle (cli.js {cli_len} bytes, rg {rg_len} bytes) \
         + /usr/bin/claude launcher"
    );
    Ok(())
}

/// Phase 86e Track A — cross-compile the static GitHub CLI (`gh`) from source.
///
/// `extracted` is the unpacked `cli/cli` source tree (`cli-<version>/`),
/// `port_dir` is `ports/util/gh`. Like the `go` port the cross is driven by the
/// **downloaded official Go toolchain** (`CGO_ENABLED=0`) — not the musl
/// cross-compiler — so the result is a fully static linux/amd64 binary (no
/// `PT_INTERP`), the same binary class as the 86d runtime probe / static
/// CPython / Clang.
///
/// The Go toolchain is resolved from the **`go` port's Portfile** (URL + SHA-256
/// of go1.24.6) so gh and the 86d probe always cross with the identical pinned
/// Go — one place to bump the version. gh's go.mod `go` directive must be `<=`
/// that toolchain minor (v2.82.1 is `go 1.24.0`), since `GOTOOLCHAIN=local`
/// refuses to auto-upgrade. gh's module dependencies are fetched via GOPROXY
/// during this host build (network, like the `go` port's toolchain download);
/// the build is reproducible because gh's committed `go.sum` pins every hash.
fn build_gh(extracted: &Path, stage: &Path, port_dir: &Path) -> Result<(), String> {
    let version = parse_portfile(&port_dir.join("Portfile"))?
        .get("VERSION")
        .cloned()
        .unwrap_or_default();

    let root = workspace_root();

    // Resolve + fetch the Go toolchain from the `go` port's Portfile (URL+SHA).
    let go_portfile = root.join("ports/lang/go/Portfile");
    let go_meta =
        parse_portfile(&go_portfile).map_err(|e| format!("read go Portfile for toolchain: {e}"))?;
    let go_url = go_meta
        .get("URL")
        .cloned()
        .ok_or_else(|| "go Portfile missing URL (needed for the gh cross-build)".to_string())?;
    let go_sha = go_meta
        .get("SHA256")
        .cloned()
        .ok_or_else(|| "go Portfile missing SHA256 (needed for the gh cross-build)".to_string())?;

    let cache_dir = root.join("target/port-src");
    let go_tarball = fetch_tarball(&go_url, &go_sha, &cache_dir)?;
    // Extract the toolchain into a gh-dedicated dir (NOT the gh source `work`
    // dir, which extract_tarball would wipe). The Go tarball's top-level is `go/`.
    let tc_work = root.join("target/port-build/gh-go-toolchain");
    let goroot = extract_tarball(&go_tarball, &tc_work)?;
    let go_bin = goroot.join("bin/go");
    if !go_bin.is_file() {
        return Err(format!(
            "go toolchain binary not found at {} (after extracting {})",
            go_bin.display(),
            go_tarball.display()
        ));
    }

    // Module/build caches outside the source tree so the build never mutates
    // `extracted` (and a stale cache never leaks into the artifact key).
    let scratch = root.join("target/port-build/gh-scratch");
    let gocache = scratch.join("gocache");
    let gomod = scratch.join("gomod");
    let home = scratch.join("home");
    for d in [&gocache, &gomod, &home] {
        fs::create_dir_all(d).map_err(|e| format!("mkdir {}: {e}", d.display()))?;
    }

    // Stage layout: /usr/bin/gh (installs to prefix=/usr, like git).
    let bindir = stage.join("usr/bin");
    fs::create_dir_all(&bindir).map_err(|e| format!("mkdir {}: {e}", bindir.display()))?;
    let out_bin = bindir.join("gh");

    // `-X internal/build.Version` stamps `gh --version`; `-s -w` strips.
    let ldflags = format!("-s -w -X github.com/cli/cli/v2/internal/build.Version={version}");
    println!(
        "ports: gh — cross-building static GitHub CLI v{version} (GOROOT={}, CGO_ENABLED=0)",
        goroot.display()
    );
    run(
        Command::new(&go_bin)
            .current_dir(extracted)
            .env("GOROOT", &goroot)
            .env("GOOS", "linux")
            .env("GOARCH", "amd64")
            .env("CGO_ENABLED", "0")
            .env("GOTOOLCHAIN", "local")
            .env("GOCACHE", &gocache)
            .env("GOMODCACHE", &gomod)
            .env("GOPATH", scratch.join("gopath"))
            .env("HOME", &home)
            .env("GOFLAGS", "-mod=mod")
            // gh's deps are not vendored in the source tarball; fetch via the
            // public proxy (network). go.sum pins every hash so this is
            // reproducible.
            .env("GOPROXY", "https://proxy.golang.org,direct")
            .arg("build")
            .arg("-trimpath")
            .arg(format!("-ldflags={ldflags}"))
            .arg("-o")
            .arg(&out_bin)
            .arg("./cmd/gh"),
        "go build gh",
    )?;

    if !out_bin.is_file() {
        return Err(format!(
            "go build produced no gh binary at {}",
            out_bin.display()
        ));
    }
    let len = fs::metadata(&out_bin).map(|m| m.len()).unwrap_or(0);
    println!(
        "ports: gh — built static gh {} ({len} bytes)",
        out_bin.display()
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

/// Phase 86a Track C.2 — bundle-only CA certificate port.
///
/// `cacert.pem` is NOT a tarball; the `fetch_tarball` call above already
/// downloaded it and verified its SHA-256.  This function stages the
/// already-verified single file to the canonical on-target path:
///
///   `etc/ssl/certs/ca-certificates.crt`
///
/// which `pkg install ca-certificates` lays under `/` so TLS consumers
/// (Phase 86c `curl`, `git` transport) can pass `--ca-bundle
/// /etc/ssl/certs/ca-certificates.crt`.  No compiler is invoked.
///
/// A SHA-256 mismatch is caught by `fetch_tarball` before this function
/// is called, so no additional verify step is needed here.
fn build_ca_certificates(blob: &Path, stage: &Path) -> Result<(), String> {
    let dest_dir = stage.join("etc/ssl/certs");
    fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir etc/ssl/certs: {e}"))?;
    let dest = dest_dir.join("ca-certificates.crt");
    fs::copy(blob, &dest).map_err(|e| format!("copy cacert.pem: {e}"))?;
    println!(
        "ca-certificates: staged cacert.pem → etc/ssl/certs/ca-certificates.crt ({} bytes)",
        file_size(&dest)
    );
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

/// Phase 86c Track B — cross-build a static musl `git` REBUILT **with** curl
/// (HTTPS smart-HTTP transport), statically linked against the staged zlib +
/// curl + mbedtls, and DESTDIR-install it at `prefix=/usr` into `stage`.
///
/// Phase 85b built git local-only (`NO_CURL`); 86c removes `NO_CURL` and links
/// the static `libcurl --with-mbedtls` so `git clone https://…` works. The TLS
/// backend is mbedTLS-via-curl, **not** OpenSSL, so `NO_OPENSSL=1` stays — git
/// pulls in no OpenSSL. git does not implement TLS itself: only the
/// `git-remote-http` helper links libcurl, so the curl flags are wired through
/// git's `CURL_CFLAGS`/`CURL_LDFLAGS` knobs (the explicit static
/// libcurl+mbedtls+zlib link line), with `CURL_CONFIG=true` to neutralize the
/// Makefile's host `curl-config --vernum` probe.
///
/// Unlike the ncurses-class ports, git has **no autotools `./configure`** — its
/// build is a plain Makefile driven entirely by `CC=<musl-gcc>`, so there is no
/// `--host` flag to pass; the cross is implied by the compiler. zlib (git's
/// object/pack-compression dependency) is consumed from `zlib_stage/usr/local`
/// via git's `ZLIB_PATH` Makefile knob plus explicit `-I`/`-L` flags.
///
/// `SKIP_DASHED_BUILT_INS=YesPlease` is essential here: git would otherwise
/// install ~100 dashed `libexec/git-core/git-<builtin>` **hardlinks** to the
/// main binary, and the `.m3pkg` packer ([`pkg_format::pack`]) stores file
/// *content* per path with no inode/hardlink dedup — so those hardlinks would
/// balloon the artifact by hundreds of MB. With this knob the dashed builtins
/// are not installed at all; every smoke subcommand (`init`/`add`/`commit`/
/// `log`/`diff`/`status`/`branch`/`merge`/`checkout`) is dispatched in-process
/// by the single `git` binary, so nothing is lost. The curl-backed remote
/// helpers (`git-remote-http`/`git-remote-https`/`git-http-fetch`) are NOT
/// dashed builtins, so they install under `libexec/git-core/` regardless.
fn build_git(
    src: &Path,
    stage: &Path,
    (cc, ar, _ranlib): &(&'static str, String, String),
    zlib_stage: &Path,
    curl_stage: &Path,
    mbedtls_stage: &Path,
) -> Result<(), String> {
    let zlib_prefix = zlib_stage.join("usr/local");
    if !zlib_prefix.join("lib/libz.a").exists() {
        return Err(format!(
            "git build: staged zlib not found at {} (build the zlib port first)",
            zlib_prefix.join("lib/libz.a").display()
        ));
    }
    // Phase 86c — the staged static libcurl + mbedTLS the HTTPS transport links.
    let curl_prefix = curl_stage.join("usr/local");
    if !curl_prefix.join("lib/libcurl.a").exists() {
        return Err(format!(
            "git build: staged curl not found at {} (build the curl port first)",
            curl_prefix.join("lib/libcurl.a").display()
        ));
    }
    let mbedtls_prefix = mbedtls_stage.join("usr/local");
    if !mbedtls_prefix.join("lib/libmbedtls.a").exists() {
        return Err(format!(
            "git build: staged mbedtls not found at {} (build the mbedtls port first)",
            mbedtls_prefix.join("lib/libmbedtls.a").display()
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
    // git's curl knobs: headers from the staged curl prefix; the link line is the
    // full static libcurl+mbedtls+zlib set in dependency order (-lcurl before its
    // mbedtls/z deps). CURL_CONFIG=true neutralizes the host curl-config probe.
    let curl_cflags = format!("-I{}/include", curl_prefix.display());
    let curl_ldflags = curl_static_link_line(&curl_prefix, &mbedtls_prefix, &zlib_prefix);

    // The knob set that carves git's minimal, dependency-light build (see
    // ports/util/git/Portfile for the per-knob rationale). Passed as
    // `make VAR=val` arguments — the git Makefile convention. `prefix=/usr`
    // additionally special-cases git's system config to `/etc/gitconfig`.
    //
    // Phase 86c: NO_CURL is REMOVED (curl is now linked for HTTPS); NO_OPENSSL
    // stays (the TLS backend is mbedTLS-via-curl). NO_EXPAT stays too — it only
    // drops `git-http-push` (dumb-HTTP WebDAV push, which needs expat); smart-HTTP
    // clone/fetch/push over `git-remote-https` needs no expat.
    let common: Vec<String> = vec![
        format!("CC={cc}"),
        format!("AR={ar}"),
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
        // curl (HTTPS smart-HTTP transport): explicit cflags + the full static
        // link line; CURL_CONFIG=true neutralizes the host curl-config probe.
        "CURL_CONFIG=true".to_string(),
        format!("CURL_CFLAGS={curl_cflags}"),
        format!("CURL_LDFLAGS={curl_ldflags}"),
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

    // git's default build also produces several large (~2.3–3.7 MB each,
    // statically linked) binaries that serve the SERVER side of the protocol (the
    // pack helpers below) plus email / large-repo workflows m3OS does not run. 86c
    // KEEPS the client remote helpers (`git-remote-http`/`git-remote-https`/
    // `git-http-fetch` — the HTTPS transport, asserted present below) but still
    // prunes the server halves + the unused helpers. Pruning them keeps the
    // `.m3pkg` lean and the in-OS
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
        // Phase 86c — curl enables the remote-curl ALIASES git-remote-ftp/ftps as
        // copies of git-remote-http; m3OS does no ftp:// clones (curl is built
        // --disable-ftp), and the no-dedup packer would store each as a full copy
        // of the multi-MB helper. Prune them; keep git-remote-http/https.
        ("libexec", "git-remote-ftp"),
        ("libexec", "git-remote-ftps"),
    ];
    for (where_, name_) in prune {
        let p = match where_ {
            "bin" => stage_prefix.join("bin").join(name_),
            _ => git_core.join(name_),
        };
        let _ = fs::remove_file(&p);
    }

    // Phase 86c — INVERTED assertions. With curl linked, the smart-HTTP remote
    // helpers MUST be present (they are the HTTPS transport). git-remote-https is
    // an alias of git-remote-http; git-http-fetch is the dumb-HTTP object fetcher
    // (built with curl alone — no expat). git-http-push is NOT required: it needs
    // expat (NO_EXPAT=1), and smart-HTTP needs no dumb-HTTP push.
    let remote_http = git_core.join("git-remote-http");
    for required in ["git-remote-http", "git-remote-https", "git-http-fetch"] {
        if !git_core.join(required).exists() && !stage_prefix.join("bin").join(required).exists() {
            return Err(format!(
                "git build: {required} MISSING — curl did not take effect (HTTPS transport absent)"
            ));
        }
    }
    if !remote_http.exists() {
        return Err(format!(
            "git build: {} missing (the curl helper that carries the TLS link)",
            remote_http.display()
        ));
    }
    // Positive proof the curl + mbedTLS TLS path actually linked into the helper:
    // these symbol names land in git-remote-http when remote-curl.c links libcurl
    // and curl's vtls/mbedtls backend links mbedTLS. The check runs here in
    // `build_git` — which executes BEFORE `seal_package`'s `strip_stage`, so the
    // staged helper is still unstripped and carries its symbol table. We assert
    // on `curl_multi_perform` (NOT `curl_easy_perform`): git 2.44 drives transfers
    // through curl's MULTI interface (`http.c` calls `curl_multi_perform`), so it
    // is a symbol git actually CALLS — a semantically meaningful, strip-robust
    // proof, where `curl_easy_perform` would be only a symtab artifact git never
    // invokes. Presence is direct, toolchain-independent proof that HTTPS rode in
    // over the trusted stack (the inverse of the 85b absence check).
    if !binary_contains(&remote_http, b"curl_multi_perform")? {
        return Err(
            "git build: git-remote-http does not reference libcurl (curl_multi_perform) — \
             curl did not link (HTTPS would not work)"
                .to_string(),
        );
    }
    if !binary_contains(&remote_http, b"mbedtls_ssl_handshake")? {
        return Err(
            "git build: git-remote-http does not reference mbedTLS (mbedtls_ssl_handshake) — \
             the mbedTLS TLS backend did not link (cert validation would not run)"
                .to_string(),
        );
    }
    // NO_OPENSSL still holds: the TLS backend is mbedTLS, so the OpenSSL API
    // symbol must remain ABSENT from both the main binary and the curl helper.
    // (Un-pruning the server pack helpers, or accidentally adopting OpenSSL, would
    // trip this.)
    for (label, p) in [("git", &git_bin), ("git-remote-http", &remote_http)] {
        if binary_contains(p, b"SSL_CTX_new")? {
            return Err(format!(
                "git build: {label} references OpenSSL (SSL_CTX_new) — NO_OPENSSL ineffective \
                 (the TLS backend must be mbedTLS-via-curl, not OpenSSL)"
            ));
        }
    }
    // The server-side pack helpers must STAY pruned — they are the server half of
    // the protocol m3OS does not run; un-pruning them would ship a server.
    for server_half in ["git-upload-pack", "git-receive-pack", "git-upload-archive"] {
        if stage_prefix.join("bin").join(server_half).exists()
            || git_core.join(server_half).exists()
        {
            return Err(format!(
                "git build: {server_half} present — the server-side pack helper prune regressed \
                 (86c is a client; un-pruning these would ship a server m3OS does not run)"
            ));
        }
    }

    println!(
        "git: produced /usr/bin/git ({} bytes, unstripped) + libexec/git-core + templates; \
         curl+mbedTLS HTTPS verified (git-remote-https + curl_multi_perform + mbedtls_ssl_handshake \
         present, SSL_CTX_new absent, server pack helpers pruned)",
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

/// Phase 86b — cross-build dropbear's `dbclient` as a static, client-only SSH
/// binary and DESTDIR-install it at `prefix=/usr` into `stage`.
///
/// This is the static `ssh` client that gives m3OS its first secure remote git
/// transport: git speaks the SSH-transport protocol itself and merely
/// fork/execs an `ssh` binary to move the bytes (`GIT_SSH_COMMAND`), so the
/// Phase 85b `git` is reused **unchanged** and dropbear is the only new
/// artifact.
///
/// CLIENT-ONLY (`make PROGRAMS=dbclient`): only dropbear's `dbclient` is built —
/// not the `dropbear` server / `dropbearkey` / `dropbearconvert` — keeping the
/// artifact a single ~250 KB static binary. `--enable-bundled-libtom` uses
/// dropbear's vendored libtomcrypt/libtommath software crypto, so the client
/// links no system crypto library and stays dependency-free. zlib is disabled
/// (`--disable-zlib`) — SSH compression is optional and GitHub accepts `none`, so
/// the client has no runtime dependency. The static link is forced by `-static`
/// in LDFLAGS plus `--disable-harden` (which drops the `-pie` flags that conflict
/// with `-static` on the musl cross), the same mechanism the `git` port uses; the
/// login bookkeeping (`syslog`/`utmp`/`wtmp`/`lastlog`) a client never uses is
/// disabled too (musl provides none of it).
///
/// Post-install the manpage is pruned and a second copy of `dbclient` is laid
/// down as `/usr/bin/ssh`, so both the dropbear name (`dbclient -y/-i/-p`, used
/// by the smoke) and the OpenSSH name (`ssh -T git@github.com`) resolve on PATH.
/// A copy, not a symlink: the `.m3pkg` packer stores file *content* per path with
/// no symlink/hardlink dedup, so a symlink would not round-trip. dbclient is a
/// single-program build whose behaviour does not switch on argv[0], so the copy
/// IS a working ssh client.
fn build_dropbear(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
) -> Result<(), String> {
    let stage_prefix = stage.join("usr");
    fs::create_dir_all(&stage_prefix).map_err(|e| format!("mkdir: {e}"))?;

    let extra_ld = musl_extra_ldflags_joined();
    let ldflags = if extra_ld.is_empty() {
        "-static".to_string()
    } else {
        format!("-static {extra_ld}")
    };

    // ./configure — static, client-suitable, no optional deps. The cross is
    // driven by CC/AR/RANLIB + --host. The static link comes from `-static` in
    // LDFLAGS + `--disable-harden` (which drops the conflicting `-pie`), the same
    // mechanism the `git` port uses; dropbear's configure has no `--enable-static`
    // knob. `--enable-bundled-libtom` uses dropbear's vendored libtomcrypt /
    // libtommath so the client needs no system crypto library and stays
    // dependency-free (DEPS= empty).
    let mut configure_cmd = Command::new("sh");
    configure_cmd
        .current_dir(src)
        .arg("./configure")
        .arg("--host=x86_64-linux-musl")
        .arg("--prefix=/usr")
        .arg("--disable-zlib")
        .arg("--disable-harden")
        .arg("--enable-bundled-libtom")
        .arg("--disable-syslog")
        .arg("--disable-lastlog")
        .arg("--disable-utmp")
        .arg("--disable-utmpx")
        .arg("--disable-wtmp")
        .arg("--disable-wtmpx")
        .env("CC", cc)
        .env("AR", ar)
        .env("RANLIB", ranlib)
        .env("CFLAGS", "-O2")
        .env("LDFLAGS", &ldflags);
    run(&mut configure_cmd, "dropbear configure")?;

    // Build ONLY the client (PROGRAMS=dbclient) — not the server/keygen tools.
    let mut make_cmd = Command::new("make");
    make_cmd
        .current_dir(src)
        .arg(format!("-j{}", num_jobs()))
        .arg("PROGRAMS=dbclient");
    run(&mut make_cmd, "dropbear make dbclient")?;

    let mut install_cmd = Command::new("make");
    install_cmd
        .current_dir(src)
        .arg("PROGRAMS=dbclient")
        .arg("install")
        .arg(format!("DESTDIR={}", stage.display()))
        .arg("prefix=/usr");
    run(&mut install_cmd, "dropbear install")?;

    let dbclient = stage_prefix.join("bin/dbclient");
    if !dbclient.exists() {
        return Err(format!("dropbear build missing {}", dbclient.display()));
    }

    // The client MUST be static: m3OS's `ld-musl` is a custom loader with no real
    // `libc.so`, so a dynamic build would carry a PT_INTERP referencing
    // `/lib/ld-musl-x86_64.so.1` and fault at startup. A static musl ELF embeds
    // libc and never names the loader, so the absence of that interp string is a
    // direct proof of `-static` (the same guard `build_python` uses). This makes
    // the static link verified-by-construction at build time, independent of the
    // opt-in QEMU smoke — so a regression in the LDFLAGS=-static + --disable-harden
    // mechanism fails the build immediately rather than at runtime on-device.
    if binary_contains(&dbclient, b"/lib/ld-musl-x86_64.so.1")? {
        return Err(format!(
            "dropbear build: {} references the dynamic loader — `-static` did not take \
             effect (m3OS's ld-musl has no real libc.so, so a dynamic dbclient cannot run)",
            dbclient.display()
        ));
    }

    // Client-only contract: the server half must NOT have been built. Its
    // presence would mean PROGRAMS=dbclient did not take effect.
    for forbidden in ["dropbear", "dropbearkey", "dropbearconvert"] {
        if stage_prefix.join("bin").join(forbidden).exists()
            || stage_prefix.join("sbin").join(forbidden).exists()
        {
            return Err(format!(
                "dropbear build: {forbidden} present — PROGRAMS=dbclient did not take \
                 effect (client-only contract broken)"
            ));
        }
    }

    // Prune the installed manpage: the slow ring-3 VFS + the no-dedup `.m3pkg`
    // packer make every shipped file a real cost, and a man page earns none.
    let _ = fs::remove_dir_all(stage_prefix.join("share/man"));

    // Lay a second copy of dbclient down as `/usr/bin/ssh` (see fn doc). On Unix
    // `fs::copy` carries the source's permission bits, so the copy stays +x.
    let ssh = stage_prefix.join("bin/ssh");
    fs::copy(&dbclient, &ssh).map_err(|e| format!("dropbear: copy dbclient->ssh: {e}"))?;

    println!(
        "dropbear: produced /usr/bin/dbclient + /usr/bin/ssh ({} bytes each, static)",
        file_size(&dbclient)
    );
    Ok(())
}

/// Phase 86c Track A — the hardware-entropy poll the trimmed mbedTLS config binds
/// CTR_DRBG to. `MBEDTLS_ENTROPY_HARDWARE_ALT` makes mbedTLS call this symbol as
/// its strong entropy source; `MBEDTLS_NO_PLATFORM_ENTROPY` removes the
/// `/dev/urandom` / file-I/O path, so this is the SOLE source. It reads the
/// Phase 86a CSPRNG via `getrandom(2)` (Linux syscall 318, which m3OS implements
/// as `sys_getrandom`). The loop tolerates short reads + `EINTR`; any other error
/// returns `MBEDTLS_ERR_ENTROPY_SOURCE_FAILED` (-0x003C) so the handshake
/// fails-closed rather than seeding from a weak source.
const M3OS_MBEDTLS_HW_ENTROPY_C: &str = r#"/* Phase 86c — m3OS mbedTLS hardware-entropy shim (sys_getrandom CSPRNG). */
#include <stddef.h>
#include <errno.h>
#include <sys/random.h>

int mbedtls_hardware_poll(void *data, unsigned char *output,
                          size_t len, size_t *olen)
{
    (void) data;
    size_t got = 0;
    while (got < len) {
        ssize_t r = getrandom(output + got, len - got, 0);
        if (r < 0) {
            if (errno == EINTR)
                continue;
            return -0x003C; /* MBEDTLS_ERR_ENTROPY_SOURCE_FAILED */
        }
        if (r == 0)
            return -0x003C; /* fail closed: never spin on a 0-byte return */
        got += (size_t) r;
    }
    *olen = got;
    return 0;
}
"#;

/// Phase 86c Track A.2 — build-time self-test of the entropy callback (linked
/// against the same shim object that ships in `libmbedcrypto.a`). On the build
/// host `getrandom(2)` is also syscall 318, so this exercises the real shim:
/// it asserts (a) the callback fills exactly the requested length and (b) two
/// successive draws differ (non-constant). A regression in the shim or in the
/// `NO_PLATFORM_ENTROPY`/`HARDWARE_ALT` config fails the build immediately.
const M3OS_MBEDTLS_ENTROPY_TEST_C: &str = r#"#include <stdio.h>
#include <string.h>
#include <stddef.h>
int mbedtls_hardware_poll(void *, unsigned char *, size_t, size_t *);
int main(void) {
    unsigned char a[32], b[32];
    size_t oa = 0, ob = 0;
    if (mbedtls_hardware_poll((void*)0, a, sizeof a, &oa) != 0) { printf("FAIL poll a\n"); return 2; }
    if (mbedtls_hardware_poll((void*)0, b, sizeof b, &ob) != 0) { printf("FAIL poll b\n"); return 3; }
    if (oa != sizeof a || ob != sizeof b) { printf("FAIL olen %zu %zu\n", oa, ob); return 4; }
    if (memcmp(a, b, sizeof a) == 0) { printf("FAIL constant\n"); return 5; }
    printf("ENTROPY_OK olen=%zu\n", oa);
    return 0;
}
"#;

/// True iff `header` has an ACTIVE (non-commented) `#define <sym>` line — i.e. the
/// option is enabled. `scripts/config.py unset` rewrites `#define X` to
/// `//#define X`, so a commented line correctly reads as disabled.
fn mbedtls_config_enabled(header: &str, sym: &str) -> bool {
    let exact = format!("#define {sym}");
    let valued = format!("#define {sym} ");
    header
        .lines()
        .map(str::trim_start)
        .any(|l| l == exact || l.starts_with(&valued))
}

/// Phase 86c Track A — cross-compile mbedTLS ≥3.6.1 as static archives for the
/// curl `--with-mbedtls` TLS backend (and, through curl, the rebuilt HTTPS git).
///
/// mbedTLS is a build-time library only: its three archives
/// (`libmbedcrypto.a` + `libmbedx509.a` + `libmbedtls.a`) are linked statically
/// into `libcurl`/`git`, so it ships no binaries and nothing `pkg install`s it
/// directly. The trimmed config is derived from the shipped DEFAULT via
/// `scripts/config.py` (guaranteed self-consistent, passes `check_config.h`),
/// edited IN PLACE so the installed `mbedtls_config.h` is byte-identical to the
/// one the libraries were compiled with — curl/git then see the exact same config
/// (no struct-size ABI skew). The trim: CLIENT-ONLY (`MBEDTLS_SSL_SRV_C` off,
/// DTLS off), TLS 1.2 + 1.3, with ChaCha20-Poly1305 + ECDHE-ECDSA-P256 (the
/// constant-time p256-m, no assembly) + ECDHE-RSA + `MBEDTLS_X509_CRT_PARSE_C` +
/// `MBEDTLS_PEM_PARSE_C` (all on in the default and verified-by-construction
/// below). The CTR_DRBG entropy source is swapped to the Phase 86a `sys_getrandom`
/// CSPRNG (`MBEDTLS_ENTROPY_HARDWARE_ALT` + the `mbedtls_hardware_poll` shim) with
/// the `/dev/urandom` platform path removed (`MBEDTLS_NO_PLATFORM_ENTROPY`).
fn build_mbedtls(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
) -> Result<(), String> {
    let prefix = stage.join("usr/local");
    fs::create_dir_all(prefix.join("lib")).map_err(|e| format!("mkdir lib: {e}"))?;
    fs::create_dir_all(prefix.join("include")).map_err(|e| format!("mkdir include: {e}"))?;

    // ── 1) Derive the trimmed client-only config from the shipped default ─────
    // `scripts/config.py` edits `include/mbedtls/mbedtls_config.h` in place. The
    // default config is a complete, check_config-passing TLS client+server config;
    // we unset the server/DTLS surface and swap the entropy source. Each op is
    // individually safe (the default already satisfies every crypto dependency).
    let config_py = src.join("scripts/config.py");
    if !config_py.exists() {
        return Err(format!(
            "mbedtls build: {} missing (the RELEASE-asset tarball is required, not the tag tarball)",
            config_py.display()
        ));
    }
    let config_ops: &[(&str, &str)] = &[
        // Client-only: drop the server half of TLS entirely.
        ("unset", "MBEDTLS_SSL_SRV_C"),
        // No DTLS (and its sub-features) — git speaks HTTPS over TCP only.
        ("unset", "MBEDTLS_SSL_PROTO_DTLS"),
        ("unset", "MBEDTLS_SSL_DTLS_ANTI_REPLAY"),
        ("unset", "MBEDTLS_SSL_DTLS_HELLO_VERIFY"),
        ("unset", "MBEDTLS_SSL_DTLS_SRTP"),
        ("unset", "MBEDTLS_SSL_DTLS_CONNECTION_ID"),
        // curl owns the socket I/O (mbedtls_ssl_set_bio with curl's send/recv),
        // so mbedTLS's own BSD-socket layer is unused.
        ("unset", "MBEDTLS_NET_C"),
        // TLS 1.3 (GitHub serves it); idempotent if already on in the default.
        ("set", "MBEDTLS_SSL_PROTO_TLS1_3"),
        // Entropy: sys_getrandom CSPRNG only — no /dev/urandom / file-I/O path.
        ("set", "MBEDTLS_NO_PLATFORM_ENTROPY"),
        ("set", "MBEDTLS_ENTROPY_HARDWARE_ALT"),
    ];
    for (op, sym) in config_ops {
        let mut c = Command::new("python3");
        c.current_dir(src).arg("scripts/config.py").arg(op).arg(sym);
        run(&mut c, &format!("mbedtls config.py {op} {sym}"))?;
    }

    // Verify-by-construction (A.1): the installed config must match the spec.
    let config_path = src.join("include/mbedtls/mbedtls_config.h");
    let config_text = fs::read_to_string(&config_path)
        .map_err(|e| format!("mbedtls: read config {}: {e}", config_path.display()))?;
    for must_on in [
        "MBEDTLS_SSL_CLI_C",
        "MBEDTLS_SSL_PROTO_TLS1_2",
        "MBEDTLS_SSL_PROTO_TLS1_3",
        "MBEDTLS_CHACHAPOLY_C",
        "MBEDTLS_CHACHA20_C",
        "MBEDTLS_POLY1305_C",
        "MBEDTLS_ECDH_C",
        "MBEDTLS_ECDSA_C",
        "MBEDTLS_X509_CRT_PARSE_C",
        "MBEDTLS_PEM_PARSE_C",
        "MBEDTLS_CTR_DRBG_C",
        "MBEDTLS_ENTROPY_C",
        "MBEDTLS_ENTROPY_HARDWARE_ALT",
        "MBEDTLS_NO_PLATFORM_ENTROPY",
    ] {
        if !mbedtls_config_enabled(&config_text, must_on) {
            return Err(format!(
                "mbedtls build: required option {must_on} is not enabled in the trimmed config"
            ));
        }
    }
    for must_off in [
        "MBEDTLS_SSL_SRV_C",
        "MBEDTLS_SSL_PROTO_DTLS",
        "MBEDTLS_NET_C",
    ] {
        if mbedtls_config_enabled(&config_text, must_off) {
            return Err(format!(
                "mbedtls build: option {must_off} is still enabled — client-only trim did not take effect"
            ));
        }
    }
    // SECP256R1 (the GitHub leaf curve) must be present for ECDHE-ECDSA-P256.
    if !mbedtls_config_enabled(&config_text, "MBEDTLS_ECP_DP_SECP256R1_ENABLED") {
        return Err(
            "mbedtls build: SECP256R1 (P-256) curve disabled — cannot validate GitHub's ECDSA leaf"
                .to_string(),
        );
    }

    // ── 2) Build ONLY the static libraries (no programs, no tests) ────────────
    // mbedTLS uses a plain Makefile (not autotools); the cross is driven by
    // CC/AR passed as make ARGUMENTS (command-line assignments override the
    // Makefile's `?=` defaults). CFLAGS is fixed to -O2; the Makefile's own
    // WARNING_CFLAGS + `-I../include` live in LOCAL_CFLAGS and are unaffected.
    let mut make_cmd = Command::new("make");
    make_cmd
        .current_dir(src)
        .arg(format!("-j{}", num_jobs()))
        .arg(format!("CC={cc}"))
        .arg(format!("AR={ar}"))
        .arg("CFLAGS=-O2")
        .arg("lib");
    run(&mut make_cmd, "mbedtls make lib")?;

    let library = src.join("library");
    let libcrypto = library.join("libmbedcrypto.a");
    for a in ["libmbedcrypto.a", "libmbedx509.a", "libmbedtls.a"] {
        let p = library.join(a);
        if !p.exists() {
            return Err(format!("mbedtls build missing {}", p.display()));
        }
    }

    // ── 3) Compile the entropy shim + fold it into libmbedcrypto.a ────────────
    // MBEDTLS_ENTROPY_HARDWARE_ALT makes the entropy module reference an external
    // `mbedtls_hardware_poll`; adding the object to the crypto archive resolves it
    // for every downstream link (curl, git) with no extra link flags.
    let shim_c = library.join("m3os_hw_entropy.c");
    fs::write(&shim_c, M3OS_MBEDTLS_HW_ENTROPY_C)
        .map_err(|e| format!("mbedtls: write entropy shim: {e}"))?;
    let shim_o = library.join("m3os_hw_entropy.o");
    let mut cc_cmd = Command::new(cc);
    cc_cmd
        .current_dir(src)
        .arg("-O2")
        .arg("-Iinclude")
        .arg("-c")
        .arg("library/m3os_hw_entropy.c")
        .arg("-o")
        .arg("library/m3os_hw_entropy.o");
    run(&mut cc_cmd, "mbedtls entropy-shim compile")?;
    let mut ar_cmd = Command::new(ar);
    ar_cmd.arg("r").arg(&libcrypto).arg(&shim_o);
    run(&mut ar_cmd, "mbedtls ar add entropy-shim")?;
    let mut ranlib_cmd = Command::new(ranlib);
    ranlib_cmd.arg(&libcrypto);
    run(&mut ranlib_cmd, "mbedtls ranlib libmbedcrypto.a")?;

    // ── 4) A.2 — build-time entropy self-test (length + non-constant) ─────────
    let test_c = src.join("m3os_entropy_test.c");
    fs::write(&test_c, M3OS_MBEDTLS_ENTROPY_TEST_C)
        .map_err(|e| format!("mbedtls: write entropy test: {e}"))?;
    let test_bin = src.join("m3os_entropy_test");
    let mut test_cc = Command::new(cc);
    test_cc
        .current_dir(src)
        .arg("-O2")
        .arg("-static")
        .arg("m3os_entropy_test.c")
        .arg("library/m3os_hw_entropy.o")
        .arg("-o")
        .arg("m3os_entropy_test");
    run(&mut test_cc, "mbedtls entropy self-test compile")?;
    let test_out = Command::new(&test_bin)
        .output()
        .map_err(|e| format!("mbedtls entropy self-test: spawn: {e}"))?;
    let test_stdout = String::from_utf8_lossy(&test_out.stdout);
    if !test_out.status.success() || !test_stdout.contains("ENTROPY_OK") {
        return Err(format!(
            "mbedtls build: entropy callback self-test FAILED (status {:?}, stdout {:?}, stderr {:?})",
            test_out.status.code(),
            test_stdout.trim(),
            String::from_utf8_lossy(&test_out.stderr).trim()
        ));
    }
    println!("mbedtls: entropy self-test: {}", test_stdout.trim());

    // ── 5) Prove no file-I/O entropy path is linked ───────────────────────────
    // The platform poll's "/dev/urandom" string only lands in entropy_poll.o when
    // MBEDTLS_NO_PLATFORM_ENTROPY is undefined; with it set, that code is #if'd
    // out so the archive cannot contain the string. Direct, toolchain-independent
    // proof that entropy is sys_getrandom-only.
    if binary_contains(&libcrypto, b"/dev/urandom")? {
        return Err(
            "mbedtls build: libmbedcrypto.a references /dev/urandom — NO_PLATFORM_ENTROPY \
             ineffective (entropy must be sys_getrandom only)"
                .to_string(),
        );
    }

    // ── 6) Install: headers (incl. the in-place-edited config) + the archives ──
    // mbedTLS's `make install` uses a confusing DESTDIR-as-prefix convention, so
    // copy explicitly. The installed mbedtls_config.h IS the trimmed one, so curl
    // and git compile against the exact config the archives were built with.
    for hdr in ["mbedtls", "psa"] {
        // Remove any prior copy first so `cp -r src dest/` cannot nest a second
        // level (`include/mbedtls/mbedtls/…`) on a warm, non-wiped stage.
        let _ = fs::remove_dir_all(prefix.join("include").join(hdr));
        let mut cp = Command::new("cp");
        cp.arg("-r")
            .arg(src.join("include").join(hdr))
            .arg(prefix.join("include"));
        run(&mut cp, &format!("mbedtls cp include/{hdr}"))?;
    }
    if prefix.join("include/mbedtls/mbedtls").exists() {
        return Err(
            "mbedtls build: header tree double-nested (include/mbedtls/mbedtls)".to_string(),
        );
    }
    for a in ["libmbedcrypto.a", "libmbedx509.a", "libmbedtls.a"] {
        fs::copy(library.join(a), prefix.join("lib").join(a))
            .map_err(|e| format!("mbedtls: install {a}: {e}"))?;
    }
    if !prefix.join("include/mbedtls/ssl.h").exists() {
        return Err("mbedtls build: installed headers missing mbedtls/ssl.h".to_string());
    }

    println!(
        "mbedtls: produced static libmbedcrypto.a ({} bytes) + libmbedx509.a ({} bytes) + \
         libmbedtls.a ({} bytes); client-only TLS1.2/1.3, entropy=sys_getrandom (no /dev/urandom)",
        file_size(&prefix.join("lib/libmbedcrypto.a")),
        file_size(&prefix.join("lib/libmbedx509.a")),
        file_size(&prefix.join("lib/libmbedtls.a")),
    );
    Ok(())
}

/// The static link line for libcurl + its mbedTLS/zlib dependencies, in the
/// dependency-first order a static link requires (a library must precede the
/// libraries it references): `-lcurl` → `-lmbedtls` → `-lmbedx509` →
/// `-lmbedcrypto` → `-lz`. Built from the STAGED host prefixes (not the on-target
/// `/usr/local`), so it is reused both by curl's own verification and by git's
/// `CURL_LDFLAGS` when it links `git-remote-http`. The mbedTLS entropy shim
/// (`mbedtls_hardware_poll`) lives inside `libmbedcrypto.a`, so `getrandom` (libc)
/// is the only further symbol and resolves from musl.
fn curl_static_link_line(curl_prefix: &Path, mbedtls_prefix: &Path, zlib_prefix: &Path) -> String {
    format!(
        "-L{}/lib -L{}/lib -L{}/lib -lcurl -lmbedtls -lmbedx509 -lmbedcrypto -lz",
        curl_prefix.display(),
        mbedtls_prefix.display(),
        zlib_prefix.display(),
    )
}

/// Phase 86c Track B — cross-compile curl/libcurl as a static HTTP/HTTPS client
/// with the mbedTLS TLS backend, linking the staged static mbedtls + zlib.
///
/// git's smart-HTTP transport does not implement TLS itself: its
/// `git-remote-http` helper links `libcurl`, and libcurl carries the mbedTLS
/// backend that validates GitHub's TLS 1.3 cert chain + hostname. So this builds
/// a static `libcurl.a` (+ the `curl` CLI, used here for build-time verification)
/// configured `--with-mbedtls --with-ca-bundle=/etc/ssl/certs/ca-certificates.crt`
/// — the SAME CAINFO path the Phase 86a bundle stages and `/etc/gitconfig`'s
/// `http.sslCAInfo` names, so git and curl agree on trust. Every protocol but
/// HTTP/HTTPS is disabled to keep the binary small; the synchronous resolver
/// (`--disable-threaded-resolver`) avoids a pthread dependency and uses musl's
/// `getaddrinfo` (Phase 77 DNS). SNI + `SSL_VERIFYHOST=2` (curl's verified default)
/// stay on — hostname verification is separate from chain validation.
fn build_curl(
    src: &Path,
    stage: &Path,
    (cc, ar, ranlib): &(&'static str, String, String),
    zlib_stage: &Path,
    mbedtls_stage: &Path,
) -> Result<(), String> {
    let zlib_prefix = zlib_stage.join("usr/local");
    if !zlib_prefix.join("lib/libz.a").exists() {
        return Err(format!(
            "curl build: staged zlib not found at {} (build the zlib port first)",
            zlib_prefix.join("lib/libz.a").display()
        ));
    }
    let mbedtls_prefix = mbedtls_stage.join("usr/local");
    if !mbedtls_prefix.join("lib/libmbedtls.a").exists() {
        return Err(format!(
            "curl build: staged mbedtls not found at {} (build the mbedtls port first)",
            mbedtls_prefix.join("lib/libmbedtls.a").display()
        ));
    }
    // ABI-skew guard (the Track A reviewer's note): the mbedtls headers curl will
    // compile against MUST be the trimmed config the archives were built with —
    // i.e. carry the hardware-entropy macro. A mismatched (e.g. system) header
    // would skew struct sizes and silently corrupt the TLS context.
    let mbedtls_config = mbedtls_prefix.join("include/mbedtls/mbedtls_config.h");
    let mbedtls_config_text = fs::read_to_string(&mbedtls_config).map_err(|e| {
        format!(
            "curl build: read staged mbedtls config {}: {e}",
            mbedtls_config.display()
        )
    })?;
    if !mbedtls_config_enabled(&mbedtls_config_text, "MBEDTLS_ENTROPY_HARDWARE_ALT") {
        return Err(
            "curl build: staged mbedtls_config.h is not the trimmed m3OS config \
             (MBEDTLS_ENTROPY_HARDWARE_ALT absent) — ABI skew risk; rebuild the mbedtls port"
                .to_string(),
        );
    }

    let stage_prefix = stage.join("usr/local");
    fs::create_dir_all(&stage_prefix).map_err(|e| format!("mkdir: {e}"))?;

    let extra_ld = musl_extra_ldflags_joined();
    let ldflags = if extra_ld.is_empty() {
        format!(
            "-static -L{}/lib -L{}/lib",
            zlib_prefix.display(),
            mbedtls_prefix.display()
        )
    } else {
        format!(
            "-static -L{}/lib -L{}/lib {extra_ld}",
            zlib_prefix.display(),
            mbedtls_prefix.display()
        )
    };
    let cppflags = format!(
        "-I{}/include -I{}/include",
        zlib_prefix.display(),
        mbedtls_prefix.display()
    );

    // ── configure: static, mbedTLS backend, HTTP/HTTPS only ───────────────────
    let mut configure_cmd = Command::new("sh");
    configure_cmd
        .current_dir(src)
        .arg("./configure")
        .arg("--host=x86_64-linux-musl")
        .arg("--prefix=/usr/local")
        .arg("--disable-shared")
        .arg("--enable-static")
        .arg(format!("--with-mbedtls={}", mbedtls_prefix.display()))
        .arg(format!("--with-zlib={}", zlib_prefix.display()))
        // curl's compiled-in default CAINFO == the Phase 86a bundle path == git's
        // http.sslCAInfo, so trust does not silently diverge across the two.
        .arg("--with-ca-bundle=/etc/ssl/certs/ca-certificates.crt")
        // Synchronous resolver (no pthread dep); musl getaddrinfo + Phase 77 DNS.
        .arg("--disable-threaded-resolver")
        // HTTP/HTTPS only — disable every other protocol to keep the binary small.
        .arg("--disable-ldap")
        .arg("--disable-ldaps")
        .arg("--disable-ftp")
        .arg("--disable-file")
        .arg("--disable-dict")
        .arg("--disable-gopher")
        .arg("--disable-imap")
        .arg("--disable-pop3")
        .arg("--disable-smtp")
        .arg("--disable-telnet")
        .arg("--disable-tftp")
        .arg("--disable-rtsp")
        .arg("--disable-smb")
        .arg("--disable-mqtt")
        // Drop optional deps m3OS does not ship (each would be a link failure or a
        // silent feature m3OS cannot serve).
        .arg("--without-libpsl")
        .arg("--without-libidn2")
        .arg("--without-brotli")
        .arg("--without-zstd")
        .arg("--without-nghttp2")
        .arg("--without-nghttp3")
        .arg("--without-ngtcp2")
        .arg("--without-libssh2")
        .arg("--without-libssh")
        .arg("--without-librtmp")
        .arg("--without-gssapi")
        .arg("--disable-manual")
        .env("CC", cc)
        .env("AR", ar)
        .env("RANLIB", ranlib)
        .env("CFLAGS", "-O2")
        .env("CPPFLAGS", &cppflags)
        .env("LDFLAGS", &ldflags);
    run(&mut configure_cmd, "curl configure")?;

    // curl links the `curl` CLI through libtool, which treats a bare `-static`
    // as "use static *libtool* libraries" — NOT "produce a fully static binary",
    // so the tool would keep a PT_INTERP and fault on m3OS (no real libc.so).
    // libtool's `-all-static` IS the fully-static request; gcc never sees it
    // (libtool parses + expands it), so it cannot go in the configure LDFLAGS
    // (gcc runs the configure link probes directly and would reject it). Pass it
    // only at `make`, as a command-line LDFLAGS override that keeps the staged
    // `-L` paths + the musl static-compat stubs. libcurl.a (an `ar` archive) is
    // unaffected. This is the standard libtool fully-static idiom.
    let make_ldflags = if extra_ld.is_empty() {
        format!(
            "-all-static -L{}/lib -L{}/lib",
            zlib_prefix.display(),
            mbedtls_prefix.display()
        )
    } else {
        format!(
            "-all-static -L{}/lib -L{}/lib {extra_ld}",
            zlib_prefix.display(),
            mbedtls_prefix.display()
        )
    };
    let mut make_cmd = Command::new("make");
    make_cmd
        .current_dir(src)
        .arg(format!("-j{}", num_jobs()))
        .arg(format!("LDFLAGS={make_ldflags}"));
    run(&mut make_cmd, "curl make")?;

    let mut install_cmd = Command::new("make");
    install_cmd
        .current_dir(src)
        .arg(format!("LDFLAGS={make_ldflags}"))
        .arg("install")
        .arg(format!("DESTDIR={}", stage.display()));
    run(&mut install_cmd, "curl install")?;

    // ── Verify the static libcurl + mbedTLS backend (B.1) ─────────────────────
    let libcurl = stage_prefix.join("lib/libcurl.a");
    if !libcurl.exists() {
        return Err(format!("curl build missing {}", libcurl.display()));
    }
    let curl_bin = stage_prefix.join("bin/curl");
    if !curl_bin.exists() {
        return Err(format!("curl build missing {}", curl_bin.display()));
    }
    // The curl CLI must be static (m3OS's ld-musl has no real libc.so).
    if binary_contains(&curl_bin, b"/lib/ld-musl-x86_64.so.1")? {
        return Err(format!(
            "curl build: {} references the dynamic loader — `-static` did not take effect",
            curl_bin.display()
        ));
    }
    // The static `curl` binary is a musl x86-64 ELF that runs on the build host:
    // `curl --version` reports its TLS backend + feature flags. Assert mbedTLS is
    // the backend and HTTPS is a supported protocol (the small-footprint TLS path).
    let ver = Command::new(&curl_bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("curl build: run curl --version: {e}"))?;
    let ver_out = String::from_utf8_lossy(&ver.stdout);
    if !ver.status.success() || !ver_out.contains("mbedTLS") {
        return Err(format!(
            "curl build: curl --version did not report the mbedTLS backend (got {:?})",
            ver_out.trim()
        ));
    }
    if !ver_out.to_lowercase().contains("https") {
        return Err(format!(
            "curl build: curl --version does not list HTTPS as a protocol (got {:?})",
            ver_out.trim()
        ));
    }
    // The compiled-in CAINFO must be the Phase 86a path (so git + curl agree).
    if !binary_contains(&curl_bin, b"/etc/ssl/certs/ca-certificates.crt")? {
        return Err(
            "curl build: binary does not embed the CAINFO path /etc/ssl/certs/ca-certificates.crt \
             (--with-ca-bundle did not take effect — trust would diverge from git)"
                .to_string(),
        );
    }
    let _ = ranlib; // consumed via .env("RANLIB", ..) above

    println!(
        "curl: produced static libcurl.a ({} bytes) + curl CLI ({} bytes); {}; \
         CAINFO=/etc/ssl/certs/ca-certificates.crt",
        file_size(&libcurl),
        file_size(&curl_bin),
        ver_out.lines().next().unwrap_or("").trim(),
    );
    Ok(())
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

/// Phase 90a (D.2) — is the JIT `build_node` variant requested? Gated on
/// `M3OS_NODE_JIT=1`. When set, `build_node` drops `--v8-options=--jitless`
/// (re-enabling full TurboFan/Maglev/Sparkplug + WASM codegen), applies the three
/// V8/Node PKU patches from the Phase 90a A.1 findings, and links a `pkey_*` shim.
/// When UNSET the build is byte-identical to the Phase 89 jitless artifact — every
/// JIT-only step in `build_node` is guarded by this predicate. The jitless `.m3pkg`
/// remains the documented default everywhere; the JIT variant is a second sealed
/// artifact under its own content key (see [`node_variant_id`]).
fn node_jit_enabled() -> bool {
    std::env::var("M3OS_NODE_JIT").is_ok_and(|v| v == "1")
}

/// Phase 90a (D.2) — the stable variant token folded into `node`'s content key so
/// a JIT build and a jitless build get DISTINCT keys (no cross-pkgcache
/// contamination: a jitless build after a JIT build is a pure MISS for the other
/// and a HIT for its own kind). Pure function of [`node_jit_enabled`] — host-tested
/// in `node_variant_id_distinguishes_jit_and_folds_into_content_key`.
fn node_variant_id() -> &'static str {
    if node_jit_enabled() { "jit" } else { "jitless" }
}

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
/// Phase 89 — cross-build a fully-static musl `node` (+ bundled `npm`) using the
/// host clang/clang++ targeting musl (musl-tools has no C++ compiler, so this is
/// the `build_llvm` model, NOT the musl-gcc C path) plus `build_llvm`'s static
/// `libc++.a`. The V8 engine is built **jitless** (`--v8-options=--jitless`, NOT
/// `--v8-lite-mode` — see the configure block) so it never requests
/// RWX/executable memory (m3OS W^X). See `ports/lang/node/Portfile` and
/// `docs/89-nodejs.md` for the full configuration rationale.
///
/// Phase 90a (D.2) — when `M3OS_NODE_JIT=1` ([`node_jit_enabled`]) this builds the
/// **JIT** variant instead: `--v8-options=--jitless` is dropped, the three A.1 V8/
/// Node PKU patches are applied (musl `pkey_*` shim + `NodePlatform::
/// GetThreadIsolatedAllocator` override + `KernelHasPkruFix` m3OS-accept), and the
/// `PKEY_DISABLE_*` macro defs are added to CXXFLAGS so V8 emits PKU-guarded W+X
/// commits the W^X v2 kernel rule permits. The default (unset) build is unchanged.
fn build_node(port_src: &Path, stage: &Path, port_dir: &Path) -> Result<(), String> {
    let root = workspace_root();
    let jobs = format!("-j{}", num_jobs());
    const NODE_TRIPLE: &str = "x86_64-unknown-linux-musl";

    // ── 1. host toolchain preflight (host clang→musl; no musl g++ needed) ────────
    let clang = std::env::var("M3OS_NODE_CLANG").unwrap_or_else(|_| "clang".to_string());
    let clangxx = std::env::var("M3OS_NODE_CLANGXX").unwrap_or_else(|_| "clang++".to_string());
    for c in [clang.as_str(), clangxx.as_str()] {
        if !probe_tool_on_path(c) {
            return Err(format!(
                "node build: host C/C++ compiler '{c}' not found on PATH. Phase 89 \
                 cross-builds Node with the host clang targeting musl (set \
                 M3OS_NODE_CLANG / M3OS_NODE_CLANGXX to override). Install clang + lld \
                 (Debian/Ubuntu: `apt install clang lld`)."
            ));
        }
    }
    if !probe_tool_on_path("ld.lld") {
        return Err(
            "node build: `ld.lld` not found on PATH (the cross-link uses \
                    `-fuse-ld=lld`). Install LLVM's linker (`apt install lld`)."
                .into(),
        );
    }
    if !probe_tool_on_path("python3") {
        return Err(
            "node build: `python3` not found on PATH (Node's `configure` + V8 \
                    GYP/torque are Python). Install python3."
                .into(),
        );
    }
    if !probe_tool_on_path("make") {
        return Err(
            "node build: `make` not found on PATH (Node's top-level build \
                    driver). Install make."
                .into(),
        );
    }

    // ── 2. reuse the `llvm` port's static musl C++ sysroot (libc++.a + builtins) ─
    // Node is C++ and musl-tools ships no C++ compiler, so — exactly like the
    // clang port — the V8/Node compile needs a musl libc++. Reuse the artifact the
    // `llvm` port already builds at target/llvm-musl-sysroot rather than rebuild it
    // (a build-time prerequisite, NOT a runtime `.m3pkg` DEP).
    let sysroot = root.join("target/llvm-musl-sysroot");
    if !sysroot.join("lib/libc++.a").exists()
        || !sysroot
            .join("lib/linux/libclang_rt.builtins-x86_64.a")
            .exists()
    {
        return Err(format!(
            "node build: musl C++ sysroot not found at {} (need lib/libc++.a + the \
             compiler-rt builtins). Build the `llvm` port first — it assembles the \
             musl sysroot and the static libc++ that the Node V8 cross-compile \
             reuses:  `cargo xtask port build llvm`  (or enable M3OS_WITH_CLANG). \
             This is a build-time prerequisite, not a runtime DEP.",
            sysroot.display()
        ));
    }

    // ── 3. persistent stable source (Node builds IN-tree; never re-extract) ──────
    // `port_build` re-extracts + re-patches `port_src` with fresh mtimes every run,
    // which would defeat ninja incrementality and force a multi-hour V8 rebuild on
    // every retry. Move the freshly-extracted+patched tree to a stable location
    // once, then reuse it (the pkgcache still seals the `.m3pkg` so a clean machine
    // pays the full build exactly once). Mirrors `build_llvm`.
    let cross_root = root.join("target/node-cross");
    fs::create_dir_all(&cross_root).map_err(|e| format!("mkdir node-cross: {e}"))?;
    // A required, non-empty VERSION — never default to "" (which would name the
    // persistent source dir `node-v`, silently mixing sources across versions and
    // hiding a Portfile parse error).
    let version = parse_portfile(&port_dir.join("Portfile"))
        .map_err(|e| format!("node: re-read Portfile for src dir name: {e}"))?
        .get("VERSION")
        .cloned()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "node: Portfile is missing a non-empty VERSION".to_string())?;
    let stable_src = cross_root.join(format!("node-v{version}"));
    // Phase 90a (D.2) — the JIT and jitless variants share this persistent source
    // tree. The JIT variant mutates V8/Node source in place (the three A.1
    // patches); the jitless variant must NEVER inherit those mutations (and vice
    // versa) or the default build would not be byte-identical. Record which variant
    // the stable tree was last prepared for in a marker file, and FORCE a re-stage
    // (discard the tree) when the requested variant differs from the marker. This
    // guarantees: (a) the default jitless build is byte-identical regardless of any
    // prior JIT build, and (b) the JIT patches are applied to a pristine tree, so
    // their in-place application is unambiguous. The pkgcache still seals each
    // variant's `.m3pkg` under its own key, so the rare re-stage on a variant flip
    // costs a one-time rebuild, not repeated work.
    let variant = node_variant_id();
    let variant_marker = stable_src.join(".m3os-node-variant");
    let marker_matches = fs::read_to_string(&variant_marker)
        .map(|s| s.trim() == variant)
        .unwrap_or(false);
    if stable_src.join("configure").exists() && !marker_matches {
        println!(
            "node: persistent source at {} was prepared for a different variant \
             (requested `{variant}`) — discarding it so the {variant} build starts \
             from a pristine tree (no cross-variant patch leak)",
            stable_src.display()
        );
        let _ = fs::remove_dir_all(&stable_src);
    }
    let src: PathBuf = if stable_src.join("configure").exists() {
        println!(
            "node: reusing persistent source {} (variant `{variant}`)",
            stable_src.display()
        );
        stable_src
    } else {
        println!(
            "node: staging persistent source at {} (once, variant `{variant}`)",
            stable_src.display()
        );
        let _ = fs::remove_dir_all(&stable_src);
        // `port_src` is the port machinery's freshly-extracted + patched tree;
        // move it to the stable location (patches ride along). Next run re-extracts
        // a fresh `port_src`, which we ignore once the stable tree exists.
        fs::rename(port_src, &stable_src).map_err(|e| format!("node: stage source: {e}"))?;
        // Record the variant this pristine tree is being prepared for, so a later
        // run with the OTHER variant re-stages instead of reusing patched source.
        fs::write(&variant_marker, variant)
            .map_err(|e| format!("node: write variant marker: {e}"))?;
        stable_src
    };
    let src = src.as_path();

    // Phase 90a (D.2) — JIT variant: apply the three A.1 V8/Node PKU patches +
    // stage the musl `pkey_*` shim into the source tree, idempotently (a sentinel
    // guards against double-application across the persistent-source reuse). No-op
    // for the default jitless build (so it stays byte-identical to Phase 89).
    // NOTE: `.cc` (C++), NOT `.c`. V8 weak-declares the `pkey_*` functions at
    // global namespace scope in C++ TUs WITHOUT `extern "C"` (e.g.
    // `memory-protection-key.cc:25` `int pkey_mprotect(...) V8_WEAK;`). On glibc
    // those declarations resolve to libc's `extern "C"` symbols because glibc's
    // `<sys/mman.h>` declares them; musl's `<sys/mman.h>` does NOT, so V8's own
    // C++-linkage declaration wins and the call sites reference the *mangled*
    // names (`_Z10pkey_allocjj`, `_Z9pkey_freei`, `_Z13pkey_mprotectPvmii`,
    // `_Z8pkey_geti`, `_Z8pkey_setij`). A C shim defines the unmangled C symbols
    // (`pkey_alloc` …), which do NOT bind those mangled weak refs — so
    // `PkeyAlloc()`'s `if (!pkey_alloc) return -1;` sees a null weak symbol,
    // pkey allocation is skipped, ThreadIsolation stays disabled, and V8 falls
    // back to plain `mprotect(RWX)`. Compiling the shim as C++ (no `extern "C"`)
    // makes its strong defs mangle to the IDENTICAL names V8 references, so they
    // bind over the weak refs and PKU engages. (Empirically confirmed:
    // `nm node | grep _Z10pkey_allocjj` showed an undefined weak ref distinct
    // from the strong C `pkey_alloc` before this fix.)
    let pkey_shim_rel = "deps/v8/src/base/platform/m3os-pkey-shim.cc";
    if node_jit_enabled() {
        apply_node_jit_patches(src, pkey_shim_rel)?;
    }

    // ── 4. toolchain env: host clang→musl for TARGET, host glibc clang for HOST ──
    // V8's mksnapshot/torque run on the build host (same arch x86_64 → native), so
    // they MUST link glibc; the final node binary is musl-static. Keep the two
    // toolsets distinct: --target/--sysroot/-static only in the TARGET vars.
    // `node.gyp` unconditionally appends `-latomic` for every Linux+clang build
    // (PR #25852). The static musl/clang sysroot has no libatomic, and x86_64
    // lowers all of V8/Node's atomics to inline instructions (the linked binary
    // has zero undefined `__atomic_*` refs), so put the empty-archive stub dir
    // (which includes `libatomic.a`) on the link path UNCONDITIONALLY — not gated
    // on `find_musl_cc` like the autotools ports — or the final link fails with
    // `ld.lld: error: unable to find library -latomic` on any host that lacks a
    // system libatomic on lld's default search path.
    let stub = crate::musl_stub_ldflags_always().join(" ");
    let sysroot_s = sysroot.display().to_string();
    let tgt = format!("--target={NODE_TRIPLE} --sysroot={sysroot_s}");
    let warn = "-Wno-unused-command-line-argument";
    // clang 16+ promoted several legacy-C constructs from warnings to hard
    // errors. The bundled OpenSSL/c-ares/etc. trip them on musl — notably
    // OpenSSL's `o_str.c` assumes `_GNU_SOURCE` ⇒ glibc's `char*`-returning
    // `strerror_r`, but musl always returns `int` (no GNU variant), and
    // `_GNU_SOURCE` is still required for V8's `pthread_getattr_np`. Downgrade
    // those clang-16 promotions back to warnings so the legacy C deps build.
    let legacy_c = "-Wno-error=int-conversion -Wno-error=implicit-function-declaration \
                    -Wno-error=incompatible-pointer-types -Wno-error=implicit-int";
    let cflags =
        format!("{tgt} -O2 -fno-omit-frame-pointer -D_GNU_SOURCE -D__MUSL__ {warn} {legacy_c}");
    // clang does NOT auto-add `<sysroot>/include/c++/v1` to the C++ search path
    // for this bare musl triple — its default C++ search for the sysroot is just
    // `<sysroot>/include` + the resource dir — so the libc++ headers the `llvm`
    // port installed there go unseen and `<cstddef>`/`<memory>` come back "file
    // not found". Point at them explicitly with `-cxx-isystem`.
    // Phase 90a (D.2) — JIT variant: musl defines neither the `PKEY_DISABLE_ACCESS`
    // nor `PKEY_DISABLE_WRITE` macros, so V8's `#ifdef PKEY_DISABLE_WRITE` arm in
    // `default-thread-isolated-allocator.cc` compiles to a `return -1` stub
    // (A.1 finding 1). Define them at their Linux values so the real
    // `pkey_alloc(... PKEY_DISABLE_WRITE)` path compiles in. Empty for the default
    // jitless build, so its CXXFLAGS stay byte-identical to Phase 89.
    let pkey_defs = if node_jit_enabled() {
        " -DPKEY_DISABLE_ACCESS=0x1 -DPKEY_DISABLE_WRITE=0x2"
    } else {
        ""
    };
    let cxxflags = format!(
        "{tgt} -stdlib=libc++ -cxx-isystem {sysroot_s}/include/c++/v1 -rtlib=compiler-rt \
         -O2 -fno-omit-frame-pointer -D_GNU_SOURCE -D__MUSL__ {warn}{pkey_defs}"
    );
    // Phase 90a (D.2) — JIT variant: compile the musl `pkey_*` shim (A.1 finding 1)
    // to an object and put it on the FINAL link line. V8 weak-declares
    // `pkey_alloc/free/mprotect/get/set`; musl provides none. Our strong defs in
    // the shim therefore bind at link (weak < strong), giving V8 a working
    // `pkey_*` surface (the three syscall wrappers + RDPKRU/WRPKRU for get/set).
    // Adding the object via LDFLAGS — rather than editing V8's gyp/GN sources list —
    // keeps the wiring robust across V8 version drift: it only needs the symbols
    // present on the node binary's link line, which strong-over-weak resolution
    // honors regardless of which translation unit the weak references live in.
    // Empty for the default jitless build → its LDFLAGS stay byte-identical.
    let pkey_shim_obj = if node_jit_enabled() {
        let obj = compile_node_pkey_shim(src, pkey_shim_rel, &clang, &cflags)?;
        format!(" {}", obj.display())
    } else {
        String::new()
    };
    let ldflags = format!(
        "{tgt} -static -stdlib=libc++ -L{sysroot_s}/lib -rtlib=compiler-rt \
         -unwindlib=libunwind -fuse-ld=lld {warn} {stub}{pkey_shim_obj}"
    );
    let ar = if probe_tool_on_path("llvm-ar") {
        "llvm-ar"
    } else {
        "ar"
    };
    let nm = if probe_tool_on_path("llvm-nm") {
        "llvm-nm"
    } else {
        "nm"
    };
    // The same env feeds `configure`, `make`, and `make install`.
    let apply_env = |cmd: &mut Command| {
        cmd.env("CC", &clang)
            .env("CXX", &clangxx)
            .env("CC_host", &clang)
            .env("CXX_host", &clangxx)
            .env("CFLAGS", &cflags)
            .env("CXXFLAGS", &cxxflags)
            .env("LDFLAGS", &ldflags)
            .env("CFLAGS_host", "-O2")
            .env("CXXFLAGS_host", "-O2")
            .env("LDFLAGS_host", "")
            .env("AR", ar)
            .env("AR_host", "ar")
            .env("NM", nm);
    };

    // ── 5. configure (static V8, small-icu, bundled deps) ────────────────────────
    // Phase 90a (D.2) — the ONLY configure difference between the variants: the
    // jitless default passes `--v8-options=--jitless` (Phase 89, W^X-clean,
    // Ignition-only, zero runtime executable memory); the JIT variant DROPS it,
    // re-enabling full TurboFan/Maglev/Sparkplug codegen + WASM, which commits
    // PKU-guarded W+X code pages the W^X v2 kernel rule permits. The A.1 finding
    // records that relying on runtime `--no-jitless` is INSUFFICIENT — the Phase 89
    // binary bakes `--jitless` as an embedded default, latching `--no-opt`, so the
    // flag must be dropped at configure time for TurboFan to actually run.
    let jit = node_jit_enabled();
    println!(
        "node: configure (static musl, {} V8, full-icu) — {}",
        if jit { "JIT (PKU-guarded)" } else { "jitless" },
        src.display()
    );
    let mut cfg = Command::new("python3");
    cfg.current_dir(src).arg("configure").args([
        "--prefix=/usr",
        "--dest-cpu=x64",
        "--dest-os=linux",
        // NB: NOT `--cross-compiling`. Build host and target are both x86_64, and
        // a fully-static musl mksnapshot/torque runs natively on the glibc host
        // (static = no loader). `--cross-compiling` would force V8's
        // `want_separate_host_toolset=1`, which emits BOTH host- and
        // target-toolset `v8_inspector_headers` rules writing the same
        // arch-independent `gen/.../js_protocol.stamp` → ninja "multiple rules
        // generate" error. A single (native) toolset generates it once.
        "--fully-static",
        "--enable-static",
        // Phase 90b — FULL ICU (not small-icu). small-icu omits the ICU
        // break-iterator/segmentation data, so `Intl.Segmenter.prototype.segment()`
        // gets a NULL `icu::BreakIterator` and V8's `JSSegments::Create` null-derefs
        // (confirmed: pid faults at `JSSegments::Create+0x2c` reading address 0x0).
        // Claude Code's interactive TUI calls `Intl.Segmenter` for Unicode grapheme
        // segmentation (terminal string width / wrapping), so small-icu makes the TUI
        // unusable. full-icu compiles the complete ICU data (incl. brkitr) statically
        // into the binary, so segmentation works with no runtime data file. Costs
        // ~30 MB of binary size; `--download=all` lets configure fetch the full ICU
        // data the source tarball does not bundle (it ships only `deps/icu-small`).
        "--with-intl=full-icu",
        "--download=all",
    ]);
    if !jit {
        // Jitless via `--v8-options=--jitless`, NOT `--v8-lite-mode`. Both make
        // V8 allocate zero runtime executable memory (Ignition-only, no
        // RWX/JIT → W^X-clean), but `--v8-lite-mode` ALSO sets
        // `v8_enable_webassembly=false`, and Node 22 unconditionally passes its
        // default `--experimental-wasm-imported-strings`/`-memory64`/`-exnref`
        // V8 flags at startup → a WASM-less V8 rejects them as a fatal "bad
        // option" (exit 9) before `node --version` ever prints. Keeping WASM
        // compiled in makes V8 recognise those flags (then `--jitless` renders
        // them inert), so node starts cleanly while staying W^X-safe.
        cfg.arg("--v8-options=--jitless");
    }
    cfg.args([
        // NB: NOT `--ninja`. V8's gyp emits `v8_inspector_headers` for BOTH the
        // host and target toolsets, both writing the same arch-independent
        // `gen/.../js_protocol.stamp` (Node's `--without-inspector` disables
        // Node's inspector but NOT V8's `v8_enable_inspector`). ninja treats
        // "multiple rules generate X" as a FATAL error; GYP's make generator (the
        // default, most-tested Node build backend) tolerates it as a benign
        // "overriding recipe" warning. So we use the make backend.
        "--openssl-no-asm",
        "--without-corepack",
        // The Node startup snapshot is built by running a host-glibc
        // `node_mksnapshot` against the musl-static target build during the
        // build; disabling it sidesteps a host/target snapshot-blob mismatch
        // (the snapshot is regenerated at first run instead). Small startup cost,
        // robust cross-build.
        "--without-node-snapshot",
        "--without-inspector",
    ]);
    apply_env(&mut cfg);
    run(&mut cfg, "node configure")?;

    // ── 6. build (GYP make backend; V8 + bundled OpenSSL/ICU = LONG) ─────────────
    println!("node: build ({jobs}) [LONG — V8 + bundled OpenSSL/ICU; multi-hour cold]");
    let mut mk = Command::new("make");
    mk.current_dir(src).arg(&jobs);
    apply_env(&mut mk);
    run(&mut mk, "node build")?;

    // ── 7. install into the DESTDIR stage (→ <stage>/usr/{bin,lib}) ──────────────
    println!("node: install (DESTDIR={})", stage.display());
    let _ = fs::remove_dir_all(stage);
    fs::create_dir_all(stage).map_err(|e| format!("mkdir stage: {e}"))?;
    let mut inst = Command::new("make");
    inst.current_dir(src).arg("install").env("DESTDIR", stage);
    apply_env(&mut inst);
    run(&mut inst, "node install")?;

    assert_node_layout(stage)?;
    Ok(())
}

/// Phase 90a (D.2) — the musl `pkey_*` shim source (A.1 finding 1). V8
/// weak-declares `pkey_alloc/free/mprotect/get/set`
/// (`src/base/platform/memory-protection-key.cc:25`,
/// `src/libplatform/default-thread-isolated-allocator.cc:28`) and null-checks them
/// at runtime; musl defines none. These STRONG defs bind over the weak references
/// at the final link. `pkey_alloc/free/mprotect` are real syscalls (nrs
/// 330/331/329 on x86_64 — the same Linux ABI numbers the B.3 kernel handlers
/// dispatch); `pkey_get/set` are NOT syscalls — they are the `RDPKRU`/`WRPKRU`
/// instructions reading/writing the per-thread PKRU register (which B.4 saves/
/// restores via XSAVE component 9). The shim mirrors glibc's libc surface so V8's
/// runtime detection + write-window code work unmodified.
///
/// IMPORTANT: this is compiled as **C++** (staged as `m3os-pkey-shim.cc`, built
/// with `-x c++`) and deliberately has NO `extern "C"`, so its five `pkey_*`
/// definitions mangle to the C++ names V8's weak refs use (`_Z10pkey_allocjj`
/// etc.). A C-linkage shim defines unmangled symbols that do NOT bind V8's
/// mangled weak refs on musl (where `<sys/mman.h>` omits the `extern "C"`
/// declarations glibc provides), so PKU would silently stay disabled.
const NODE_PKEY_SHIM_C: &str = r#"/* m3OS Phase 90a (D.2) — musl pkey_* shim for V8 PKU JIT (compiled as C++).
 *
 * V8 weak-declares pkey_alloc/free/mprotect/get/set and null-checks them; musl
 * provides none. These strong definitions bind over the weak refs at link time.
 * Linked into `node` via LDFLAGS (an object on the final link line), NOT a gyp
 * sources-list edit, so the wiring is robust across V8 version drift.
 *
 * Built as C++ with NO `extern "C"` so the symbols mangle to V8's weak-ref names
 * (`_Z10pkey_allocjj`, `_Z9pkey_freei`, `_Z13pkey_mprotectPvmii`,
 * `_Z8pkey_geti`, `_Z8pkey_setij`). On musl, V8's own un-`extern "C"`
 * declarations give the call sites C++ linkage, so a C shim would not bind them.
 *
 * pkey_alloc/free/mprotect -> raw syscalls (x86_64 nrs 330/331/329, the Linux ABI
 *   the m3OS Phase 90a B.3 kernel handlers honor).
 * pkey_get/set            -> RDPKRU/WRPKRU instructions (NOT syscalls); they
 *   read/write the per-thread PKRU register.  Each protection key owns 2 bits in
 *   PKRU: bit (2*key) = Access-Disable, bit (2*key+1) = Write-Disable.
 */
#include <sys/syscall.h>
#include <unistd.h>
#include <errno.h>
#include <stdint.h>

#ifndef SYS_pkey_mprotect
#define SYS_pkey_mprotect 329
#endif
#ifndef SYS_pkey_alloc
#define SYS_pkey_alloc 330
#endif
#ifndef SYS_pkey_free
#define SYS_pkey_free 331
#endif

#ifndef PKEY_DISABLE_ACCESS
#define PKEY_DISABLE_ACCESS 0x1
#endif
#ifndef PKEY_DISABLE_WRITE
#define PKEY_DISABLE_WRITE 0x2
#endif

/* Read the full 32-bit PKRU via RDPKRU (ECX must be 0; EDX clobbered). */
static inline uint32_t m3os_rdpkru(void) {
    uint32_t pkru, edx;
    __asm__ volatile("rdpkru" : "=a"(pkru), "=d"(edx) : "c"(0));
    return pkru;
}

/* Write the full 32-bit PKRU via WRPKRU (ECX and EDX must be 0). */
static inline void m3os_wrpkru(uint32_t pkru) {
    __asm__ volatile("wrpkru" : : "a"(pkru), "c"(0), "d"(0));
}

/* musl's syscall() already returns -1 and sets errno on error (it does NOT return
 * -errno), so we must NOT write errno ourselves — pass the result straight through. */
int pkey_alloc(unsigned int flags, unsigned int access_rights) {
    return (int)syscall(SYS_pkey_alloc, (long)flags, (long)access_rights);
}

int pkey_free(int pkey) {
    return (int)syscall(SYS_pkey_free, (long)pkey);
}

int pkey_mprotect(void *addr, size_t len, int prot, int pkey) {
    return (int)syscall(SYS_pkey_mprotect, addr, (long)len, (long)prot, (long)pkey);
}

/* Return the access rights (PKEY_DISABLE_ACCESS|PKEY_DISABLE_WRITE) for `pkey`
 * by decoding its 2-bit field in the live PKRU. */
int pkey_get(int pkey) {
    if (pkey < 0 || pkey > 15) { errno = EINVAL; return -1; }
    uint32_t pkru = m3os_rdpkru();
    unsigned shift = (unsigned)pkey * 2u;
    int rights = 0;
    if (pkru & (1u << shift)) rights |= PKEY_DISABLE_ACCESS;
    if (pkru & (1u << (shift + 1u))) rights |= PKEY_DISABLE_WRITE;
    return rights;
}

/* Set the 2-bit field for `pkey` in the live PKRU from `rights`. */
int pkey_set(int pkey, unsigned int rights) {
    if (pkey < 0 || pkey > 15) { errno = EINVAL; return -1; }
    if (rights & ~(unsigned)(PKEY_DISABLE_ACCESS | PKEY_DISABLE_WRITE)) {
        errno = EINVAL; return -1;
    }
    uint32_t pkru = m3os_rdpkru();
    unsigned shift = (unsigned)pkey * 2u;
    pkru &= ~(0x3u << shift);
    if (rights & PKEY_DISABLE_ACCESS) pkru |= (1u << shift);
    if (rights & PKEY_DISABLE_WRITE) pkru |= (1u << (shift + 1u));
    m3os_wrpkru(pkru);
    return 0;
}
"#;

/// Phase 90a (D.2) — apply the three A.1 V8/Node PKU patches to `src` and stage the
/// `pkey_*` shim at `shim_rel` (relative to `src`). Idempotent: a per-step sentinel
/// guards re-application across the persistent-source reuse, so a second `build_node`
/// run (or a retry) does not double-apply or fail. Each patch fails loudly if its
/// anchor text is absent (a V8 version drift surfaces here at patch time, not as a
/// silently-broken JIT build three layers up). JIT-variant only — never invoked for
/// the default jitless build.
fn apply_node_jit_patches(src: &Path, shim_rel: &str) -> Result<(), String> {
    // ── shim TU: write it into the source tree (provenance + idempotency) ────────
    let shim_path = src.join(shim_rel);
    if let Some(parent) = shim_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("node-jit: mkdir for shim: {e}"))?;
    }
    // Always (re)write the shim to the canonical content — idempotent by value.
    fs::write(&shim_path, NODE_PKEY_SHIM_C)
        .map_err(|e| format!("node-jit: write pkey shim {}: {e}", shim_path.display()))?;
    println!("node-jit: staged pkey shim at {}", shim_path.display());

    // ── patch 2: NodePlatform::GetThreadIsolatedAllocator() override ─────────────
    // A.1 finding 2: `ThreadIsolation::Initialize()` requires
    // `platform->GetThreadIsolatedAllocator()`; the `v8::Platform` default returns
    // nullptr (`deps/v8/include/v8-platform.h:1068`) and Node's `NodePlatform`
    // never overrides it. Override it to return V8 libplatform's
    // `DefaultThreadIsolatedAllocator` (the same one `v8::platform::
    // NewDefaultPlatform` wires), so V8's PKU JIT path engages. The concrete type is
    // only reachable inside the `v8_libplatform` TU, so the override delegates to a
    // free factory injected there by `patch_node_thread_isolated_factory` (below).
    patch_node_platform_allocator(src)?;
    patch_node_thread_isolated_factory(src)?;

    // ── patch 3: KernelHasPkruFix() accepts the m3OS uname release ───────────────
    // A.1 finding 3: `KernelHasPkruFix()` parses `uname()` release and requires
    // Linux >= 5.13 (a PKRU-across-fork fix); m3OS reports `0.90.0` and would be
    // rejected. m3OS's B.4 implements correct PKRU inherit-on-clone/reset-on-exec
    // semantics (what the Linux check guards), so accept m3OS.
    patch_kernel_has_pkru_fix(src)?;

    // ── patch 4: define PKEY_DISABLE_ACCESS / PKEY_DISABLE_WRITE in the V8 TUs ────
    // THE engagement-critical patch. V8's `PkeyAlloc()`
    // (default-thread-isolated-allocator.cc:53) calls `pkey_alloc(0,
    // PKEY_DISABLE_WRITE)` ONLY inside `#ifdef PKEY_DISABLE_WRITE`; without the macro
    // it compiles to `return -1`, so V8 never gets a pkey, `ThreadIsolation` stays
    // disabled, and V8 falls back to plain `mprotect(RWX)` (which m3OS W^X v2
    // rejects). glibc's <sys/mman.h> defines these; musl's does NOT, and V8's gyp
    // build bakes its own per-TU cflags at configure time and does NOT inherit our
    // top-level `CXXFLAGS -D…` — so the `-DPKEY_DISABLE_*` we pass reaches node's own
    // sources but NEVER V8's `v8_libplatform`/`v8_base` TUs. Define them in-source.
    // (Verified empirically: the unpatched JIT binary emitted ZERO pkey_alloc
    // syscalls and used mprotect(RWX); the compile line for the allocator TU carried
    // no -DPKEY_DISABLE_WRITE.) Values match V8's `Permission` enum
    // (kDisableAccess=1, kDisableWrite=2 — memory-protection-key.h:42-44), so the
    // header's `static_assert(kDisableWrite == PKEY_DISABLE_WRITE)` holds.
    patch_pkey_disable_macros(src)?;

    Ok(())
}

/// Define `PKEY_DISABLE_ACCESS`/`PKEY_DISABLE_WRITE` in the two V8 TUs that need
/// them — musl's `<sys/mman.h>` omits them and V8's gyp build does not see our
/// `CXXFLAGS -D`. Without this, `PkeyAlloc()` is a `return -1` stub and PKU never
/// engages. Idempotent via a sentinel; fails if an anchor is absent.
fn patch_pkey_disable_macros(src: &Path) -> Result<(), String> {
    const SENTINEL: &str = "m3os-pku-jit: PKEY_DISABLE_* macros";
    let defs = format!(
        "\n// {SENTINEL}\n\
         #ifndef PKEY_DISABLE_ACCESS\n#define PKEY_DISABLE_ACCESS 0x1\n#endif\n\
         #ifndef PKEY_DISABLE_WRITE\n#define PKEY_DISABLE_WRITE 0x2\n#endif\n"
    );

    // (a) default-thread-isolated-allocator.cc — inject AFTER its <sys/mman.h>
    //     include (PkeyAlloc uses PKEY_DISABLE_WRITE further down).
    let alloc = src.join("deps/v8/src/libplatform/default-thread-isolated-allocator.cc");
    let alloc_text = fs::read_to_string(&alloc)
        .map_err(|e| format!("node-jit: read {}: {e}", alloc.display()))?;
    if !alloc_text.contains(SENTINEL) {
        let anchor = "#include <sys/mman.h>";
        if !alloc_text.contains(anchor) {
            return Err(format!(
                "node-jit: anchor `{anchor}` not found in {} — V8 layout drifted; \
                 update patch_pkey_disable_macros (A.1 finding 1)",
                alloc.display()
            ));
        }
        let patched = alloc_text.replacen(anchor, &format!("{anchor}{defs}"), 1);
        fs::write(&alloc, patched)
            .map_err(|e| format!("node-jit: write {}: {e}", alloc.display()))?;
        println!(
            "node-jit: patched {} (PKEY_DISABLE_* macros)",
            alloc.display()
        );
    }

    // (b) memory-protection-key.cc — inject BEFORE the first include so the header's
    //     `static_assert(kDisableAccess == PKEY_DISABLE_ACCESS)` (memory-protection-key.h:49)
    //     sees the macros.
    let mpk = src.join("deps/v8/src/base/platform/memory-protection-key.cc");
    let mpk_text =
        fs::read_to_string(&mpk).map_err(|e| format!("node-jit: read {}: {e}", mpk.display()))?;
    if !mpk_text.contains(SENTINEL) {
        let anchor = "#include \"src/base/platform/memory-protection-key.h\"";
        if !mpk_text.contains(anchor) {
            return Err(format!(
                "node-jit: anchor `{anchor}` not found in {} — V8 layout drifted; \
                 update patch_pkey_disable_macros (A.1 finding 1)",
                mpk.display()
            ));
        }
        let patched = mpk_text.replacen(anchor, &format!("{defs}{anchor}"), 1);
        fs::write(&mpk, patched).map_err(|e| format!("node-jit: write {}: {e}", mpk.display()))?;
        println!(
            "node-jit: patched {} (PKEY_DISABLE_* macros)",
            mpk.display()
        );
    }

    Ok(())
}

/// Patch `src/node_platform.{h,cc}` so `NodePlatform::GetThreadIsolatedAllocator()`
/// returns V8 libplatform's `DefaultThreadIsolatedAllocator`. Idempotent via a
/// sentinel; fails if the expected anchors are absent.
fn patch_node_platform_allocator(src: &Path) -> Result<(), String> {
    const SENTINEL: &str = "m3os-pku-jit: GetThreadIsolatedAllocator override";

    // -- header: declare the override inside the NodePlatform class ----------------
    let hdr = src.join("src/node_platform.h");
    let hdr_text =
        fs::read_to_string(&hdr).map_err(|e| format!("node-jit: read {}: {e}", hdr.display()))?;
    if !hdr_text.contains(SENTINEL) {
        // Anchor on a stable existing v8::Platform override in NodePlatform. Every
        // Node 18–22 NodePlatform overrides `NumberOfWorkerThreads()`; insert our
        // declaration immediately after it.
        let anchor = "int NumberOfWorkerThreads() override;";
        if !hdr_text.contains(anchor) {
            return Err(format!(
                "node-jit: anchor `{anchor}` not found in {} — V8/Node layout drifted; \
                 update patch_node_platform_allocator (A.1 finding 2)",
                hdr.display()
            ));
        }
        let inject = format!(
            "{anchor}\n  // {SENTINEL}\n  \
             v8::ThreadIsolatedAllocator* GetThreadIsolatedAllocator() override;"
        );
        let patched = hdr_text.replacen(anchor, &inject, 1);
        fs::write(&hdr, patched).map_err(|e| format!("node-jit: write {}: {e}", hdr.display()))?;
        println!("node-jit: patched {} (allocator decl)", hdr.display());
    }

    // -- impl: define the override in node_platform.cc -----------------------------
    let cc = src.join("src/node_platform.cc");
    let cc_text =
        fs::read_to_string(&cc).map_err(|e| format!("node-jit: read {}: {e}", cc.display()))?;
    if !cc_text.contains(SENTINEL) {
        // Anchor on the existing definition of NumberOfWorkerThreads(). Append our
        // definition right after its closing brace. Use the libplatform default
        // allocator factory (the same one NewDefaultPlatform installs).
        let anchor = "int NodePlatform::NumberOfWorkerThreads() {";
        if !cc_text.contains(anchor) {
            return Err(format!(
                "node-jit: anchor `{anchor}` not found in {} — V8/Node layout drifted; \
                 update patch_node_platform_allocator (A.1 finding 2)",
                cc.display()
            ));
        }
        // CRITICAL: the override definition + the factory forward-declaration MUST be
        // appended at GLOBAL (end-of-file) scope, NOT injected in-place inside the
        // `namespace node { ... }` body. Opening `namespace v8 { namespace platform {`
        // anywhere inside `namespace node` creates a *nested* `node::v8::platform`,
        // which then poisons every subsequent unqualified `v8::` lookup in the TU
        // (`no type named 'ThreadIsolatedAllocator' in namespace 'node::v8'` — a 20-error
        // cascade through the rest of node_platform.cc). So we append at file scope and
        // fully qualify every V8 name as `::v8::` to be lookup-proof regardless of the
        // enclosing-namespace context.
        //
        // The thread-isolated allocator's concrete type
        // (`v8::platform::DefaultThreadIsolatedAllocator`) lives in the V8-internal
        // header `src/libplatform/default-thread-isolated-allocator.h`, whose own
        // includes require the V8 root on the include path. node_lib compiles
        // node_platform.cc with only `deps/v8/include` (the `v8_libplatform`
        // dependent-settings export), NOT the V8 root — so that header is unreachable
        // here. Instead delegate to a free factory injected into
        // `default-thread-isolated-allocator.cc` (which IS compiled in `v8_libplatform`
        // with the V8 root and the complete type) and forward-declare it here. Only the
        // *pointer* type `::v8::ThreadIsolatedAllocator` is named here, already visible
        // via the `libplatform/libplatform.h` -> `v8-platform.h` include chain
        // (declared at v8-platform.h:622).
        //
        // The `anchor` presence check above already verified this is the NodePlatform
        // TU; we append rather than splice the method body, so we no longer depend on the
        // exact method-body text (robust across Node minor versions).
        let patched = format!(
            "{cc_text}\n\
             // {SENTINEL}\n\
             // A.1 finding 2: NodePlatform must provide the thread-isolated (PKU)\n\
             // allocator V8's ThreadIsolation::Initialize() requires; the v8::Platform\n\
             // default returns nullptr. Delegate to libplatform's default allocator —\n\
             // the same instance v8::platform::DefaultPlatform::GetThreadIsolatedAllocator\n\
             // returns — via a free factory injected into V8's\n\
             // default-thread-isolated-allocator.cc (which can see the concrete type).\n\
             // Appended at file scope (NOT inside namespace node) with fully-qualified\n\
             // ::v8:: names so no nested node::v8 namespace is ever created.\n\
             namespace v8 {{ namespace platform {{\n\
             ::v8::ThreadIsolatedAllocator* M3osDefaultThreadIsolatedAllocator();\n\
             }} }}  // namespace ::v8::platform\n\
             namespace node {{\n\
             ::v8::ThreadIsolatedAllocator* NodePlatform::GetThreadIsolatedAllocator() {{\n  \
             return ::v8::platform::M3osDefaultThreadIsolatedAllocator();\n}}\n\
             }}  // namespace node\n"
        );
        fs::write(&cc, patched).map_err(|e| format!("node-jit: write {}: {e}", cc.display()))?;
        println!("node-jit: patched {} (allocator def)", cc.display());
    }

    Ok(())
}

/// Patch `deps/v8/src/libplatform/default-thread-isolated-allocator.cc` so
/// `KernelHasPkruFix()` returns true on m3OS (A.1 finding 3). Idempotent via a
/// sentinel; fails if the function is absent.
fn patch_kernel_has_pkru_fix(src: &Path) -> Result<(), String> {
    const SENTINEL: &str = "m3os-pku-jit: KernelHasPkruFix accepts m3OS";
    let f = src.join("deps/v8/src/libplatform/default-thread-isolated-allocator.cc");
    let text =
        fs::read_to_string(&f).map_err(|e| format!("node-jit: read {}: {e}", f.display()))?;
    if text.contains(SENTINEL) {
        return Ok(());
    }
    // Inject an early `return true;` (guarded by the sentinel comment) at the top of
    // the function body. m3OS's B.4 implements the PKRU-across-fork semantics the
    // Linux >= 5.13 check guards, so the gate is satisfied. Anchor on the function
    // signature; the body opens with `{` on the same or next token.
    let anchor = "bool KernelHasPkruFix() {";
    if !text.contains(anchor) {
        return Err(format!(
            "node-jit: anchor `{anchor}` not found in {} — V8 layout drifted; update \
             patch_kernel_has_pkru_fix (A.1 finding 3)",
            f.display()
        ));
    }
    let inject = format!(
        "{anchor}\n  // {SENTINEL}\n  \
         // A.1 finding 3: m3OS reports release `0.90.0`, which the upstream\n  \
         // Linux >= 5.13 parse would reject. m3OS's Phase 90a B.4 implements the\n  \
         // PKRU inherit-on-clone / reset-on-exec semantics the kernel-version gate\n  \
         // guards, so accept unconditionally on m3OS.\n  \
         return true;"
    );
    let patched = text.replacen(anchor, &inject, 1);
    fs::write(&f, patched).map_err(|e| format!("node-jit: write {}: {e}", f.display()))?;
    println!("node-jit: patched {} (KernelHasPkruFix)", f.display());
    Ok(())
}

/// Patch `deps/v8/src/libplatform/default-thread-isolated-allocator.cc` to add the
/// free factory `v8::platform::M3osDefaultThreadIsolatedAllocator()` that the
/// patched `NodePlatform::GetThreadIsolatedAllocator()` delegates to (A.1 finding
/// 2). This TU is compiled in the `v8_libplatform` target — the one place with the
/// V8 root on its include path AND the complete `DefaultThreadIsolatedAllocator`
/// type — so it can construct the allocator; node_platform.cc cannot (it only sees
/// `deps/v8/include`). The body mirrors `DefaultPlatform::GetThreadIsolatedAllocator`
/// (default-platform.cc:285) exactly: a function-local static allocator, returned
/// only when `Valid()`. Idempotent via a sentinel; fails if the namespace-close
/// anchor is absent.
fn patch_node_thread_isolated_factory(src: &Path) -> Result<(), String> {
    const SENTINEL: &str = "m3os-pku-jit: M3osDefaultThreadIsolatedAllocator factory";
    let f = src.join("deps/v8/src/libplatform/default-thread-isolated-allocator.cc");
    let text =
        fs::read_to_string(&f).map_err(|e| format!("node-jit: read {}: {e}", f.display()))?;
    if text.contains(SENTINEL) {
        return Ok(());
    }
    // Inject the factory just before the close of the `v8::platform` namespace, so it
    // sees `DefaultThreadIsolatedAllocator` (defined just above) and is itself in
    // `v8::platform`. Anchor on that namespace-close marker.
    let anchor = "}  // namespace v8::platform";
    if !text.contains(anchor) {
        return Err(format!(
            "node-jit: anchor `{anchor}` not found in {} — V8 layout drifted; update \
             patch_node_thread_isolated_factory (A.1 finding 2)",
            f.display()
        ));
    }
    // Mirror DefaultPlatform::GetThreadIsolatedAllocator() (default-platform.cc:285):
    // a function-local static avoids touching any class layout, and `.Valid()` gates
    // the non-PKU build to nullptr (the `&alloc` is a `ThreadIsolatedAllocator*` since
    // DefaultThreadIsolatedAllocator publicly derives ThreadIsolatedAllocator).
    let inject = format!(
        "// {SENTINEL}\n\
         // A.1 finding 2: free factory the patched NodePlatform::\n\
         // GetThreadIsolatedAllocator() delegates to. Mirrors\n\
         // DefaultPlatform::GetThreadIsolatedAllocator (default-platform.cc:285).\n\
         v8::ThreadIsolatedAllocator* M3osDefaultThreadIsolatedAllocator() {{\n  \
         static DefaultThreadIsolatedAllocator alloc;\n  \
         return alloc.Valid() ? &alloc : nullptr;\n\
         }}\n\n\
         {anchor}"
    );
    let patched = text.replacen(anchor, &inject, 1);
    fs::write(&f, patched).map_err(|e| format!("node-jit: write {}: {e}", f.display()))?;
    println!(
        "node-jit: patched {} (thread-isolated allocator factory)",
        f.display()
    );
    Ok(())
}

/// Phase 90a (D.2) — compile the staged `pkey_*` shim (at `shim_rel` under `src`)
/// to an object with the cross clang + the node TARGET cflags, returning the
/// object path to append to LDFLAGS. JIT-variant only. The compile is cheap (one
/// tiny TU) and re-run each build — idempotent by overwrite, and it picks up any
/// cflags change.
fn compile_node_pkey_shim(
    src: &Path,
    shim_rel: &str,
    clang: &str,
    cflags: &str,
) -> Result<PathBuf, String> {
    let shim_c = src.join(shim_rel);
    let obj = src.join(format!("{shim_rel}.o"));
    // Split the joined cflags into args (whitespace-separated; the node cflags carry
    // no embedded spaces in any single token).
    let mut cmd = Command::new(clang);
    for tok in cflags.split_whitespace() {
        cmd.arg(tok);
    }
    // CRITICAL: compile as C++ (`-x c++`), so the shim's `pkey_*` definitions get
    // C++ linkage and mangle to the SAME names V8's weak refs use
    // (`_Z10pkey_allocjj`, `_Z9pkey_freei`, `_Z13pkey_mprotectPvmii`,
    // `_Z8pkey_geti`, `_Z8pkey_setij`). V8 declares these weak functions in C++
    // TUs without `extern "C"`, and musl's `<sys/mman.h>` does not declare them,
    // so the call sites reference the mangled symbols — a C-linkage shim would
    // NOT bind them (the `.cc` extension alone would suffice, but `-x c++` is
    // explicit since `cflags` here are the C-target flags). The shim uses only C
    // headers, which are valid in C++. `-fno-exceptions`/`-fno-rtti` keep it
    // free of any libstdc++/libc++ dependency on the link line.
    cmd.arg("-x")
        .arg("c++")
        .arg("-fno-exceptions")
        .arg("-fno-rtti");
    cmd.arg("-c").arg(&shim_c).arg("-o").arg(&obj);
    run(&mut cmd, "node-jit pkey shim compile")?;
    println!("node-jit: compiled pkey shim object {}", obj.display());
    Ok(obj)
}

/// Assert the staged `node` is a fully-static ELF (no `PT_INTERP`) — the
/// loaderless contract (m3OS's `ld-musl` has no real `libc.so`), and that the
/// bundled `npm` was staged. Mirrors how `build_python`/`build_go` prove
/// `-static`.
fn assert_node_layout(stage: &Path) -> Result<(), String> {
    let node_bin = stage.join("usr/bin/node");
    if !node_bin.exists() {
        return Err(format!(
            "node: staged binary missing at {}",
            node_bin.display()
        ));
    }
    // Fail-closed static-link check using the same byte-scan guard the
    // python/dropbear ports use: a fully-static binary must NOT reference the
    // dynamic loader. `binary_contains` propagates Err on a read failure (it
    // never silently passes), so a missing/unreadable binary fails the gate.
    if binary_contains(&node_bin, b"/lib/ld-musl-x86_64.so.1")? {
        return Err("node: staged `node` references the dynamic loader \
                    `/lib/ld-musl-x86_64.so.1` — not fully static (m3OS's ld-musl \
                    has no real `libc.so`). Check --fully-static."
            .into());
    }
    // Belt-and-suspenders: when `readelf` is present, also reject a PT_INTERP
    // segment — but only trust its output on a clean (zero) exit, otherwise a
    // failed `readelf` would vacuously "pass" the INTERP check.
    if probe_tool_on_path("readelf") {
        let out = Command::new("readelf")
            .args(["-l"])
            .arg(&node_bin)
            .output()
            .map_err(|e| format!("node: readelf failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "node: `readelf -l {}` exited non-zero ({}) — cannot verify the \
                 static-link contract: {}",
                node_bin.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            ));
        }
        if String::from_utf8_lossy(&out.stdout).contains("INTERP") {
            return Err("node: staged `node` has a PT_INTERP segment — not fully \
                        static (m3OS has no ld-musl `libc.so`). Check --fully-static."
                .into());
        }
    }
    if !stage.join("usr/bin/npm").exists() {
        return Err("node: bundled `npm` missing under usr/bin (do not pass \
                    --without-npm)."
            .into());
    }
    Ok(())
}

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
    // The cross-build links the runtimes + clang with `-fuse-ld=lld`, so the
    // host clang needs LLVM's linker on PATH. On Debian the clang package bundles
    // it; Arch ships it as a separate `lld` package.
    if !probe_tool_on_path("ld.lld") {
        return Err(
            "llvm build: `ld.lld` not found on PATH — the cross-build links with \
             `-fuse-ld=lld`. Install LLVM's linker (Debian/Ubuntu: `apt install lld`; \
             Arch: `pacman -S lld`)."
                .into(),
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
            // The musl sysroot has no C++ stdlib yet (libc++ is what THIS stage
            // builds), so cmake's default link-based compiler ABI check fails
            // pulling `-lstdc++` (clang defaults to libstdc++ on Linux — notably
            // on Arch). Make the checks compile-only (build a static lib, no
            // link) — the canonical LLVM-runtimes cross-build setting.
            .arg("-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY")
            .arg("-DLLVM_ENABLE_RUNTIMES=libcxx;libcxxabi;libunwind;compiler-rt")
            .arg("-DLIBCXX_ENABLE_SHARED=OFF")
            .arg("-DLIBCXXABI_ENABLE_SHARED=OFF")
            .arg("-DLIBUNWIND_ENABLE_SHARED=OFF")
            .arg("-DLIBCXX_ENABLE_STATIC=ON")
            .arg("-DLIBCXXABI_ENABLE_STATIC=ON")
            .arg("-DLIBUNWIND_ENABLE_STATIC=ON")
            .arg("-DLIBCXX_HAS_MUSL_LIBC=ON")
            // musl does NOT provide glibc's `__cxa_thread_atexit_impl`, and with
            // STATIC_LIBRARY try-compiles the auto-detection can't link-probe for
            // it, so state it explicitly: libc++abi uses its own pthread-key
            // thread_local-dtor fallback instead of the (absent) libc symbol —
            // otherwise the final `lld` link fails `undefined: __cxa_thread_atexit_impl`.
            .arg("-DLIBCXXABI_HAS_CXA_THREAD_ATEXIT_IMPL=OFF")
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
        // Search the clang executable's OWN directory for default config files
        // (`clang.cfg` / `clang++.cfg`). A relative value is resolved against the
        // binary dir, so this is relocation-safe (the resource-dir contract). The
        // staged config files (assemble_llvm_stage) force `-static` so a bare
        // `clang hello.c` produces a runnable static binary on m3OS.
        .arg("-DCLANG_CONFIG_FILE_SYSTEM_DIR=.")
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
    // musl headers/libs live at distro-specific paths:
    //   Debian/Ubuntu (`musl-dev`): /usr/include/x86_64-linux-musl + /usr/lib/x86_64-linux-musl
    //   Arch (`musl`):              /usr/lib/musl/include          + /usr/lib/musl/lib
    let (musl_inc, musl_lib) = [
        (
            "/usr/include/x86_64-linux-musl",
            "/usr/lib/x86_64-linux-musl",
        ),
        ("/usr/lib/musl/include", "/usr/lib/musl/lib"),
    ]
    .into_iter()
    .find(|(inc, lib)| {
        Path::new(inc).join("stdio.h").exists() && Path::new(lib).join("libc.a").exists()
    })
    .ok_or_else(|| {
        "llvm build: musl headers/libs not found (Debian/Ubuntu: `apt install musl-dev`; \
         Arch: `pacman -S musl`). Checked /usr/include/x86_64-linux-musl + \
         /usr/lib/x86_64-linux-musl and /usr/lib/musl/{include,lib}."
            .to_string()
    })?;
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
        &format!("{musl_inc}/."),
        inc.to_str().unwrap(),
        "musl headers",
    )?;
    cp_a(&format!("{musl_lib}/."), lib.to_str().unwrap(), "musl libs")?;
    // Linux UAPI headers — symlink the host kernel uapi dirs into the sysroot.
    // Debian's `musl-dev` ships NO UAPI, so the symlink provides it. Arch's
    // `musl` package BUNDLES the UAPI (`linux/`, `asm/`, `asm-generic/`), so the
    // `cp_a` above already copied them — in that case the dir is present and we
    // leave it (symlinking over a populated dir fails with EEXIST).
    let asm_dir = linux_uapi_arch_include().join("asm");
    for (target, name) in [
        ("/usr/include/linux", "linux"),
        ("/usr/include/asm-generic", "asm-generic"),
        (asm_dir.to_str().unwrap_or("/usr/include/asm"), "asm"),
    ] {
        let link = inc.join(name);
        if link.exists() {
            // Already provided (Arch musl bundles the UAPI) — keep it.
            continue;
        }
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

    // Static-by-default config files. m3OS runs ONLY static binaries — its
    // `ld-musl` is a custom loader with no real `libc.so`, so a dynamic (PIE)
    // executable faults at startup with `DT_NEEDED not found: libc.so`. clang
    // defaults to a dynamic/PIE link for `*-linux-musl`, so a bare `clang
    // hello.c` would produce an unrunnable binary. These config files (loaded
    // because the build bakes `-DCLANG_CONFIG_FILE_SYSTEM_DIR=.` → clang searches
    // its own bin dir for `<mode>.cfg`) prepend `-static` so the FLAGLESS driver
    // produces a runnable static ELF — the in-design "bare clang works on m3OS"
    // contract. `-static` is a no-op for compile-only / query invocations.
    let cfg_body = "# m3OS default: link statically. m3OS has no real libc.so /\n\
                    # dynamic C runtime, so a dynamic executable cannot run. See\n\
                    # ports/lang/llvm/Portfile + assemble_llvm_stage.\n\
                    -static\n";
    for cfg in ["clang.cfg", "clang++.cfg"] {
        fs::write(bin.join(cfg), cfg_body).map_err(|e| format!("write {cfg}: {e}"))?;
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
    // Phase 88 follow-up — the bare `clang` invocation above passes NO -static, so
    // a runnable output proves the staged `clang.cfg` static default took effect.
    // A dynamic output (PT_INTERP → /lib/ld-musl…) cannot run on m3OS (no real
    // libc.so), so its absence is the by-construction proof — the same guard the
    // dropbear/python ports use. This also guards the crt-strip regression: a
    // re-stripped crt1.o makes the link fail outright (caught by the compile
    // step), and an intact-but-dynamic default is caught here.
    if binary_contains(&c_out, b"/lib/ld-musl-x86_64.so.1")? {
        return Err(format!(
            "llvm validate: {} references the dynamic loader — the static-by-default \
             clang.cfg did not take effect (m3OS has no libc.so; a dynamic clang \
             output cannot run on-device)",
            c_out.display()
        ));
    }
    run(&mut Command::new(&c_out), "llvm validate: run C binary")?;

    // C++: clang++ h.cpp -o h_cpp && run (links the self-contained libc++).
    let cpp_out = tmp.join("h_cpp");
    let mut cxx = Command::new(&clangxx);
    cxx.arg(format!("--sysroot={}", hsys.display()))
        .arg(&cpp_src)
        .arg("-o")
        .arg(&cpp_out);
    run(&mut cxx, "llvm validate: clang++ compile h.cpp")?;
    if binary_contains(&cpp_out, b"/lib/ld-musl-x86_64.so.1")? {
        return Err(format!(
            "llvm validate: {} references the dynamic loader — the static-by-default \
             clang++.cfg did not take effect (m3OS has no libc.so)",
            cpp_out.display()
        ));
    }
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

/// Substring search over a file's raw bytes. Used to assert the *absence* of
/// forbidden byte sequences in a built binary: libcurl / OpenSSL API symbols
/// ([`build_git`]) and the dynamic-loader path that betrays a non-static link
/// ([`build_python`], [`build_dropbear`]).
///
/// This guard **fails closed**: a read failure returns `Err`, which the caller
/// propagates as a build error. Returning `Ok(false)` ("needle absent") on an
/// unreadable file would silently weaken the assertion — the build would pass
/// without ever proving the forbidden bytes are gone. The caller already
/// verified the binary exists, so a read failure here is a genuine anomaly worth
/// aborting on rather than skipping the check.
fn binary_contains(path: &Path, needle: &[u8]) -> Result<bool, String> {
    let bytes = fs::read(path).map_err(|e| {
        format!(
            "binary_contains: cannot read {} to verify: {e}",
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

/// Phase 85b → 86c — build the `git` toolchain and its full dependency chain
/// (`zlib → mbedtls → ca-certificates → curl → git`) into their `.m3pkg`
/// artifacts. 85b shipped a local-only git; 86c rebuilds it WITH curl, so this
/// now drives the whole HTTPS transport chain (see the body for the ordering and
/// why `ca-certificates` precedes `curl`). Separate from [`build_phase_69d_ports`]
/// so a routine image build does not pay the multi-minute cross-compile; the
/// `git-local-smoke` / `git-https-smoke` gates (and any explicit
/// `cargo xtask port build git`) drive this. A warm pkgcache makes each a
/// zero-compiler hit.
pub fn build_git_port() -> Result<(), String> {
    if musl_cc().is_none() {
        return Err(
            "no musl cross-compiler on PATH (install musl-tools or musl-gcc-cross-bin)".to_string(),
        );
    }
    // Phase 86c — git is now rebuilt WITH curl, so the full transport chain is
    // built in dependency-first order: zlib -> mbedtls -> ca-certificates ->
    // curl -> git. ca-certificates is a data-only package (download + stage, no
    // compiler) that curl lists as a runtime dep (the trust store), so its
    // `.m3pkg` must exist for the image to bundle it + the in-OS solver to install
    // it under git. A warm pkgcache makes each a zero-compiler hit.
    port_build("zlib").map_err(|e| format!("port zlib: {e}"))?;
    port_build("mbedtls").map_err(|e| format!("port mbedtls: {e}"))?;
    port_build("ca-certificates").map_err(|e| format!("port ca-certificates: {e}"))?;
    port_build("curl").map_err(|e| format!("port curl: {e}"))?;
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

/// Phase 86d Track D — build the `go` port into its `.m3pkg`. Unlike the other
/// ports it has **no musl dependency** (it cross-builds with the downloaded Go
/// toolchain, `CGO_ENABLED=0`) and **no runtime `DEPS`** (the binary is fully
/// static, so nothing else is built first). Separate from the routine ports so
/// only the `go-runtime-smoke` gate (and an explicit `cargo xtask port build
/// go`) pays the toolchain download + cross-build; a warm pkgcache makes a
/// repeat a zero-compiler hit.
pub fn build_go_port() -> Result<(), String> {
    port_build("go").map_err(|e| format!("port go: {e}"))
}

/// Phase 86e Track A — build the `gh` port (static GitHub CLI) into its
/// `.m3pkg`. Like `go` it has **no musl dependency** (it cross-builds with the
/// downloaded Go toolchain, `CGO_ENABLED=0`) and **no runtime `DEPS`** (the
/// binary is fully static). Separate from the routine ports so only the
/// `gh-smoke` gate, the opt-in `M3OS_WITH_GH` image feature, and an explicit
/// `cargo xtask port build gh` pay the toolchain download + cross-build; a warm
/// pkgcache makes a repeat a zero-compiler hit.
pub fn build_gh_port() -> Result<(), String> {
    port_build("gh").map_err(|e| format!("port gh: {e}"))
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

/// Phase 89 — build the fully-static musl `node` (+ bundled `npm`) into its
/// `.m3pkg` artifact so the image populator can bundle it into `/usr/pkg/` (gated
/// behind `M3OS_WITH_NODE`). `build_node` reuses the `llvm` port's musl libc++
/// sysroot and runs the V8/Node cross-build; a warm pkgcache makes a repeat build
/// a zero-compiler hit. The `node-smoke` gate drives this (and SKIPs with reason
/// when the host C++ toolchain or the llvm sysroot is absent).
pub fn build_node_port() -> Result<(), String> {
    port_build("node").map_err(|e| format!("port node: {e}"))?;
    Ok(())
}

/// Phase 90b — build the `claude-code` port (the pinned Claude Code npm bundle)
/// into its `.m3pkg`. A FETCH-AND-STAGE port: no compiler, no musl dependency —
/// the npm registry tarball is fetched + SHA-verified + staged + sealed. The
/// `DEPS=node` is a RUNTIME dependency resolved by the in-OS solver at
/// `pkg install` time, so the HOST build needs only the registry fetch + the
/// `node` Portfile (whose content key folds into claude-code's). The
/// `claude-smoke` gate + the opt-in `M3OS_WITH_CLAUDE` image feature drive this;
/// a warm pkgcache makes a repeat a zero-fetch hit. Callers that want the bundled
/// runtime present (the gate, the image block) build `build_node_port()` first.
pub fn build_claude_code_port() -> Result<(), String> {
    port_build("claude-code").map_err(|e| format!("port claude-code: {e}"))?;
    Ok(())
}

/// Phase 86b — build the static client-only `dropbear` (`dbclient`) into its
/// `.m3pkg` artifact so the image populator can bundle it into `/usr/pkg/` as
/// `ssh.m3pkg`. dropbear has no runtime deps (`--disable-zlib`), so nothing is
/// built first. The `git-ssh-smoke` gate (and any explicit
/// `cargo xtask port build dropbear`) drives this; a warm pkgcache makes a
/// repeat build a zero-compiler hit.
pub fn build_dropbear_port() -> Result<(), String> {
    if musl_cc().is_none() {
        return Err(
            "no musl cross-compiler on PATH (install musl-tools or musl-gcc-cross-bin)".to_string(),
        );
    }
    port_build("dropbear").map_err(|e| format!("port dropbear: {e}"))?;
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
        // Phase 86c — git now depends on zlib + curl (HTTPS transport); curl
        // transitively pulls mbedtls + ca-certificates via the solver.
        assert_eq!(port_deps("git"), &["zlib", "curl"]);
        // Phase 86c — mbedtls is a leaf; curl links the staged zlib + mbedtls and
        // needs ca-certificates (the runtime trust store) so the solver installs
        // the CA bundle when git pulls curl.
        assert_eq!(port_deps("mbedtls"), &[] as &[&str]);
        let curl_deps = port_deps("curl");
        assert!(
            curl_deps.contains(&"zlib")
                && curl_deps.contains(&"mbedtls")
                && curl_deps.contains(&"ca-certificates"),
            "curl must list zlib, mbedtls, and ca-certificates as deps"
        );
        // Phase 85c — python's one *runtime* dependency is zlib (zlib/gzip).
        // ncurses (for the statically-linked _curses/_curses_panel) is a
        // BUILD-only dep, deliberately absent here so the in-OS solver does not
        // re-install its terminfo DB; its pin is folded via build_recipe_id.
        assert_eq!(port_deps("python"), &["zlib"]);
        // Phase 86a Track C.2 — the CA bundle is a self-contained data file.
        assert_eq!(port_deps("ca-certificates"), &[] as &[&str]);
        // Phase 90b — claude-code's cli.js runs under the Node runtime (node),
        // and the launcher's NODE_EXTRA_CA_CERTS needs the ca-certificates CA
        // bundle to validate api.anthropic.com; the in-OS solver installs both
        // dependency-first before claude-code.
        assert_eq!(port_deps("claude-code"), &["node", "ca-certificates"]);
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
            "zlib",
            "ncurses",
            "libevent",
            "less",
            "htop",
            "tmux",
            "git",
            "python",
            "llvm",
            "dropbear",
            "mbedtls",
            "curl",
            "go",
            "gh",
            "node",
            "claude-code",
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

    // ── Phase 90a (D.2) — the JIT/jitless variant folds into node's key ──────

    /// `node_variant_id()` must distinguish the JIT and jitless variants by the
    /// `M3OS_NODE_JIT` env var, AND that distinction must propagate into node's
    /// computed content key — so a JIT build and a jitless build seal under
    /// DISTINCT keys (one is a pure pkgcache MISS for the other). This is the
    /// host-test proof that the key fold (`variant=jit|jitless` in
    /// `compute_port_key_inner`'s node arm) works, since the multi-hour real build
    /// cannot be run in the gate. The env var is process-global, so this test
    /// drives both states serially and restores the prior value.
    #[test]
    fn node_variant_id_distinguishes_jit_and_folds_into_content_key() {
        let prior = std::env::var("M3OS_NODE_JIT").ok();

        // jitless (env unset) → "jitless"
        unsafe {
            std::env::remove_var("M3OS_NODE_JIT");
        }
        assert_eq!(
            node_variant_id(),
            "jitless",
            "unset M3OS_NODE_JIT must select the jitless variant"
        );
        assert!(!node_jit_enabled());

        // JIT (env = "1") → "jit"
        unsafe {
            std::env::set_var("M3OS_NODE_JIT", "1");
        }
        assert_eq!(
            node_variant_id(),
            "jit",
            "M3OS_NODE_JIT=1 must select the jit variant"
        );
        assert!(node_jit_enabled());

        // A non-"1" value is NOT the JIT variant (only the exact opt-in engages it,
        // so the default jitless build stays byte-identical for any other value).
        unsafe {
            std::env::set_var("M3OS_NODE_JIT", "0");
        }
        assert_eq!(
            node_variant_id(),
            "jitless",
            "M3OS_NODE_JIT=0 must stay on the jitless variant"
        );

        // Prove the fold reaches the actual content key. node has no deps and the
        // toolchain probes return the same value in both states, so ONLY the
        // `variant=` token differs → the keys must differ.
        if let Some(port_dir) = find_port_dir("node") {
            unsafe {
                std::env::remove_var("M3OS_NODE_JIT");
            }
            let jitless_key = compute_port_key("node", &port_dir);
            unsafe {
                std::env::set_var("M3OS_NODE_JIT", "1");
            }
            let jit_key = compute_port_key("node", &port_dir);
            if let (Ok(a), Ok(b)) = (jitless_key, jit_key) {
                assert_ne!(
                    a, b,
                    "the JIT and jitless node builds must seal under DISTINCT content \
                     keys (no cross-pkgcache contamination)"
                );
            }
        }

        // Restore the prior env so parallel tests / later runs are unaffected.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("M3OS_NODE_JIT", v),
                None => std::env::remove_var("M3OS_NODE_JIT"),
            }
        }
    }

    // ── Phase 86e — go_toolchain_id folds the Go Portfile into go/gh keys ────

    /// `go_toolchain_id()` must read the committed `ports/lang/go/Portfile` and
    /// return a string of the form `"go-toolchain|<version>|<sha>"`. This keeps
    /// the content-addressed pkgcache honest for the `go` and `gh` ports: bumping
    /// `ports/lang/go/Portfile` auto-invalidates their `.m3pkg`s, and a musl-gcc
    /// change does NOT spuriously invalidate them (musl is irrelevant to a pure-Go
    /// `CGO_ENABLED=0` build). The test is hermetic — it reads the committed
    /// Portfile (workspace_root() resolves to the repo root at test time).
    #[test]
    fn go_toolchain_id_folds_go_portfile_version_and_sha() {
        let id = go_toolchain_id();
        // Must start with the well-known prefix.
        assert!(
            id.starts_with("go-toolchain|"),
            "go_toolchain_id must start with 'go-toolchain|', got: {id:?}"
        );
        // Must contain the pinned Go version from ports/lang/go/Portfile.
        assert!(
            id.contains("1.24.6"),
            "go_toolchain_id must contain the pinned version '1.24.6', got: {id:?}"
        );
        // Must contain a recognisable prefix of the pinned SHA-256.
        assert!(
            id.contains("bbca37cc"),
            "go_toolchain_id must contain the SHA-256 prefix 'bbca37cc', got: {id:?}"
        );
        // Must NOT be the unknown sentinel (the Portfile is present in the repo).
        assert_ne!(
            id, "go-toolchain|unknown",
            "go_toolchain_id must not return the sentinel when the Portfile is present"
        );
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
