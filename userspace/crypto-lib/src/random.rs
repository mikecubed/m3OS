//! CSPRNG seeded from the kernel's `getrandom` syscall.
//!
//! **Entropy note:** The kernel `getrandom` implementation seeds its PRNG
//! from RDRAND mixed with TSC, and reseeds from RDRAND every 256 bytes.
//! This provides hardware-backed entropy on supported CPUs. The underlying
//! xorshift64-multiply PRNG has not been audited for production use;
//! generated keys are suitable for testing and development but should not
//! be used to protect real secrets.

use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore, SeedableRng};

use crate::CryptoError;

/// Cryptographically secure pseudorandom number generator state.
pub struct CsprngState {
    rng: ChaCha20Rng,
}

/// Initialize the CSPRNG by reading 32 bytes from the kernel's `getrandom` syscall.
///
/// The seed is zeroed from stack memory after initializing the RNG.
pub fn csprng_init() -> Result<CsprngState, CryptoError> {
    let mut seed = [0u8; 32];
    let n = syscall_lib::getrandom(&mut seed);
    if n < 32 {
        // Zero partial seed before returning (volatile to prevent elision).
        unsafe { core::ptr::write_volatile(&mut seed, [0u8; 32]) };
        return Err(CryptoError::SeedingFailed);
    }
    let rng = ChaCha20Rng::from_seed(seed);
    // Zero the seed from stack memory using a volatile write to prevent
    // the compiler from optimizing the zeroing away.
    unsafe { core::ptr::write_volatile(&mut seed, [0u8; 32]) };
    Ok(CsprngState { rng })
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
