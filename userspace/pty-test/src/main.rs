//! PTY subsystem test program (Phase 29).
//!
//! Exercises PTY pair allocation, master/slave I/O, line discipline,
//! signal delivery, and raw mode.

#![no_std]
#![no_main]

use syscall_lib::{close, exit, fork, ioctl, open, read, write, write_str};

/// TIOCGPTN — get PTY number.
const TIOCGPTN: usize = 0x80045430;
/// TIOCSPTLCK — lock/unlock PTY slave.
const TIOCSPTLCK: usize = 0x40045431;

fn print(s: &[u8]) {
    let _ = write(1, s);
}

fn ok(name: &[u8]) {
    print(b"  PASS: ");
    print(name);
    print(b"\n");
}

fn fail(name: &[u8]) {
    print(b"  FAIL: ");
    print(name);
    print(b"\n");
}

/// Build a `/dev/pts/N\0` path from a PTY number.
fn pts_path(pty_num: u32) -> [u8; 16] {
    let mut path = [0u8; 16];
    path[..9].copy_from_slice(b"/dev/pts/");
    let mut n = pty_num;
    let mut tmp = [0u8; 3];
    let mut len = 0;
    loop {
        tmp[len] = b'0' + (n % 10) as u8;
        len += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for i in 0..len {
        path[9 + i] = tmp[len - 1 - i];
    }
    path[9 + len] = 0;
    path
}

/// Test 1: Open /dev/ptmx and verify we get a valid master FD.
fn test_ptmx_open() -> bool {
    let fd = open(b"/dev/ptmx\0", 2, 0); // O_RDWR
    if fd < 0 {
        fail(b"ptmx_open: open /dev/ptmx failed");
        return false;
    }
    close(fd as i32);
    ok(b"ptmx_open");
    true
}

/// Test 2: TIOCGPTN returns a valid PTY number.
fn test_tiocgptn() -> bool {
    let fd = open(b"/dev/ptmx\0", 2, 0);
    if fd < 0 {
        fail(b"tiocgptn: open failed");
        return false;
    }
    let mfd = fd as i32;
    let mut pty_num: u32 = 0xFFFF;
    let ret = ioctl(mfd, TIOCGPTN, &mut pty_num as *mut _ as usize);
    close(mfd);
    if ret < 0 || pty_num > 15 {
        fail(b"tiocgptn: ioctl failed or bad pty number");
        return false;
    }
    ok(b"tiocgptn");
    true
}

/// Test 3: Unlock slave and open /dev/pts/N.
fn test_slave_open() -> bool {
    let fd = open(b"/dev/ptmx\0", 2, 0);
    if fd < 0 {
        fail(b"slave_open: open ptmx failed");
        return false;
    }
    let mfd = fd as i32;

    // Unlock the slave.
    let zero: i32 = 0;
    let ret = ioctl(mfd, TIOCSPTLCK, &zero as *const _ as usize);
    if ret < 0 {
        close(mfd);
        fail(b"slave_open: unlock failed");
        return false;
    }

    // Get PTY number.
    let mut pty_num: u32 = 0;
    let ret = ioctl(mfd, TIOCGPTN, &mut pty_num as *mut _ as usize);
    if ret < 0 {
        close(mfd);
        fail(b"slave_open: get pty number failed");
        return false;
    }

    let path = pts_path(pty_num);

    let sfd = open(&path, 2, 0);
    close(mfd);
    if sfd < 0 {
        fail(b"slave_open: open /dev/pts/N failed");
        return false;
    }
    close(sfd as i32);
    ok(b"slave_open");
    true
}

/// Test 4: Locked slave returns error.
fn test_slave_locked() -> bool {
    let fd = open(b"/dev/ptmx\0", 2, 0);
    if fd < 0 {
        fail(b"slave_locked: open ptmx failed");
        return false;
    }
    let mfd = fd as i32;

    // Get PTY number (don't unlock).
    let mut pty_num: u32 = 0;
    let ret = ioctl(mfd, TIOCGPTN, &mut pty_num as *mut _ as usize);
    if ret < 0 {
        close(mfd);
        fail(b"slave_locked: get pty number failed");
        return false;
    }

    let path = pts_path(pty_num);

    let sfd = open(&path, 2, 0);
    close(mfd);
    if sfd >= 0 {
        close(sfd as i32);
        fail(b"slave_locked: should have failed");
        return false;
    }
    ok(b"slave_locked");
    true
}

/// Test 5: Master-to-slave write and read (raw mode).
fn test_raw_io() -> bool {
    match syscall_lib::openpty() {
        Ok((mfd, sfd)) => {
            // Set raw mode on slave: clear ICANON, ECHO, ISIG.
            let mut termios = match syscall_lib::tcgetattr(sfd) {
                Ok(t) => t,
                Err(_) => {
                    close(mfd);
                    close(sfd);
                    fail(b"raw_io: tcgetattr failed");
                    return false;
                }
            };
            termios.c_lflag &= !(0o000002 | 0o000010 | 0o000001); // ~(ICANON|ECHO|ISIG)
            if syscall_lib::tcsetattr(sfd, &termios).is_err() {
                close(mfd);
                close(sfd);
                fail(b"raw_io: tcsetattr failed");
                return false;
            }

            // Write "hi" to master, read from slave.
            let _ = write(mfd, b"hi");

            let mut buf = [0u8; 16];
            let n = read(sfd, &mut buf);
            close(mfd);
            close(sfd);

            if n == 2 && buf[0] == b'h' && buf[1] == b'i' {
                ok(b"raw_io");
                true
            } else {
                fail(b"raw_io: data mismatch");
                false
            }
        }
        Err(_) => {
            fail(b"raw_io: openpty failed");
            false
        }
    }
}

/// Test 6: Slave-to-master write and read.
fn test_s2m_io() -> bool {
    match syscall_lib::openpty() {
        Ok((mfd, sfd)) => {
            // Set raw mode.
            let mut termios = match syscall_lib::tcgetattr(sfd) {
                Ok(t) => t,
                Err(_) => {
                    close(mfd);
                    close(sfd);
                    fail(b"s2m_io: tcgetattr failed");
                    return false;
                }
            };
            termios.c_lflag &= !(0o000002 | 0o000010 | 0o000001);
            termios.c_oflag &= !0o000001; // clear OPOST
            if syscall_lib::tcsetattr(sfd, &termios).is_err() {
                close(mfd);
                close(sfd);
                fail(b"s2m_io: tcsetattr failed");
                return false;
            }

            // Slave writes, master reads.
            let _ = write(sfd, b"ok");

            let mut buf = [0u8; 16];
            let n = read(mfd, &mut buf);
            close(mfd);
            close(sfd);

            if n == 2 && buf[0] == b'o' && buf[1] == b'k' {
                ok(b"s2m_io");
                true
            } else {
                fail(b"s2m_io: data mismatch");
                false
            }
        }
        Err(_) => {
            fail(b"s2m_io: openpty failed");
            false
        }
    }
}

/// Test 7: Allocate multiple PTY pairs (at least 8).
fn test_multiple_ptys() -> bool {
    let mut fds = [(0i32, 0i32); 8];
    for (i, slot) in fds.iter_mut().enumerate() {
        match syscall_lib::openpty() {
            Ok((mfd, sfd)) => *slot = (mfd, sfd),
            Err(_) => {
                // Clean up already allocated.
                for j in 0..i {
                    close(fds[j].0);
                    close(fds[j].1);
                }
                fail(b"multiple_ptys: failed to allocate 8 pairs");
                return false;
            }
        }
    }
    // Clean up all.
    for (mfd, sfd) in &fds {
        close(*mfd);
        close(*sfd);
    }
    ok(b"multiple_ptys (8 pairs)");
    true
}

/// Test 8: Master close delivers EOF to slave reader (via fork).
fn test_master_close_eof() -> bool {
    match syscall_lib::openpty() {
        Ok((mfd, sfd)) => {
            // Set raw mode.
            let mut termios = match syscall_lib::tcgetattr(sfd) {
                Ok(t) => t,
                Err(_) => {
                    close(mfd);
                    close(sfd);
                    fail(b"master_close_eof: tcgetattr failed");
                    return false;
                }
            };
            termios.c_lflag &= !(0o000002 | 0o000010 | 0o000001);
            if syscall_lib::tcsetattr(sfd, &termios).is_err() {
                close(mfd);
                close(sfd);
                fail(b"master_close_eof: tcsetattr failed");
                return false;
            }

            let pid = fork();
            if pid < 0 {
                close(mfd);
                close(sfd);
                fail(b"master_close_eof: fork failed");
                return false;
            }
            if pid == 0 {
                // Child: close master, try to read from slave.
                close(mfd);
                let mut buf = [0u8; 16];
                let n = read(sfd, &mut buf);
                close(sfd);
                if n == 0 {
                    exit(0); // EOF as expected
                } else {
                    exit(1);
                }
            } else {
                // Parent: close master to trigger EOF on slave.
                close(sfd);
                // Small delay to let child start reading.
                for _ in 0..100000 {
                    core::hint::spin_loop();
                }
                close(mfd);

                let mut status: i32 = 0;
                syscall_lib::waitpid(pid as i32, &mut status, 0);
                // WEXITSTATUS: status >> 8
                let code = (status >> 8) & 0xFF;
                if code == 0 {
                    ok(b"master_close_eof");
                    true
                } else {
                    fail(b"master_close_eof: child got non-EOF");
                    false
                }
            }
        }
        Err(_) => {
            fail(b"master_close_eof: openpty failed");
            false
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    write_str(1, "pty-test: Phase 29 PTY subsystem tests\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    let tests: [fn() -> bool; 8] = [
        test_ptmx_open,
        test_tiocgptn,
        test_slave_open,
        test_slave_locked,
        test_raw_io,
        test_s2m_io,
        test_multiple_ptys,
        test_master_close_eof,
    ];

    for test in &tests {
        if test() {
            passed += 1;
        } else {
            failed += 1;
        }
    }

    write_str(1, "pty-test: ");
    syscall_lib::write_u64(1, passed as u64);
    write_str(1, " passed, ");
    syscall_lib::write_u64(1, failed as u64);
    write_str(1, " failed\n");

    if failed == 0 { exit(0) } else { exit(1) }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(2, "pty-test: PANIC\n");
    exit(101)
}
