//! m3OS init — PID 1 userspace process (Phase 20–27).
//!
//! Responsibilities:
//! - Print boot banner
//! - Mount persistent storage at /data
//! - Fork+exec `/bin/login` for user authentication (Phase 27)
//! - Reap all orphaned children (zombie prevention)
//! - Re-spawn login when the shell exits
//! - Never exit (kernel panics if PID 1 dies)
#![no_std]
#![no_main]

use syscall_lib::{
    execve, exit, fork, mount, nanosleep, waitpid, write_str, STDOUT_FILENO, WNOHANG,
};

const LOGIN_PATH: &[u8] = b"/bin/login\0";
const LOGIN_ARGV0: &[u8] = b"/bin/login\0";
const ENV_PATH: &[u8] = b"PATH=/bin:/sbin:/usr/bin\0";
const ENV_HOME: &[u8] = b"HOME=/\0";
const ENV_TERM: &[u8] = b"TERM=m3os\0";
const ENV_EDITOR: &[u8] = b"EDITOR=/bin/edit\0";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Fds 0/1/2 are pre-opened by the kernel for PID 1.
    write_str(STDOUT_FILENO, "\nm3OS init (PID 1)\n");

    // Phase 24: Mount persistent storage at /data.
    let ret = mount(
        b"/dev/blk0\0".as_ptr(),
        b"/data\0".as_ptr(),
        b"vfat\0".as_ptr(),
    );
    if ret == 0 {
        write_str(STDOUT_FILENO, "init: /data mounted (vfat)\n");
    } else {
        write_str(STDOUT_FILENO, "init: /data mount failed (");
        syscall_lib::write_u64(STDOUT_FILENO, (-ret) as u64);
        write_str(STDOUT_FILENO, ")\n");
    }

    // Phase 27: Set initial file permissions.
    // /data/etc/shadow should be root-only readable.
    let chmod_ret = syscall_lib::chmod(b"/data/etc/shadow\0", 0o600);
    if chmod_ret != 0 {
        write_str(
            STDOUT_FILENO,
            "init: warning: chmod /data/etc/shadow failed\n",
        );
    }

    // Create /tmp/home for user home directories.
    let mkdir_ret = unsafe {
        syscall_lib::syscall2(
            syscall_lib::SYS_MKDIR,
            b"/tmp/home\0".as_ptr() as u64,
            0o755,
        )
    };
    if mkdir_ret as i64 != 0 && mkdir_ret as i64 != -17 {
        write_str(STDOUT_FILENO, "init: mkdir /tmp/home failed (");
        syscall_lib::write_u64(STDOUT_FILENO, (-(mkdir_ret as i64)) as u64);
        write_str(STDOUT_FILENO, ")\n");
    }

    // Make /tmp world-writable so non-root users can create files.
    syscall_lib::chmod(b"/tmp\0", 0o1777);

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

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "init: PANIC\n");
    exit(101)
}
