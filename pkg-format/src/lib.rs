//! Phase 85a — content-addressed `.m3pkg` package format + content key.
//!
//! This crate is the **single** authoritative implementation of the `.m3pkg`
//! v1 format and the package content key, shared by:
//!   - `xtask` (host packer/unpacker, Track B `seal_package`/resolve), via the
//!     default `std` feature; and
//!   - the userspace `pkg` installer (Track C), which depends on this crate
//!     with `default-features = false` for the `no_std` parse/verify surface
//!     and lays the entries down via syscalls.
//!
//! Keeping one implementation avoids the packer and the in-OS installer drifting
//! apart on the byte layout. The pure logic (key + parse + verify) is host-
//! tested here, mirroring the `kernel_core::storage` host-test pattern.
//!
//! # Hashing choice (recorded per task A.2)
//!
//! Content hashes and the content key both use **SHA-256** (FIPS 180-4). The
//! design doc names BLAKE3, but `blake3`/`zstd` are unavailable in the offline
//! build environment; the task's A.2 fallback clause permits a `.sha256`-based
//! v1 provided the choice is recorded — which it is here and in
//! `docs/roadmap/85a-package-infrastructure.md`.
//!
//! The SHA-256 is a compact self-contained pure-`u32` implementation
//! ([`sha256`]) rather than the RustCrypto `sha2` crate, because `sha2` fails to
//! codegen on the soft-float / no-SSE `x86_64-unknown-none` target the userspace
//! `pkg` installer builds for ("Do not know how to split the result of this
//! operator"). The pure implementation has zero external dependencies, builds on
//! every target, and is pinned by a known-answer test (`SHA-256("abc")`).
//!
//! # `.m3pkg` v1 byte layout (all integers little-endian)
//!
//! ```text
//! Region 0 — fixed prefix (79 bytes):
//!   [0..5]    magic        = b"M3PKG"          (5 bytes)
//!   [5]       version      = 0x01              (1 byte — the format version)
//!   [6]       sig_present  = 0x00              (1 byte — 0 = unsigned v1)
//!   [7..71]   signature    = [0u8; 64]         (64 bytes — ed25519, reserved/zeroed)
//!   [71..75]  entry_count  : u32               (4 bytes)
//!   [75..79]  index_len    : u32               (4 bytes — length of Region 1)
//!
//! Region 1 — entry index (index_len bytes), `entry_count` entries each:
//!   path_len     : u32
//!   path         : path_len bytes (UTF-8, '/'-separated relative path)
//!   mode         : u32 (full unix st_mode incl. type bits S_IFREG / S_IFLNK)
//!   content_len  : u64
//!   content_hash : 32 bytes (SHA-256 of the entry content)
//!   data_offset  : u64 (offset of the content within Region 2)
//!
//! Region 2 — data blob: entry contents concatenated in index order.
//!
//! total length = 79 + index_len + sum(content_len)
//! ```
//!
//! The ed25519 `signature` field is reserved (zeroed) in v1 so the Phase 86
//! networked repo can populate it without a format break (forward-compat).

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Compact, self-contained SHA-256 (FIPS 180-4). Pure `u32` arithmetic so it
/// codegens on the soft-float / no-SSE `x86_64-unknown-none` target where the
/// RustCrypto `sha2` crate does not. Validated by the `content_hash_matches_known_sha256`
/// known-answer test.
pub mod sha256 {
    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    /// Streaming SHA-256 state.
    pub struct Hasher {
        state: [u32; 8],
        len: u64,
        buf: [u8; 64],
        buf_len: usize,
    }

    impl Default for Hasher {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Hasher {
        /// Fresh hasher.
        pub fn new() -> Self {
            Hasher {
                state: H0,
                len: 0,
                buf: [0u8; 64],
                buf_len: 0,
            }
        }

        fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    block[i * 4],
                    block[i * 4 + 1],
                    block[i * 4 + 2],
                    block[i * 4 + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let mut h = *state;
            for i in 0..64 {
                let s1 = h[4].rotate_right(6) ^ h[4].rotate_right(11) ^ h[4].rotate_right(25);
                let ch = (h[4] & h[5]) ^ ((!h[4]) & h[6]);
                let t1 = h[7]
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = h[0].rotate_right(2) ^ h[0].rotate_right(13) ^ h[0].rotate_right(22);
                let maj = (h[0] & h[1]) ^ (h[0] & h[2]) ^ (h[1] & h[2]);
                let t2 = s0.wrapping_add(maj);
                h[7] = h[6];
                h[6] = h[5];
                h[5] = h[4];
                h[4] = h[3].wrapping_add(t1);
                h[3] = h[2];
                h[2] = h[1];
                h[1] = h[0];
                h[0] = t1.wrapping_add(t2);
            }
            for i in 0..8 {
                state[i] = state[i].wrapping_add(h[i]);
            }
        }

        /// Feed bytes into the hasher.
        pub fn update(&mut self, mut data: &[u8]) {
            self.len = self.len.wrapping_add(data.len() as u64);
            while !data.is_empty() {
                let n = core::cmp::min(64 - self.buf_len, data.len());
                self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
                self.buf_len += n;
                data = &data[n..];
                if self.buf_len == 64 {
                    let block = self.buf;
                    Self::compress(&mut self.state, &block);
                    self.buf_len = 0;
                }
            }
        }

        /// Finish and return the 32-byte digest.
        pub fn finalize(mut self) -> [u8; 32] {
            let bit_len = self.len.wrapping_mul(8);
            self.update(&[0x80]);
            while self.buf_len != 56 {
                self.update(&[0]);
            }
            self.update(&bit_len.to_be_bytes());
            debug_assert_eq!(self.buf_len, 0);
            let mut out = [0u8; 32];
            for i in 0..8 {
                out[i * 4..i * 4 + 4].copy_from_slice(&self.state[i].to_be_bytes());
            }
            out
        }
    }

    /// One-shot SHA-256.
    pub fn digest(data: &[u8]) -> [u8; 32] {
        let mut h = Hasher::new();
        h.update(data);
        h.finalize()
    }
}

/// Magic prefix (5 bytes) — followed immediately by the 1-byte format version.
pub const MAGIC: &[u8; 5] = b"M3PKG";
/// `.m3pkg` format version written into byte `[5]`.
pub const FORMAT_VERSION: u8 = 1;
/// Reserved ed25519 signature length (bytes).
pub const SIGNATURE_LEN: usize = 64;
/// Size of Region 0 (the fixed prefix).
pub const PREFIX_LEN: usize = 5 + 1 + 1 + SIGNATURE_LEN + 4 + 4; // = 79

const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;

/// Parse / verify error. `&'static str` keeps the type `no_std`-clean (no
/// allocation required to surface a failure).
pub type Error = &'static str;

/// SHA-256 of `data` as a 32-byte array. The single content-hash primitive,
/// identical on host and in `no_std`.
pub fn content_hash(data: &[u8]) -> [u8; 32] {
    sha256::digest(data)
}

/// Lowercase hex of a byte slice.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// Compute the portable content key for a package.
///
/// The key is `hex(SHA-256(domain-separated(tarball_sha, toolchain_id,
/// build_flags, sorted dep_keys)))`. Identical inputs always produce an
/// identical key; changing **any** input changes the key.
///
/// ## Inputs IN the key
/// - `tarball_sha`  — the upstream source tarball's SHA-256 (from the Portfile).
/// - `toolchain_id` — the resolved musl toolchain identity string.
/// - `build_flags`  — a recipe-identity string (e.g. the Portfile content +
///   patches digest plus the port's configure flags, composed by xtask's
///   `recipe_digest` / `package_key`).
/// - `dep_keys`     — the package keys of direct build dependencies (sorted
///   internally so caller ordering is irrelevant).
///
/// ## Inputs deliberately OUT of the key (so the cache survives moves/machines)
/// - any absolute or `target/` directory path,
/// - the `xtask`/`port_build.rs` source bytes (so editing an *unrelated* recipe
///   does not invalidate this package — the old `.stamp` folded these in, which
///   over-invalidated; recipe-specific flag changes are captured via the
///   Portfile `BUILD_FLAGS` field folded into `build_flags` instead),
/// - wall-clock time and the build host's identity.
pub fn compute_package_key(
    tarball_sha: &str,
    toolchain_id: &str,
    build_flags: &str,
    dep_keys: &[String],
) -> String {
    let mut h = sha256::Hasher::new();
    h.update(b"M3PKG-KEY-v1\0");
    h.update(b"tarball\0");
    h.update(tarball_sha.as_bytes());
    h.update(b"\0toolchain\0");
    h.update(toolchain_id.as_bytes());
    h.update(b"\0flags\0");
    h.update(build_flags.as_bytes());
    h.update(b"\0deps\0");
    let mut sorted: Vec<&String> = dep_keys.iter().collect();
    sorted.sort();
    for d in sorted {
        h.update(d.as_bytes());
        h.update(b"\0");
    }
    to_hex(&h.finalize())
}

/// A single packed entry as recorded in the index (Region 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Relative, '/'-separated path under the install prefix.
    pub path: String,
    /// Full unix `st_mode` (includes the file-type bits so symlinks survive).
    pub mode: u32,
    /// SHA-256 of the entry content.
    pub content_hash: [u8; 32],
    /// Content length in bytes.
    pub content_len: u64,
    /// Offset of the content within the data blob (Region 2).
    pub data_offset: u64,
}

