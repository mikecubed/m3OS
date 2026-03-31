//! cat — concatenate files to stdout.
#![no_std]
#![no_main]

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() <= 1 {
        cat_fd(0);
        return 0;
    }
    let mut ret = 0;
    for arg in &args[1..] {
        let bytes = arg.as_bytes();
        if bytes.len() > 255 {
            write_str(STDERR_FILENO, "cat: path too long\n");
            ret = 1;
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;
        let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
        if fd < 0 {
            write_str(STDERR_FILENO, "cat: cannot open file\n");
            ret = 1;
            continue;
        }
        cat_fd(fd as i32);
        close(fd as i32);
    }
    ret
}

fn cat_fd(fd: i32) {
    let mut buf = [0u8; 4096];
    loop {
        let n = read(fd, &mut buf);
        if n <= 0 {
            break;
        }
        write_all(STDOUT_FILENO, &buf[..n as usize]);
    }
}

fn write_all(fd: i32, data: &[u8]) {
    let mut off = 0;
    while off < data.len() {
        let w = write(fd, &data[off..]);
        if w <= 0 {
            break;
        }
        off += w as usize;
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
