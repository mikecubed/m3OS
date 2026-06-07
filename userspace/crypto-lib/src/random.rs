//! CSPRNG seeded from the kernel's `getrandom` syscall.
//!
//! **Entropy note (Phase 86a):** The kernel `getrandom` implementation now
//! sources a **ChaCha20 DRBG** seeded from RDSEED → RDRAND → TSC (in
//! preference order) during early boot.  The DRBG performs fast-key-erasure
//! after each draw and reseeds at a 60-second or 1 MiB output ceiling.
//! This supersedes the previous xorshift64-multiply PRNG that was seeded per
//! call.
//!
//! **Key rotation note:** Secrets generated under the *previous* weak PRNG
//! (any m3OS boot prior to Phase 86a) are NOT automatically rotated by this
//! upgrade.  Affected artifacts that should be manually regenerated include:
//!   - The `sshd` Ed25519 host key (`/etc/ssh/ssh_host_ed25519_key`)
//!   - Any `passwd`/`shadow` password hashes (salts were derived from the
//!     old PRNG)
//!
//! To regenerate the host key: delete the file and restart `sshd`.
//! To regenerate password hashes: use `passwd` for each account.

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
