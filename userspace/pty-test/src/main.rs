//! PTY subsystem test program (Phase 29).
//!
//! Exercises PTY pair allocation, master/slave I/O, line discipline,
//! signal delivery, and raw mode.

#![no_std]
#![no_main]

use syscall_lib::{
    POLLERR, POLLHUP, POLLIN, PollFd, WNOHANG, close, dup2, execve, exit, fork, ioctl, kill, open,
    pipe, poll, read, setsid, waitpid, write, write_str,
};

syscall_lib::entry_point!(pty_main);

/// TIOCGPTN — get PTY number.
const TIOCGPTN: usize = 0x80045430;
/// TIOCSPTLCK — lock/unlock PTY slave.
const TIOCSPTLCK: usize = 0x40045431;
/// TIOCSCTTY — set controlling terminal.
const TIOCSCTTY: usize = 0x540E;

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

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn spawn_ion_shell() -> Result<(i32, i32), ()> {
    let (mfd, sfd) = syscall_lib::openpty().map_err(|_| ())?;
    let pid = fork();
    if pid < 0 {
        close(mfd);
        close(sfd);
        return Err(());
    }

    if pid == 0 {
        close(mfd);
        if setsid() < 0 {
            exit(1);
        }
        if ioctl(sfd, TIOCSCTTY, 0) < 0 {
            exit(2);
        }
        if dup2(sfd, 0) < 0 || dup2(sfd, 1) < 0 || dup2(sfd, 2) < 0 {
            exit(3);
        }
        if sfd > 2 {
            close(sfd);
        }

        let shell = b"/bin/ion\0";
        let env_home = b"HOME=/root\0";
        let env_user = b"USER=root\0";
        let env_path = b"PATH=/bin:/sbin:/usr/bin\0";
        let env_term = b"TERM=xterm\0";
        let argv = [shell.as_ptr(), core::ptr::null()];
        let envp = [
            env_home.as_ptr(),
            env_user.as_ptr(),
            env_path.as_ptr(),
            env_term.as_ptr(),
            core::ptr::null(),
        ];
        let _ = execve(shell, &argv, &envp);
        exit(127);
    }

    close(sfd);
    Ok((mfd, pid as i32))
}

fn wait_for_prompt(mfd: i32, timeout_polls: usize) -> Result<bool, ()> {
    let mut captured = [0u8; 512];
    let mut captured_len = 0usize;

    for _ in 0..timeout_polls {
        let mut pfd = PollFd {
            fd: mfd,
            events: POLLIN,
            revents: 0,
        };
        let ready = poll(core::slice::from_mut(&mut pfd), 100);
        if ready < 0 {
            return Err(());
        }
        if ready == 0 {
            continue;
        }
        if (pfd.revents & (POLLIN | POLLHUP | POLLERR)) == 0 {
            continue;
        }

        let mut chunk = [0u8; 128];
        let n = read(mfd, &mut chunk);
        if n < 0 {
            return Err(());
        }
        if n == 0 {
            break;
        }

        let n = n as usize;
        let copy = core::cmp::min(n, captured.len().saturating_sub(captured_len));
        if copy > 0 {
            captured[captured_len..captured_len + copy].copy_from_slice(&chunk[..copy]);
            captured_len += copy;
        }
        let view = &captured[..captured_len];
        if contains_bytes(view, b"m3os") && contains_bytes(view, b"# ") {
            return Ok(true);
        }
    }

    Ok(false)
}

fn cleanup_shell(mfd: i32, pid: i32, request_exit: bool) -> i32 {
    if request_exit {
        let _ = write(mfd, b"exit\n");
    }
    close(mfd);

    let mut status = 0i32;
    let waited = waitpid(pid, &mut status, WNOHANG);
    if waited == 0 {
        // SIGHUP first, then poll briefly before escalating to SIGKILL.
        let _ = kill(pid, syscall_lib::SIGHUP);
        for _ in 0..20 {
            if waitpid(pid, &mut status, WNOHANG) != 0 {
                return status;
            }
            // Yield CPU briefly — no sleep syscall, so spin a short poll.
            let mut pfd = PollFd {
                fd: -1,
                events: 0,
                revents: 0,
            };
            let _ = poll(core::slice::from_mut(&mut pfd), 50);
        }
        // Ion didn't exit from SIGHUP — force kill.
        let _ = kill(pid, syscall_lib::SIGKILL);
        let _ = waitpid(pid, &mut status, 0);
    }
    status
}

