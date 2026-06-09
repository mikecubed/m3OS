//! true — exit 0.
#![no_std]
#![no_main]

// Phase 86f FIX 2: naked _start trampoline so RSP ≡ 0 mod 16 at entry does
// not misalign SSE-enabled stack spills.  This binary ignores argv/envp so
// we use a bare `call true_main` without saving RSP.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym true_main,
    );
}

fn true_main() -> ! {
    syscall_lib::exit(0)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