impl Entry {
    /// True if this entry is a symbolic link (content is the link target).
    pub fn is_symlink(&self) -> bool {
        (self.mode & S_IFMT) == S_IFLNK
    }
    /// The unix permission bits (mode without the file-type bits).
    pub fn perm_bits(&self) -> u32 {
        self.mode & 0o7777
    }
}

/// A parsed `.m3pkg` manifest plus the offset at which the data blob begins.
#[derive(Clone, Debug)]
pub struct Manifest {
    /// Format version (byte `[5]`).
    pub version: u8,
    /// Whether the reserved signature field is populated (always `false` in v1).
    pub sig_present: bool,
    /// The reserved 64-byte ed25519 signature (zeroed in v1).
    pub signature: [u8; SIGNATURE_LEN],
    /// The entry index.
    pub entries: Vec<Entry>,
    /// Byte offset where Region 2 (the data blob) starts.
    pub data_start: usize,
}

impl Manifest {
    /// Borrow the content bytes for `entry` from the full package buffer.
    /// Returns `Err` if the recorded offset/length fall outside `bytes`.
    pub fn entry_content<'a>(&self, bytes: &'a [u8], entry: &Entry) -> Result<&'a [u8], Error> {
        let start = self
            .data_start
            .checked_add(entry.data_offset as usize)
            .ok_or("entry offset overflow")?;
        let end = start
            .checked_add(entry.content_len as usize)
            .ok_or("entry length overflow")?;
        bytes.get(start..end).ok_or("entry content out of bounds")
    }
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let s = bytes.get(at..at + 4).ok_or("truncated u32")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64, Error> {
    let s = bytes.get(at..at + 8).ok_or("truncated u64")?;
    Ok(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

/// Parse the header (Regions 0 + 1) of a `.m3pkg` buffer. Does **not** hash the
/// data blob — call [`verify`] for content integrity.
pub fn parse(bytes: &[u8]) -> Result<Manifest, Error> {
    if bytes.len() < PREFIX_LEN {
        return Err("buffer shorter than prefix");
    }
    if &bytes[0..5] != MAGIC {
        return Err("bad magic");
    }
    let version = bytes[5];
    if version != FORMAT_VERSION {
        return Err("unsupported format version");
    }
    let sig_present = bytes[6] != 0;
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(&bytes[7..7 + SIGNATURE_LEN]);
    let entry_count = read_u32(bytes, 71)? as usize;
    let index_len = read_u32(bytes, 75)? as usize;

    let index_start = PREFIX_LEN;
    let data_start = index_start
        .checked_add(index_len)
        .ok_or("index length overflow")?;
    if data_start > bytes.len() {
        return Err("index extends past buffer");
    }
    let index = &bytes[index_start..data_start];

    // Reject an `entry_count` that cannot possibly fit in the index region
    // *before* reserving for it. A corrupt/hostile count (up to u32::MAX) must
    // surface as a parse error, not an unbounded `Vec::with_capacity` — which
    // `panic = "abort"` turns into an installer abort in the no_std `pkg`
    // binary. The minimum bytes per index entry is fixed at 56 (path_len u32 +
    // mode u32 + content_len u64 + content_hash 32 + data_offset u64), plus a
    // zero-length path; so at most `index_len / 56` entries can be encoded.
    const MIN_ENTRY_BYTES: usize = 4 + 4 + 8 + 32 + 8;
    if entry_count > index.len() / MIN_ENTRY_BYTES {
        return Err("entry_count exceeds index capacity");
    }

    let mut entries = Vec::with_capacity(entry_count);
    let mut cur = 0usize;
    for _ in 0..entry_count {
        let path_len = read_u32(index, cur)? as usize;
        cur += 4;
        let path_bytes = index.get(cur..cur + path_len).ok_or("truncated path")?;
        let path_str = core::str::from_utf8(path_bytes).map_err(|_| "path not utf-8")?;
        // Path-traversal containment: every entry path MUST be relative and
        // free of `..` components, so neither the host `unpack` nor the in-OS
        // installer can be steered to write outside the install prefix by a
        // malformed/hostile artifact. (The packer only ever emits prefix-
        // relative `usr/...` paths, so no legitimate artifact is rejected.)
        if path_str.is_empty()
            || path_str.starts_with('/')
            || path_str.split('/').any(|c| c == "..")
        {
            return Err("unsafe entry path (absolute or contains '..')");
        }
        let path = path_str.into();
        cur += path_len;
        let mode = read_u32(index, cur)?;
        cur += 4;
        let content_len = read_u64(index, cur)?;
        cur += 8;
        let hash_bytes = index.get(cur..cur + 32).ok_or("truncated hash")?;
        let mut content_hash = [0u8; 32];
        content_hash.copy_from_slice(hash_bytes);
        cur += 32;
        let data_offset = read_u64(index, cur)?;
        cur += 8;
        entries.push(Entry {
            path,
            mode,
            content_hash,
            content_len,
            data_offset,
        });
    }
    if cur != index.len() {
        return Err("trailing bytes in entry index");
    }
    Ok(Manifest {
        version,
        sig_present,
        signature,
        entries,
        data_start,
    })
}

/// Verify a `.m3pkg` buffer end-to-end: header well-formed, every entry's
/// content in-bounds, and every recorded SHA-256 matches the actual bytes.
/// Returns `false` on any inconsistency (a single flipped byte fails).
pub fn verify(bytes: &[u8]) -> bool {
    let manifest = match parse(bytes) {
        Ok(m) => m,
        Err(_) => return false,
    };
    for entry in &manifest.entries {
        let content = match manifest.entry_content(bytes, entry) {
            Ok(c) => c,
            Err(_) => return false,
        };
        if content.len() as u64 != entry.content_len {
            return false;
        }
        if content_hash(content) != entry.content_hash {
            return false;
        }
    }
    true
}

/// An owned entry plus its content, used by [`serialize`] (and `std` `pack`).
pub struct OwnedEntry {
    /// Relative, '/'-separated path under the install prefix.
    pub path: String,
    /// Full unix `st_mode` (file-type bits included).
    pub mode: u32,
    /// The raw entry content (file bytes, or symlink target bytes).
    pub content: Vec<u8>,
}

/// Serialize a set of entries into a `.m3pkg` v1 byte buffer. Entries are
/// emitted in the order given; callers wanting a deterministic artifact should
/// sort by `path` first ([`pack`] does this).
pub fn serialize(entries: &[OwnedEntry]) -> Vec<u8> {
    // Build the index and the data blob together so offsets line up.
    let mut index = Vec::new();
    let mut data = Vec::new();
    for e in entries {
        let offset = data.len() as u64;
        let hash = content_hash(&e.content);
        index.extend_from_slice(&(e.path.len() as u32).to_le_bytes());
        index.extend_from_slice(e.path.as_bytes());
        index.extend_from_slice(&e.mode.to_le_bytes());
        index.extend_from_slice(&(e.content.len() as u64).to_le_bytes());
        index.extend_from_slice(&hash);
        index.extend_from_slice(&offset.to_le_bytes());
        data.extend_from_slice(&e.content);
    }

    let mut out = Vec::with_capacity(PREFIX_LEN + index.len() + data.len());
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.push(0); // sig_present = 0 (unsigned v1)
    out.extend_from_slice(&[0u8; SIGNATURE_LEN]); // reserved signature
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    out.extend_from_slice(&(index.len() as u32).to_le_bytes());
    out.extend_from_slice(&index);
    out.extend_from_slice(&data);
    out
}

#[cfg(feature = "std")]
mod host {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    fn collect(dir: &Path, base: &Path, out: &mut Vec<OwnedEntry>) -> Result<(), String> {
        let rd = fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
        for ent in rd {
            let ent = ent.map_err(|e| format!("dir entry: {e}"))?;
            let path = ent.path();
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("lstat {}: {e}", path.display()))?;
            let ft = meta.file_type();
            if ft.is_dir() {
                collect(&path, base, out)?;
            } else if ft.is_symlink() {
                let target = fs::read_link(&path)
                    .map_err(|e| format!("readlink {}: {e}", path.display()))?;
                let rel = path
                    .strip_prefix(base)
                    .map_err(|_| "strip_prefix failed".to_string())?
                    .to_string_lossy()
                    .into_owned();
                out.push(OwnedEntry {
                    path: rel,
                    mode: meta.mode(),
                    content: target.to_string_lossy().into_owned().into_bytes(),
                });
            } else if ft.is_file() {
                let content =
                    fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
                let rel = path
                    .strip_prefix(base)
                    .map_err(|_| "strip_prefix failed".to_string())?
                    .to_string_lossy()
                    .into_owned();
                out.push(OwnedEntry {
                    path: rel,
                    mode: meta.mode(),
                    content,
                });
            }
            // Other node types (fifo/socket/device) are not packaged.
        }
        Ok(())
    }

    /// Pack a DESTDIR-staged tree into a deterministic `.m3pkg` v1 buffer.
    /// Regular files and symlinks are captured with their unix mode; empty
    /// directories are not stored (they are recreated implicitly on unpack).
    pub fn pack(stage_dir: &Path) -> Result<Vec<u8>, String> {
        let mut entries = Vec::new();
        if stage_dir.is_dir() {
            collect(stage_dir, stage_dir, &mut entries)?;
        } else {
            return Err(format!("stage dir {} does not exist", stage_dir.display()));
        }
        // Deterministic ordering — byte-identical artifact across runs/machines.
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(serialize(&entries))
    }

    /// Unpack a `.m3pkg` buffer under `dest`, creating parent directories,
    /// restoring file modes and symlinks, and verifying every content hash.
    /// Returns the number of entries written.
    pub fn unpack(bytes: &[u8], dest: &Path) -> Result<usize, String> {
        let manifest = parse(bytes).map_err(|e| format!("parse: {e}"))?;
        for entry in &manifest.entries {
            let content = manifest
                .entry_content(bytes, entry)
                .map_err(|e| format!("content {}: {e}", entry.path))?;
            if content_hash(content) != entry.content_hash {
                return Err(format!("hash mismatch for {}", entry.path));
            }
            let out_path = dest.join(&entry.path);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
            }
            // Replace any pre-existing node at the target.
            let _ = fs::remove_file(&out_path);
            if entry.is_symlink() {
                let target = std::str::from_utf8(content)
                    .map_err(|_| format!("symlink target not utf-8: {}", entry.path))?;
                std::os::unix::fs::symlink(target, &out_path)
                    .map_err(|e| format!("symlink {}: {e}", out_path.display()))?;
            } else {
                fs::write(&out_path, content)
                    .map_err(|e| format!("write {}: {e}", out_path.display()))?;
                fs::set_permissions(&out_path, fs::Permissions::from_mode(entry.perm_bits()))
                    .map_err(|e| format!("chmod {}: {e}", out_path.display()))?;
            }
        }
        Ok(manifest.entries.len())
    }
}

