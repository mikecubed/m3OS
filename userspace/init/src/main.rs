//! m3OS init — PID 1 userspace process (Phase 20–28).
//!
//! Responsibilities:
//! - Print boot banner
//! - Mount ext2 root filesystem at /
//! - Fork+exec `/bin/login` for user authentication (Phase 27)
//! - Reap all orphaned children (zombie prevention)
//! - Re-spawn login when the shell exits
//! - Never exit (kernel panics if PID 1 dies)
#![no_std]
#![no_main]

use syscall_lib::{
    STDOUT_FILENO, WNOHANG, execve, exit, fork, mount, nanosleep, waitpid, write_str,
};

const LOGIN_PATH: &[u8] = b"/bin/login\0";
const LOGIN_ARGV0: &[u8] = b"/bin/login\0";
const TELNETD_PATH: &[u8] = b"/bin/telnetd\0";
const TELNETD_ARGV0: &[u8] = b"/bin/telnetd\0";
const ENV_PATH: &[u8] = b"PATH=/bin:/sbin:/usr/bin\0";
const ENV_HOME: &[u8] = b"HOME=/\0";
const ENV_TERM: &[u8] = b"TERM=m3os\0";
const ENV_EDITOR: &[u8] = b"EDITOR=/bin/edit\0";

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Fds 0/1/2 are pre-opened by the kernel for PID 1.
    write_str(STDOUT_FILENO, "\nm3OS init (PID 1)\n");

    // Phase 28: Mount ext2 root filesystem at /.
    let ret = mount(b"/dev/blk0\0".as_ptr(), b"/\0".as_ptr(), b"ext2\0".as_ptr());
    if ret == 0 {
        write_str(STDOUT_FILENO, "init: / mounted (ext2)\n");
    } else {
        write_str(STDOUT_FILENO, "init: / mount failed (");
        syscall_lib::write_u64(STDOUT_FILENO, (-ret) as u64);
        write_str(STDOUT_FILENO, ")\n");
    }

    // Make /tmp world-writable so non-root users can create files.
    syscall_lib::chmod(b"/tmp\0", 0o1777);

    // Phase 30: spawn telnetd daemon (background, not waited on).
    spawn_telnetd();

    // Spawn the first login session.
    let mut login_pid = spawn_login();
    if login_pid < 0 {
        write_str(STDOUT_FILENO, "init: failed to spawn login\n");
        exit(1);
    }

    // Reap loop: wait for any child, re-spawn login if it exits.
    loop {
        let mut status: i32 = 0;
        let ret = waitpid(-1, &mut status, WNOHANG);

        if ret > 0 {
            if ret == login_pid {
                write_str(
                    STDOUT_FILENO,
                    "\ninit: session ended, respawning login...\n",
                );
                login_pid = spawn_login();
                if login_pid < 0 {
                    write_str(STDOUT_FILENO, "init: failed to respawn login\n");
                    exit(1);
                }
            }
        } else {
            nanosleep(1);
        }
    }
}

fn spawn_login() -> isize {
    let pid = fork();
    if pid == 0 {
        let envp: [*const u8; 5] = [
            ENV_PATH.as_ptr(),
            ENV_HOME.as_ptr(),
            ENV_TERM.as_ptr(),
            ENV_EDITOR.as_ptr(),
            core::ptr::null(),
        ];

        let argv: [*const u8; 2] = [LOGIN_ARGV0.as_ptr(), core::ptr::null()];
        let ret = execve(LOGIN_PATH, &argv, &envp);

        write_str(STDOUT_FILENO, "init: login execve failed (");
        syscall_lib::write_u64(STDOUT_FILENO, (-ret) as u64);
        write_str(STDOUT_FILENO, ")\n");
        exit(1);
    }
    if pid as u64 == u64::MAX {
        return -1;
    }
    pid
}

fn spawn_telnetd() {
    let pid = fork();
    if pid == 0 {
        let envp: [*const u8; 3] = [ENV_PATH.as_ptr(), ENV_HOME.as_ptr(), core::ptr::null()];
        let argv: [*const u8; 2] = [TELNETD_ARGV0.as_ptr(), core::ptr::null()];
        let ret = execve(TELNETD_PATH, &argv, &envp);
        write_str(STDOUT_FILENO, "init: telnetd execve failed (");
        syscall_lib::write_u64(STDOUT_FILENO, (-ret) as u64);
        write_str(STDOUT_FILENO, ")\n");
        exit(1);
    }
    if pid > 0 {
        write_str(STDOUT_FILENO, "init: telnetd started\n");
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "init: PANIC\n");
    exit(101)
}
