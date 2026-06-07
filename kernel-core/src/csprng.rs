//! ChaCha20-based CSPRNG (Deterministic Random Bit Generator) for m3OS.
//!
//! ## Design
//!
//! - **`EntropyPool`**: accumulates credited bits; gates the `READY` transition
//!   at `POOL_READY_BITS = 256` credited bits.
//! - **`ChaChaDrbg`**: 32-byte ChaCha20 key + 64-bit counter + 64-bit nonce.
//!   Pure-integer ARX (no SIMD/XMM) — safe in ring 0 / interrupt context.
//!   Implements **fast-key-erasure**: after each `fill` the key is overwritten
//!   with the next keystream block so a captured post-draw state cannot
//!   reproduce prior output.
//! - **Global accessor**: a `spin::Mutex<ChaChaDrbg>` protected singleton with
//!   free-function wrappers for use from the kernel without explicit locking.
//!
//! ## Phase 86a — legacy `prng.rs` removed
//!
//! The legacy `kernel_core::prng::Prng` (xorshift64-multiply) had its last
//! caller removed in Phase 86a — `/dev/urandom` and `/dev/random` now source
//! this ChaCha20 DRBG, and `getrandom` / `AT_RANDOM` / the TCP ISN already did.
//! With no remaining callers the `prng` module was deleted from the tree, so no
//! kernel path can fall back to a non-cryptographic PRNG.

#[cfg(not(feature = "std"))]
use core::fmt;
#[cfg(feature = "std")]
use std::fmt;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by [`ChaChaDrbg`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrbgError {
    /// Output requested before the DRBG has accumulated ≥256 credited bits.
    NotReady,
}

impl fmt::Display for DrbgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DrbgError::NotReady => f.write_str("DRBG not ready: insufficient credited entropy"),
        }
    }
}

// ---------------------------------------------------------------------------
// Entropy pool
// ---------------------------------------------------------------------------

/// Minimum credited bits required before the DRBG transitions to `READY`.
pub const POOL_READY_BITS: usize = 256;

/// Accumulates credited entropy bits and gates the `READY` transition.
#[derive(Debug, Default, Clone)]
pub struct EntropyPool {
    credited_bits: usize,
}

impl EntropyPool {
    /// Create an empty entropy pool.
    pub const fn new() -> Self {
        EntropyPool { credited_bits: 0 }
    }

    /// Credit `bits` of entropy into the pool.
    pub fn credit(&mut self, bits: usize) {
        self.credited_bits = self.credited_bits.saturating_add(bits);
    }

    /// Returns `true` once ≥`POOL_READY_BITS` credited bits have been added.
    pub fn is_ready(&self) -> bool {
        self.credited_bits >= POOL_READY_BITS
    }

    /// Total credited bits so far.
    pub fn credited_bits(&self) -> usize {
        self.credited_bits
    }
}

// ---------------------------------------------------------------------------
// DRBG state enum
// ---------------------------------------------------------------------------

/// State of the [`ChaChaDrbg`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrbgState {
    /// No entropy has been mixed in yet.
    Empty,
    /// Some entropy has been mixed in but <256 credited bits.
    Early,
    /// ≥256 credited bits mixed in; secure output available.
    Ready,
}

// ---------------------------------------------------------------------------
// ChaCha20 block function (pure-integer ARX, 20 rounds, no SIMD)
// ---------------------------------------------------------------------------

/// ChaCha20 quarter-round ARX on a `[u32; 16]` working state by index.
///
/// Operating on word indices rather than `&mut u32` references avoids the
/// borrow-checker limitation that prevents taking multiple mutable references
/// into the same array simultaneously.
#[inline(always)]
fn quarter_round(ws: &mut [u32; 16], ai: usize, bi: usize, ci: usize, di: usize) {
    ws[ai] = ws[ai].wrapping_add(ws[bi]);
    ws[di] ^= ws[ai];
    ws[di] = ws[di].rotate_left(16);
    ws[ci] = ws[ci].wrapping_add(ws[di]);
    ws[bi] ^= ws[ci];
    ws[bi] = ws[bi].rotate_left(12);
    ws[ai] = ws[ai].wrapping_add(ws[bi]);
    ws[di] ^= ws[ai];
    ws[di] = ws[di].rotate_left(8);
    ws[ci] = ws[ci].wrapping_add(ws[di]);
    ws[bi] ^= ws[ci];
    ws[bi] = ws[bi].rotate_left(7);
}

