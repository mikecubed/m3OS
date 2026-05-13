//! Phase 64a — syscall wrappers for talking to init via
//! `/run/init.cmd` and `/run/services.status`. Binary-only.
//!
//! The pure-logic parser and the step-name → init-manifest-name
//! mapping live in `session_manager::init_status`, where they are
//! host-tested. This module supplies the thin file-I/O layer that the
//! daemon's [`InitSupervisorBackend`](crate::init_backend) calls
//! during a stop / restart motion.
//!
//! See `init_status` for the rationale on why `session_manager`
//! delegates to init instead of signalling children directly.

use core::sync::atomic::{AtomicU64, Ordering};

use session_manager::init_status::{InitServiceStatus, STATUS_BUF_BYTES, parse_status_for};
use syscall_lib::{
    O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDOUT_FILENO, close, open, read, write, write_str,
};

/// Path to init's control-command file. Init reads this file on every
/// reap-loop iteration and dispatches the first command it finds; the
/// file is truncated after reading so back-to-back writes don't collide.
/// Mirrors `userspace/init/src/main.rs`'s `CMD_FILE`.
const CMD_FILE: &[u8] = b"/run/init.cmd\0";

/// Path to init's status file. Init writes one line per service in
/// the format `name <status> pid=<pid> restarts=<count> changed=<epoch>`
/// every ~1 s; the parser in `init_status` consumes its bytes.
const STATUS_FILE: &[u8] = b"/run/services.status\0";

/// Issue a `stop <name>` command to init via `/run/init.cmd`.
///
/// Init's handler sets the service's `restart_policy` to `Never`, then
/// runs its own SIGTERM/grace/SIGKILL/waitpid motion. The next
/// `/run/services.status` write reflects the new state. Returns
/// `Ok(())` if the command was successfully staged for init to read;
/// `Err(())` on file-system transport failure.
pub fn cmd_stop(init_name: &str) -> Result<(), ()> {
    write_cmd(b"stop ", init_name.as_bytes())
}

/// Issue a `restart <name>` command to init via `/run/init.cmd`.
///
/// Init's handler stops the service, resets `restart_count` to 0, and
/// re-starts it through the normal manifest-driven path.
pub fn cmd_restart(init_name: &str) -> Result<(), ()> {
    write_cmd(b"restart ", init_name.as_bytes())
}

fn write_cmd(verb: &[u8], name: &[u8]) -> Result<(), ()> {
    let fd = open(CMD_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0o600);
    if fd < 0 {
        write_str(
            STDOUT_FILENO,
            "session_manager: init_proxy: open /run/init.cmd failed\n",
        );
        return Err(());
    }
    let mut ok = true;
    if write(fd as i32, verb) < 0 {
        ok = false;
    }
    if ok && write(fd as i32, name) < 0 {
        ok = false;
    }
    if ok && write(fd as i32, b"\n") < 0 {
        ok = false;
    }
    close(fd as i32);
    if ok { Ok(()) } else { Err(()) }
}

/// Read `/run/services.status` and return the row for `init_name`, or
/// `None` if init has not yet written a status file or the named
/// service is not listed.
pub fn read_service_status(init_name: &str) -> Option<InitServiceStatus> {
    let fd = open(STATUS_FILE, O_RDONLY, 0);
    if fd < 0 {
        return None;
    }
    let mut buf = [0u8; STATUS_BUF_BYTES];
    let mut total: usize = 0;
    while total < buf.len() {
        let n = read(fd as i32, &mut buf[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    close(fd as i32);
    parse_status_for(&buf[..total], init_name)
}

/// Fallback monotonic-time counter — see `crate::runtime::SyscallClock`
/// for the same pattern. Used so deferred-reply deadline arithmetic
/// always makes forward progress even if `clock_gettime` is broken.
static FALLBACK_NOW_MS: AtomicU64 = AtomicU64::new(0);
const FALLBACK_STEP_MS: u64 = 25;

/// Monotonic milliseconds. Wrapper around `clock_gettime` with the
/// same fallback semantics as `runtime::SyscallClock` so a transient
/// clock failure cannot freeze the deferred-reply deadlines.
pub fn now_ms() -> u64 {
    let (tv_sec, tv_nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    if tv_sec < 0 {
        return FALLBACK_NOW_MS.fetch_add(FALLBACK_STEP_MS, Ordering::Relaxed) + FALLBACK_STEP_MS;
    }
    let real = (tv_sec as u64)
        .saturating_mul(1_000)
        .saturating_add((tv_nsec.max(0) as u64) / 1_000_000);
    let mut cur = FALLBACK_NOW_MS.load(Ordering::Relaxed);
    while real > cur {
        match FALLBACK_NOW_MS.compare_exchange_weak(cur, real, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(observed) => cur = observed,
        }
    }
    real
}
