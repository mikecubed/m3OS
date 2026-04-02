//! readlink — print symbolic link targets.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, readlink as sys_readlink, write, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() != 2 {
        write_str(STDERR_FILENO, "usage: readlink <path>\n");
        return 1;
    }

    let bytes = args[1].as_bytes();
    if bytes.len() > 255 {
        write_str(STDERR_FILENO, "readlink: path too long\n");
        return 1;
    }
    let mut path = [0u8; 256];
    path[..bytes.len()].copy_from_slice(bytes);
    path[bytes.len()] = 0;

    let mut target = [0u8; 256];
    let n = sys_readlink(&path[..=bytes.len()], &mut target);
    if n < 0 {
        write_str(STDERR_FILENO, "readlink: cannot read link\n");
        return 1;
    }
    let _ = write(STDOUT_FILENO, &target[..n as usize]);
    let _ = write(STDOUT_FILENO, b"\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
