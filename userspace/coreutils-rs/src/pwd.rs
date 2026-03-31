//! pwd — print working directory.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, getcwd, write, write_str};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
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