/// Produce a 64-byte ChaCha20 keystream block.
///
/// `key`     — 32 bytes (8 × u32 little-endian)
/// `counter` — 64-bit block counter
/// `nonce`   — 64-bit nonce (we use the upper 64 bits of the standard 96-bit
///             nonce field, leaving the lower 32 bits as 0; this keeps the
///             counter space to 2^64 blocks = 1 ZiB which is more than enough)
fn chacha20_block(key: &[u8; 32], counter: u64, nonce: u64, out: &mut [u8; 64]) {
    // ChaCha20 initial state (IETF, RFC 7539):
    // words 0–3:  constant  "expa nd 3 2-by te k"
    // words 4–11: key (256 bit)
    // word  12:   counter low 32 bits
    // word  13:   counter high 32 bits (we extend to 64-bit counter)
    // words 14–15: nonce (96 bit split; we put our 64-bit nonce here)
    let k = key;
    let initial: [u32; 16] = [
        0x6170_7865u32,
        0x3320_646e,
        0x7962_2d32,
        0x6b20_6574,
        // key words
        u32::from_le_bytes([k[0], k[1], k[2], k[3]]),
        u32::from_le_bytes([k[4], k[5], k[6], k[7]]),
        u32::from_le_bytes([k[8], k[9], k[10], k[11]]),
        u32::from_le_bytes([k[12], k[13], k[14], k[15]]),
        u32::from_le_bytes([k[16], k[17], k[18], k[19]]),
        u32::from_le_bytes([k[20], k[21], k[22], k[23]]),
        u32::from_le_bytes([k[24], k[25], k[26], k[27]]),
        u32::from_le_bytes([k[28], k[29], k[30], k[31]]),
        // counter (64-bit, split across words 12 and 13)
        counter as u32,
        (counter >> 32) as u32,
        // nonce (64-bit, split across words 14 and 15)
        nonce as u32,
        (nonce >> 32) as u32,
    ];

    let mut ws = initial;

    for _ in 0..10 {
        // Column rounds
        quarter_round(&mut ws, 0, 4, 8, 12);
        quarter_round(&mut ws, 1, 5, 9, 13);
        quarter_round(&mut ws, 2, 6, 10, 14);
        quarter_round(&mut ws, 3, 7, 11, 15);
        // Diagonal rounds
        quarter_round(&mut ws, 0, 5, 10, 15);
        quarter_round(&mut ws, 1, 6, 11, 12);
        quarter_round(&mut ws, 2, 7, 8, 13);
        quarter_round(&mut ws, 3, 4, 9, 14);
    }

    // Add initial state
    for i in 0..16 {
        ws[i] = ws[i].wrapping_add(initial[i]);
    }

    // Serialise to little-endian bytes
    for (i, word) in ws.iter().enumerate() {
        let b = word.to_le_bytes();
        out[i * 4..i * 4 + 4].copy_from_slice(&b);
    }
}

// ---------------------------------------------------------------------------
// ChaChaDrbg
// ---------------------------------------------------------------------------

/// Output ceiling (bytes) before `needs_reseed` fires.
pub const RESEED_OUTPUT_CEILING: usize = 1 << 20; // 1 MiB

/// ChaCha20-based Deterministic Random Bit Generator.
///
/// Internal state: 32-byte key + 64-bit block counter + 64-bit nonce.
/// After each draw, fast-key-erasure replaces the key with the first 32 bytes
/// of the next keystream block, so capturing the post-draw state cannot
/// reproduce prior output.
#[derive(Debug, Clone)]
pub struct ChaChaDrbg {
    key: [u8; 32],
    counter: u64,
    nonce: u64,
    pool: EntropyPool,
    state: DrbgState,
    bytes_since_reseed: usize,
}

impl ChaChaDrbg {
    /// Create a new, empty DRBG (state = `Empty`).
    pub const fn new() -> Self {
        ChaChaDrbg {
            key: [0u8; 32],
            counter: 0,
            nonce: 0,
            pool: EntropyPool::new(),
            state: DrbgState::Empty,
            bytes_since_reseed: 0,
        }
    }

