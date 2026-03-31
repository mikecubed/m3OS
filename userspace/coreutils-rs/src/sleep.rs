//! sleep — sleep for N seconds.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, nanosleep, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_str(STDERR_FILENO, "usage: sleep <seconds>\n");
        return 1;
    }
    let secs = parse_u64(args[1].as_bytes());
    nanosleep(secs);
    0
}

fn parse_u64(s: &[u8]) -> u64 {
    let mut n: u64 = 0;
    for &b in s {
        if b.is_ascii_digit() {
            n = n.wrapping_mul(10).wrapping_add((b - b'0') as u64);
        } else {
            break;
        }
    }
    n
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
