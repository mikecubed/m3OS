//! ASLR probe (Phase 110 Track B.1) — prints the address of a stack local.
//!
//! The address is derived from the initial RSP, which the kernel ELF loader
//! randomizes per `execve` (`map_user_stack` returns a CSPRNG-jittered stack
//! top). Running this binary twice therefore prints different addresses; the
//! `aslr-smoke` gate execs it several times and asserts they are not all equal.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, write_str, write_u64};

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    // A stack local whose address is taken — the compiler must materialize it
    // on the (randomized) stack, so its address tracks the initial RSP.
    let local: u64 = 0;
    let addr = core::ptr::addr_of!(local) as u64;
    write_str(STDOUT_FILENO, "ASLR_PROBE:sp=");
    write_u64(STDOUT_FILENO, addr);
    write_str(STDOUT_FILENO, "\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "aslr-probe: PANIC\n");
    syscall_lib::exit(101)
}
