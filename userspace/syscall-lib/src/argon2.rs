//! Argon2id (RFC 9106) — memory-hard password hashing (Phase 110 Track C).
//!
//! A `no_std` (heap-using) implementation built on the BLAKE2b primitive in
//! [`crate::blake2b`]. Only the **argon2id** variant is provided — the type
//! byte is fixed at 2 and the data-independent/​dependent split (RFC 9106
//! §3.4.1.3: data-independent in the first two slices of the first pass,
//! data-dependent thereafter) is hardcoded.
//!
//! This is the replacement for the `$sha256i$` iterated-SHA-256 shadow format:
//! memory-hardness makes a stolen `/etc/shadow` expensive to crack on GPU/ASIC.
//! [`verify_password`](crate::sha256::verify_password) dispatches `$argon2id$`
//! entries here. Correctness is pinned by the RFC 9106 §5.3 test vector
//! (`crypto-lib`'s `argon2id_rfc9106_vector` host test, via the re-export).
//!
//! Architecture note: this lives in `syscall-lib`, not `crypto-lib`, because
//! `crypto-lib` depends on `syscall-lib` (for `getrandom`) — putting the hash
//! here and re-exporting it from `crypto-lib` avoids a dependency cycle while
//! keeping `verify_password`'s local dispatch free of a `crypto-lib` edge.

extern crate alloc;

use crate::blake2b::{Blake2b, blake2b};
use alloc::vec::Vec;

/// One Argon2 memory block: 1024 bytes as 128 little-endian 64-bit words.
type Block = [u64; 128];

const ZERO_BLOCK: Block = [0u64; 128];
/// argon2id type constant (RFC 9106 §3.1 `y`).
const ARGON2_TYPE_ID: u64 = 2;
/// argon2 version 1.3 (`0x13` = 19).
const ARGON2_VERSION: u32 = 0x13;
/// Pseudo-random address pairs produced per data-independent address block.
const ADDRESSES_PER_BLOCK: usize = 128;

/// Argon2id cost parameters.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// Memory size in KiB (number of 1 KiB blocks, before the `4·p` rounding).
    pub m_kib: u32,
    /// Number of passes (iterations), ≥ 1.
    pub t: u32,
    /// Parallelism (lanes), ≥ 1.
    pub p: u32,
    /// Output tag length in bytes (≥ 4).
    pub tag_len: usize,
}

/// Conservative default for m3OS login: 4 MiB, 3 passes, single lane, 32-byte
/// tag. Memory-hard (vastly stronger than iterated SHA-256) while staying well
/// inside a userspace process heap and fast enough for an interactive login
/// (~6 ms native; a few hundred ms under TCG — negligible against a QEMU boot).
pub const DEFAULT_PARAMS: Params = Params {
    m_kib: 4096,
    t: 3,
    p: 1,
    tag_len: 32,
};

// ---------------------------------------------------------------------------
// Compression function G and its permutation P (RFC 9106 §3.4, §3.5)
// ---------------------------------------------------------------------------

/// The Argon2 `fBlaMka` mixing add: `a + b + 2·lo32(a)·lo32(b)` (RFC 9106 §3.5).
#[inline(always)]
fn fbla(x: u64, y: u64) -> u64 {
    let lo = (x & 0xffff_ffff).wrapping_mul(y & 0xffff_ffff);
    x.wrapping_add(y).wrapping_add(lo.wrapping_mul(2))
}

