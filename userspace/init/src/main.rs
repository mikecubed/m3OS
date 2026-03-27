//! m3OS init — PID 1 userspace process (Phase 20/21).
//!
//! Responsibilities:
//! - Print boot banner
//! - Fork+exec `/bin/sh0` as the interactive shell
//! - Ion available at `/bin/ion` for script mode (Phase 22 for interactive)
//! - Reap all orphaned children (zombie prevention)
//! - Re-spawn the shell if it exits
//! - Never exit (kernel panics if PID 1 dies)
#![no_std]
#![no_main]

use syscall_lib::{execve, exit, fork, nanosleep, waitpid, write_str, STDOUT_FILENO, WNOHANG};

const ION_PATH: &[u8] = b"/bin/ion\0";
const ION_ARGV0: &[u8] = b"/bin/ion\0";
const SH0_PATH: &[u8] = b"/bin/sh0\0";
const SH0_ARGV0: &[u8] = b"/bin/sh0\0";
const ENV_PATH: &[u8] = b"PATH=/bin:/sbin:/usr/bin\0";
const ENV_HOME: &[u8] = b"HOME=/\0";
const ENV_TERM: &[u8] = b"TERM=m3os\0";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Fds 0/1/2 are pre-opened by the kernel for PID 1.
    write_str(STDOUT_FILENO, "\nm3OS init (PID 1)\n");

    // Spawn the first shell.
    let mut shell_pid = spawn_shell();
    if shell_pid < 0 {
        write_str(STDOUT_FILENO, "init: failed to spawn shell\n");
        exit(1);
    }

    // Reap loop: wait for any child, re-spawn shell if it exits.
    loop {
        let mut status: i32 = 0;
        let ret = waitpid(-1, &mut status, WNOHANG);

        if ret > 0 {
            // A child exited. If it was the shell, re-spawn it.
            if ret == shell_pid {
                write_str(STDOUT_FILENO, "\ninit: shell exited, respawning...\n");
                shell_pid = spawn_shell();
                if shell_pid < 0 {
                    write_str(STDOUT_FILENO, "init: failed to respawn shell\n");
                    exit(1);
                }
            }
            // Otherwise, we just reaped an orphan — continue.
        } else {
            // No children ready or ECHILD — yield CPU time.
            nanosleep(1);
        }
    }
}

fn spawn_shell() -> isize {
    let pid = fork();
    if pid == 0 {
        // Child: try ion first, fall back to sh0.
        let envp: [*const u8; 4] = [
            ENV_PATH.as_ptr(),
            ENV_HOME.as_ptr(),
            ENV_TERM.as_ptr(),
            core::ptr::null(),
        ];

        // Phase 21: ion is available at /bin/ion for script mode (ion -c 'cmd').
        // Interactive mode requires Phase 22 (termios) for liner's TTY handling.
        // Use sh0 as the interactive shell for now.
        let sh0_argv_primary: [*const u8; 2] = [SH0_ARGV0.as_ptr(), core::ptr::null()];
        execve(SH0_PATH, &sh0_argv_primary, &envp);

        // sh0 not available — try ion as fallback (interactive requires Phase 22).
        write_str(STDOUT_FILENO, "init: sh0 not available, trying ion\n");
        let ion_argv: [*const u8; 2] = [ION_ARGV0.as_ptr(), core::ptr::null()];
        let ret = execve(ION_PATH, &ion_argv, &envp);

        // Both failed.
        write_str(STDOUT_FILENO, "init: execve failed (");
        syscall_lib::write_u64(STDOUT_FILENO, (-ret) as u64);
        write_str(STDOUT_FILENO, ")\n");
        exit(1);
    }
    if pid as u64 == u64::MAX {
        // fork failed
        return -1;
    }
    pid
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "init: PANIC\n");
    exit(101)
}