    /// Mix `data` into the key and credit `credited_bits` of entropy.
    ///
    /// Absorption: XOR `data` (wrapping cyclically) into the key, then
    /// increment the nonce to ensure forward separation.
    /// Transitions `Empty` → `Early` on first call; `Early` → `Ready`
    /// when cumulative credited bits reach [`POOL_READY_BITS`].
    pub fn add_entropy(&mut self, data: &[u8], credited_bits: usize) {
        // XOR/absorb data into the key (wrapping if data > 32 bytes)
        for (i, &byte) in data.iter().enumerate() {
            self.key[i % 32] ^= byte;
        }
        // Mix in the data length as a domain separator
        let len_bytes = (data.len() as u64).to_le_bytes();
        for (i, &b) in len_bytes.iter().enumerate() {
            self.key[(24 + i) % 32] ^= b;
        }
        // Advance nonce to separate entropy injections
        self.nonce = self.nonce.wrapping_add(1);

        self.pool.credit(credited_bits);

        self.state = if self.pool.is_ready() {
            DrbgState::Ready
        } else {
            DrbgState::Early
        };
    }

    /// Returns `true` once ≥256 credited bits have been accumulated.
    pub fn is_ready(&self) -> bool {
        self.state == DrbgState::Ready
    }

    /// Current [`DrbgState`].
    pub fn state(&self) -> DrbgState {
        self.state
    }

    /// Fill `out` with cryptographically secure random bytes.
    ///
    /// Returns [`DrbgError::NotReady`] if `!is_ready()`.
    /// On success, performs **fast-key-erasure**: overwrites the internal key
    /// with the first 32 bytes of the next keystream block.
    pub fn fill(&mut self, out: &mut [u8]) -> Result<(), DrbgError> {
        if !self.is_ready() {
            return Err(DrbgError::NotReady);
        }
        self.fill_inner(out);
        Ok(())
    }

    /// Fill `out` even when `!is_ready()` (for `GRND_INSECURE`).
    ///
    /// Uses the same keystream as `fill` but does not require the READY state.
    /// Output is statistically fine but cryptographically unvetted when
    /// insufficient entropy has been credited.
    pub fn fill_insecure(&mut self, out: &mut [u8]) {
        self.fill_inner(out);
    }

    /// Core keystream generation + fast-key-erasure.
    fn fill_inner(&mut self, out: &mut [u8]) {
        let mut written = 0usize;
        while written < out.len() {
            let mut block = [0u8; 64];
            chacha20_block(&self.key, self.counter, self.nonce, &mut block);
            self.counter = self.counter.wrapping_add(1);

            let to_copy = (out.len() - written).min(64);
            out[written..written + to_copy].copy_from_slice(&block[..to_copy]);
            written += to_copy;
        }
        self.bytes_since_reseed = self.bytes_since_reseed.saturating_add(out.len());

        // Fast-key-erasure: derive a fresh key from the next block.
        self.erase_key();
    }

    /// Replace the current key with the first 32 bytes of the next keystream block.
    fn erase_key(&mut self) {
        let mut block = [0u8; 64];
        chacha20_block(&self.key, self.counter, self.nonce, &mut block);
        self.counter = self.counter.wrapping_add(1);
        self.key.copy_from_slice(&block[..32]);
    }

    /// Bytes output since the last reseed (or construction).
    pub fn bytes_since_reseed(&self) -> usize {
        self.bytes_since_reseed
    }

    /// Returns `true` when the output ceiling has been reached and a reseed is advisable.
    pub fn needs_reseed(&self) -> bool {
        self.bytes_since_reseed >= RESEED_OUTPUT_CEILING
    }

    /// Reseed the DRBG with fresh entropy, resetting the output counter.
    ///
    /// Equivalent to `add_entropy` but also resets `bytes_since_reseed`.
    pub fn reseed(&mut self, data: &[u8], credited_bits: usize) {
        self.add_entropy(data, credited_bits);
        self.bytes_since_reseed = 0;
        // Reset counter on reseed for forward secrecy.
        self.counter = 0;
    }
}

