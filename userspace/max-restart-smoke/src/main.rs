//! Phase 55b Track F.3d-1 — max_restart 6-kill loop + service status=failed.
//!
//! This binary scripts the 6-kill loop that tips `nvme_driver` into
//! `PermanentlyStopped` (reported as `permanently-stopped` in
//! `/run/services.status`). The service config has `restart=on-failure` and
//! `max_restart=5`, so the 6th crash exceeds the budget and init stops
//! respawning.
//!
//! # Sequence
//!
//! 1. Wait for `nvme_driver` to be `running` (precondition — driver must be up).
//! 2. Loop 6 times:
//!    a. Fork a child that execs `service kill nvme_driver`.
//!    b. Wait for the child to exit (SIGKILL delivered).
//!    c. After the first 5 kills: wait for `nvme_driver` to return to `running`
//!       (up to RESTART_WAIT_SECS seconds) before issuing the next kill.
//!    d. After the 6th kill: wait for status to be `permanently-stopped`.
//! 3. Emit `MAX_RESTART_SMOKE:PASS` on success or
//!    `MAX_RESTART_SMOKE:FAIL <reason>` on any sub-step failure.
//!
//! # Timing
//!
//! Each kill + restart cycle takes ~1 s (DRIVER_RESTART_TIMEOUT_MS = 1000).
//! 6 cycles ≈ 6–10 s, well within the QEMU regression budget (180 s).
//! The 10-second restart-count reset window in init means kills must arrive
//! within 10 s of each prior restart. We wait at most RESTART_WAIT_SECS (5 s)
//! per cycle, so the count never resets before the 6th kill.
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDOUT_FILENO, close, execve, exit, fork, nanosleep, open, read, waitpid, write_str,
};

// Service name (matches nvme_driver service config key `name=`).
const NVME_DRIVER_NAME: &[u8] = b"nvme_driver\0";

// Paths for service control binary.
const SERVICE_BIN: &[u8] = b"/bin/service\0";
const SERVICE_ARGV0: &[u8] = b"service\0";
const SERVICE_KILL_ARG: &[u8] = b"kill\0";

// Status file path (init writes `/run/services.status`).
const STATUS_PATH: &[u8] = b"/run/services.status\0";

// How long to wait for a state transition per poll loop.
const RESTART_WAIT_SECS: u64 = 5;
// How long to wait for permanently-stopped after the 6th kill.
const FAILED_WAIT_SECS: u64 = 10;

