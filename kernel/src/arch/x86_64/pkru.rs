//! Thin safe wrappers around `RDPKRU` / `WRPKRU` — the PKRU register
//! (Phase 90a Track B.3).
//!
//! PKRU (Protection-Key Rights for User pages) is a 32-bit register holding two
//! bits per protection key: `AD` (access-disable) and `WD` (write-disable). The
//! CPU AND-masks the per-key rights into a user page's effective permissions
//! using the 4-bit key tag in the page's PTE (bits 59..=62). PKRU is **not** a
//! syscall surface — it is read/written in ring 3 (and here in ring 0) via the
//! `RDPKRU`/`WRPKRU` instructions, which `#UD` unless `CR4.PKE` is set. Every
//! caller must therefore gate on [`cpuid::pku_usable`] (which implies the kernel
//! set `CR4.PKE` on this core in `enable_xsave_state`).
//!
//! The 32-bit layout: for key `k`, bit `2*k` is `AD` and bit `2*k+1` is `WD`.
//! This mirrors the `PKEY_DISABLE_ACCESS` (0x1) / `PKEY_DISABLE_WRITE` (0x2)
//! `init_access_rights` flags `pkey_alloc` records — see
//! [`kernel_core::pkey::PKEY_DISABLE_ACCESS`] / `PKEY_DISABLE_WRITE`.
//!
//! **B.4 carries PKRU across context switches** (and signal frames) via XSAVE
//! component 9; the live-register writes here are correct on their own — they
//! program *this thread's current* PKRU — and B.4 is what makes that value
//! persist across a `switch_context`.

use kernel_core::pkey::{PKEY_DISABLE_ACCESS, PKEY_DISABLE_WRITE};

/// Read the current core's PKRU register.
///
/// `RDPKRU` reads PKRU into EAX with ECX=0 (EDX must be 0 on input and is zeroed
/// on output). Wrapped immediately in a safe fn; callers must have confirmed
/// `cpuid::pku_usable()` so `CR4.PKE` is set and the instruction does not `#UD`.
#[inline]
pub fn rdpkru() -> u32 {
    let pkru: u32;
    // SAFETY: `RDPKRU` is a hardware register read with no memory effects. It
    // `#UD`s only when `CR4.PKE` is clear, which the caller's `pku_usable()`
    // gate rules out. ECX must be 0; EDX is clobbered (set to 0 by the CPU).
    unsafe {
        core::arch::asm!(
            "rdpkru",
            in("ecx") 0u32,
            out("eax") pkru,
            out("edx") _,
            options(nomem, nostack, preserves_flags),
        );
    }
    pkru
}

/// Write the current core's PKRU register.
///
/// `WRPKRU` writes EAX to PKRU with ECX=0 and EDX=0 (a non-zero ECX/EDX `#GP`s).
/// Wrapped immediately in a safe fn; callers must have confirmed
/// `cpuid::pku_usable()`. As with `RDPKRU`, this programs only *this thread's
/// current* PKRU on *this core* — persistence across context switches is Track
/// B.4 (XSAVE component 9).
#[inline]
pub fn wrpkru(value: u32) {
    // SAFETY: `WRPKRU` writes the architectural PKRU register; it has no memory
    // effects and is serializing. It `#UD`s only when `CR4.PKE` is clear (ruled
    // out by the caller's `pku_usable()` gate) and `#GP`s only when ECX/EDX are
    // non-zero, which we pin to 0 here.
    unsafe {
        core::arch::asm!(
            "wrpkru",
            in("eax") value,
            in("ecx") 0u32,
            in("edx") 0u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// The `(AD, WD)` bit pair offsets for protection key `k` within PKRU.
/// Bit `2*k` = access-disable, bit `2*k+1` = write-disable.
#[inline]
const fn pkru_bits_for_key(key: u8) -> (u32, u32) {
    let ad = 1u32 << (2 * key as u32);
    let wd = 1u32 << (2 * key as u32 + 1);
    (ad, wd)
}

/// Apply `pkey_alloc`'s `init_access_rights` for `key` to the **calling
/// thread's live PKRU** on this core (Track B.3 `sys_pkey_alloc`).
///
/// Reads PKRU, clears `key`'s `AD`/`WD` bits, then sets them from
/// `init_access_rights` (`PKEY_DISABLE_ACCESS` → `AD`, `PKEY_DISABLE_WRITE` →
/// `WD`), and writes PKRU back. This matches Linux's `pkey_alloc`: the new key's
/// PKRU slot is initialised to the requested rights so the very first access
/// through a page tagged with the key already honours them.
///
/// Caller MUST have confirmed [`cpuid::pku_usable`] is true (so `CR4.PKE` is set
/// and `RDPKRU`/`WRPKRU` do not `#UD`). `key` must be a real protection key
/// (1..=15); key 0's PKRU slot is always full-access and is never restricted.
///
/// **B.4 carries this across switches** — the write here updates the current
/// register; B.4's XSAVE component-9 save/restore is what persists it.
pub fn apply_init_rights(key: u8, init_access_rights: u32) {
    let (ad, wd) = pkru_bits_for_key(key);
    let mut pkru = rdpkru();
    // Clear this key's two bits, then set per the requested init rights.
    pkru &= !(ad | wd);
    if init_access_rights & PKEY_DISABLE_ACCESS != 0 {
        pkru |= ad;
    }
    if init_access_rights & PKEY_DISABLE_WRITE != 0 {
        pkru |= wd;
    }
    wrpkru(pkru);
}

/// Phase 90b — W^X v2 cross-thread READ recovery: grant the **calling thread**
/// READ access to protection key `key` by clearing only its access-disable (`AD`)
/// bit in the live PKRU, leaving the write-disable (`WD`) bit untouched.
///
/// PKRU is per-thread hardware state. A real-world Node process allocates a
/// write-deny key for its V8 code space, then spawns worker/background threads;
/// a sibling thread that DATA-reads a pkey-tagged executable code page it never
/// inherited access to traps with a `PROTECTION_KEY` page fault. The W^X v2
/// invariant only needs *writes* gated per-thread-window — read+execute of
/// guarded code is process-wide — so the page-fault handler calls this to grant
/// the read and retry. Leaving `WD` set keeps writes gated (W^X intact), so this
/// does NOT relax the W^X write-protection. Caller MUST have confirmed
/// [`cpuid::pku_usable`]. The next context-switch XSAVE persists the new register.
pub fn grant_read_access(key: u8) {
    let (ad, _wd) = pkru_bits_for_key(key);
    let pkru = rdpkru() & !ad;
    wrpkru(pkru);
}
