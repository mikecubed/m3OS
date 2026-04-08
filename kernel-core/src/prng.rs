//! Xorshift64-multiply PRNG for kernel entropy (Phase 48).
//!
//! Extracted from the kernel syscall handler to enable host-side testing.
//! The PRNG is NOT cryptographically secure but provides reasonable
//! statistical quality when seeded with hardware entropy (RDRAND).

/// A simple xorshift64-multiply PRNG.
#[derive(Debug)]
pub struct Prng {
    state: u64,
}

impl Prng {
    /// Create a new PRNG with the given seed.
    /// If seed is 0, uses a non-zero fallback to avoid the zero fixed point.
    pub fn new(seed: u64) -> Self {
        let state = if seed == 0 {
            0xDEAD_BEEF_CAFE_BABE
        } else {
            seed
        };
        Self { state }
    }

    /// Fill the output buffer with pseudorandom bytes.
    pub fn fill_bytes(&mut self, out: &mut [u8]) {
        for byte in out.iter_mut() {
            self.state ^= self.state >> 12;
            self.state ^= self.state << 25;
            self.state ^= self.state >> 27;
            *byte = (self.state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
    }

    /// Reseed the PRNG by mixing in new entropy.
    pub fn reseed(&mut self, entropy: u64) {
        self.state ^= entropy;
        if self.state == 0 {
            self.state = 0xDEAD_BEEF_CAFE_BABE;
        }
        // Run a few rounds to mix
        let mut discard = [0u8; 8];
        self.fill_bytes(&mut discard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_different_seeds_produce_different_output() {
        let mut prng1 = Prng::new(12345);
        let mut prng2 = Prng::new(67890);
        let mut buf1 = [0u8; 32];
        let mut buf2 = [0u8; 32];
        prng1.fill_bytes(&mut buf1);
        prng2.fill_bytes(&mut buf2);
        assert_ne!(buf1, buf2);
    }

    #[test]
    fn test_zero_seed_produces_nonzero_output() {
        let mut prng = Prng::new(0);
        let mut buf = [0u8; 32];
        prng.fill_bytes(&mut buf);
        assert_ne!(buf, [0u8; 32]);
    }

    #[test]
    fn test_deterministic_with_same_seed() {
        let mut prng1 = Prng::new(42);
        let mut prng2 = Prng::new(42);
        let mut buf1 = [0u8; 64];
        let mut buf2 = [0u8; 64];
        prng1.fill_bytes(&mut buf1);
        prng2.fill_bytes(&mut buf2);
        assert_eq!(buf1, buf2);
    }

    #[test]
    fn test_reseed_changes_output() {
        let mut prng1 = Prng::new(42);
        let mut prng2 = Prng::new(42);
        prng2.reseed(99999);
        let mut buf1 = [0u8; 32];
        let mut buf2 = [0u8; 32];
        prng1.fill_bytes(&mut buf1);
        prng2.fill_bytes(&mut buf2);
        assert_ne!(buf1, buf2);
    }
}
