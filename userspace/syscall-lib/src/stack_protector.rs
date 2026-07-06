//! Stack-smashing protector runtime (Phase 110 Track B.2).
//!
//! The userspace target builds with `-Z stack-protector=strong`, so the
//! compiler emits, for every function with a stack buffer (or an
//! address-taken local), a canary store in the prologue and a compare in the
//! epilogue that branches to `__stack_chk_fail` on a mismatch. This module
//! supplies the two runtime symbols LLVM references — the guard value and the
//! failure handler — that a hosted libc would otherwise provide.
//!
//! The guard is seeded per process from the kernel CSPRNG at startup by
//! [`seed_guard`], called from the entry-point trampoline (`start::run_main*`)
//! before `main`. `run_main*` diverges (it ends in `exit`), so its own
//! epilogue canary check is never reached and the mid-function reseed cannot
//! trip it.

/// The stack canary. LLVM's stack protector reads this global symbol in each
/// protected prologue/epilogue. Initialized to a fixed non-zero sentinel so
/// canaries are functional from the very first instruction (before the CSPRNG
/// reseed); any smash that overwrites the canary with a different value is
/// caught. Seeded per process by [`seed_guard`].
#[unsafe(no_mangle)]
pub static mut __stack_chk_guard: u64 = 0x1d59_a7c0_de5e_ed01;

/// Stack-smash handler. LLVM calls this when a protected function's epilogue
/// finds its canary clobbered. Never returns — a corrupted frame must not be
/// allowed to return into hijacked control flow.
#[unsafe(no_mangle)]
pub extern "C" fn __stack_chk_fail() -> ! {
    crate::write_str(
        crate::STDOUT_FILENO,
        "*** stack smashing detected: terminated\n",
    );
    crate::exit(134) // 128 + SIGABRT, matching glibc's __stack_chk_fail exit
}

/// Seed [`__stack_chk_guard`] from the kernel CSPRNG. Called once at process
/// start from the (divergent) entry-point trampoline, before `main`, so the
/// guard an attacker would need to forge is unpredictable per process rather
/// than the compile-time sentinel.
///
/// MUST only be called from a function that never returns (see the module
/// note) — it rewrites the very guard a returning caller's epilogue would
/// check. A `getrandom` short-read leaves the fixed sentinel in place (still a
/// functional canary).
pub fn seed_guard() {
    let mut seed = [0u8; 8];
    if crate::getrandom(&mut seed) == 8 {
        let v = u64::from_le_bytes(seed) | 1; // keep it non-zero
        // SAFETY: single-threaded process start; LLVM reads this symbol only
        // via the canary sequences, which observe the written value on the
        // next protected prologue. `addr_of_mut!` avoids forming a reference to
        // the `static mut`.
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(__stack_chk_guard), v);
        }
    }
}
