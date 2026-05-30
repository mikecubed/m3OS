#![no_std]
#![no_main]

//! Phase 77 Track F.1 — `epoll_*` verification smoke test.
//!
//! The three handlers (`epoll_create1`/`epoll_ctl`/`epoll_wait`) already exist
//! in the kernel; this binary proves the full path end to end against a pipe:
//! ADD a readable fd, make it readable, assert `epoll_wait` reports it with the
//! correct event mask and `data` token, then exercise `EPOLL_CTL_MOD`,
//! `EPOLL_CTL_DEL`, and the timeout path. Prints `EPOLL_SMOKE:PASS` or
//! `EPOLL_SMOKE:FAIL <detail>`.

use syscall_lib::{STDOUT_FILENO, close, exit, pipe, read, syscall1, syscall6, write, write_str};

const SYS_EPOLL_WAIT: u64 = 232;
const SYS_EPOLL_CTL: u64 = 233;
const SYS_EPOLL_CREATE1: u64 = 291;

const EPOLLIN: u32 = 0x001;
const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;
const EPOLL_CTL_MOD: u64 = 3;
const EPOLL_CLOEXEC: u64 = 0x8_0000;

// The kernel reads/writes a packed `struct epoll_event { u32 events; u64 data; }`
// = 12 bytes (no padding). Build it by hand to match exactly.
fn encode_event(events: u32, data: u64) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&events.to_ne_bytes());
    buf[4..12].copy_from_slice(&data.to_ne_bytes());
    buf
}

fn decode_event(buf: &[u8; 12]) -> (u32, u64) {
    let events = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let data = u64::from_ne_bytes([
        buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11],
    ]);
    (events, data)
}

fn epoll_create1(flags: u64) -> i64 {
    unsafe { syscall1(SYS_EPOLL_CREATE1, flags) as i64 }
}

fn epoll_ctl(epfd: i32, op: u64, fd: i32, event: *const u8) -> i64 {
    unsafe {
        syscall6(
            SYS_EPOLL_CTL,
            epfd as u64,
            op,
            fd as u64,
            event as u64,
            0,
            0,
        ) as i64
    }
}

fn epoll_wait(epfd: i32, events: *mut u8, maxevents: i32, timeout_ms: i64) -> i64 {
    unsafe {
        syscall6(
            SYS_EPOLL_WAIT,
            epfd as u64,
            events as u64,
            maxevents as u64,
            timeout_ms as u64,
            0,
            0,
        ) as i64
    }
}

fn fail(detail: &str) -> ! {
    write_str(STDOUT_FILENO, "EPOLL_SMOKE:FAIL ");
    write_str(STDOUT_FILENO, detail);
    write_str(STDOUT_FILENO, "\n");
    exit(1)
}

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    // 1. Create the epoll instance.
    let epfd = epoll_create1(EPOLL_CLOEXEC);
    if epfd < 0 {
        fail("epoll_create1");
    }
    let epfd = epfd as i32;

    // 2. A pipe gives us a deterministically-readable fd.
    let mut fds = [0i32; 2];
    if pipe(&mut fds) < 0 {
        fail("pipe");
    }
    let (rd, wr) = (fds[0], fds[1]);

    // 3. Register the read end for EPOLLIN with a recognisable token.
    let ev = encode_event(EPOLLIN, 0x1234);
    if epoll_ctl(epfd, EPOLL_CTL_ADD, rd, ev.as_ptr()) < 0 {
        fail("ctl_add");
    }

    // 4. Make it readable, then assert epoll_wait reports it.
    if write(wr, b"x") != 1 {
        fail("write1");
    }
    let mut out = [0u8; 12];
    let n = epoll_wait(epfd, out.as_mut_ptr(), 1, 1000);
    if n != 1 {
        fail("wait_add_count");
    }
    let (got_events, got_data) = decode_event(&out);
    if got_events & EPOLLIN == 0 {
        fail("wait_add_mask");
    }
    if got_data != 0x1234 {
        fail("wait_add_data");
    }
    // Drain the byte so the fd is no longer readable.
    let mut buf = [0u8; 8];
    let _ = read(rd, &mut buf);

    // 5. EPOLL_CTL_MOD — change the token, prove the new token is reported.
    let ev2 = encode_event(EPOLLIN, 0x5678);
    if epoll_ctl(epfd, EPOLL_CTL_MOD, rd, ev2.as_ptr()) < 0 {
        fail("ctl_mod");
    }
    if write(wr, b"y") != 1 {
        fail("write2");
    }
    let n = epoll_wait(epfd, out.as_mut_ptr(), 1, 1000);
    if n != 1 {
        fail("wait_mod_count");
    }
    let (_, got_data) = decode_event(&out);
    if got_data != 0x5678 {
        fail("wait_mod_data");
    }
    let _ = read(rd, &mut buf);

    // 6. EPOLL_CTL_DEL — after delete, a readable fd must NOT be reported.
    if epoll_ctl(epfd, EPOLL_CTL_DEL, rd, core::ptr::null()) < 0 {
        fail("ctl_del");
    }
    if write(wr, b"z") != 1 {
        fail("write3");
    }
    let n = epoll_wait(epfd, out.as_mut_ptr(), 1, 200);
    if n != 0 {
        fail("wait_del_should_be_empty");
    }
    let _ = read(rd, &mut buf);

    // 7. Timeout path — no registered-and-ready fd, short timeout → 0.
    let n = epoll_wait(epfd, out.as_mut_ptr(), 1, 100);
    if n != 0 {
        fail("wait_timeout_nonzero");
    }

    close(rd);
    close(wr);
    close(epfd);

    write_str(STDOUT_FILENO, "EPOLL_SMOKE:PASS\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(101)
}
