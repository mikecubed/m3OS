//! Fork test: validates basic and nested fork/wait/pipe behavior.
//!
//! Validation:
//! - P11-T021 — fork child exits 42; waitpid returns 42.
//! - Nested prompt-like flow — child forks a grandchild, reads its pipe output
//!   through EOF, then waits for it to exit.
#![no_std]
#![no_main]

use syscall_lib::{
    SYS_FORK, SYS_WAITPID, close, exit, pipe, read, serial_print, syscall0, syscall2, waitpid,
    write,
};

fn fail(msg: &str, code: i32) -> ! {
    serial_print(msg);
    exit(code)
}

fn basic_wait_test() {
    let child_pid = unsafe { syscall0(SYS_FORK) };

    if child_pid == 0 {
        exit(42)
    } else if child_pid == u64::MAX {
        fail("fork-test: fork() failed\n", 1)
    } else {
        let mut wstatus: i32 = -1;
        let waited = unsafe { syscall2(SYS_WAITPID, child_pid, &mut wstatus as *mut i32 as u64) };
        if waited == u64::MAX {
            fail("fork-test: waitpid failed\n", 2)
        }
        let exit_code = (wstatus >> 8) & 0xff;
        if exit_code != 42 {
            fail("fork-test: FAIL — unexpected exit code\n", 3)
        }
    }
}

fn nested_prompt_like_test() {
    let outer_pid = unsafe { syscall0(SYS_FORK) as isize };
    if outer_pid < 0 {
        fail("fork-test: nested outer fork failed\n", 4)
    }

    if outer_pid == 0 {
        let mut fds = [0i32; 2];
        if pipe(&mut fds) < 0 {
            fail("fork-test: nested pipe failed\n", 5)
        }

        let inner_pid = unsafe { syscall0(SYS_FORK) as isize };
        if inner_pid < 0 {
            fail("fork-test: nested inner fork failed\n", 6)
        }

        if inner_pid == 0 {
            close(fds[0]);
            let payload = b"prompt-ready";
            let mut written = 0usize;
            while written < payload.len() {
                let n = write(fds[1], &payload[written..]);
                if n <= 0 {
                    fail("fork-test: nested write failed\n", 7)
                }
                written += n as usize;
            }
            close(fds[1]);
            exit(0)
        }

        close(fds[1]);

        let expected = b"prompt-ready";
        let mut buf = [0u8; 32];
        let mut total = 0usize;
        while total < expected.len() {
            let n = read(fds[0], &mut buf[total..]);
            if n < 0 {
                fail("fork-test: nested read failed\n", 8)
            }
            if n == 0 {
                fail("fork-test: nested read unexpected EOF\n", 8)
            }
            total += n as usize;
        }
        if &buf[..total] != expected {
            fail("fork-test: nested read payload failed\n", 8)
        }

        let eof = read(fds[0], &mut buf);
        if eof != 0 {
            fail("fork-test: nested read EOF failed\n", 9)
        }
        close(fds[0]);

        let mut inner_status = -1i32;
        let inner_waited = waitpid(inner_pid as i32, &mut inner_status, 0);
        if inner_waited != inner_pid || ((inner_status >> 8) & 0xff) != 0 {
            fail("fork-test: nested waitpid failed\n", 10)
        }

        exit(0)
    }

    let mut outer_status = -1i32;
    let outer_waited = waitpid(outer_pid as i32, &mut outer_status, 0);
    if outer_waited != outer_pid || ((outer_status >> 8) & 0xff) != 0 {
        fail("fork-test: nested outer waitpid failed\n", 11)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    basic_wait_test();
    nested_prompt_like_test();
    serial_print("fork-test: PASS — basic and nested flows succeeded\n");
    exit(0)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(100)
}
