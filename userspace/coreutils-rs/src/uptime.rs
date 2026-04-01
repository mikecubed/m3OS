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

    // Format as "up H:MM:SS\n" or "up Xd H:MM:SS\n"
    // Max: "up 18446744073709551615d 23:59:59\n" = 38 chars — 48 bytes is safe.
    let mut buf = [0u8; 48];
    let mut pos = 0;

    let put = |buf: &mut [u8], pos: &mut usize, b: u8| {
        if *pos < buf.len() {
            buf[*pos] = b;
            *pos += 1;
        }
    };
    let put_u64 = |buf: &mut [u8], pos: &mut usize, n: u64| {
        if n == 0 {
            put(buf, pos, b'0');
            return;
        }
        let mut tmp = [0u8; 20];
        let mut len = 0usize;
        let mut v = n;
        while v > 0 {
            tmp[len] = b'0' + (v % 10) as u8;
            v /= 10;
            len += 1;
        }
        for i in (0..len).rev() {
            put(buf, pos, tmp[i]);
        }
    };

    // "up "
    for &b in b"up " {
        put(&mut buf, &mut pos, b);
    }

    if hours >= 24 {
        put_u64(&mut buf, &mut pos, hours / 24);
        put(&mut buf, &mut pos, b'd');
        put(&mut buf, &mut pos, b' ');
        put_u64(&mut buf, &mut pos, hours % 24);
    } else {
        put_u64(&mut buf, &mut pos, hours);
    }
    put(&mut buf, &mut pos, b':');
    put(&mut buf, &mut pos, b'0' + (minutes / 10) as u8);
    put(&mut buf, &mut pos, b'0' + (minutes % 10) as u8);
    put(&mut buf, &mut pos, b':');
    put(&mut buf, &mut pos, b'0' + (seconds / 10) as u8);
    put(&mut buf, &mut pos, b'0' + (seconds % 10) as u8);
    put(&mut buf, &mut pos, b'\n');

    let _ = write(STDOUT_FILENO, &buf[..pos]);
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
