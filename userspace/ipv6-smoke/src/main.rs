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
    AF_INET6, IPPROTO_ICMPV6, SOCK_DGRAM, SOCK_STREAM, SYS_RECVMSG, SockaddrIn6, accept, bind6,
    close, connect6, exit, listen, read, sendto6, socket, write,
};

const STDOUT: i32 = 1;
const UNSPECIFIED: [u8; 16] = [0u8; 16];
const LOOPBACK: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    case_socket();
    case_bind();
    case_loopback();
    case_tcp();
    case_recvmsg();
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

/// Full dual-stack TCP over IPv6 via the `::1` internal loopback (Phase 91): a
/// listening v6 TCP socket + a client `connect6(::1)` complete the three-way
/// handshake through the kernel's self-address loopback (synchronously, since
/// `ipv6::send_from` re-injects self-addressed packets), then a byte payload
/// round-trips client → server. Exercises the family-aware `TcpConnection`,
/// the IPv6 pseudo-header checksum, and `handle_tcp_v6` end-to-end.
fn case_tcp() {
    const PORT: u16 = 0x3000;

    let srv = socket(AF_INET6 as i32, SOCK_STREAM as i32, 0);
    if srv < 0 {
        fail("tcp", "server socket");
    }
    let srv = srv as i32;
    let bind_addr = SockaddrIn6::new(UNSPECIFIED, PORT);
    if bind6(srv, &bind_addr) < 0 {
        fail("tcp", "bind6");
    }
    if listen(srv, 1) < 0 {
        fail("tcp", "listen");
    }

    let cli = socket(AF_INET6 as i32, SOCK_STREAM as i32, 0);
    if cli < 0 {
        fail("tcp", "client socket");
    }
    let cli = cli as i32;
    let conn_addr = SockaddrIn6::new(LOOPBACK, PORT);
    // Blocking connect — the internal loopback drives the whole handshake
    // synchronously, so this returns once both ends are Established.
    if connect6(cli, &conn_addr) < 0 {
        fail("tcp", "connect6 ::1");
    }

    let conn = accept(srv, None);
    if conn < 0 {
        fail("tcp", "accept");
    }
    let conn = conn as i32;

    let msg = b"PING6TCP";
    if write(cli, msg) < 0 {
        fail("tcp", "write");
    }
    let mut buf = [0u8; 16];
    let n = read(conn, &mut buf);
    if n < 8 || &buf[..8] != msg {
        fail("tcp", "data round-trip mismatch");
    }
    // The round-trip succeeded — emit before teardown. A graceful close over the
    // synchronous `::1` loopback would half-close-deadlock (the peer's close
    // cannot run on this single thread), so let process exit reap the fds.
    ok("tcp");
}

/// `recvmsg` over AF_INET6 UDP (Phase 91 `sys_recvmsg_inet6`): a v6 UDP datagram
/// sent to `::1` loops back internally and is read with `recvmsg`, asserting the
/// payload plus that `msg_name` was filled with a 28-byte `sockaddr_in6`
/// (`sin6_family == AF_INET6`, `msg_namelen == 28`).
fn case_recvmsg() {
    const PORT: u16 = 0x3100;

    let rfd = socket(AF_INET6 as i32, SOCK_DGRAM as i32, 0);
    if rfd < 0 {
        fail("recvmsg", "recv socket");
    }
    let rfd = rfd as i32;
    if bind6(rfd, &SockaddrIn6::new(UNSPECIFIED, PORT)) < 0 {
        fail("recvmsg", "bind6");
    }

    let sfd = socket(AF_INET6 as i32, SOCK_DGRAM as i32, 0);
    if sfd < 0 {
        fail("recvmsg", "send socket");
    }
    let sfd = sfd as i32;
    let payload = b"UDP6MSG!";
    if sendto6(sfd, payload, 0, &SockaddrIn6::new(LOOPBACK, PORT)) < 0 {
        fail("recvmsg", "sendto6 ::1");
    }

    // Hand-build `struct msghdr` (56 bytes) + one `struct iovec` (16 bytes).
    let mut data = [0u8; 64];
    let mut name = [0u8; 28];
    let mut iov = [0u8; 16];
    iov[0..8].copy_from_slice(&(data.as_mut_ptr() as u64).to_ne_bytes()); // iov_base
    iov[8..16].copy_from_slice(&(data.len() as u64).to_ne_bytes()); // iov_len
    let mut mh = [0u8; 56];
    mh[0..8].copy_from_slice(&(name.as_mut_ptr() as u64).to_ne_bytes()); // msg_name
    mh[8..12].copy_from_slice(&28u32.to_ne_bytes()); // msg_namelen
    mh[16..24].copy_from_slice(&(iov.as_mut_ptr() as u64).to_ne_bytes()); // msg_iov
    mh[24..32].copy_from_slice(&1u64.to_ne_bytes()); // msg_iovlen

    let n = unsafe {
        syscall_lib::syscall3(SYS_RECVMSG, rfd as u64, mh.as_mut_ptr() as u64, 0) as isize
    };
    if n < 8 || &data[..8] != payload {
        fail("recvmsg", "payload round-trip mismatch");
    }
    // sin6_family at offset 0 of the filled msg_name.
    let fam = u16::from_ne_bytes([name[0], name[1]]);
    if fam != AF_INET6 as u16 {
        fail("recvmsg", "msg_name not AF_INET6");
    }
    // msghdr is value-result: msg_namelen written back at offset 8 must be 28.
    let namelen = u32::from_ne_bytes([mh[8], mh[9], mh[10], mh[11]]);
    if namelen != 28 {
        fail("recvmsg", "msg_namelen != 28");
    }

    close(sfd);
    close(rfd);
    ok("recvmsg");
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