fn wait_for_pipe_byte(fd: i32, timeout_polls: usize) -> Result<Option<u8>, ()> {
    for _ in 0..timeout_polls {
        let mut pfd = PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        };
        let ready = poll(core::slice::from_mut(&mut pfd), 100);
        if ready < 0 {
            return Err(());
        }
        if ready == 0 {
            continue;
        }
        if (pfd.revents & (POLLIN | POLLHUP | POLLERR)) == 0 {
            continue;
        }

        let mut byte = [0u8; 1];
        let n = read(fd, &mut byte);
        if n < 0 {
            return Err(());
        }
        if n == 0 {
            return Ok(None);
        }
        return Ok(Some(byte[0]));
    }

    Ok(None)
}

fn spawn_ion_supervisor() -> Result<(i32, i32, i32), ()> {
    let mut ready_pipe = [0i32; 2];
    let mut hold_pipe = [0i32; 2];
    if pipe(&mut ready_pipe) < 0 {
        return Err(());
    }
    if pipe(&mut hold_pipe) < 0 {
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        return Err(());
    }

    let pid = fork();
    if pid < 0 {
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        close(hold_pipe[0]);
        close(hold_pipe[1]);
        return Err(());
    }

    if pid == 0 {
        close(ready_pipe[0]);
        close(hold_pipe[1]);

        let exit_code = match spawn_ion_shell() {
            Ok((mfd, shell_pid)) => {
                let ready = matches!(wait_for_prompt(mfd, 100), Ok(true));
                let status = [if ready { b'1' } else { b'0' }];
                let _ = write(ready_pipe[1], &status);
                close(ready_pipe[1]);

                if ready {
                    let mut buf = [0u8; 1];
                    let _ = read(hold_pipe[0], &mut buf);
                }
                close(hold_pipe[0]);

                let shell_status = cleanup_shell(mfd, shell_pid, ready);
                if ready && ((shell_status >> 8) & 0xFF) == 0 {
                    0
                } else {
                    1
                }
            }
            Err(_) => {
                let _ = write(ready_pipe[1], b"E");
                close(ready_pipe[1]);
                close(hold_pipe[0]);
                2
            }
        };
        exit(exit_code);
    }

    close(ready_pipe[1]);
    close(hold_pipe[0]);
    Ok((ready_pipe[0], hold_pipe[1], pid as i32))
}

