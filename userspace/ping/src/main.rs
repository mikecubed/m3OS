//! Userspace ping utility — Phase 23.
//!
//! Sends ICMP echo requests via a DGRAM/ICMP socket and prints round-trip
//! times.  Default target is 10.0.2.2 (QEMU gateway).

#![no_std]
#![no_main]

use syscall_lib::{
    close, exit, nanosleep, read, sendto, socket, write_str, write_u64, SockaddrIn, AF_INET,
    IPPROTO_ICMP, SOCK_DGRAM, STDOUT_FILENO,
};

const DEFAULT_TARGET: [u8; 4] = [10, 0, 2, 2];
const PING_COUNT: u16 = 4;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let target = DEFAULT_TARGET;

    // Print header
    write_str(STDOUT_FILENO, "PING ");
    print_ip(target);
    write_str(STDOUT_FILENO, "\n");

    // Open ICMP DGRAM socket
    let fd = socket(AF_INET as i32, SOCK_DGRAM as i32, IPPROTO_ICMP as i32);
    if fd < 0 {
        write_str(STDOUT_FILENO, "ping: socket() failed\n");
        exit(1);
    }
    let fd = fd as i32;

    let addr = SockaddrIn::new(target, 0);
    let mut received = 0u16;

    for seq in 0..PING_COUNT {
        // Build echo request payload: id(2) + seq(2) + padding
        let id: u16 = 1;
        let mut payload = [0u8; 36]; // 4 bytes header + 32 bytes padding
        payload[0] = (id >> 8) as u8;
        payload[1] = id as u8;
        payload[2] = (seq >> 8) as u8;
        payload[3] = seq as u8;
        // Fill padding
        let mut i = 4;
        while i < 36 {
            payload[i] = 0xAB;
            i += 1;
        }

        // Record send time via a debug syscall (we use tick count from reply)
        let send_tick = get_tick();

        // Send echo request
        let sent = sendto(fd, &payload, 0, &addr);
        if sent < 0 {
            write_str(STDOUT_FILENO, "ping: sendto() failed\n");
            continue;
        }

        // Wait for reply (recvfrom returns the tick count as data)
        let mut reply_buf = [0u8; 8];
        let n = read(fd, &mut reply_buf);
        if n < 0 {
            write_str(STDOUT_FILENO, "ping: read error\n");
        } else if n == 8 {
            let reply_tick = u64::from_le_bytes(reply_buf);
            let rtt = reply_tick.wrapping_sub(send_tick);
            // Convert ticks to approximate ms (PIT at ~100 Hz → 1 tick ≈ 10ms)
            let rtt_ms = rtt * 10;

            write_str(STDOUT_FILENO, "Reply from ");
            print_ip(target);
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

        // Wait 1 second between pings
        if seq + 1 < PING_COUNT {
            nanosleep(1);
        }
    }

    // Summary
    write_str(STDOUT_FILENO, "--- ");
    print_ip(target);
    write_str(STDOUT_FILENO, " ping statistics ---\n");
    write_u64(STDOUT_FILENO, PING_COUNT as u64);
    write_str(STDOUT_FILENO, " packets transmitted, ");
    write_u64(STDOUT_FILENO, received as u64);
    write_str(STDOUT_FILENO, " received\n");

    close(fd);
    exit(0)
}

fn print_ip(ip: [u8; 4]) {
    write_u64(STDOUT_FILENO, ip[0] as u64);
    write_str(STDOUT_FILENO, ".");
    write_u64(STDOUT_FILENO, ip[1] as u64);
    write_str(STDOUT_FILENO, ".");
    write_u64(STDOUT_FILENO, ip[2] as u64);
    write_str(STDOUT_FILENO, ".");
    write_u64(STDOUT_FILENO, ip[3] as u64);
}

/// Read the PIT tick count via clock_gettime(CLOCK_MONOTONIC).
fn get_tick() -> u64 {
    let mut ts = [0u64; 2]; // tv_sec, tv_nsec
    unsafe {
        syscall_lib::syscall2(
            syscall_lib::SYS_CLOCK_GETTIME,
            syscall_lib::CLOCK_MONOTONIC,
            ts.as_mut_ptr() as u64,
        );
    }
    ts[0] * 100
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(101)
}
