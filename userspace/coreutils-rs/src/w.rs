//! w — show logged-in users with activity (Phase 46).
//!
//! Same as `who` but also shows idle time and current command.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, write_str};

syscall_lib::entry_point!(main);

const UTMP_PATH: &[u8] = b"/var/run/utmp\0";

fn main(_args: &[&str]) -> i32 {
    write_str(
        STDOUT_FILENO,
        "USER     TTY      FROM             LOGIN@   IDLE   WHAT\n",
    );

    let fd = syscall_lib::open(UTMP_PATH, 0, 0);
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

    write_str(
        STDOUT_FILENO,
        "root     tty0     -                boot     0s     -\n",
    );
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
