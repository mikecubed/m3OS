//! echo — print arguments to stdout.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, write, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    for (i, arg) in args[1..].iter().enumerate() {
        if i > 0 {
            write_str(STDOUT_FILENO, " ");
        }
        let _ = write(STDOUT_FILENO, arg.as_bytes());
    }
    write_str(STDOUT_FILENO, "\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
