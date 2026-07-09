//! ROP / return-address-overwrite PoC (Phase 110 Track B.3) — proves the CET
//! user shadow stack catches a backward-edge control-flow hijack.
//!
//! `vulnerable()` overwrites its own return address on the ordinary stack with
//! the address of `pwned()`, then executes `ret`. This is the tail of a classic
//! stack buffer overflow reduced to its essence: a *precise, deterministic*
//! return-address overwrite. Determinism is deliberate — this PoC can only be
//! *validated* on CET silicon (QEMU TCG models no CET), so a fragile,
//! offset-dependent buffer overflow would give us nothing to trust at the bench.
//! Doing the overwrite in a `#[unsafe(naked)]` function (no prologue/epilogue,
//! and therefore no canary) means `[rsp]` on entry *is* the return-address slot
//! the `call` pushed — the overwrite target is exact on every build.
//!
//! - **CET on** (Tiger Lake, default image): `ret` compares the ordinary-stack
//!   return address against the hardware shadow-stack copy, sees the mismatch,
//!   and raises `#CP`; the kernel's `control_protection_fault_body` kills the
//!   process (`[int] userspace #CP (CET control-protection): … — process
//!   killed`) and `pwned()` never runs → no `ROP_CET_POC:PWNED` line. The PASS.
//! - **CET off** (`M3OS_MITIGATIONS=off`, or `CET_SS` masked in `probe_cet`):
//!   `ret` transfers to the planted address, `pwned()` runs and prints PWNED
//!   with no `#CP`. This is the CET-off control.
//!
//! The crate ships WITHOUT the `-Zstack-protector=strong` userspace canary —
//! xtask builds it with `-Zstack-protector=none` (see `build_userspace_bins`).
//! The naked overwrite has no canary of its own, but disabling it crate-wide
//! matches the documented build and removes any chance a helper's canary fires
//! first: CET is the layer under test here.
//!
//! Bench arm: Block 2b of the 2026-07-09 Dell validation runbook; `next-dell-
//! session.md` Phase 110 "B.3 — CET catches a real ROP/overwrite".
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, exit, write_str};

syscall_lib::entry_point!(main);

/// Marker "gadget" — where a real ROP chain would land. Diverges (never
/// returns) because the overwrite unbalances the stack: after `vulnerable`'s
/// `ret` consumes the original return slot, there is no valid frame to return
/// into, so `pwned()` exits the process rather than returning.
#[inline(never)]
extern "C" fn pwned() -> ! {
    write_str(
        STDOUT_FILENO,
        "ROP_CET_POC:PWNED return-address overwrite succeeded (CET OFF)\n",
    );
    exit(0)
}

/// Overwrite this function's own return address — the top of the ordinary stack
/// on entry, where the caller's `call` pushed it — with `target`, then `ret`.
///
/// Naked so there is no prologue/epilogue (and no canary) between entry and the
/// overwrite: `[rsp]` on entry is exactly the return-address slot. With CET the
/// `ret` mismatches the shadow-stack copy → `#CP`; without CET it jumps to
/// `target`.
///
/// # Safety
///
/// Corrupts the caller's control flow by construction. `target` must be the
/// address of a diverging function, since the ordinary return path is destroyed.
#[unsafe(naked)]
extern "C" fn vulnerable(target: u64) {
    core::arch::naked_asm!(
        // rdi = target (SysV AMD64 arg 0); [rsp] = the pushed return address.
        "mov [rsp], rdi",
        "ret",
    )
}

fn main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "ROP_CET_POC:before\n");
    // Overwrite our return address with pwned(). Under CET this `ret` #CP-kills
    // us (the kernel prints the #CP line); with CET off it transfers to pwned().
    // SAFETY: intentional control-flow corruption — the whole point of the PoC.
    // pwned() diverges, so the destroyed return path is never taken.
    vulnerable(pwned as *const () as usize as u64);
    // Unreachable in both configs: vulnerable()'s `ret` either #CPs (CET on) or
    // jumps to pwned() (CET off) — it can never return here. If this ever
    // prints, CET failed AND the planted address was wrong: a real regression.
    write_str(STDOUT_FILENO, "ROP_CET_POC:after-NOT-OVERWRITTEN\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "rop-cet-poc: PANIC\n");
    syscall_lib::exit(101)
}