impl Default for ChaChaDrbg {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SipHash-2-4 (RFC-standard one-way keyed hash)
// ---------------------------------------------------------------------------

/// SipHash-2-4 with a 128-bit key (`key0`, `key1`) over arbitrary `data`.
///
/// Implements the standard SipHash-2-4 specification:
/// - 2 compression rounds per input block
/// - 4 finalization rounds
/// - Standard magic-constant initialization XOR'd with the 128-bit key
///
/// Pure-integer (no SIMD), `no_std`-compatible.  Used as the TCP ISN PRF
/// (RFC 6528 §3): `siphash24(secret0, secret1, &four_tuple_bytes)` is
/// one-way, so one observed ISN does not reveal the per-boot secret.
pub fn siphash24(key0: u64, key1: u64, data: &[u8]) -> u64 {
    // Standard SipHash initialization constants.
    let mut v0: u64 = key0 ^ 0x736f_6d65_7073_6575;
    let mut v1: u64 = key1 ^ 0x646f_7261_6e64_6f6d;
    let mut v2: u64 = key0 ^ 0x6c79_6765_6e65_7261;
    let mut v3: u64 = key1 ^ 0x7465_6462_7974_6573;

    /// One SipRound.
    #[inline(always)]
    fn sip_round(v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64) {
        *v0 = v0.wrapping_add(*v1);
        *v1 = v1.rotate_left(13);
        *v1 ^= *v0;
        *v0 = v0.rotate_left(32);
        *v2 = v2.wrapping_add(*v3);
        *v3 = v3.rotate_left(16);
        *v3 ^= *v2;
        *v0 = v0.wrapping_add(*v3);
        *v3 = v3.rotate_left(21);
        *v3 ^= *v0;
        *v2 = v2.wrapping_add(*v1);
        *v1 = v1.rotate_left(17);
        *v1 ^= *v2;
        *v2 = v2.rotate_left(32);
    }

    // Process complete 8-byte blocks (2 compression rounds each).
    let n_blocks = data.len() / 8;
    for i in 0..n_blocks {
        let block_bytes: [u8; 8] = [
            data[i * 8],
            data[i * 8 + 1],
            data[i * 8 + 2],
            data[i * 8 + 3],
            data[i * 8 + 4],
            data[i * 8 + 5],
            data[i * 8 + 6],
            data[i * 8 + 7],
        ];
        let m = u64::from_le_bytes(block_bytes);
        v3 ^= m;
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
        v0 ^= m;
    }

    // Build the final block: remaining bytes + length byte in the top byte.
    let remainder = data.len() % 8;
    let last_block_offset = n_blocks * 8;
    let mut last: u64 = (data.len() as u64 & 0xff) << 56;
    // Pack remaining bytes little-endian into the low bits.
    for i in 0..remainder {
        last |= (data[last_block_offset + i] as u64) << (i * 8);
    }

    v3 ^= last;
    sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    v0 ^= last;

    // Finalization: 4 rounds.
    v2 ^= 0xff;
    sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    sip_round(&mut v0, &mut v1, &mut v2, &mut v3);

    v0 ^ v1 ^ v2 ^ v3
}

// ---------------------------------------------------------------------------
// Global accessor (spin::Mutex-protected singleton)
// ---------------------------------------------------------------------------

/// The global DRBG instance, protected by a spin lock.
static GLOBAL: spin::Mutex<ChaChaDrbg> = spin::Mutex::new(ChaChaDrbg::new());

/// Mix `data` into the global DRBG, crediting `bits` of entropy.
pub fn seed_global(data: &[u8], bits: usize) {
    GLOBAL.lock().add_entropy(data, bits);
}

/// Returns `true` if the global DRBG has reached the `READY` state.
pub fn global_ready() -> bool {
    GLOBAL.lock().is_ready()
}

/// Fill `out` with secure random bytes from the global DRBG.
///
/// Returns [`DrbgError::NotReady`] if the DRBG is not yet `READY`.
pub fn global_fill(out: &mut [u8]) -> Result<(), DrbgError> {
    GLOBAL.lock().fill(out)
}

/// Fill `out` from the global DRBG without requiring `READY` (`GRND_INSECURE`).
pub fn global_fill_insecure(out: &mut [u8]) {
    GLOBAL.lock().fill_insecure(out);
}

/// Returns `true` if the global DRBG has reached its output ceiling.
pub fn global_needs_reseed() -> bool {
    GLOBAL.lock().needs_reseed()
}

/// Reseed the global DRBG with fresh entropy.
pub fn global_reseed(data: &[u8], bits: usize) {
    GLOBAL.lock().reseed(data, bits);
}

/// Return a random `u32` from the global DRBG, or `None` if not ready.
pub fn global_random_u32() -> Option<u32> {
    let mut drbg = GLOBAL.lock();
    if !drbg.is_ready() {
        return None;
    }
    let mut buf = [0u8; 4];
    // fill_inner always succeeds (we checked is_ready above)
    drbg.fill_inner(&mut buf);
    Some(u32::from_le_bytes(buf))
}

/// Return a random `u64` from the global DRBG, or `None` if not ready.
pub fn global_random_u64() -> Option<u64> {
    let mut drbg = GLOBAL.lock();
    if !drbg.is_ready() {
        return None;
    }
    let mut buf = [0u8; 8];
    drbg.fill_inner(&mut buf);
    Some(u64::from_le_bytes(buf))
}

// ---------------------------------------------------------------------------
// Host tests (TDD — written first, then implementation made green)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Test A: entropy / state transitions / fill / fill_insecure
    // -----------------------------------------------------------------------

