//! `ptrace-tracee` — the debuggee for the `ptrace-gdbserver-smoke` gate
//! (Phase 111 Track D.3).
//!
//! A trivial program `m3gdbserver` launches under trace: it exec-stops before
//! its first instruction (so the debugger can set a breakpoint at the entry),
//! does a little work when resumed, and exits with a fixed code (7) the gate
//! asserts via the RSP `W07` stop reply.
#![no_std]
#![no_main]

/// Exit code the gate asserts (via `W07`).
const TRACEE_EXIT_CODE: i32 = 7;

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!("xor rbp, rbp", "call {f}", f = sym tracee_main);
}

fn tracee_main() -> ! {
    // A little work so a single-step visibly advances RIP before the exit.
    let mut acc: u64 = 0;
    for i in 0..64u64 {
        acc = acc.wrapping_add(i.wrapping_mul(3));
    }
    core::hint::black_box(acc);
    syscall_lib::exit(TRACEE_EXIT_CODE)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
