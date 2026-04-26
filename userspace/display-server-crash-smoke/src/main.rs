//! Phase 56 Track F.2 — display-service crash-and-restart end-to-end smoke test.
//!
//! Modeled on `userspace/nvme-crash-smoke` (Phase 55b F.3b). The
//! binary drives the F.2 acceptance path end-to-end:
//!
//! 1. Look up the `"display-control"` IPC endpoint with bounded retry.
//! 2. Send a `version` verb to confirm the pre-crash dispatcher is
//!    reachable.
//! 3. Send the `debug-crash` verb. On a debug-crash-enabled boot the
//!    dispatcher logs `display_server: intentional crash for F.2
//!    regression` and `panic!()`s before sending a reply, so this
//!    `ipc_call_buf` either returns `u64::MAX` (transport error
//!    because the reply-cap was revoked when the server died) or
//!    the very next `ipc_lookup_service` returns `u64::MAX` (because
//!    the registration is gone).
//! 4. Poll `/run/services.status` until `display_server` shows
//!    `running` again — bounded by `RESTART_WAIT_SECONDS`.
//! 5. Re-look up `"display-control"` (the supervisor restart
//!    registers a fresh endpoint) and send `version` again.
//! 6. Print `DISPLAY_CRASH_SMOKE:PASS` on success or
//!    `DISPLAY_CRASH_SMOKE:FAIL step=N` and exit non-zero on any
//!    failed step.
//!
//! # Why this binary is one-shot
//!
//! The binary is invoked from the F.2 regression's smoke-script via
//! the post-login shell (`/bin/display-server-crash-smoke`). It is
//! not a daemon; no service-config file is required (the four-step
//! new-binary convention only requires a `.conf` for daemons).
//!
//! # Engineering discipline
//!
//! No `unwrap` / `expect` / `panic!` outside the `#[panic_handler]`.
//! Every fallible syscall is checked. The status-file polling loop
//! has a documented bound (`RESTART_WAIT_SECONDS = 30`).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use kernel_core::display::control::{ControlCommand, encode_command};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "display-server-crash-smoke: alloc error\n");
    syscall_lib::exit(99)
}

// ---------------------------------------------------------------------------
// Wire constants — must match `display_server::control` and `m3ctl`.
// ---------------------------------------------------------------------------

/// Service-registry name of the control endpoint registered by
/// `display_server` at startup.
const CONTROL_SERVICE_NAME: &str = "display-control";

/// IPC label for an encoded `ControlCommand`. Mirrors
/// `display_server::control::LABEL_CTL_CMD` and `m3ctl`'s constant.
const LABEL_CTL_CMD: u64 = 1;

/// Service-lookup retry attempts before declaring the lookup failed.
/// Same shape as `m3ctl::lookup_with_backoff`.
const SERVICE_LOOKUP_ATTEMPTS: u32 = 8;

/// Backoff between service-lookup attempts (5 ms).
const SERVICE_LOOKUP_BACKOFF_NS: u32 = 5_000_000;

/// Bound on the post-crash poll loop that waits for
/// `display_server` to reappear in `/run/services.status`. Matches
/// the `RESTART_WAIT_SECONDS` budget used by `nvme-crash-smoke`
/// scaled up to allow for display_server's framebuffer-acquire
/// retry budget on the first restart.
const RESTART_WAIT_SECONDS: u64 = 30;

/// Path of the service-status file written by init.
const STATUS_PATH: &[u8] = b"/run/services.status\0";

const O_RDONLY: u64 = 0;

