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
    let sb = args[1].as_bytes();
    if sb.len() > 255 {
        write_str(STDERR_FILENO, "mv: source path too long\n");
        return 1;
    }
    let mut src = [0u8; 256];
    let slen = sb.len();
    src[..slen].copy_from_slice(sb);
    src[slen] = 0;

    let db = args[2].as_bytes();
    if db.len() > 255 {
        write_str(STDERR_FILENO, "mv: destination path too long\n");
        return 1;
    }
    let mut dst = [0u8; 256];
    let dlen = db.len();
    dst[..dlen].copy_from_slice(db);
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
