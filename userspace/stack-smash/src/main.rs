//! Stack-smash probe (Phase 110 Track B.2) — deliberately overflows a stack
//! buffer to prove the `-Z stack-protector` canary catches it.
//!
//! `smash` writes past a 16-byte buffer, clobbering the canary the compiler
//! placed above it. On return, `smash`'s epilogue compares the canary against
//! `__stack_chk_guard`, finds it corrupted, and calls `__stack_chk_fail`
//! (`syscall_lib::stack_protector`), which prints `*** stack smashing detected`
//! and exits — so `STACK_SMASH:after-NOT-CAUGHT` must never print. The
//! `stack-smash-smoke` gate asserts the detection message appears and the
//! not-caught line does not.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, getrandom, write_str};

syscall_lib::entry_point!(main);

/// Overflow a 16-byte stack buffer by `n` bytes. `#[inline(never)]` so it keeps
/// its own protected frame; the writes are `volatile` (never elided) and `n` is
/// opaque to the optimizer (drawn from `getrandom`), so the overflow can't be
/// proven away.
#[inline(never)]
fn smash(n: usize) {
    let mut buf = [0u8; 16];
    let mut i = 0;
    while i < n {
        // SAFETY: intentionally out-of-bounds past buf[16] — this is the whole
        // point of the probe. The writes stay within the mapped stack region
        // (they grow toward the stack top).
        unsafe {
            core::ptr::write_volatile(buf.as_mut_ptr().add(i), 0x41u8);
        }
        i += 1;
    }
    // Keep `buf` live so the frame (and its canary) are not optimized away.
    unsafe {
        core::ptr::read_volatile(buf.as_ptr());
    }
}

fn main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "STACK_SMASH:before\n");
    // n = 64 or 65 — enough to reach the canary above the 16-byte buffer, kept
    // opaque via a random low bit so the optimizer can't fold the loop away.
    let mut b = [0u8; 1];
    let _ = getrandom(&mut b);
    let n = 64usize + (b[0] as usize & 1);
    smash(n);
    // Unreachable when the canary fires (smash's epilogue aborts first).
    write_str(STDOUT_FILENO, "STACK_SMASH:after-NOT-CAUGHT\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "stack-smash: PANIC\n");
    syscall_lib::exit(101)
}
