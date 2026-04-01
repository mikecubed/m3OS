//! uptime — display time since boot.
#![no_std]
#![no_main]

use syscall_lib::{CLOCK_MONOTONIC, STDOUT_FILENO, clock_gettime, write};

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    let (sec, _nsec) = clock_gettime(CLOCK_MONOTONIC);
    if sec < 0 {
        let _ = write(STDOUT_FILENO, b"uptime: clock_gettime failed\n");
        return 1;
    }
    let total = sec as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;

    let _ = write(STDOUT_FILENO, b"up ");

    // Format as H:MM:SS or Xd H:MM:SS
    let mut buf = [0u8; 32];
    let mut pos = 0;

    if hours >= 24 {
        let days = hours / 24;
        let h = hours % 24;
        pos += write_u64_to_buf(days, &mut buf[pos..]);
        buf[pos] = b'd';
        pos += 1;
        buf[pos] = b' ';
        pos += 1;
        pos += write_u64_to_buf(h, &mut buf[pos..]);
    } else {
        pos += write_u64_to_buf(hours, &mut buf[pos..]);
    }
    buf[pos] = b':';
    pos += 1;
    buf[pos] = b'0' + (minutes / 10) as u8;
    pos += 1;
    buf[pos] = b'0' + (minutes % 10) as u8;
    pos += 1;
    buf[pos] = b':';
    pos += 1;
    buf[pos] = b'0' + (seconds / 10) as u8;
    pos += 1;
    buf[pos] = b'0' + (seconds % 10) as u8;
    pos += 1;
    buf[pos] = b'\n';
    pos += 1;

    let _ = write(STDOUT_FILENO, &buf[..pos]);
    0
}

fn write_u64_to_buf(mut n: u64, buf: &mut [u8]) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut len = 0;
    while n > 0 {
        tmp[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    for i in 0..len {
        buf[i] = tmp[len - 1 - i];
    }
    len
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
