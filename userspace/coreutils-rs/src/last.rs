//! last — show recent login history (Phase 46).
//!
//! Reads /var/log/wtmp for login/logout records.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, write_str};

syscall_lib::entry_point!(main);

const WTMP_PATH: &[u8] = b"/var/log/wtmp\0";

fn main(_args: &[&str]) -> i32 {
    write_str(
        STDOUT_FILENO,
        "USER     TTY      FROM             LOGIN            LOGOUT           DURATION\n",
    );

    let fd = syscall_lib::open(WTMP_PATH, 0, 0);
    if fd >= 0 {
        let mut buf = [0u8; 4096];
        let n = syscall_lib::read(fd as i32, &mut buf);
        syscall_lib::close(fd as i32);
        if n > 0 {
            let text = unsafe { core::str::from_utf8_unchecked(&buf[..n as usize]) };
            for line in text.split('\n') {
                if !line.is_empty() {
                    write_str(STDOUT_FILENO, line);
                    write_str(STDOUT_FILENO, "\n");
                }
            }
            return 0;
        }
    }

    write_str(STDOUT_FILENO, "(no login records found)\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
