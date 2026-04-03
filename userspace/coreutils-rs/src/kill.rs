//! kill — send a signal to a process.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, kill, write_str};

syscall_lib::entry_point!(main);

/// Parse a decimal integer string. Returns `None` on empty or non-digit input.
fn parse_int(s: &str) -> Option<i32> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (neg, digits) = if bytes[0] == b'-' {
        (true, &bytes[1..])
    } else {
        (false, bytes)
    };
    if digits.is_empty() {
        return None;
    }
    let mut val: i32 = 0;
    for &b in digits {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.wrapping_mul(10).wrapping_add((b - b'0') as i32);
    }
    Some(if neg { -val } else { val })
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_str(STDERR_FILENO, "usage: kill [-SIGNAL] PID...\n");
        return 1;
    }

    // kill -l: list signal names.
    if args[1] == "-l" {
        write_str(STDOUT_FILENO, "HUP INT KILL TERM CHLD CONT STOP TSTP\n");
        return 0;
    }

    let mut sig = 15i32; // SIGTERM
    let mut argi = 1usize;

    if args[1].starts_with('-') && args[1].len() > 1 {
        let sig_str = &args[1][1..];
        match parse_int(sig_str) {
            Some(s) => sig = s,
            None => {
                write_str(STDERR_FILENO, "kill: invalid signal\n");
                return 1;
            }
        }
        argi = 2;
    }

    if argi >= args.len() {
        write_str(STDERR_FILENO, "usage: kill [-SIGNAL] PID...\n");
        return 1;
    }

    let mut status = 0i32;
    for arg in &args[argi..] {
        match parse_int(arg) {
            Some(pid) if pid > 0 => {
                if kill(pid, sig) != 0 {
                    write_str(STDERR_FILENO, "kill: failed\n");
                    status = 1;
                }
            }
            _ => {
                write_str(STDERR_FILENO, "kill: invalid pid\n");
                status = 1;
            }
        }
    }
    status
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