#[cfg(feature = "std")]
pub use host::{pack, unpack};

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    // ── A.1 — content key ────────────────────────────────────────────────

    #[test]
    fn key_is_stable_for_identical_inputs() {
        let deps = vec!["depA".to_string(), "depB".to_string()];
        let a = compute_package_key("abc123", "musl-1.2", "flags=x", &deps);
        let b = compute_package_key("abc123", "musl-1.2", "flags=x", &deps);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64); // SHA-256 hex
    }

    #[test]
    fn key_dep_order_does_not_matter() {
        let a = compute_package_key("sha", "tc", "f", &["d1".to_string(), "d2".to_string()]);
        let b = compute_package_key("sha", "tc", "f", &["d2".to_string(), "d1".to_string()]);
        assert_eq!(a, b, "dep_keys are sorted internally");
    }

    #[test]
    fn changing_any_input_changes_the_key() {
        let base = compute_package_key("sha", "tc", "flags", &["d".to_string()]);
        assert_ne!(
            base,
            compute_package_key("SHA", "tc", "flags", &["d".to_string()])
        );
        assert_ne!(
            base,
            compute_package_key("sha", "TC", "flags", &["d".to_string()])
        );
        assert_ne!(
            base,
            compute_package_key("sha", "tc", "FLAGS", &["d".to_string()])
        );
        assert_ne!(
            base,
            compute_package_key("sha", "tc", "flags", &["D".to_string()])
        );
        assert_ne!(base, compute_package_key("sha", "tc", "flags", &[]));
    }

    #[test]
    fn key_field_separation_prevents_collisions() {
        // Without domain separators these would collide.
        let a = compute_package_key("ab", "c", "", &[]);
        let b = compute_package_key("a", "bc", "", &[]);
        assert_ne!(a, b);
    }

    // ── A.2 — pack / unpack / verify ─────────────────────────────────────

    fn staged_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("usr/local/bin")).unwrap();
        fs::create_dir_all(root.join("usr/local/lib")).unwrap();
        // executable
        fs::write(root.join("usr/local/bin/tput"), b"#!/bin/sh\necho tput\n").unwrap();
        fs::set_permissions(
            root.join("usr/local/bin/tput"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        // data file, different perms
        fs::write(root.join("usr/local/lib/libncurses.a"), vec![0xABu8; 4096]).unwrap();
        fs::set_permissions(
            root.join("usr/local/lib/libncurses.a"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        // symlink (terminfo-alias style)
        std::os::unix::fs::symlink("tput", root.join("usr/local/bin/clear")).unwrap();
        dir
    }

    #[test]
    fn pack_unpack_round_trips_bytes_and_modes() {
        let src = staged_tree();
        let bytes = pack(src.path()).unwrap();
        assert!(verify(&bytes), "freshly packed artifact must verify");

        let dst = tempfile::tempdir().unwrap();
        let n = unpack(&bytes, dst.path()).unwrap();
        assert_eq!(n, 3, "two files + one symlink");

        // Byte-for-byte content.
        assert_eq!(
            fs::read(src.path().join("usr/local/bin/tput")).unwrap(),
            fs::read(dst.path().join("usr/local/bin/tput")).unwrap()
        );
        assert_eq!(
            fs::read(src.path().join("usr/local/lib/libncurses.a")).unwrap(),
            fs::read(dst.path().join("usr/local/lib/libncurses.a")).unwrap()
        );
        // Modes preserved.
        let tput_mode = fs::metadata(dst.path().join("usr/local/bin/tput"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(tput_mode, 0o755);
        let lib_mode = fs::metadata(dst.path().join("usr/local/lib/libncurses.a"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(lib_mode, 0o644);
        // Symlink restored as a symlink pointing at the same target.
        let link_meta = fs::symlink_metadata(dst.path().join("usr/local/bin/clear")).unwrap();
        assert!(link_meta.file_type().is_symlink());
        assert_eq!(
            fs::read_link(dst.path().join("usr/local/bin/clear")).unwrap(),
            std::path::PathBuf::from("tput")
        );
    }

    #[test]
    fn pack_is_deterministic() {
        let src = staged_tree();
        let a = pack(src.path()).unwrap();
        let b = pack(src.path()).unwrap();
        assert_eq!(a, b, "byte-identical artifact across packs");
    }

    #[test]
    fn verify_detects_a_flipped_content_byte() {
        let src = staged_tree();
        let mut bytes = pack(src.path()).unwrap();
        assert!(verify(&bytes));
        // Flip a byte in the data blob (last byte is in the libncurses content).
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert!(!verify(&bytes), "a flipped content byte must fail verify");
    }

    #[test]
    fn verify_rejects_bad_magic_and_truncation() {
        let src = staged_tree();
        let good = pack(src.path()).unwrap();
        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert!(!verify(&bad_magic));
        assert!(!verify(&good[..PREFIX_LEN - 1]), "truncated prefix");
        assert!(!verify(&[]));
    }

    #[test]
    fn header_reserves_zeroed_signature_field() {
        let src = staged_tree();
        let bytes = pack(src.path()).unwrap();
        let manifest = parse(&bytes).unwrap();
        assert_eq!(manifest.version, FORMAT_VERSION);
        assert!(!manifest.sig_present, "v1 is unsigned");
        assert_eq!(
            manifest.signature, [0u8; SIGNATURE_LEN],
            "reserved + zeroed"
        );
    }

    #[test]
    fn empty_directories_are_dropped_but_files_survive() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("empty/nested")).unwrap();
        fs::write(dir.path().join("file"), b"x").unwrap();
        let bytes = pack(dir.path()).unwrap();
        let m = parse(&bytes).unwrap();
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].path, "file");
    }
}

#[cfg(test)]
mod nostd_logic_tests {
    // Pure-logic tests that do not need the filesystem — these would also pass
    // under a no_std harness. They run as part of `cargo test -p pkg-format`.
    use super::*;

    #[test]
    fn to_hex_roundtrips_known_values() {
        assert_eq!(to_hex(&[0x00, 0xff, 0x10]), "00ff10");
        assert_eq!(to_hex(&[]), "");
    }

    #[test]
    fn content_hash_matches_known_sha256() {
        // SHA-256("abc")
        let h = content_hash(b"abc");
        assert_eq!(
            to_hex(&h),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // SHA-256("") — empty input.
        assert_eq!(
            to_hex(&content_hash(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn content_hash_handles_multi_block_input() {
        // 1,000,000 'a' bytes — the FIPS long-message test vector, exercises
        // many compression blocks + padding across the final block boundary.
        let data = alloc::vec![b'a'; 1_000_000];
        assert_eq!(
            to_hex(&content_hash(&data)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        // 56-byte input — forces an extra padding block (worst-case boundary,
        // since 0x80 + 8-byte length cannot fit after 56 message bytes).
        // hashlib.sha256(b'x'*56).hexdigest()
        assert_eq!(
            to_hex(&content_hash(&[b'x'; 56])),
            "04c26261370ee7541549d16dee320c723e3fd14671e66a099afe0a377c16888e"
        );
    }

    #[test]
    fn parse_rejects_path_traversal_and_absolute_paths() {
        // A hostile artifact whose entry path escapes the install prefix must
        // be rejected at parse time (so unpack/install can never write OOB).
        for evil in ["../etc/passwd", "/etc/passwd", "usr/../../etc/x", ".."] {
            let bytes = serialize(&[OwnedEntry {
                path: evil.into(),
                mode: 0o100644,
                content: b"x".to_vec(),
            }]);
            assert!(
                parse(&bytes).is_err(),
                "parse must reject unsafe path {evil:?}"
            );
            assert!(!verify(&bytes), "verify must reject unsafe path {evil:?}");
        }
    }

    #[test]
    fn parse_rejects_oversized_entry_count_without_huge_alloc() {
        // Craft a valid one-entry artifact, then corrupt entry_count (bytes
        // [71..75]) to u32::MAX. parse must Err on the capacity check rather
        // than reserving a multi-GB Vec (which would abort the no_std installer).
        let mut bytes = serialize(&[OwnedEntry {
            path: "file".into(),
            mode: 0o100644,
            content: b"x".to_vec(),
        }]);
        bytes[71..75].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&bytes).is_err());
        assert!(!verify(&bytes));
    }

    #[test]
    fn serialize_then_parse_roundtrips_entries() {
        let entries = vec![
            OwnedEntry {
                path: "bin/a".into(),
                mode: 0o100755,
                content: b"hello".to_vec(),
            },
            OwnedEntry {
                path: "lib/b".into(),
                mode: 0o100644,
                content: vec![1, 2, 3, 4],
            },
        ];
        let bytes = serialize(&entries);
        assert!(verify(&bytes));
        let m = parse(&bytes).unwrap();
        assert_eq!(m.entries.len(), 2);
        assert_eq!(m.entries[0].path, "bin/a");
        assert_eq!(m.entries[0].mode, 0o100755);
        assert_eq!(m.entry_content(&bytes, &m.entries[0]).unwrap(), b"hello");
        assert_eq!(
            m.entry_content(&bytes, &m.entries[1]).unwrap(),
            &[1, 2, 3, 4]
        );
    }
}
