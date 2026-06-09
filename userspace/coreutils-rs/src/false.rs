//! false — exit 1.
#![no_std]
#![no_main]

// Phase 86f FIX 2: naked _start trampoline.  This binary ignores argv/envp.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym false_main,
    );
}

fn false_main() -> ! {
    syscall_lib::exit(1)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
