//! Minimal userspace binary: calls exit(0) immediately.
//!
//! Validation: P11-T019 — load a statically linked ELF, confirm exit code 0.
#![no_std]
#![no_main]

use syscall_lib::exit;

// Phase 86f FIX 2: naked _start trampoline.  This binary ignores argv/envp.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym exit0_main,
    );
}

fn exit0_main() -> ! {
    exit(0)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(101)
}
