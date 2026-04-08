//! Minimal SHA-256 implementation for password hashing (Phase 27).
//!
//! No external dependencies. Implements FIPS 180-4 SHA-256.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H_INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

fn ch(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (!x & z)
}

fn maj(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (x & z) ^ (y & z)
}

fn bsig0(x: u32) -> u32 {
    x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)
}

fn bsig1(x: u32) -> u32 {
    x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)
}

fn ssig0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}

fn ssig1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
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
        w[i] = ssig1(w[i - 2])
            .wrapping_add(w[i - 7])
            .wrapping_add(ssig0(w[i - 15]))
            .wrapping_add(w[i - 16]);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;

    for i in 0..64 {
        let t1 = h
            .wrapping_add(bsig1(e))
            .wrapping_add(ch(e, f, g))
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let t2 = bsig0(a).wrapping_add(maj(a, b, c));
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// Compute SHA-256 hash of input data.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state = H_INIT;
    let bit_len = (data.len() as u64) * 8;

    // Process complete 64-byte blocks.
    let mut offset = 0;
    while offset + 64 <= data.len() {
        let block: &[u8; 64] = data[offset..offset + 64].try_into().unwrap();
        compress(&mut state, block);
        offset += 64;
    }

    // Final block(s) with padding.
    let remaining = data.len() - offset;
    let mut buf = [0u8; 128]; // at most 2 blocks
    buf[..remaining].copy_from_slice(&data[offset..]);
    buf[remaining] = 0x80;

    let blocks = if remaining < 56 { 1 } else { 2 };
    let len_offset = blocks * 64 - 8;
    buf[len_offset..len_offset + 8].copy_from_slice(&bit_len.to_be_bytes());

    for i in 0..blocks {
        let block: &[u8; 64] = buf[i * 64..(i + 1) * 64].try_into().unwrap();
        compress(&mut state, block);
    }

    let mut result = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        result[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    result
}

/// Hash a password with salt: SHA-256(salt || password).
pub fn hash_password(password: &[u8], salt: &[u8]) -> [u8; 32] {
    // Concatenate salt + password into a buffer.
    let total = salt.len() + password.len();
    let mut buf = [0u8; 512];
    if total > buf.len() {
        // Truncate if too long (shouldn't happen in practice).
        return sha256(password);
    }
    buf[..salt.len()].copy_from_slice(salt);
    buf[salt.len()..total].copy_from_slice(password);
    sha256(&buf[..total])
}

/// Convert bytes to hex string (lowercase), writing into the provided buffer.
/// Returns the number of hex characters written (2 * data.len()).
pub fn to_hex(data: &[u8], out: &mut [u8]) -> usize {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let n = data.len().min(out.len() / 2);
    for i in 0..n {
        out[i * 2] = HEX[(data[i] >> 4) as usize];
        out[i * 2 + 1] = HEX[(data[i] & 0x0f) as usize];
    }
    n * 2
}

/// Parse hex string into bytes. Returns number of bytes parsed.
pub fn from_hex(hex: &[u8], out: &mut [u8]) -> usize {
    let pairs = hex.len() / 2;
    let n = pairs.min(out.len());
    for i in 0..n {
        let hi = hex_digit(hex[i * 2]);
        let lo = hex_digit(hex[i * 2 + 1]);
        out[i] = (hi << 4) | lo;
    }
    n
}

fn hex_digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

/// Hash a password with salt using iterated SHA-256 (multiple rounds).
///
/// Format: $sha256i$<rounds>$<hex_salt>$<hex_hash>
/// Round 1 computes SHA-256(salt || password).
/// Rounds 2..N compute SHA-256(previous_hash || salt || password).
pub fn hash_password_iterated(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut hash = hash_password(password, salt);
    for _ in 1..rounds {
        // SHA-256(hash || salt || password)
        let total = 32 + salt.len() + password.len();
        let mut buf = [0u8; 512];
        if total > buf.len() {
            return hash; // safety cap
        }
        buf[..32].copy_from_slice(&hash);
        buf[32..32 + salt.len()].copy_from_slice(salt);
        buf[32 + salt.len()..total].copy_from_slice(password);
        hash = sha256(&buf[..total]);
    }
    hash
}

/// Parse a decimal u32 from bytes. Rejects non-digit characters and caps
/// the result at 100,000 to prevent extreme CPU usage during verification.
fn parse_u32_bytes(s: &[u8]) -> u32 {
    const MAX_ROUNDS: u32 = 100_000;
    let mut n: u32 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return 0; // reject malformed input
        }
        n = match n
            .checked_mul(10)
            .and_then(|v| v.checked_add((b - b'0') as u32))
        {
            Some(v) if v <= MAX_ROUNDS => v,
            _ => return 0, // overflow or exceeds cap
        };
    }
    n
}

/// Verify a password against a shadow entry.
///
/// Supports two formats:
/// - Legacy: `$sha256$<hex_salt>$<hex_hash>`
/// - Iterated: `$sha256i$<rounds>$<hex_salt>$<hex_hash>`
///
/// Uses constant-time comparison to prevent timing attacks.
pub fn verify_password(password: &[u8], shadow_entry: &[u8]) -> bool {
    if shadow_entry.starts_with(b"$sha256i$") {
        // Iterated format: $sha256i$<rounds>$<hex_salt>$<hex_hash>
        let rest = &shadow_entry[9..]; // skip "$sha256i$"
        let first_dollar = match rest.iter().position(|&b| b == b'$') {
            Some(p) => p,
            None => return false,
        };
        let rounds_str = &rest[..first_dollar];
        let rounds = parse_u32_bytes(rounds_str);
        if rounds == 0 {
            return false;
        }
        let rest2 = &rest[first_dollar + 1..];
        let second_dollar = match rest2.iter().position(|&b| b == b'$') {
            Some(p) => p,
            None => return false,
        };
        let hex_salt = &rest2[..second_dollar];
        let hex_hash = &rest2[second_dollar + 1..];

        let mut parsed_salt = [0u8; 32];
        let salt_len = from_hex(hex_salt, &mut parsed_salt);

        let computed = hash_password_iterated(password, &parsed_salt[..salt_len], rounds);

        let mut computed_hex = [0u8; 64];
        to_hex(&computed, &mut computed_hex);

        // Constant-time comparison
        if hex_hash.len() != 64 {
            return false;
        }
        let mut diff = 0u8;
        for i in 0..64 {
            diff |= computed_hex[i] ^ hex_hash[i];
        }
        diff == 0
    } else if shadow_entry.starts_with(b"$sha256$") {
        // Legacy format (unchanged)
        let rest = &shadow_entry[8..];
        let dollar_pos = match rest.iter().position(|&b| b == b'$') {
            Some(p) => p,
            None => return false,
        };
        let hex_salt = &rest[..dollar_pos];
        let hex_hash = &rest[dollar_pos + 1..];

        let mut parsed_salt = [0u8; 32];
        let salt_len = from_hex(hex_salt, &mut parsed_salt);

        let computed = hash_password(password, &parsed_salt[..salt_len]);

        let mut computed_hex = [0u8; 64];
        to_hex(&computed, &mut computed_hex);

        if computed_hex.len() < hex_hash.len() || hex_hash.len() != 64 {
            return false;
        }
        let mut diff = 0u8;
        for i in 0..64 {
            diff |= computed_hex[i] ^ hex_hash[i];
        }
        diff == 0
    } else {
        false
    }
}
