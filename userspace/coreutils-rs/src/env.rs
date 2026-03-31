//! env — print environment variables.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, write, write_str};

syscall_lib::entry_point_with_env!(main);

fn main(_args: &[&str], env: &[&str]) -> i32 {
    for e in env {
        let _ = write(STDOUT_FILENO, e.as_bytes());
        write_str(STDOUT_FILENO, "\n");
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
