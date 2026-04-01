//! touch — create files or update modification timestamps.
#![no_std]
#![no_main]

use syscall_lib::{O_CREAT, O_WRONLY, STDERR_FILENO, close, open, stat, utimensat_now, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_str(STDERR_FILENO, "usage: touch FILE...\n");
        return 1;
    }
    let mut ret = 0;
    for arg in &args[1..] {
        let bytes = arg.as_bytes();
        if bytes.len() > 254 {
            write_str(STDERR_FILENO, "touch: path too long\n");
            ret = 1;
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;
        let path_z = &path[..=bytes.len()];

        // Try to stat — if file exists, update timestamps.
        let mut st = syscall_lib::Stat::zeroed();
        if stat(path_z, &mut st) >= 0 {
            if utimensat_now(path_z) < 0 {
                write_str(STDERR_FILENO, "touch: cannot update timestamps: ");
                write_str(STDERR_FILENO, arg);
                write_str(STDERR_FILENO, "\n");
                ret = 1;
            }
        } else {
            // File doesn't exist — create it.
            let fd = open(path_z, O_WRONLY | O_CREAT, 0o644);
            if fd < 0 {
                write_str(STDERR_FILENO, "touch: cannot create: ");
                write_str(STDERR_FILENO, arg);
                write_str(STDERR_FILENO, "\n");
                ret = 1;
            } else {
                close(fd as i32);
            }
        }
    }
    ret
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