    #[test]
    fn t_not_ready_below_256_bits() {
        let mut drbg = ChaChaDrbg::new();
        // Credit only 128 bits — should NOT be ready yet
        drbg.add_entropy(&[0xAAu8; 32], 128);
        assert!(
            !drbg.is_ready(),
            "DRBG must not be ready with only 128 credited bits"
        );
        assert_eq!(drbg.state(), DrbgState::Early);
    }

    #[test]
    fn t_ready_at_256_bits() {
        let mut drbg = ChaChaDrbg::new();
        drbg.add_entropy(&[0x55u8; 32], 256);
        assert!(
            drbg.is_ready(),
            "DRBG must be ready after 256 credited bits"
        );
        assert_eq!(drbg.state(), DrbgState::Ready);
    }

    #[test]
    fn t_ready_via_two_add_entropy_calls() {
        let mut drbg = ChaChaDrbg::new();
        drbg.add_entropy(&[0x11u8; 16], 128);
        assert!(!drbg.is_ready());
        drbg.add_entropy(&[0x22u8; 16], 128);
        assert!(drbg.is_ready(), "must be ready after cumulative 256 bits");
    }

    #[test]
    fn t_fill_returns_not_ready_before_seed() {
        let mut drbg = ChaChaDrbg::new();
        let mut out = [0u8; 32];
        assert_eq!(drbg.fill(&mut out), Err(DrbgError::NotReady));
    }

    #[test]
    fn t_fill_succeeds_after_seed() {
        let mut drbg = ChaChaDrbg::new();
        drbg.add_entropy(&[0xDEu8; 32], 256);
        let mut out = [0u8; 64];
        assert_eq!(drbg.fill(&mut out), Ok(()));
        // Output should be non-zero
        assert_ne!(out, [0u8; 64]);
    }

    #[test]
    fn t_fill_insecure_works_before_ready() {
        let mut drbg = ChaChaDrbg::new();
        // No entropy credited at all
        let mut out = [0u8; 32];
        drbg.fill_insecure(&mut out);
        // We can't assert non-zero (key starts all-zero → first block could be predictable)
        // But it must not panic — just verify we got here.
        // With all-zero key/counter/nonce ChaCha20 still produces non-zero output:
        assert_ne!(
            out, [0u8; 32],
            "ChaCha20 with zero state must not produce all-zeros"
        );
    }

    #[test]
    fn t_grnd_nonblock_semantics() {
        // simulate GRND_NONBLOCK: if !ready, caller gets NotReady
        let mut drbg = ChaChaDrbg::new();
        drbg.add_entropy(&[0xFFu8; 32], 64); // only 64 bits — not ready
        let mut out = [0u8; 32];
        assert_eq!(
            drbg.fill(&mut out),
            Err(DrbgError::NotReady),
            "secure fill must fail when !ready (GRND_NONBLOCK behavior)"
        );
        // GRND_INSECURE: must succeed
        drbg.fill_insecure(&mut out);
    }

    // -----------------------------------------------------------------------
    // Test B: fast-key-erasure
    // -----------------------------------------------------------------------

