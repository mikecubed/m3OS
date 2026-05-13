//! Phase 64 Track A.3 — deterministic test child.
//!
//! Three modes selected by argv[1]:
//! - `exit-immediately` — `exit(0)` on entry. Used by C.1's crash-loop
//!   integration test: under `session_manager`'s supervisor the
//!   immediate-exit child consumes a fresh restart attempt on each
//!   spawn, exhausting `MAX_RESTART_COUNT` after 3 attempts.
//! - `ignore-sigterm` — install a no-op SIGTERM handler, then sleep
//!   forever. Used by B.1's grace-period test: the child does not
//!   respond to SIGTERM, so `stop_service` waits for `SIGTERM_GRACE_MS`
//!   and escalates to SIGKILL.
//! - `exit-on-sigterm` — install a SIGTERM handler that calls `exit(0)`,
//!   then sleep forever. Used by B.1's clean-stop test: the child exits
//!   immediately on SIGTERM with no SIGKILL escalation.
//!
//! Missing or unrecognized argv defaults to `exit-immediately` so the
//! binary's failure modes are obvious.
//!
//! ## Why a dedicated binary
//!
//! Existing test children (`exit0`, `fork-test`) don't provide a
//! switchable signal-handling mode. `crash_stub` is the single
//! Phase 64 child that covers all three lifecycle test scenarios, so
//! the integration tests don't have to multiplex argv parsing across
//! several disparate binaries.

#![no_std]
#![no_main]

use syscall_lib::{SIGTERM, STDOUT_FILENO, exit, nanosleep_for, rt_sigaction_simple, write_str};

syscall_lib::entry_point!(program_main);

/// SIGTERM handler used in `exit-on-sigterm` mode. Calls `exit(0)` so
/// the kernel reaps the child immediately on receipt of SIGTERM.
extern "C" fn exit_on_sigterm(_signum: i32) {
    write_str(STDOUT_FILENO, "crash_stub: SIGTERM received; exiting\n");
    exit(0);
}

/// SIGTERM handler used in `ignore-sigterm` mode. Logs receipt and
/// returns; the child stays alive so `stop_service` must escalate to
/// SIGKILL after the grace period.
extern "C" fn ignore_sigterm(_signum: i32) {
    write_str(STDOUT_FILENO, "crash_stub: SIGTERM received; ignoring\n");
}

fn program_main(args: &[&str]) -> i32 {
    let mode = if args.len() >= 2 {
        args[1]
    } else {
        "exit-immediately"
    };

    match mode {
        "exit-immediately" => {
            write_str(STDOUT_FILENO, "crash_stub: exit-immediately\n");
            0
        }
        "ignore-sigterm" => {
            let rc = rt_sigaction_simple(SIGTERM as usize, ignore_sigterm);
            if rc != 0 {
                write_str(STDOUT_FILENO, "crash_stub: rt_sigaction(SIGTERM) failed\n");
                return 1;
            }
            write_str(
                STDOUT_FILENO,
                "crash_stub: ignore-sigterm armed; sleeping\n",
            );
            sleep_forever();
        }
        "exit-on-sigterm" => {
            let rc = rt_sigaction_simple(SIGTERM as usize, exit_on_sigterm);
            if rc != 0 {
                write_str(STDOUT_FILENO, "crash_stub: rt_sigaction(SIGTERM) failed\n");
                return 1;
            }
            write_str(
                STDOUT_FILENO,
                "crash_stub: exit-on-sigterm armed; sleeping\n",
            );
            sleep_forever();
        }
        _ => {
            write_str(
                STDOUT_FILENO,
                "crash_stub: unknown mode; defaulting to exit-immediately\n",
            );
            0
        }
    }
}

/// Loop on long `nanosleep` calls so the child is reachable by SIGTERM /
/// SIGKILL but never wakes spontaneously. The chunk size (1 second) keeps
/// the loop short enough that a future audit tracing this binary still
/// sees periodic activity.
fn sleep_forever() -> ! {
    loop {
        let _ = nanosleep_for(1, 0);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "crash_stub: PANIC\n");
    exit(101)
}
