//! SHA-1 (FIPS 180-4) and HMAC-SHA-1 — vendored, pure `no_std`.
//!
//! SHA-1 is cryptographically broken for collision-resistance but remains
//! required for WPA2-PSK (PBKDF2-HMAC-SHA1, PRF-SHA1).  No `sha1` crate
//! dependency is introduced; the algorithm is ~80 lines and fully deterministic.

// ── SHA-1 constants ──────────────────────────────────────────────────────────

const H0: u32 = 0x6745_2301;
const H1: u32 = 0xEFCD_AB89;
const H2: u32 = 0x98BA_DCFE;
const H3: u32 = 0x1032_5476;
const H4: u32 = 0xC3D2_E1F0;

const K0: u32 = 0x5A82_7999; // rounds  0-19
const K1: u32 = 0x6ED9_EBA1; // rounds 20-39
const K2: u32 = 0x8F1B_BCDC; // rounds 40-59
const K3: u32 = 0xCA62_C1D6; // rounds 60-79

// ── incremental state ────────────────────────────────────────────────────────

/// Incremental SHA-1 hasher.  Mirrors `Sha256Hasher` in style.
pub struct Sha1State {
    h: [u32; 5],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64, // total message bytes
}

impl Sha1State {
    /// Create a new, empty SHA-1 state.
    pub fn new() -> Self {
        Self {
            h: [H0, H1, H2, H3, H4],
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    /// Feed more bytes into the hasher.
    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);

        // Fill the internal buffer first.
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                compress(&mut self.h, &block);
                self.buf_len = 0;
            }
        }

        // Process full blocks directly from `data`.
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            compress(&mut self.h, block.try_into().unwrap());
            data = rest;
        }

        // Stash the remainder.
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    /// Produce the 20-byte digest, consuming `self`.
    pub fn finalize(mut self) -> [u8; 20] {
        // Padding: 0x80 byte, zeroes, then 64-bit big-endian bit-length.
        let bit_len: u64 = self.total_len.wrapping_mul(8);

        // Write the 0x80 pad byte.
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;

        // If there is not enough room for the 8-byte length, flush and start a
        // new block.
        if self.buf_len > 56 {
            // Zero the rest of this block.
            for b in &mut self.buf[self.buf_len..] {
                *b = 0;
            }
            let block = self.buf;
            compress(&mut self.h, &block);
            self.buf = [0u8; 64];
            self.buf_len = 0;
        }

        // Zero the gap between the pad byte and the length field.
        for b in &mut self.buf[self.buf_len..56] {
            *b = 0;
        }

        // Append bit-length as big-endian u64.
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        compress(&mut self.h, &block);

        // Serialise the five 32-bit words as big-endian.
        let mut out = [0u8; 20];
        for (i, &word) in self.h.iter().enumerate() {
            out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

impl Default for Sha1State {
    fn default() -> Self {
        Self::new()
    }
}

// ── single-shot helper ────────────────────────────────────────────────────────

/// Compute SHA-1 of `data`.  Returns a 20-byte digest.
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut s = Sha1State::new();
    s.update(data);
    s.finalize()
}

// ── SHA-1 block compression ──────────────────────────────────────────────────

#[inline(always)]
fn compress(h: &mut [u32; 5], block: &[u8; 64]) {
    // Expand block into 80 words.
    let mut w = [0u32; 80];
    for (i, chunk) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes(chunk.try_into().unwrap());
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }

    let [mut a, mut b, mut c, mut d, mut e] = *h;

    for (i, &wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | ((!b) & d), K0),
            20..=39 => (b ^ c ^ d, K1),
            40..=59 => ((b & c) | (b & d) | (c & d), K2),
            _ => (b ^ c ^ d, K3),
        };
        let temp = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }

    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
}

// ── HMAC-SHA-1 ───────────────────────────────────────────────────────────────

const SHA1_BLOCK: usize = 64;

/// Compute HMAC-SHA-1 (RFC 2104).
///
/// Keys longer than 64 bytes are first hashed with SHA-1.
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut state = HmacSha1State::new(key);
    state.update(data);
    state.finalize()
}

/// Incremental HMAC-SHA-1.  Mirrors `HmacSha256State` in style.
pub struct HmacSha1State {
    inner: Sha1State,
    opad_key: [u8; SHA1_BLOCK],
}