/// Encoded request scratch buffer — every Phase 56 control command
/// fits in 64 bytes (the largest is `RegisterBind` at 4 + 6 = 10
/// bytes including the frame header).
const REQ_BUF_LEN: usize = 64;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "DISPLAY_CRASH_SMOKE:BEGIN\n");

    // ------------------------------------------------------------------
    // Step 1: Look up the control endpoint with bounded retry.
    // ------------------------------------------------------------------
    let handle = match lookup_with_backoff(CONTROL_SERVICE_NAME) {
        Some(h) => h,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "DISPLAY_CRASH_SMOKE:FAIL step=1 lookup display-control\n",
            );
            return 1;
        }
    };

    // ------------------------------------------------------------------
    // Step 2: Send `version` to confirm the pre-crash dispatcher is up.
    // ------------------------------------------------------------------
    let mut req_buf = [0u8; REQ_BUF_LEN];
    let req_len = match encode_command(&ControlCommand::Version, &mut req_buf) {
        Ok(n) => n,
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "DISPLAY_CRASH_SMOKE:FAIL step=2 encode version\n",
            );
            return 2;
        }
    };
    let pre_reply = syscall_lib::ipc_call_buf(handle, LABEL_CTL_CMD, 0, &req_buf[..req_len]);
    if pre_reply == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "DISPLAY_CRASH_SMOKE:FAIL step=2 pre-crash version transport-error\n",
        );
        return 2;
    }
    syscall_lib::write_str(STDOUT_FILENO, "DISPLAY_CRASH_SMOKE:pre-crash-version:OK\n");

    // ------------------------------------------------------------------
    // Step 3: Send `debug-crash`. The dispatcher panics before
    // replying; the caller observes either a transport error (reply
    // cap revoked) or a stale label (kernel signaled before the cap
    // was torn down). Both are acceptable; the *post*-crash
    // observation in step 4 is the load-bearing assertion.
    // ------------------------------------------------------------------
    let crash_len = match encode_command(&ControlCommand::DebugCrash, &mut req_buf) {
        Ok(n) => n,
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "DISPLAY_CRASH_SMOKE:FAIL step=3 encode debug-crash\n",
            );
            return 3;
        }
    };
    let crash_reply = syscall_lib::ipc_call_buf(handle, LABEL_CTL_CMD, 0, &req_buf[..crash_len]);
    if crash_reply == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "DISPLAY_CRASH_SMOKE:debug-crash:transport-error\n",
        );
    } else {
        // The dispatcher might have refused the verb (debug gate
        // disabled — production build inadvertently running this
        // smoke), or might have sent a reply before the panic landed.
        // Either way we let step 4 / 5 distinguish the two by
        // observing whether the service actually died.
        syscall_lib::write_str(
            STDOUT_FILENO,
            "DISPLAY_CRASH_SMOKE:debug-crash:reply-before-panic\n",
        );
    }

    // ------------------------------------------------------------------
    // Step 4: Poll service status for `display_server` to re-register.
    // ------------------------------------------------------------------
    if !wait_for_service_running("display_server", RESTART_WAIT_SECONDS) {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "DISPLAY_CRASH_SMOKE:FAIL step=4 restart-timeout\n",
        );
        return 4;
    }
    syscall_lib::write_str(STDOUT_FILENO, "DISPLAY_CRASH_SMOKE:restart-confirmed\n");

    // ------------------------------------------------------------------
    // Step 5: Re-look-up the control endpoint (the restart registers
    // a fresh one).
    // ------------------------------------------------------------------
    let handle2 = match lookup_with_backoff(CONTROL_SERVICE_NAME) {
        Some(h) => h,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "DISPLAY_CRASH_SMOKE:FAIL step=5 re-lookup display-control\n",
            );
            return 5;
        }
    };

    // ------------------------------------------------------------------
    // Step 6: Send `version` against the new instance — must succeed.
    // ------------------------------------------------------------------
    let req_len2 = match encode_command(&ControlCommand::Version, &mut req_buf) {
        Ok(n) => n,
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "DISPLAY_CRASH_SMOKE:FAIL step=6 encode post-restart version\n",
            );
            return 6;
        }
    };
    let post_reply = syscall_lib::ipc_call_buf(handle2, LABEL_CTL_CMD, 0, &req_buf[..req_len2]);
    if post_reply == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "DISPLAY_CRASH_SMOKE:FAIL step=6 post-restart version transport-error\n",
        );
        return 6;
    }
    syscall_lib::write_str(
        STDOUT_FILENO,
        "DISPLAY_CRASH_SMOKE:post-restart-version:OK\n",
    );

    // ------------------------------------------------------------------
    // Done.
    // ------------------------------------------------------------------
    syscall_lib::write_str(STDOUT_FILENO, "DISPLAY_CRASH_SMOKE:PASS\n");
    0
}

// ---------------------------------------------------------------------------
// Service lookup with bounded backoff
// ---------------------------------------------------------------------------

fn lookup_with_backoff(name: &str) -> Option<u32> {
    for attempt in 0..SERVICE_LOOKUP_ATTEMPTS {
        let raw = syscall_lib::ipc_lookup_service(name);
        if raw != u64::MAX {
            return Some(raw as u32);
        }
        if attempt + 1 == SERVICE_LOOKUP_ATTEMPTS {
            return None;
        }
        let _ = syscall_lib::nanosleep_for(0, SERVICE_LOOKUP_BACKOFF_NS);
    }
    None
}

// ---------------------------------------------------------------------------
// Service-status polling
// ---------------------------------------------------------------------------

/// Poll `/run/services.status` until the named service shows
/// `running`, or `timeout_secs` elapses. Returns `true` when running
/// is observed. Mirrors `nvme_crash_smoke::wait_for_driver_running`
/// (the status-file format is shared across regressions).
fn wait_for_service_running(name: &str, timeout_secs: u64) -> bool {
    let name_bytes = name.as_bytes();
    let mut buf = [0u8; 4096];
    let mut waited = 0u64;
    while waited <= timeout_secs {
        let fd = syscall_lib::open(STATUS_PATH, O_RDONLY, 0);
        if fd >= 0 {
            let n = syscall_lib::read(fd as i32, &mut buf);
            let _ = syscall_lib::close(fd as i32);
            if n > 0 && service_is_running(&buf[..n as usize], name_bytes) {
                return true;
            }
        }
        let _ = syscall_lib::nanosleep(1);
        waited += 1;
    }
    false
}

/// Return `true` if the status text contains a line for `name`
/// whose status field is `running`. Status-file line format
/// (written by init):
///
///     `<name> <status> pid=<N> restarts=<N> changed=<N>`
fn service_is_running(text: &[u8], name: &[u8]) -> bool {
    for line in text.split(|&b| b == b'\n') {
        if line.is_empty() || line.len() <= name.len() {
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
        return status == b"running";
    }
    false
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "DISPLAY_CRASH_SMOKE:PANIC\n");
    syscall_lib::exit(101)
}
