//! Userspace `ping6` — Phase 91.
//!
//! Sends ICMPv6 echo requests via an `AF_INET6` DGRAM/ICMPv6 socket and prints
//! round-trip times. The default target is `::1`, served by the kernel's
//! ICMPv6 loopback short-circuit (m3OS has no routed `lo`). An optional argv
//! target (e.g. `ping6 fec0::2`) pings over the wire after NDP resolution.

#![no_std]
#![no_main]

use syscall_lib::{
    AF_INET6, IPPROTO_ICMPV6, SOCK_DGRAM, STDOUT_FILENO, SockaddrIn6, close, exit, nanosleep, read,
    sendto6, socket, write_str, write_u64,
};

const PING_COUNT: u16 = 4;
const LOOPBACK: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    // argv[0] is the program name; argv[1] (if present) is the target literal.
    let target = match args.get(1) {
        Some(s) => match parse_ipv6(s) {
            Some(a) => a,
            None => {
                write_str(STDOUT_FILENO, "ping6: invalid IPv6 address\n");
                return 1;
            }
        },
        None => LOOPBACK,
    };

    write_str(STDOUT_FILENO, "PING6 ");
    print_ipv6(&target);
    write_str(STDOUT_FILENO, "\n");

    let fd = socket(AF_INET6 as i32, SOCK_DGRAM as i32, IPPROTO_ICMPV6 as i32);
    if fd < 0 {
        write_str(STDOUT_FILENO, "ping6: socket() failed\n");
        return 1;
    }
    let fd = fd as i32;
    let addr = SockaddrIn6::new(target, 0);
    let mut received = 0u16;

    for seq in 0..PING_COUNT {
        // Echo request payload: id(2) + seq(2) + 32 bytes padding.
        let id: u16 = 1;
        let mut payload = [0u8; 36];
        payload[0] = (id >> 8) as u8;
        payload[1] = id as u8;
        payload[2] = (seq >> 8) as u8;
        payload[3] = seq as u8;
        let mut i = 4;
        while i < 36 {
            payload[i] = 0xAB;
            i += 1;
        }

        let send_tick = get_tick();
        if sendto6(fd, &payload, 0, &addr) < 0 {
            write_str(STDOUT_FILENO, "ping6: sendto6() failed\n");
            continue;
        }

        let mut reply_buf = [0u8; 8];
        let n = read(fd, &mut reply_buf);
        if n == 8 {
            let reply_tick = u64::from_le_bytes(reply_buf);
            let rtt_ms = reply_tick.wrapping_sub(send_tick) * 10;
            write_str(STDOUT_FILENO, "Reply from ");
            print_ipv6(&target);
            write_str(STDOUT_FILENO, ": seq=");
            write_u64(STDOUT_FILENO, seq as u64);
            write_str(STDOUT_FILENO, " time=");
            write_u64(STDOUT_FILENO, rtt_ms);
            write_str(STDOUT_FILENO, "ms\n");
            received += 1;
        } else {
            write_str(STDOUT_FILENO, "Request timed out seq=");
            write_u64(STDOUT_FILENO, seq as u64);
            write_str(STDOUT_FILENO, "\n");
        }

        if seq + 1 < PING_COUNT {
            nanosleep(1);
        }
    }

    write_str(STDOUT_FILENO, "--- ");
    print_ipv6(&target);
    write_str(STDOUT_FILENO, " ping6 statistics ---\n");
    write_u64(STDOUT_FILENO, PING_COUNT as u64);
    write_str(STDOUT_FILENO, " packets transmitted, ");
    write_u64(STDOUT_FILENO, received as u64);
    write_str(STDOUT_FILENO, " received\n");

    close(fd);
    if received > 0 { 0 } else { 1 }
}

/// Parse an IPv6 literal (with a single `::` compression) into 16 octets.
fn parse_ipv6(s: &str) -> Option<[u8; 16]> {
    let bytes = s.as_bytes();
    // Locate a "::" compression marker (at most one).
    let mut dcolon: Option<usize> = None;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b':' && bytes[i + 1] == b':' {
            dcolon = Some(i);
            break;
        }
        i += 1;
    }

    let mut head = [0u16; 8];
    let mut tail = [0u16; 8];
    let mut nhead = 0usize;
    let mut ntail = 0usize;

    let (head_str, tail_str) = match dcolon {
        Some(p) => (&s[..p], &s[p + 2..]),
        None => (s, ""),
    };

    if !parse_groups(head_str, &mut head, &mut nhead) {
        return None;
    }
    if dcolon.is_some() {
        if !parse_groups(tail_str, &mut tail, &mut ntail) {
            return None;
        }
    } else if nhead != 8 {
        return None; // no "::" requires exactly 8 groups
    }
    if nhead + ntail > 8 {
        return None;
    }

    let mut out = [0u8; 16];
    for (g, &v) in head[..nhead].iter().enumerate() {
        out[g * 2] = (v >> 8) as u8;
        out[g * 2 + 1] = v as u8;
    }
    // Tail groups are right-aligned into the last 8 - ntail .. 8 group slots.
    let tail_start = 8 - ntail;
    for (k, &v) in tail[..ntail].iter().enumerate() {
        let g = tail_start + k;
        out[g * 2] = (v >> 8) as u8;
        out[g * 2 + 1] = v as u8;
    }
    Some(out)
}

/// Parse a colon-separated list of hex groups (no `::`). Empty string yields 0
/// groups. Returns false on malformed input.
fn parse_groups(s: &str, out: &mut [u16; 8], n: &mut usize) -> bool {
    if s.is_empty() {
        return true;
    }
    for part in s.split(':') {
        if part.is_empty() || part.len() > 4 || *n >= 8 {
            return false;
        }
        let mut v: u16 = 0;
        for &c in part.as_bytes() {
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return false,
            };
            v = (v << 4) | d as u16;
        }
        out[*n] = v;
        *n += 1;
    }
    true
}

fn print_ipv6(addr: &[u8; 16]) {
    for g in 0..8 {
        if g > 0 {
            write_str(STDOUT_FILENO, ":");
        }
        let v = ((addr[g * 2] as u16) << 8) | addr[g * 2 + 1] as u16;
        print_hex(v);
    }
}

fn print_hex(mut v: u16) {
    let digits = b"0123456789abcdef";
    let mut buf = [0u8; 4];
    let mut started = false;
    let mut pos = 0;
    for shift in [12, 8, 4, 0] {
        let nyb = ((v >> shift) & 0xf) as usize;
        if nyb != 0 || started || shift == 0 {
            buf[pos] = digits[nyb];
            pos += 1;
            started = true;
        }
        let _ = &mut v;
    }
    if let Ok(s) = core::str::from_utf8(&buf[..pos]) {
        write_str(STDOUT_FILENO, s);
    }
}

fn get_tick() -> u64 {
    let mut ts = [0u64; 2];
    let ret = unsafe {
        syscall_lib::syscall2(
            syscall_lib::SYS_CLOCK_GETTIME,
            syscall_lib::CLOCK_MONOTONIC,
            ts.as_mut_ptr() as u64,
        )
    };
    if ret as i64 >= 0 { ts[0] * 100 } else { 0 }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(101)
}
