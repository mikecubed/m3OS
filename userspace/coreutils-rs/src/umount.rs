//! umount — unmount a filesystem.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, umount, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() != 2 {
        write_str(STDERR_FILENO, "usage: umount TARGET\n");
        return 1;
    }
    let bytes = args[1].as_bytes();
    if bytes.len() > 255 {
        write_str(STDERR_FILENO, "umount: path too long\n");
        return 1;
    }
    let mut path = [0u8; 256];
    path[..bytes.len()].copy_from_slice(bytes);
    path[bytes.len()] = 0;
    if umount(&path[..=bytes.len()]) != 0 {
        write_str(STDERR_FILENO, "umount: failed\n");
        return 1;
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
