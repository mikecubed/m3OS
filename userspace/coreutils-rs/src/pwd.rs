//! pwd — print working directory.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, getcwd, write, write_str};

// Phase 86f FIX 2: naked _start trampoline.  This binary ignores argv/envp.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym pwd_main,
    );
}

fn pwd_main() -> ! {
    let mut buf = [0u8; 256];
    let ret = getcwd(&mut buf);
    if ret >= 0 {
        // Find the null terminator or use full buffer.
        let len = buf.iter().position(|&b| b == 0).unwrap_or(ret as usize);
        let _ = write(STDOUT_FILENO, &buf[..len]);
        write_str(STDOUT_FILENO, "\n");
    }
    syscall_lib::exit(0)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
