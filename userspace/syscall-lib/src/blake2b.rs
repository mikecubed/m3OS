//! BLAKE2b (RFC 7693) — the core hash primitive of Argon2 (Phase 110 Track C).
//!
//! A `no_std`, dependency-free implementation supporting keyed hashing and
//! variable output length (1..=64 bytes). Argon2's `H` and the variable-length
//! `H'` hash (RFC 9106 §3.2, §3.3) are built on this. Kept small and literal
//! (no SIMD, no unrolled `G`) — the OS login path hashes a handful of times, so
//! clarity beats throughput.
//!
//! Validated by the RFC 7693 Appendix A "abc" vector and, transitively, by the
//! RFC 9106 Argon2id test vector in `crypto-lib`.

/// BLAKE2b initialization vector (RFC 7693 §2.6 — the SHA-512 IV).
const IV: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];

/// Message word permutation schedule σ (RFC 7693 §2.7). 12 rounds; BLAKE2b
/// reuses rounds 0..9 for rounds 10 and 11.
const SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

/// Streaming BLAKE2b state.
pub struct Blake2b {
    h: [u64; 8],
    /// Total bytes compressed so far (the low counter; the high counter is
    /// unused — Argon2 never hashes ≥ 2^64 bytes).
    t: u128,
    buf: [u8; 128],
    buf_len: usize,
    out_len: usize,
}

#[inline(always)]
fn g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

impl Blake2b {
    /// New unkeyed BLAKE2b producing `out_len` (1..=64) output bytes.
    pub fn new(out_len: usize) -> Self {
        Self::with_key(out_len, &[])
    }

    /// New (optionally keyed) BLAKE2b producing `out_len` (1..=64) output bytes.
    /// `key` may be up to 64 bytes; an empty key is the unkeyed hash.
    pub fn with_key(out_len: usize, key: &[u8]) -> Self {
        debug_assert!((1..=64).contains(&out_len));
        debug_assert!(key.len() <= 64);
        let mut h = IV;
        // Parameter block XOR (RFC 7693 §2.5): digest_length | (key_length << 8)
        // | (fanout=1 << 16) | (depth=1 << 24). Sequential mode, no salt/personal.
        h[0] ^= 0x0101_0000 ^ ((key.len() as u64) << 8) ^ (out_len as u64);
        let mut state = Blake2b {
            h,
            t: 0,
            buf: [0u8; 128],
            buf_len: 0,
            out_len,
        };
        // A keyed hash absorbs one zero-padded block of key material first.
        if !key.is_empty() {
            let mut block = [0u8; 128];
            block[..key.len()].copy_from_slice(key);
            state.update(&block);
        }
        state
    }

    fn compress(&mut self, block: &[u8; 128], last: bool) {
        let mut m = [0u64; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u64::from_le_bytes(block[i * 8..i * 8 + 8].try_into().unwrap());
        }
        let mut v = [0u64; 16];
        v[..8].copy_from_slice(&self.h);
        v[8..].copy_from_slice(&IV);
        v[12] ^= self.t as u64;
        v[13] ^= (self.t >> 64) as u64;
        if last {
            v[14] = !v[14];
        }
        for s in SIGMA.iter() {
            g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
            g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
            g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
            g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
            g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
            g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
            g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
            g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
        }
        for i in 0..8 {
            self.h[i] ^= v[i] ^ v[i + 8];
        }
    }

