//! rm — remove files.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, unlink, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_str(STDERR_FILENO, "usage: rm <file>\n");
        return 1;
    }
    let mut ret = 0;
    for arg in &args[1..] {
        let bytes = arg.as_bytes();
        if bytes.len() > 255 {
            write_str(STDERR_FILENO, "rm: path too long\n");
            ret = 1;
            continue;
        }
        // Build a null-terminated path on the stack.
        let mut path = [0u8; 256];
        let len = bytes.len();
        path[..len].copy_from_slice(bytes);
        path[len] = 0;
        if unlink(&path[..=len]) < 0 {
            write_str(STDERR_FILENO, "rm: failed\n");
            ret = 1;
        }
    }
    ret
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
