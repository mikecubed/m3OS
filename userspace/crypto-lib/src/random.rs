//! CSPRNG seeded from the kernel's `getrandom` syscall.

use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore, SeedableRng};

use crate::CryptoError;

/// Cryptographically secure pseudorandom number generator state.
pub struct CsprngState {
    rng: ChaCha20Rng,
}

/// Initialize the CSPRNG by reading 32 bytes from the kernel's `getrandom` syscall.
pub fn csprng_init() -> Result<CsprngState, CryptoError> {
    let mut seed = [0u8; 32];
    let n = syscall_lib::getrandom(&mut seed);
    if n < 32 {
        return Err(CryptoError::SeedingFailed);
    }
    Ok(CsprngState {
        rng: ChaCha20Rng::from_seed(seed),
    })
}

/// Fill `buf` with cryptographically secure random bytes.
pub fn csprng_fill(state: &mut CsprngState, buf: &mut [u8]) {
    state.rng.fill_bytes(buf);
}

impl CsprngState {
    /// Access the inner RNG for APIs that need `CryptoRng + RngCore`.
    pub fn rng(&mut self) -> &mut ChaCha20Rng {
        &mut self.rng
    }

    /// Create a CSPRNG from a fixed seed (for deterministic testing only).
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            rng: ChaCha20Rng::from_seed(seed),
        }
    }
}
