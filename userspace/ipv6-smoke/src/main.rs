//! Phase 91 Track E.1 — `ipv6-smoke`.
//!
//! Always-on, CI-deterministic regression for the dual-stack IPv6 substrate,
//! exercised entirely from ring 3 with no real network (the same role
//! `pku-smoke` plays for PKU). Each case emits a distinct
//! `IPV6_SMOKE:<case>:ok` serial sentinel; a clean run ends with
//! `SMOKE:ipv6-smoke:PASS`. An assertion failure prints
//! `IPV6_SMOKE:<case>:FAIL <reason>` and exits 2; a panic prints
//! `IPV6_SMOKE:panic`.
//!
//! Cases:
//! - `socket`  — `socket(AF_INET6, SOCK_DGRAM/SOCK_STREAM)` succeed; an unknown
//!   family returns an error (A.6).
//! - `bind`    — `bind6([::]:port)` round-trips a `sockaddr_in6` through the
//!   kernel `sockaddr_from_user6` helper (A.6, the `IPV6_BIND_OK` arm).
//! - `loopback`— a `ping6 ::1` echo round-trips via the ICMPv6 socket path +
//!   the kernel's `::1` internal loopback through the real `handle_icmpv6`
//!   echo->reply path (B.1, the `IPV6_LOOPBACK_OK` + `ICMPV6_ECHO_OK` arms).
//!
//! Live SLAAC / NDP-resolve / stateless-DHCPv6-DNS run over QEMU SLIRP
//! `ipv6=on` and are asserted by the `xtask` gate on the kernel's serial log
//! lines, not here. AAAA + RFC 6724 ride the musl resolver path.

#![no_std]
#![no_main]

use syscall_lib::{
    AF_INET6, IPPROTO_ICMPV6, SOCK_DGRAM, SOCK_STREAM, SockaddrIn6, close, exit, read, sendto6,
    socket, write,
};

const STDOUT: i32 = 1;
const UNSPECIFIED: [u8; 16] = [0u8; 16];
const LOOPBACK: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    case_socket();
    case_bind();
    case_loopback();
    emit("SMOKE:ipv6-smoke:PASS\n");
    0
}

/// AF_INET6 socket creation (A.6).
fn case_socket() {
    let dg = socket(AF_INET6 as i32, SOCK_DGRAM as i32, 0);
    if dg < 0 {
        fail("socket", "AF_INET6 SOCK_DGRAM failed");
    }
    close(dg as i32);

    let st = socket(AF_INET6 as i32, SOCK_STREAM as i32, 0);
    if st < 0 {
        fail("socket", "AF_INET6 SOCK_STREAM failed");
    }
    close(st as i32);

    // An unknown address family must be rejected.
    let bad = socket(99, SOCK_DGRAM as i32, 0);
    if bad >= 0 {
        fail("socket", "unknown family unexpectedly succeeded");
    }
    ok("socket");
}

/// bind6 round-trips a sockaddr_in6 (A.6).
fn case_bind() {
    let fd = socket(AF_INET6 as i32, SOCK_DGRAM as i32, 0);
    if fd < 0 {
        fail("bind", "socket failed");
    }
    let fd = fd as i32;
    let addr = SockaddrIn6::new(UNSPECIFIED, 12345);
    if syscall_lib::bind6(fd, &addr) < 0 {
        fail("bind", "bind6 returned error");
    }
    close(fd);
    ok("bind");
}

/// ping6 ::1 echo round-trips via the loopback short-circuit (B.1).
fn case_loopback() {
    let fd = socket(AF_INET6 as i32, SOCK_DGRAM as i32, IPPROTO_ICMPV6 as i32);
    if fd < 0 {
        fail("loopback", "ICMPv6 socket failed");
    }
    let fd = fd as i32;
    let addr = SockaddrIn6::new(LOOPBACK, 0);

    // Echo request: id=1, seq=0, then padding.
    let mut payload = [0u8; 16];
    payload[0] = 0;
    payload[1] = 1; // id = 1
    payload[2] = 0;
    payload[3] = 0; // seq = 0
    let mut i = 4;
    while i < 16 {
        payload[i] = 0xAB;
        i += 1;
    }

    if sendto6(fd, &payload, 0, &addr) < 0 {
        fail("loopback", "sendto6 ::1 failed");
    }
    let mut reply = [0u8; 8];
    let n = read(fd, &mut reply);
    close(fd);
    if n != 8 {
        fail("loopback", "no echo reply from ::1");
    }
    ok("loopback");
}

fn emit(s: &str) {
    let _ = write(STDOUT, s.as_bytes());
}

fn ok(case: &str) {
    emit("IPV6_SMOKE:");
    emit(case);
    emit(":ok\n");
}

fn fail(case: &str, reason: &str) -> ! {
    emit("IPV6_SMOKE:");
    emit(case);
    emit(":FAIL ");
    emit(reason);
    emit("\n");
    exit(2)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    let _ = write(STDOUT, b"IPV6_SMOKE:panic\n");
    exit(101)
}
