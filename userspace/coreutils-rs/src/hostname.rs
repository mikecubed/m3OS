//! hostname — display or set the system hostname (Phase 46).
//!
//! Usage: hostname [newname]
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, write_str};

syscall_lib::entry_point!(main);

const HOSTNAME_PATH: &[u8] = b"/etc/hostname\0";

fn main(args: &[&str]) -> i32 {
    if args.len() > 1 {
        // Set hostname (root only).
        if syscall_lib::getuid() != 0 {
            write_str(STDERR_FILENO, "hostname: must be root to set hostname\n");
            return 1;
        }
        let name = args[1];
        let fd = syscall_lib::open(
            HOSTNAME_PATH,
            syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC,
            0o644,
        );
        if fd < 0 {
            write_str(STDERR_FILENO, "hostname: cannot write /etc/hostname\n");
            return 1;
        }
        syscall_lib::write(fd as i32, name.as_bytes());
        syscall_lib::write(fd as i32, b"\n");
        syscall_lib::close(fd as i32);
        return 0;
    }

    // Display hostname.
    let fd = syscall_lib::open(HOSTNAME_PATH, 0, 0);
    if fd >= 0 {
        let mut buf = [0u8; 256];
        let n = syscall_lib::read(fd as i32, &mut buf);
        syscall_lib::close(fd as i32);
        if n > 0 {
            let data = &buf[..n as usize];
            // Trim trailing newline.
            let end = data.iter().position(|&b| b == b'\n').unwrap_or(n as usize);
            let name = unsafe { core::str::from_utf8_unchecked(&data[..end]) };
            write_str(STDOUT_FILENO, name);
            write_str(STDOUT_FILENO, "\n");
            return 0;
        }
    }

    // Default hostname.
    write_str(STDOUT_FILENO, "m3os\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
