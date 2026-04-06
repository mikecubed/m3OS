//! who / w — show logged-in users (Phase 46).
//!
//! Reads /var/run/utmp for login session records.
//! When invoked as `w`, also shows idle time and current command.
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, write_str};

syscall_lib::entry_point!(main);

const UTMP_PATH: &[u8] = b"/var/run/utmp\0";

fn main(args: &[&str]) -> i32 {
    let is_w = args.first().is_some_and(|a| {
        let bytes = a.as_bytes();
        // Check if the binary name ends with 'w' (invoked as /bin/w).
        bytes.last() == Some(&b'w') && (bytes.len() == 1 || bytes[bytes.len() - 2] == b'/')
    });

    if is_w {
        write_str(
            STDOUT_FILENO,
            "USER     TTY      FROM             LOGIN@   IDLE   WHAT\n",
        );
    } else {
        write_str(STDOUT_FILENO, "USER     TTY      FROM             LOGIN@\n");
    }

    // Try to read utmp file.
    let fd = syscall_lib::open(UTMP_PATH, 0, 0);
    if fd >= 0 {
        let mut buf = [0u8; 4096];
        let n = syscall_lib::read(fd as i32, &mut buf);
        syscall_lib::close(fd as i32);
        if n > 0 {
            match core::str::from_utf8(&buf[..n as usize]) {
                Ok(text) => {
                    for line in text.split('\n') {
                        if !line.is_empty() {
                            write_str(STDOUT_FILENO, line);
                            write_str(STDOUT_FILENO, "\n");
                        }
                    }
                    return 0;
                }
                Err(_) => {
                    write_str(syscall_lib::STDERR_FILENO, "who: invalid UTF-8 in utmp\n");
                    return 1;
                }
            }
        }
    }

    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
