//! meminfo — display kernel memory statistics.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, meminfo, write};

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    let mut buf = [0u8; 4096];
    let n = meminfo(&mut buf);
    if n > 0 {
        let _ = write(STDOUT_FILENO, &buf[..n]);
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