impl HmacSha1State {
    /// Create a new HMAC-SHA-1 state for the given `key`.
    pub fn new(key: &[u8]) -> Self {
        // If key > block size, compress it first.
        let mut k_block = [0u8; SHA1_BLOCK];
        if key.len() > SHA1_BLOCK {
            let h = sha1(key);
            k_block[..20].copy_from_slice(&h);
        } else {
            k_block[..key.len()].copy_from_slice(key);
        }

        // Compute ipad and opad.
        let mut ipad = [0u8; SHA1_BLOCK];
        let mut opad = [0u8; SHA1_BLOCK];
        for i in 0..SHA1_BLOCK {
            ipad[i] = k_block[i] ^ 0x36;
            opad[i] = k_block[i] ^ 0x5c;
        }

        // Start inner hash: SHA-1(ipad || ...)
        let mut inner = Sha1State::new();
        inner.update(&ipad);

        Self {
            inner,
            opad_key: opad,
        }
    }

    /// Feed more bytes.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Produce the 20-byte HMAC, consuming `self`.
    pub fn finalize(self) -> [u8; 20] {
        // inner_hash = SHA-1(ipad || message)
        let inner_hash = self.inner.finalize();

        // outer = SHA-1(opad || inner_hash)
        let mut outer = Sha1State::new();
        outer.update(&self.opad_key);
        outer.update(&inner_hash);
        outer.finalize()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to convert a lowercase hex string to a byte array.
    fn hex_to_bytes<const N: usize>(s: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0]);
            let lo = hex_nibble(chunk[1]);
            out[i] = (hi << 4) | lo;
        }
        out
    }

    fn hex_nibble(b: u8) -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => panic!("bad hex nibble"),
        }
    }

    #[test]
    fn sha1_kat() {
        // FIPS 180-4 / SHA-1 known-answer tests.

        // Empty string — FIPS 180 example
        assert_eq!(
            sha1(b""),
            hex_to_bytes::<20>("da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            "sha1(empty)"
        );

        // "abc" — FIPS 180-4 §B.1
        assert_eq!(
            sha1(b"abc"),
            hex_to_bytes::<20>("a9993e364706816aba3e25717850c26c9cd0d89d"),
            "sha1(abc)"
        );

        // 56-byte message — FIPS 180-4 §B.2 (crosses two compression blocks)
        assert_eq!(
            sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            hex_to_bytes::<20>("84983e441c3bd26ebaae4aa1f95129e5e54670f1"),
            "sha1(448-bit)"
        );

        // 128-byte message — exercises the `while data.len() >= 64` multi-full-
        // block compression loop in `update()` directly with real data (not just
        // transitively via HMAC/PBKDF2). Reference value from Python hashlib.
        assert_eq!(
            sha1(&[b'a'; 128]),
            hex_to_bytes::<20>("ad5b3fdbcb526778c2839d2f151ea753995e26a0"),
            "sha1(128*'a')"
        );
    }

    #[test]
    fn hmac_sha1_rfc2202() {
        // RFC 2202 Test Case 1 — key = 20×0x0b, data = "Hi There"
        let key1 = [0x0bu8; 20];
        assert_eq!(
            hmac_sha1(&key1, b"Hi There"),
            hex_to_bytes::<20>("b617318655057264e28bc0b6fb378c8ef146be00"),
            "RFC 2202 TC1"
        );

        // RFC 2202 Test Case 2 — key = "Jefe", data = "what do ya want for nothing?"
        assert_eq!(
            hmac_sha1(b"Jefe", b"what do ya want for nothing?"),
            hex_to_bytes::<20>("effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"),
            "RFC 2202 TC2"
        );

        // RFC 2202 Test Case 6 — 80-byte 0xaa key (> 64-byte block) exercises
        // the `key.len() > SHA1_BLOCK` compression branch in HmacSha1State::new,
        // which no other vector reaches (PBKDF2/WPA keys are all < 64 bytes).
        let big_key = [0xaau8; 80];
        assert_eq!(
            hmac_sha1(
                &big_key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            hex_to_bytes::<20>("aa4ae5e15272d00e95705637ce8a3b55ed402112"),
            "RFC 2202 TC6"
        );

        // RFC 2202 Test Case 7 — same 80-byte key, multi-block data.
        assert_eq!(
            hmac_sha1(
                &big_key,
                b"Test Using Larger Than Block-Size Key and Larger Than One Block-Size Data"
            ),
            hex_to_bytes::<20>("e8e99d0f45237d786d6bbaa7965c7808bbff1a91"),
            "RFC 2202 TC7"
        );
    }

    #[test]
    fn hmac_sha1_incremental() {
        let mut state = HmacSha1State::new(b"Jefe");
        state.update(b"what do ya want ");
        state.update(b"for nothing?");
        assert_eq!(
            state.finalize(),
            hmac_sha1(b"Jefe", b"what do ya want for nothing?"),
            "incremental == single-shot"
        );
    }
}
