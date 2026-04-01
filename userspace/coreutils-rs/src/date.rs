//! date — display current date and time.
#![no_std]
#![no_main]

use syscall_lib::{CLOCK_REALTIME, STDOUT_FILENO, clock_gettime, format_datetime, gmtime, write};

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    let (sec, _nsec) = clock_gettime(CLOCK_REALTIME);
    if sec < 0 {
        let _ = write(STDOUT_FILENO, b"date: clock_gettime failed\n");
        return 1;
    }
    let dt = gmtime(sec as u64);
    let mut buf = [0u8; 64];
    let n = format_datetime(&dt, &mut buf);
    let _ = write(STDOUT_FILENO, &buf[..n]);
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