    #[test]
    fn t_fast_key_erasure() {
        let mut drbg = ChaChaDrbg::new();
        drbg.add_entropy(&[0xABu8; 32], 256);

        // Capture state BEFORE draw
        let pre_key = drbg.key;
        let pre_counter = drbg.counter;

        // Draw 64 bytes
        let mut out1 = [0u8; 64];
        drbg.fill(&mut out1).unwrap();

        // Replay from the captured pre-draw state
        let mut replay_block = [0u8; 64];
        chacha20_block(&pre_key, pre_counter, drbg.nonce, &mut replay_block);
        // `out1` should equal the replay (determinism check first)
        assert_eq!(
            out1, replay_block,
            "replay must match: ChaCha20 is deterministic"
        );

        // Now verify forward secrecy: the key HAS changed after the draw
        assert_ne!(
            drbg.key, pre_key,
            "key must be erased/replaced after draw (fast-key-erasure)"
        );

        // A second draw CANNOT be reproduced from `pre_key` + `pre_counter`
        let mut out2 = [0u8; 64];
        drbg.fill(&mut out2).unwrap();
        // Attempt to reproduce out2 from pre-draw state (counter advanced by 1 for FKE)
        // The new key is derived from block[pre_counter+1], so using pre_key+1 gives the
        // FKE derivation block but not out2's block (which uses the new key at counter 0).
        let mut fake_block = [0u8; 64];
        chacha20_block(
            &pre_key,
            pre_counter.wrapping_add(1),
            drbg.nonce,
            &mut fake_block,
        );
        assert_ne!(
            out2,
            fake_block[..64],
            "past key cannot reproduce subsequent output (fast-key-erasure)"
        );
    }

    // -----------------------------------------------------------------------
    // Test C: statistical quality — monobit + chi-square on 1 MiB
    // -----------------------------------------------------------------------

    #[test]
    fn t_statistical_monobit_and_chisquare() {
        let mut drbg = ChaChaDrbg::new();
        // Seed with distinct bytes to avoid degenerate all-zero key
        let seed: [u8; 32] =
            core::array::from_fn(|i| (i as u8).wrapping_mul(0x37).wrapping_add(0x55));
        drbg.add_entropy(&seed, 256);

        const MIB: usize = 1 << 20; // 1 MiB
        let mut buf = vec![0u8; MIB];
        drbg.fill(&mut buf).unwrap();

        // --- Monobit test ---
        let ones: u64 = buf.iter().map(|&b| b.count_ones() as u64).sum();
        let bits = (MIB * 8) as u64;
        let half = bits / 2;
        // Generous bound: deviation must be < 1% of bits
        let threshold = bits / 100;
        let deviation = ones.abs_diff(half);
        assert!(
            deviation < threshold,
            "monobit: ones={ones} half={half} deviation={deviation} threshold={threshold}"
        );

        // --- Chi-square test over byte values (255 degrees of freedom) ---
        // Expected frequency per byte value: MIB / 256
        let expected = MIB as f64 / 256.0;
        let mut freq = [0u64; 256];
        for &b in &buf {
            freq[b as usize] += 1;
        }
        let chi2: f64 = freq
            .iter()
            .map(|&f| {
                let diff = f as f64 - expected;
                diff * diff / expected
            })
            .sum();
        // For 255 df, the 99.9% upper critical value is ~368.
        // We use 450 as a very conservative bound to avoid flakiness.
        assert!(
            chi2 < 450.0,
            "chi-square: chi2={chi2:.2} (255 df, threshold=450); output looks non-random"
        );
        // Also assert it's not suspiciously LOW (< 100 would mean output is too uniform)
        assert!(
            chi2 > 100.0,
            "chi-square: chi2={chi2:.2} suspiciously low — output may be degenerate"
        );
    }

    // -----------------------------------------------------------------------
    // Test D: reseed / needs_reseed
    // -----------------------------------------------------------------------

    #[test]
    fn t_needs_reseed_fires_at_ceiling() {
        let mut drbg = ChaChaDrbg::new();
        drbg.add_entropy(&[0x99u8; 32], 256);
        assert!(!drbg.needs_reseed());

        // Draw slightly less than the ceiling — should not need reseed yet
        let chunk = 65536;
        let mut buf = vec![0u8; chunk];
        let iters = RESEED_OUTPUT_CEILING / chunk;
        for _ in 0..iters - 1 {
            drbg.fill(&mut buf).unwrap();
        }
        // One more to push over
        drbg.fill(&mut buf).unwrap();
        assert!(
            drbg.needs_reseed(),
            "needs_reseed must fire at the output ceiling"
        );
    }

