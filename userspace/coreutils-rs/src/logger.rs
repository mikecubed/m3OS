//! logger — send messages to the system log (Phase 46).
//!
//! Usage: logger [-t tag] [-p priority] message...
#![no_std]
#![no_main]

use syscall_lib::{AF_UNIX, SOCK_DGRAM, STDERR_FILENO, SockaddrUn, sendto_unix, socket, write_str};

syscall_lib::entry_point!(main);

const DEV_LOG: &str = "/dev/log";

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_str(
            STDERR_FILENO,
            "usage: logger [-t tag] [-p priority] message...\n",
        );
        return 1;
    }

    let mut tag = "user";
    let mut priority = 14u8; // user.info = (1 << 3) | 6... simplified to 14
    let mut msg_start = 1usize;

    // Parse optional flags.
    let mut i = 1;
    while i < args.len() {
        if args[i] == "-t" && i + 1 < args.len() {
            tag = args[i + 1];
            i += 2;
            msg_start = i;
        } else if args[i] == "-p" && i + 1 < args.len() {
            // Parse numeric priority with checked arithmetic.
            let p_bytes = args[i + 1].as_bytes();
            if p_bytes.is_empty() || !p_bytes.iter().all(|b| b.is_ascii_digit()) {
                write_str(STDERR_FILENO, "logger: invalid priority value\n");
                return 1;
            }
            let mut val = 0u16;
            let mut overflow = false;
            for &b in p_bytes {
                val = match val
                    .checked_mul(10)
                    .and_then(|v| v.checked_add((b - b'0') as u16))
                {
                    Some(v) => v,
                    None => {
                        overflow = true;
                        break;
                    }
                };
            }
            if overflow || val > 191 {
                // Max valid syslog priority: facility 23 * 8 + severity 7 = 191
                write_str(STDERR_FILENO, "logger: priority out of range (0-191)\n");
                return 1;
            }
            priority = val as u8;
            i += 2;
            msg_start = i;
        } else {
            break;
        }
    }

    if msg_start >= args.len() {
        write_str(STDERR_FILENO, "logger: no message specified\n");
        return 1;
    }

    // Build the syslog message: <priority>tag: message
    let mut buf = [0u8; 1024];
    let mut pos = 0usize;

    // <priority>
    buf[pos] = b'<';
    pos += 1;
    let p_str = format_u8(priority);
    for &b in p_str.iter().take_while(|&&b| b != 0) {
        buf[pos] = b;
        pos += 1;
    }
    buf[pos] = b'>';
    pos += 1;

    // tag:
    for &b in tag.as_bytes() {
        if pos >= buf.len() - 2 {
            break;
        }
        buf[pos] = b;
        pos += 1;
    }
    buf[pos] = b':';
    pos += 1;
    buf[pos] = b' ';
    pos += 1;

    // message words
    for (j, word) in args[msg_start..].iter().enumerate() {
        if j > 0 && pos < buf.len() - 1 {
            buf[pos] = b' ';
            pos += 1;
        }
        for &b in word.as_bytes() {
            if pos >= buf.len() - 1 {
                break;
            }
            buf[pos] = b;
            pos += 1;
        }
    }

    // Send to /dev/log via Unix datagram socket.
    let fd = socket(AF_UNIX as i32, SOCK_DGRAM as i32, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "logger: cannot create socket\n");
        return 1;
    }

    let addr = SockaddrUn::new(DEV_LOG);
    let ret = sendto_unix(fd as i32, &buf[..pos], 0, &addr);
    syscall_lib::close(fd as i32);

    if ret < 0 {
        write_str(
            STDERR_FILENO,
            "logger: failed to send to /dev/log (is syslogd running?)\n",
        );
        return 1;
    }

    0
}

fn format_u8(mut n: u8) -> [u8; 4] {
    let mut buf = [0u8; 4];
    if n == 0 {
        buf[0] = b'0';
        return buf;
    }
    let mut i = 0;
    let mut tmp = [0u8; 3];
    while n > 0 {
        tmp[i] = b'0' + (n % 10);
        n /= 10;
        i += 1;
    }
    for j in 0..i {
        buf[j] = tmp[i - 1 - j];
    }
    buf
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
