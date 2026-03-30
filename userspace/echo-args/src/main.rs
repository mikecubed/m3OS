//! Echo-args: reads argc/argv from the SysV ABI stack and prints them.
//!
//! Validation: P11-T020 — program reads argc/argv via ABI, writes to serial.
#![no_std]
#![no_main]

use syscall_lib::serial_print;

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    serial_print("echo-args: argc=");
    print_usize(args.len());
    serial_print("\n");

    for (i, arg) in args.iter().enumerate() {
        serial_print("echo-args: argv[");
        print_usize(i);
        serial_print("]=");
        serial_print(arg);
        serial_print("\n");
    }

    0
}

fn print_usize(n: usize) {
    let mut buf = [0u8; 20];
    let mut pos = 20usize;
    if n == 0 {
        serial_print("0");
        return;
    }
    let mut v = n;
    while v > 0 {
        pos -= 1;
        buf[pos] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    if let Ok(s) = core::str::from_utf8(&buf[pos..]) {
        serial_print(s);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(100)
}
