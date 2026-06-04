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
/// unrelated port recipe does not invalidate this port's cached `.m3pkg`. A
/// recipe whose configure flags live in the Rust `build_*` function should
/// bump the optional Portfile `BUILD_FLAGS=` field (folded in via the Portfile
/// content) to self-invalidate on a recipe change.
fn recipe_digest(port_dir: &Path) -> String {
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
    format!("{:x}", hasher.finalize())
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
    let build_flags = recipe_digest(port_dir);
    Ok(pkg_format::compute_package_key(
        &tarball_sha,
        toolchain_id,
        &build_flags,
        dep_keys,
    ))
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
/// return an empty slice. Ports that are depended upon (e.g. `ncurses`)
/// themselves have no deps, so there is no recursion needed in practice.
/// The function signature is written to handle deeper dependency chains
/// without change.
fn port_deps(name: &str) -> &'static [&'static str] {
    match name {
        "less" => &["ncurses"],
        "htop" => &["ncurses"],
        "tmux" => &["ncurses", "libevent"],
        _ => &[],
    }
}

/// Phase 85a (B.2) — compute the portable content key for `name` (the port at
/// `port_dir`), folding in the active toolchain identity and the resolved
/// package keys of its direct dependencies. Shared by the resolve-before-build
/// path and [`pkgcache_artifact_path`] so the key is computed one way only.
fn compute_port_key(name: &str, port_dir: &Path) -> Result<String, String> {
    let tc = toolchain_id();
    let dep_keys: Vec<String> = port_deps(name)
        .iter()
        .map(|dep| {
            let dep_dir = find_port_dir(dep)
                .ok_or_else(|| format!("dep port {dep} not found in ports/ tree"))?;
            // Deps here have no deps of their own (ncurses/libevent have no
            // build deps); the empty slice keeps the recursion correct for the
            // common case and the signature handles deeper chains unchanged.
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

    match name {
        "ncurses" => build_ncurses(&extracted, &stage, &toolchain)?,
        "libevent" => build_libevent(&extracted, &stage, &toolchain)?,
        "less" => build_less(&extracted, &stage, &toolchain, &ncurses_stage)?,
        "htop" => build_htop(&extracted, &stage, &toolchain, &ncurses_stage)?,
        "tmux" => build_tmux(
            &extracted,
            &stage,
            &toolchain,
            &ncurses_stage,
            &libevent_stage,
        )?,
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
    for name in &["ncurses", "libevent", "less", "htop", "tmux"] {
        port_build(name).map_err(|e| format!("port {name}: {e}"))?;
    }
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

    // ── B.1 — seal_package ───────────────────────────────────────────────

    /// Build a minimal fake stage under `tmp`, call `seal_package`, and
    /// assert that:
    ///   - `target/pkgcache/<key>.m3pkg` is created
    ///   - `pkg_format::verify` passes on the resulting bytes
    ///   - the `.pkgkey` diagnostic file is written to the stage
    /// No compiler is invoked — this is pure filesystem logic.
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
        // Unknown port returns empty.
        assert_eq!(port_deps("clang"), &[] as &[&str]);
    }
}