    #[test]
    fn t_reseed_resets_counter() {
        let mut drbg = ChaChaDrbg::new();
        drbg.add_entropy(&[0x77u8; 32], 256);
        let mut buf = vec![0u8; RESEED_OUTPUT_CEILING];
        drbg.fill(&mut buf).unwrap();
        assert!(drbg.needs_reseed());
        drbg.reseed(&[0x88u8; 32], 256);
        assert!(
            !drbg.needs_reseed(),
            "reseed must reset the output byte counter"
        );
    }

    // -----------------------------------------------------------------------
    // Test E: two draws from the same ready DRBG differ
    // -----------------------------------------------------------------------

    #[test]
    fn t_two_draws_differ() {
        let mut drbg = ChaChaDrbg::new();
        drbg.add_entropy(&[0x42u8; 32], 256);
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        drbg.fill(&mut a).unwrap();
        drbg.fill(&mut b).unwrap();
        assert_ne!(
            a, b,
            "consecutive draws must differ (fast-key-erasure advances state)"
        );
    }

    // -----------------------------------------------------------------------
    // Test F: SipHash-2-4 official test vectors
    //
    // Keys from the SipHash reference implementation / paper:
    //   key0 = 0x0706050403020100
    //   key1 = 0x0f0e0d0c0b0a0908
    //
    // Vectors from the official SipHash-2-4 reference output:
    //   empty input  → 0x726fdb47dd0e0e31
    //   input [0x00] → 0x74f839c593dc67fd
    //   input [0x00,0x01,...,0x0f] (16 bytes) → 0xa129ca6149be45e5
    // -----------------------------------------------------------------------

    #[test]
    fn t_siphash24_empty_vector() {
        // Official SipHash-2-4 test vector: empty input.
        // key0 = 0x0706050403020100, key1 = 0x0f0e0d0c0b0a0908
        // Expected: 0x726fdb47dd0e0e31
        let key0: u64 = 0x0706_0504_0302_0100;
        let key1: u64 = 0x0f0e_0d0c_0b0a_0908;
        let result = siphash24(key0, key1, &[]);
        assert_eq!(
            result, 0x726f_db47_dd0e_0e31,
            "SipHash-2-4 empty vector mismatch: got {result:#018x}"
        );
    }

    #[test]
    fn t_siphash24_one_byte_vector() {
        // Official SipHash-2-4 test vector: 1-byte input [0x00].
        // key0 = 0x0706050403020100, key1 = 0x0f0e0d0c0b0a0908
        // Expected: 0x74f839c593dc67fd
        let key0: u64 = 0x0706_0504_0302_0100;
        let key1: u64 = 0x0f0e_0d0c_0b0a_0908;
        let result = siphash24(key0, key1, &[0x00]);
        assert_eq!(
            result, 0x74f8_39c5_93dc_67fd,
            "SipHash-2-4 1-byte vector mismatch: got {result:#018x}"
        );
    }

    #[test]
    fn t_siphash24_fifteen_byte_vector() {
        // Official SipHash-2-4 test vector: 15-byte input [0x00..=0x0e].
        // key0 = 0x0706050403020100, key1 = 0x0f0e0d0c0b0a0908
        // Expected: 0xa129ca6149be45e5  (from SipHash reference appendix A, row 15)
        let key0: u64 = 0x0706_0504_0302_0100;
        let key1: u64 = 0x0f0e_0d0c_0b0a_0908;
        let input: [u8; 15] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e,
        ];
        let result = siphash24(key0, key1, &input);
        assert_eq!(
            result, 0xa129_ca61_49be_45e5,
            "SipHash-2-4 15-byte vector mismatch: got {result:#018x}"
        );
    }

    #[test]
    fn t_siphash24_different_keys_differ() {
        // Sanity: different keys produce different output for the same data.
        let r1 = siphash24(0x0101_0101_0101_0101, 0x0202_0202_0202_0202, b"hello");
        let r2 = siphash24(0x0303_0303_0303_0303, 0x0404_0404_0404_0404, b"hello");
        assert_ne!(
            r1, r2,
            "different keys must produce different SipHash output"
        );
    }
}
