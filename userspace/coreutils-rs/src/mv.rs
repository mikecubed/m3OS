//! mv — rename/move files.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, rename, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() < 3 {
        write_str(STDERR_FILENO, "usage: mv <src> <dst>\n");
        return 1;
    }
    let mut src = [0u8; 256];
    let sb = args[1].as_bytes();
    let slen = sb.len().min(255);
    src[..slen].copy_from_slice(&sb[..slen]);
    src[slen] = 0;

    let mut dst = [0u8; 256];
    let db = args[2].as_bytes();
    let dlen = db.len().min(255);
    dst[..dlen].copy_from_slice(&db[..dlen]);
    dst[dlen] = 0;

    if rename(&src[..=slen], &dst[..=dlen]) < 0 {
        write_str(STDERR_FILENO, "mv: rename failed\n");
        return 1;
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
