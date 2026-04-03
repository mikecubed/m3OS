//! dmesg — print kernel message buffer from /proc/kmsg.
#![no_std]
#![no_main]

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write, write_str};

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    let path = b"/proc/kmsg\0";
    let fd = open(path, O_RDONLY, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "dmesg: cannot open /proc/kmsg\n");
        return 1;
    }
    let fd = fd as i32;
    let mut buf = [0u8; 512];
    loop {
        let n = read(fd, &mut buf);
        if n == 0 {
            break;
        }
        if n < 0 {
            write_str(STDERR_FILENO, "dmesg: read error\n");
            close(fd);
            return 1;
        }
        let mut off = 0usize;
        let n = n as usize;
        while off < n {
            let w = write(STDOUT_FILENO, &buf[off..n]);
            if w <= 0 {
                close(fd);
                return 1;
            }
            off += w as usize;
        }
    }
    close(fd);
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