fn cleanup_supervisor(ready_fd: i32, hold_fd: i32, pid: i32) -> i32 {
    close(ready_fd);
    close(hold_fd);

    let mut status = 0i32;
    let waited = waitpid(pid, &mut status, WNOHANG);
    if waited == 0 {
        let _ = kill(pid, syscall_lib::SIGHUP);
        for _ in 0..20 {
            if waitpid(pid, &mut status, WNOHANG) != 0 {
                return status;
            }
            let mut pfd = PollFd {
                fd: -1,
                events: 0,
                revents: 0,
            };
            let _ = poll(core::slice::from_mut(&mut pfd), 50);
        }
        let _ = kill(pid, syscall_lib::SIGKILL);
        let _ = waitpid(pid, &mut status, 0);
    }
    status
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
                #[allow(clippy::needless_range_loop)]
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

/// Test 9: Spawn ion on a PTY and verify the initial prompt is emitted.
fn test_ion_prompt() -> bool {
    match spawn_ion_shell() {
        Ok((mfd, pid)) => match wait_for_prompt(mfd, 100) {
            Ok(saw_prompt) => {
                let status = cleanup_shell(mfd, pid, saw_prompt);
                if saw_prompt && ((status >> 8) & 0xFF) == 0 {
                    ok(b"ion_prompt");
                    true
                } else {
                    fail(b"ion_prompt: prompt not observed");
                    false
                }
            }
            Err(_) => {
                let _ = cleanup_shell(mfd, pid, false);
                fail(b"ion_prompt: poll/read failed");
                false
            }
        },
        Err(_) => {
            fail(b"ion_prompt: openpty failed");
            false
        }
    }
}

/// Test 10: Two PTY-backed ion shells should both reach a prompt without
/// needing input in the first session to unblock the second.
fn test_dual_ion_prompts() -> bool {
    let (mfd_a, pid_a) = match spawn_ion_shell() {
        Ok(shell) => shell,
        Err(_) => {
            fail(b"dual_ion_prompts: first openpty failed");
            return false;
        }
    };

    let first_ready = match wait_for_prompt(mfd_a, 100) {
        Ok(ready) => ready,
        Err(_) => {
            let _ = cleanup_shell(mfd_a, pid_a, false);
            fail(b"dual_ion_prompts: first prompt poll/read failed");
            return false;
        }
    };
    if !first_ready {
        let _ = cleanup_shell(mfd_a, pid_a, false);
        fail(b"dual_ion_prompts: first prompt not observed");
        return false;
    }

    let (mfd_b, pid_b) = match spawn_ion_shell() {
        Ok(shell) => shell,
        Err(_) => {
            let _ = cleanup_shell(mfd_a, pid_a, true);
            fail(b"dual_ion_prompts: second openpty failed");
            return false;
        }
    };

    let second_ready = match wait_for_prompt(mfd_b, 100) {
        Ok(ready) => ready,
        Err(_) => {
            let _ = cleanup_shell(mfd_b, pid_b, false);
            let _ = cleanup_shell(mfd_a, pid_a, true);
            fail(b"dual_ion_prompts: second prompt poll/read failed");
            return false;
        }
    };

    let status_b = cleanup_shell(mfd_b, pid_b, second_ready);
    let status_a = cleanup_shell(mfd_a, pid_a, true);

    if second_ready && ((status_a >> 8) & 0xFF) == 0 && ((status_b >> 8) & 0xFF) == 0 {
        ok(b"dual_ion_prompts");
        true
    } else {
        fail(b"dual_ion_prompts: second prompt not observed");
        false
    }
}

/// Test 11: Two independent PTY session-supervisor processes should both
/// reach a prompt without activity in the first session.
fn test_dual_ion_supervisors() -> bool {
    let (ready_a, hold_a, pid_a) = match spawn_ion_supervisor() {
        Ok(supervisor) => supervisor,
        Err(_) => {
            fail(b"dual_ion_supervisors: first supervisor spawn failed");
            return false;
        }
    };

    let first_ready = match wait_for_pipe_byte(ready_a, 60) {
        Ok(Some(b'1')) => true,
        Ok(Some(_)) | Ok(None) => false,
        Err(_) => {
            let _ = cleanup_supervisor(ready_a, hold_a, pid_a);
            fail(b"dual_ion_supervisors: first supervisor poll/read failed");
            return false;
        }
    };
    if !first_ready {
        let _ = cleanup_supervisor(ready_a, hold_a, pid_a);
        fail(b"dual_ion_supervisors: first prompt not observed");
        return false;
    }

    let (ready_b, hold_b, pid_b) = match spawn_ion_supervisor() {
        Ok(supervisor) => supervisor,
        Err(_) => {
            let _ = cleanup_supervisor(ready_a, hold_a, pid_a);
            fail(b"dual_ion_supervisors: second supervisor spawn failed");
            return false;
        }
    };

    let second_ready = match wait_for_pipe_byte(ready_b, 60) {
        Ok(Some(b'1')) => true,
        Ok(Some(_)) | Ok(None) => false,
        Err(_) => {
            let _ = cleanup_supervisor(ready_b, hold_b, pid_b);
            let _ = cleanup_supervisor(ready_a, hold_a, pid_a);
            fail(b"dual_ion_supervisors: second supervisor poll/read failed");
            return false;
        }
    };

    let status_b = cleanup_supervisor(ready_b, hold_b, pid_b);
    let status_a = cleanup_supervisor(ready_a, hold_a, pid_a);

    if second_ready && ((status_a >> 8) & 0xFF) == 0 && ((status_b >> 8) & 0xFF) == 0 {
        ok(b"dual_ion_supervisors");
        true
    } else {
        fail(b"dual_ion_supervisors: second prompt not observed");
        false
    }
}

fn pty_main(args: &[&str]) -> i32 {
    let quick = args.contains(&"--quick");

    if quick {
        write_str(1, "pty-test: Phase 29 PTY subsystem tests (quick)\n");
    } else {
        write_str(1, "pty-test: Phase 29 PTY subsystem tests\n");
    }

    let mut passed = 0u32;
    let mut failed = 0u32;

    let quick_tests: [fn() -> bool; 8] = [
        test_ptmx_open,
        test_tiocgptn,
        test_slave_open,
        test_slave_locked,
        test_raw_io,
        test_s2m_io,
        test_multiple_ptys,
        test_master_close_eof,
    ];
    let ion_tests: [fn() -> bool; 3] = [
        test_ion_prompt,
        test_dual_ion_prompts,
        test_dual_ion_supervisors,
    ];

    for test in &quick_tests {
        if test() {
            passed += 1;
        } else {
            failed += 1;
        }
    }
    if !quick {
        for test in &ion_tests {
            if test() {
                passed += 1;
            } else {
                failed += 1;
            }
        }
    }

    write_str(1, "pty-test: ");
    syscall_lib::write_u64(1, passed as u64);
    write_str(1, " passed, ");
    syscall_lib::write_u64(1, failed as u64);
    write_str(1, " failed\n");

    if failed == 0 { 0 } else { 1 }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(2, "pty-test: PANIC\n");
    exit(101)
}