// Total number of kills: max_restart=5 → 6th kill tips over.
const TOTAL_KILLS: u32 = 6;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "MAX_RESTART_SMOKE:BEGIN\n");

    // ------------------------------------------------------------------
    // Precondition: nvme_driver must be running before we start.
    // ------------------------------------------------------------------
    if !wait_for_status(b"nvme_driver", b"running", RESTART_WAIT_SECS) {
        write_str(
            STDOUT_FILENO,
            "MAX_RESTART_SMOKE:FAIL precondition nvme_driver not running\n",
        );
        return 1;
    }
    write_str(STDOUT_FILENO, "MAX_RESTART_SMOKE:precondition:OK\n");

    // ------------------------------------------------------------------
    // 6-kill loop.
    // ------------------------------------------------------------------
    let mut kill_num: u32 = 0;
    while kill_num < TOTAL_KILLS {
        kill_num += 1;

        // Fork a child that runs `service kill nvme_driver`.
        let child_pid = fork();
        if child_pid < 0 {
            write_str(STDOUT_FILENO, "MAX_RESTART_SMOKE:FAIL fork\n");
            return 2;
        }

        if child_pid == 0 {
            // Child: exec `service kill nvme_driver`.
            let argv: [*const u8; 4] = [
                SERVICE_ARGV0.as_ptr(),
                SERVICE_KILL_ARG.as_ptr(),
                NVME_DRIVER_NAME.as_ptr(),
                core::ptr::null(),
            ];
            let envp: [*const u8; 1] = [core::ptr::null()];
            let _ = execve(SERVICE_BIN, &argv, &envp);
            // execve returns only on error.
            exit(126);
        }

        // Wait for the service-kill child to finish.
        let mut child_status = 0i32;
        let waited = waitpid(child_pid as i32, &mut child_status, 0);
        if waited != child_pid as isize {
            write_str(STDOUT_FILENO, "MAX_RESTART_SMOKE:FAIL waitpid\n");
            return 2;
        }

        write_str(STDOUT_FILENO, "MAX_RESTART_SMOKE:kill:");
        write_dec(kill_num);
        write_str(STDOUT_FILENO, ":delivered\n");

        if kill_num < TOTAL_KILLS {
            // Kills 1–5: wait for init to restart the driver before next kill.
            // This ensures restart_count increments properly; if we kill again
            // before init has restarted the driver the process doesn't exist
            // and the service kill is a no-op.
            if !wait_for_status(b"nvme_driver", b"running", RESTART_WAIT_SECS) {
                write_str(
                    STDOUT_FILENO,
                    "MAX_RESTART_SMOKE:FAIL restart-not-seen kill:",
                );
                write_dec(kill_num);
                write_str(STDOUT_FILENO, "\n");
                return 3;
            }
            write_str(STDOUT_FILENO, "MAX_RESTART_SMOKE:kill:");
            write_dec(kill_num);
            write_str(STDOUT_FILENO, ":restarted\n");
        }
    }

    // ------------------------------------------------------------------
    // After the 6th kill: wait for permanently-stopped.
    // ------------------------------------------------------------------
    if !wait_for_status(b"nvme_driver", b"permanently-stopped", FAILED_WAIT_SECS) {
        write_str(
            STDOUT_FILENO,
            "MAX_RESTART_SMOKE:FAIL service not permanently-stopped after 6 kills\n",
        );
        return 4;
    }

    write_str(STDOUT_FILENO, "MAX_RESTART_SMOKE:PASS\n");
    0
}

/// Write a u32 decimal value to stdout without heap allocation.
fn write_dec(mut n: u32) {
    if n == 0 {
        write_str(STDOUT_FILENO, "0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut pos = 10usize;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    // SAFETY: all bytes in buf[pos..] are ASCII digits.
    let slice = &buf[pos..];
    // write_str requires &str; convert from known-ASCII slice.
    if let Ok(s) = core::str::from_utf8(slice) {
        write_str(STDOUT_FILENO, s);
    }
}

/// Poll `/run/services.status` until the named service shows the given
/// status token, or `timeout_secs` elapses.  Returns `true` when matched.
///
/// Status-file line format (written by init):
///   `<name> <status> pid=<N> restarts=<N> changed=<N>`
fn wait_for_status(name: &[u8], want_status: &[u8], timeout_secs: u64) -> bool {
    let mut buf = [0u8; 4096];
    let mut waited = 0u64;
    loop {
        let fd = open(STATUS_PATH, O_RDONLY, 0);
        if fd >= 0 {
            let n = read(fd as i32, &mut buf);
            close(fd as i32);
            if n > 0 {
                let text = &buf[..n as usize];
                if service_has_status(text, name, want_status) {
                    return true;
                }
            }
        }
        if waited >= timeout_secs {
            break;
        }
        let _ = nanosleep(1);
        waited += 1;
    }
    false
}

/// Return `true` if `text` contains a line for `name` whose status token
/// equals `want_status`.
fn service_has_status(text: &[u8], name: &[u8], want_status: &[u8]) -> bool {
    for line in text.split(|&b| b == b'\n') {
        if line.len() <= name.len() {
            continue;
        }
        if &line[..name.len()] != name {
            continue;
        }
        if line[name.len()] != b' ' {
            continue;
        }
        let rest = &line[name.len() + 1..];
        let status_end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
        let status = &rest[..status_end];
        if status == want_status {
            return true;
        }
    }
    false
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "MAX_RESTART_SMOKE:PANIC\n");
    exit(101)
}