    /// Absorb `data`. A full 128-byte block is only compressed once it is known
    /// **not** to be the final block (BLAKE2b flags the last block specially),
    /// so a block is held back until more data or `finalize` arrives.
    pub fn update(&mut self, mut data: &[u8]) {
        if data.is_empty() {
            return;
        }
        // Fill and flush the pending buffer first, but only when strictly more
        // data follows (keep at least one byte buffered for the last-block flag).
        if self.buf_len == 128 {
            self.t += 128;
            let block = self.buf;
            self.compress(&block, false);
            self.buf_len = 0;
        }
        if self.buf_len > 0 {
            let take = core::cmp::min(128 - self.buf_len, data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if data.is_empty() {
                return;
            }
            // buf is now full and more data follows → safe to compress.
            self.t += 128;
            let block = self.buf;
            self.compress(&block, false);
            self.buf_len = 0;
        }
        // Compress full blocks while strictly more than one block remains.
        while data.len() > 128 {
            let mut block = [0u8; 128];
            block.copy_from_slice(&data[..128]);
            self.t += 128;
            self.compress(&block, false);
            data = &data[128..];
        }
        // Buffer the tail (1..=128 bytes) for finalize / the next update.
        self.buf[..data.len()].copy_from_slice(data);
        self.buf_len = data.len();
    }

    /// Finalize into `out` (must be exactly `out_len` bytes).
    pub fn finalize(mut self, out: &mut [u8]) {
        debug_assert_eq!(out.len(), self.out_len);
        self.t += self.buf_len as u128;
        let mut block = [0u8; 128];
        block[..self.buf_len].copy_from_slice(&self.buf[..self.buf_len]);
        self.compress(&block, true);
        let mut digest = [0u8; 64];
        for (i, word) in self.h.iter().enumerate() {
            digest[i * 8..i * 8 + 8].copy_from_slice(&word.to_le_bytes());
        }
        out.copy_from_slice(&digest[..self.out_len]);
    }
}

/// One-shot unkeyed BLAKE2b into a caller-sized buffer (1..=64 bytes).
pub fn blake2b(data: &[u8], out: &mut [u8]) {
    let mut h = Blake2b::new(out.len());
    h.update(data);
    h.finalize(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> std::string::String {
        use std::fmt::Write;
        let mut s = std::string::String::new();
        for b in bytes {
            write!(s, "{b:02x}").unwrap();
        }
        s
    }

    #[test]
    fn rfc7693_abc_vector() {
        // RFC 7693 Appendix A: BLAKE2b-512 of "abc".
        let mut out = [0u8; 64];
        blake2b(b"abc", &mut out);
        assert_eq!(
            hex(&out),
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1\
             7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
        );
    }

    #[test]
    fn empty_input_512() {
        // Known BLAKE2b-512 of the empty string.
        let mut out = [0u8; 64];
        blake2b(b"", &mut out);
        assert_eq!(
            hex(&out),
            "786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419\
             d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce"
        );
    }

    #[test]
    fn short_output_len() {
        // BLAKE2b-256 (out_len=32) of "abc" — first 32 bytes differ from the
        // 512 digest because out_len is folded into the parameter block.
        let mut out = [0u8; 32];
        blake2b(b"abc", &mut out);
        assert_eq!(
            hex(&out),
            "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319"
        );
    }

    #[test]
    fn streaming_matches_oneshot() {
        // Feeding the input in awkward chunks must equal the one-shot hash,
        // exercising the buffer-carry + last-block-flag logic.
        let data: std::vec::Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
        let mut oneshot = [0u8; 64];
        blake2b(&data, &mut oneshot);
        for chunk in [1usize, 63, 64, 65, 127, 128, 129] {
            let mut h = Blake2b::new(64);
            for piece in data.chunks(chunk) {
                h.update(piece);
            }
            let mut streamed = [0u8; 64];
            h.finalize(&mut streamed);
            assert_eq!(streamed, oneshot, "chunk size {chunk}");
        }
    }

    #[test]
    fn keyed_hash() {
        // RFC 7693 keyed test: key = 00..3f (64 bytes), input = empty.
        let key: std::vec::Vec<u8> = (0..64u8).collect();
        let mut h = Blake2b::with_key(64, &key);
        h.update(b"");
        let mut out = [0u8; 64];
        h.finalize(&mut out);
        assert_eq!(
            hex(&out),
            "10ebb67700b1868efb4417987acf4690ae9d972fb7a590c2f02871799aaa4786\
             b5e996e8f0f4eb981fc214b005f42d2ff4233499391653df7aefcbc13fc51568"
        );
    }
}