/// BLAKE2-style mixing on four registers with the Argon2 `fBlaMka` add.
#[inline(always)]
fn gb(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize) {
    v[a] = fbla(v[a], v[b]);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = fbla(v[c], v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = fbla(v[a], v[b]);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = fbla(v[c], v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

/// The Argon2 permutation P over 16 words (RFC 9106 §3.5).
#[inline(always)]
fn p_perm(v: &mut [u64; 16]) {
    gb(v, 0, 4, 8, 12);
    gb(v, 1, 5, 9, 13);
    gb(v, 2, 6, 10, 14);
    gb(v, 3, 7, 11, 15);
    gb(v, 0, 5, 10, 15);
    gb(v, 1, 6, 11, 12);
    gb(v, 2, 7, 8, 13);
    gb(v, 3, 4, 9, 14);
}

/// The compression function `G(X, Y) = P_columns(P_rows(R)) ⊕ R` where
/// `R = X ⊕ Y` (RFC 9106 §3.5). Returns the fresh block; the caller applies the
/// pass-> 0 output-XOR itself.
fn compress(x: &Block, y: &Block) -> Block {
    let mut r = [0u64; 128];
    for i in 0..128 {
        r[i] = x[i] ^ y[i];
    }
    let mut b = r;
    // Round over the 8 rows (16 consecutive words each).
    for row in 0..8 {
        let mut t = [0u64; 16];
        t.copy_from_slice(&b[row * 16..row * 16 + 16]);
        p_perm(&mut t);
        b[row * 16..row * 16 + 16].copy_from_slice(&t);
    }
    // Round over the 8 columns (2-word registers strided by 16).
    for col in 0..8 {
        let mut t = [0u64; 16];
        for k in 0..8 {
            t[2 * k] = b[2 * col + 16 * k];
            t[2 * k + 1] = b[2 * col + 16 * k + 1];
        }
        p_perm(&mut t);
        for k in 0..8 {
            b[2 * col + 16 * k] = t[2 * k];
            b[2 * col + 16 * k + 1] = t[2 * k + 1];
        }
    }
    for i in 0..128 {
        b[i] ^= r[i];
    }
    b
}

// ---------------------------------------------------------------------------
// H0 and the variable-length hash H' (RFC 9106 §3.2, §3.3)
// ---------------------------------------------------------------------------

/// The variable-length hash `H'^T` (RFC 9106 §3.3): BLAKE2b directly for
/// `out.len() ≤ 64`, otherwise the 32-byte-chained construction.
fn h_prime(input: &[u8], out: &mut [u8]) {
    let t = out.len();
    let t_le = (t as u32).to_le_bytes();
    if t <= 64 {
        let mut h = Blake2b::new(t);
        h.update(&t_le);
        h.update(input);
        h.finalize(out);
        return;
    }
    // T > 64: V1 = BLAKE2b64(LE32(T) || input); Vi = BLAKE2b64(V(i-1));
    // take the low 32 bytes of each Vi, and a final block of length T-32r.
    let mut v = [0u8; 64];
    let mut h = Blake2b::new(64);
    h.update(&t_le);
    h.update(input);
    h.finalize(&mut v);
    out[..32].copy_from_slice(&v[..32]);
    let mut pos = 32;
    let mut remaining = t - 32;
    while remaining > 64 {
        let mut nv = [0u8; 64];
        blake2b(&v, &mut nv);
        v = nv;
        out[pos..pos + 32].copy_from_slice(&v[..32]);
        pos += 32;
        remaining -= 32;
    }
    // Final partial block: BLAKE2b with output length `remaining` (1..=64).
    let mut last = [0u8; 64];
    let mut hf = Blake2b::new(remaining);
    hf.update(&v);
    hf.finalize(&mut last[..remaining]);
    out[pos..pos + remaining].copy_from_slice(&last[..remaining]);
}

fn bytes_to_block(bytes: &[u8; 1024]) -> Block {
    let mut b = [0u64; 128];
    for (i, word) in b.iter_mut().enumerate() {
        *word = u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap());
    }
    b
}

fn block_to_bytes(block: &Block) -> [u8; 1024] {
    let mut out = [0u8; 1024];
    for (i, word) in block.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// The reference-index computation (RFC 9106 §3.4.1.2)
// ---------------------------------------------------------------------------

/// Map the pseudo-random `j1` (low 32 bits) to an absolute column in the
/// referenced lane. `pos_in_segment` is `i`, `same_lane` is whether the
/// reference lane equals the current lane.
#[allow(clippy::too_many_arguments)]
fn index_alpha(
    pass: u32,
    slice: u32,
    pos_in_segment: u32,
    j1: u32,
    same_lane: bool,
    lane_len: u32,
    seg_len: u32,
) -> u32 {
    // Reference-area size (signed intermediate: the cross-lane, index-0 cases
    // are `-1` before the segment base is added — always ≥ 0 for valid inputs).
    let ref_area: i64 = if pass == 0 {
        if slice == 0 {
            pos_in_segment as i64 - 1
        } else if same_lane {
            (slice * seg_len + pos_in_segment) as i64 - 1
        } else {
            (slice * seg_len) as i64 + if pos_in_segment == 0 { -1 } else { 0 }
        }
    } else if same_lane {
        (lane_len - seg_len + pos_in_segment) as i64 - 1
    } else {
        (lane_len - seg_len) as i64 + if pos_in_segment == 0 { -1 } else { 0 }
    };
    debug_assert!(ref_area >= 0);
    let ref_area = ref_area as u64;

    // Quadratic map of j1 into [0, ref_area) biased toward recent blocks.
    let mut rel = j1 as u64;
    rel = (rel * rel) >> 32;
    rel = (ref_area * rel) >> 32;
    let rel = ref_area - 1 - rel;

    // Segment base: pass 0 references from block 0; later passes reference from
    // the slice *after* the current one (wrapping at the last slice back to 0).
    let start = if pass == 0 || slice == 3 {
        0u64
    } else {
        ((slice + 1) * seg_len) as u64
    };

    ((start + rel) % lane_len as u64) as u32
}

// ---------------------------------------------------------------------------
// Core
// ---------------------------------------------------------------------------

/// Argon2id raw hash (RFC 9106). Writes `params.tag_len` bytes to `out`.
///
/// `secret` (key `K`) and `ad` (associated data `X`) are usually empty for
/// password hashing; they are supported so the RFC 9106 §5.3 test vector (which
/// sets both) validates the implementation. Returns `false` on an invalid
/// parameter set (out-of-range `p`/`t`/`tag_len`, or `out.len()` mismatch).
pub fn argon2id_raw(
    password: &[u8],
    salt: &[u8],
    secret: &[u8],
    ad: &[u8],
    params: &Params,
    out: &mut [u8],
) -> bool {
    if params.p == 0
        || params.t == 0
        || params.tag_len < 4
        || out.len() != params.tag_len
        || salt.len() < 8
    {
        return false;
    }
    let p = params.p;
    // m' = 4·p·floor(m / 4p), and at least 8·p blocks (RFC 9106 §3.1).
    let mut m_prime = (params.m_kib / (4 * p)) * (4 * p);
    if m_prime < 8 * p {
        m_prime = 8 * p;
    }
    let lane_len = m_prime / p; // q
    let seg_len = lane_len / 4; // q/4

    // --- H0 (RFC 9106 §3.2) ---------------------------------------------
    let mut h0_in: Vec<u8> = Vec::new();
    let push_le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    push_le32(&mut h0_in, p);
    push_le32(&mut h0_in, params.tag_len as u32);
    push_le32(&mut h0_in, params.m_kib);
    push_le32(&mut h0_in, params.t);
    push_le32(&mut h0_in, ARGON2_VERSION);
    push_le32(&mut h0_in, ARGON2_TYPE_ID as u32);
    push_le32(&mut h0_in, password.len() as u32);
    h0_in.extend_from_slice(password);
    push_le32(&mut h0_in, salt.len() as u32);
    h0_in.extend_from_slice(salt);
    push_le32(&mut h0_in, secret.len() as u32);
    h0_in.extend_from_slice(secret);
    push_le32(&mut h0_in, ad.len() as u32);
    h0_in.extend_from_slice(ad);
    let mut h0 = [0u8; 64];
    blake2b(&h0_in, &mut h0);

    // --- Memory matrix + the two initial blocks per lane ----------------
    let mut mem: Vec<Block> = alloc::vec![ZERO_BLOCK; m_prime as usize];
    let mut init_in = [0u8; 72]; // H0 || LE32(col) || LE32(lane)
    init_in[..64].copy_from_slice(&h0);
    for lane in 0..p {
        for col in 0..2u32 {
            init_in[64..68].copy_from_slice(&col.to_le_bytes());
            init_in[68..72].copy_from_slice(&lane.to_le_bytes());
            let mut block_bytes = [0u8; 1024];
            h_prime(&init_in, &mut block_bytes);
            mem[(lane * lane_len + col) as usize] = bytes_to_block(&block_bytes);
        }
    }

    // --- Fill (RFC 9106 §3.6) -------------------------------------------
    for pass in 0..params.t {
        for slice in 0..4u32 {
            for lane in 0..p {
                fill_segment(&mut mem, pass, lane, slice, lane_len, seg_len, p, params.t);
            }
        }
    }

    // --- Finalize: C = XOR of the last column across lanes, then H'^τ ----
    let mut c = mem[(lane_len - 1) as usize]; // lane 0 last column
    for lane in 1..p {
        let last = mem[(lane * lane_len + lane_len - 1) as usize];
        for i in 0..128 {
            c[i] ^= last[i];
        }
    }
    let c_bytes = block_to_bytes(&c);
    h_prime(&c_bytes, out);
    true
}

/// Fill one (pass, lane, slice) segment.
#[allow(clippy::too_many_arguments)]
fn fill_segment(
    mem: &mut [Block],
    pass: u32,
    lane: u32,
    slice: u32,
    lane_len: u32,
    seg_len: u32,
    lanes: u32,
    passes: u32,
) {
    // argon2id: data-independent addressing in the first two slices of pass 0.
    let data_indep = pass == 0 && slice < 2;

    // Address-block state for the data-independent path.
    let mut input_block = ZERO_BLOCK;
    let mut addr_block = ZERO_BLOCK;
    let mut addr_ctr = 0u64;
    if data_indep {
        input_block[0] = pass as u64;
        input_block[1] = lane as u64;
        input_block[2] = slice as u64;
        input_block[3] = (lane_len * lanes) as u64; // m'
        input_block[4] = passes as u64;
        input_block[5] = ARGON2_TYPE_ID;
    }
    let next_addresses = |ctr: &mut u64, input: &mut Block, addr: &mut Block| {
        *ctr += 1;
        input[6] = *ctr;
        *addr = compress(&ZERO_BLOCK, input);
        *addr = compress(&ZERO_BLOCK, addr);
    };

    // Pass 0, slice 0 leaves the two initial blocks in place and pre-generates
    // the first address block (its loop starts at i=2, so `i % 128 == 0` never
    // fires to generate it).
    let start_i = if pass == 0 && slice == 0 { 2 } else { 0 };
    if data_indep && pass == 0 && slice == 0 {
        next_addresses(&mut addr_ctr, &mut input_block, &mut addr_block);
    }

    for i in start_i..seg_len {
        let curr_col = slice * seg_len + i;
        let prev_col = if curr_col == 0 {
            lane_len - 1
        } else {
            curr_col - 1
        };
        let prev_off = (lane * lane_len + prev_col) as usize;

        let pseudo_rand = if data_indep {
            if (i as usize).is_multiple_of(ADDRESSES_PER_BLOCK) {
                next_addresses(&mut addr_ctr, &mut input_block, &mut addr_block);
            }
            addr_block[(i as usize) % ADDRESSES_PER_BLOCK]
        } else {
            mem[prev_off][0]
        };

        let j1 = (pseudo_rand & 0xffff_ffff) as u32;
        let j2 = (pseudo_rand >> 32) as u32;

        // Reference lane: forced to the current lane in the very first segment.
        let ref_lane = if pass == 0 && slice == 0 {
            lane
        } else {
            j2 % lanes
        };
        let same_lane = ref_lane == lane;
        let ref_col = index_alpha(pass, slice, i, j1, same_lane, lane_len, seg_len);
        let ref_off = (ref_lane * lane_len + ref_col) as usize;

        let prev = mem[prev_off];
        let refb = mem[ref_off];
        let mut new_block = compress(&prev, &refb);
        // Passes after the first XOR the new block into the existing one
        // (version 1.3 behaviour).
        if pass != 0 {
            let cur_off = (lane * lane_len + curr_col) as usize;
            for k in 0..128 {
                new_block[k] ^= mem[cur_off][k];
            }
        }
        let cur_off = (lane * lane_len + curr_col) as usize;
        mem[cur_off] = new_block;
    }
}

/// Argon2id password hash: `secret`/`ad` empty. Writes `params.tag_len` bytes.
pub fn argon2id_hash(password: &[u8], salt: &[u8], params: &Params, out: &mut [u8]) -> bool {
    argon2id_raw(password, salt, &[], &[], params, out)
}

/// Constant-time verify of `password` against `expected` (the raw tag) under
/// `params`. Returns `false` on any parameter/length mismatch.
pub fn argon2id_verify(password: &[u8], salt: &[u8], params: &Params, expected: &[u8]) -> bool {
    if expected.len() != params.tag_len {
        return false;
    }
    let mut computed = alloc::vec![0u8; params.tag_len];
    if !argon2id_hash(password, salt, params, &mut computed) {
        return false;
    }
    ct_eq(&computed, expected)
}

/// Constant-time byte comparison (no early-out on the first differing byte).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Shadow-field format: $argon2id$v=19$m=<m>,t=<t>,p=<p>$<hex_salt>$<hex_hash>
// ---------------------------------------------------------------------------
//
// Hex (not PHC base64) for the salt/hash, matching the existing `$sha256i$`
// shadow format and reusing the codebase's hex convention — there is no
// external-tool interop requirement (m3OS owns its `/etc/shadow`).

/// The canonical `/etc/shadow` prefix for argon2id entries.
pub const SHADOW_PREFIX: &[u8] = b"$argon2id$";

/// Upper bound on the memory parameter accepted from a stored hash, so a
/// malformed/hostile `/etc/shadow` entry cannot request a multi-terabyte
/// allocation (the `Vec` would abort the process). 512 MiB is far above any
/// value m3OS writes (`DEFAULT_PARAMS` = 8 MiB) yet bounded.
const MAX_VERIFY_M_KIB: u32 = 512 * 1024;

fn append(out: &mut [u8], pos: &mut usize, bytes: &[u8]) -> Option<()> {
    if *pos + bytes.len() > out.len() {
        return None;
    }
    out[*pos..*pos + bytes.len()].copy_from_slice(bytes);
    *pos += bytes.len();
    Some(())
}

fn write_dec(out: &mut [u8], pos: &mut usize, mut v: u32) -> Option<()> {
    if v == 0 {
        return append(out, pos, b"0");
    }
    let mut digits = [0u8; 10];
    let mut n = 0;
    while v > 0 {
        digits[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    for i in (0..n).rev() {
        append(out, pos, &[digits[i]])?;
    }
    Some(())
}

fn hex_encode(bytes: &[u8], out: &mut [u8], pos: &mut usize) -> Option<()> {
    const H: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        append(out, pos, &[H[(b >> 4) as usize], H[(b & 0xf) as usize]])?;
    }
    Some(())
}

fn hex_decode(hex: &[u8], out: &mut [u8]) -> Option<usize> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) || hex.len() / 2 > out.len() {
        return None;
    }
    fn nib(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    for i in 0..hex.len() / 2 {
        out[i] = (nib(hex[2 * i])? << 4) | nib(hex[2 * i + 1])?;
    }
    Some(hex.len() / 2)
}

fn parse_dec(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(v)
}

fn parse_mtp(field: &[u8]) -> Option<(u32, u32, u32)> {
    let (mut m, mut t, mut p) = (None, None, None);
    for kv in field.split(|&b| b == b',') {
        if let Some(v) = kv.strip_prefix(b"m=") {
            m = parse_dec(v);
        } else if let Some(v) = kv.strip_prefix(b"t=") {
            t = parse_dec(v);
        } else if let Some(v) = kv.strip_prefix(b"p=") {
            p = parse_dec(v);
        } else {
            return None;
        }
    }
    Some((m?, t?, p?))
}

/// Build a full `$argon2id$…` shadow field for `password`+`salt` into `out`,
/// returning its byte length (or `None` if `out` is too small / hashing fails).
/// `out` should be at least 160 bytes for `DEFAULT_PARAMS` with a 16-byte salt.
pub fn build_shadow_field(
    password: &[u8],
    salt: &[u8],
    params: &Params,
    out: &mut [u8],
) -> Option<usize> {
    let mut tag = alloc::vec![0u8; params.tag_len];
    if !argon2id_hash(password, salt, params, &mut tag) {
        return None;
    }
    let mut pos = 0usize;
    append(out, &mut pos, b"$argon2id$v=19$m=")?;
    write_dec(out, &mut pos, params.m_kib)?;
    append(out, &mut pos, b",t=")?;
    write_dec(out, &mut pos, params.t)?;
    append(out, &mut pos, b",p=")?;
    write_dec(out, &mut pos, params.p)?;
    append(out, &mut pos, b"$")?;
    hex_encode(salt, out, &mut pos)?;
    append(out, &mut pos, b"$")?;
    hex_encode(&tag, out, &mut pos)?;
    Some(pos)
}

/// Verify `password` against a full `$argon2id$v=19$m=…,t=…,p=…$salt$hash`
/// shadow field. Parses the cost parameters **from the stored entry** (so a
/// future default change never breaks old hashes) and fails closed on any
/// malformed field, an unsupported version, or an out-of-range memory value.
pub fn verify_shadow_field(password: &[u8], entry: &[u8]) -> bool {
    let mut parts = entry.split(|&b| b == b'$');
    if parts.next() != Some(&b""[..]) {
        return false; // text before the leading '$'
    }
    if parts.next() != Some(&b"argon2id"[..]) {
        return false;
    }
    // Only version 0x13 (=19) is defined.
    match parts.next() {
        Some(b"v=19") => {}
        _ => return false,
    }
    let mtp = match parts.next() {
        Some(f) => f,
        None => return false,
    };
    let salt_hex = match parts.next() {
        Some(s) => s,
        None => return false,
    };
    let hash_hex = match parts.next() {
        Some(h) => h,
        None => return false,
    };
    if parts.next().is_some() {
        return false; // trailing garbage field
    }
    let (m_kib, t, p) = match parse_mtp(mtp) {
        Some(v) => v,
        None => return false,
    };
    if m_kib > MAX_VERIFY_M_KIB || t == 0 || p == 0 {
        return false;
    }
    let mut salt = [0u8; 64];
    let salt_len = match hex_decode(salt_hex, &mut salt) {
        Some(l) => l,
        None => return false,
    };
    let mut expected = [0u8; 64];
    let hash_len = match hex_decode(hash_hex, &mut expected) {
        Some(l) => l,
        None => return false,
    };
    if salt_len < 8 || hash_len < 4 {
        return false;
    }
    let params = Params {
        m_kib,
        t,
        p,
        tag_len: hash_len,
    };
    argon2id_verify(password, &salt[..salt_len], &params, &expected[..hash_len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> alloc::string::String {
        use core::fmt::Write;
        let mut s = alloc::string::String::new();
        for b in bytes {
            write!(s, "{b:02x}").unwrap();
        }
        s
    }

    #[test]
    fn rfc9106_argon2id_vector() {
        // RFC 9106 §5.3: m=32 KiB, t=3, p=4, tag=32, with secret K[8] and AD[12].
        let password = [0x01u8; 32];
        let salt = [0x02u8; 16];
        let secret = [0x03u8; 8];
        let ad = [0x04u8; 12];
        let params = Params {
            m_kib: 32,
            t: 3,
            p: 4,
            tag_len: 32,
        };
        let mut out = [0u8; 32];
        assert!(argon2id_raw(
            &password, &salt, &secret, &ad, &params, &mut out
        ));
        assert_eq!(
            hex(&out),
            "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"
        );
    }

    #[test]
    fn hash_then_verify_roundtrips() {
        // Small params keep the test fast; the vector above pins correctness.
        let params = Params {
            m_kib: 64,
            t: 2,
            p: 1,
            tag_len: 32,
        };
        let salt = *b"a-16-byte-saltxx";
        let mut tag = [0u8; 32];
        assert!(argon2id_hash(b"correct horse", &salt, &params, &mut tag));
        assert!(argon2id_verify(b"correct horse", &salt, &params, &tag));
        assert!(!argon2id_verify(b"wrong horse", &salt, &params, &tag));
    }

    #[test]
    fn shadow_field_roundtrips() {
        let params = Params {
            m_kib: 64,
            t: 2,
            p: 1,
            tag_len: 32,
        };
        let salt = *b"sixteen-byte-slt";
        let mut buf = [0u8; 200];
        let n = build_shadow_field(b"hunter2", &salt, &params, &mut buf).unwrap();
        let entry = &buf[..n];
        // Shape check.
        assert!(entry.starts_with(b"$argon2id$v=19$m=64,t=2,p=1$"));
        // Correct password verifies; wrong password and a tampered tag do not.
        assert!(verify_shadow_field(b"hunter2", entry));
        assert!(!verify_shadow_field(b"hunter3", entry));
        let mut tampered = buf;
        tampered[n - 1] ^= 0x01;
        assert!(!verify_shadow_field(b"hunter2", &tampered[..n]));
    }

    #[test]
    fn verify_shadow_field_fails_closed_on_malformed() {
        for bad in [
            &b""[..],
            b"$argon2id$",
            b"$argon2id$v=18$m=64,t=2,p=1$aabb$ccdd", // wrong version
            b"$argon2id$v=19$m=64,t=2$aabb$ccdd",     // missing p
            b"$argon2id$v=19$m=64,t=2,p=1$xyz$ccdd",  // non-hex salt
            b"$argon2id$v=19$m=99999999,t=2,p=1$aabbccddaabbccdd$ccdd", // m over cap
            b"$sha256i$10000$aabb$ccdd",              // not argon2id
        ] {
            assert!(!verify_shadow_field(b"pw", bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_bad_params() {
        let mut out = [0u8; 32];
        // p = 0, t = 0, short salt, wrong out length all fail closed.
        assert!(!argon2id_raw(
            b"pw",
            b"saltsalt",
            &[],
            &[],
            &Params {
                m_kib: 64,
                t: 0,
                p: 1,
                tag_len: 32
            },
            &mut out
        ));
        assert!(!argon2id_raw(
            b"pw",
            b"saltsalt",
            &[],
            &[],
            &Params {
                m_kib: 64,
                t: 1,
                p: 0,
                tag_len: 32
            },
            &mut out
        ));
        assert!(!argon2id_raw(
            b"pw",
            b"short",
            &[],
            &[],
            &Params {
                m_kib: 64,
                t: 1,
                p: 1,
                tag_len: 32
            },
            &mut out
        ));
    }
}
