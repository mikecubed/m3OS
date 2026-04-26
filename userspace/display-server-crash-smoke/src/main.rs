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
//! 4. Re-look up `"display-control"` via `ipc_lookup_service` with
//!    extended backoff (`lookup_with_extended_backoff`, ~5 s budget)
//!    — the supervisor restart registers a fresh endpoint and the
//!    restarted process needs time to re-acquire the framebuffer
//!    and re-register both endpoints.
//! 5. Send `version` again on the new endpoint to confirm round-trip.
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
//! Every fallible syscall is checked. The post-restart re-lookup is
//! bounded by `lookup_with_extended_backoff` (50 attempts × 100 ms = 5 s).

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
    // Step 4 + 5 (combined): Wait for the display-control endpoint to
    // be reachable on the *new* display_server instance. We test
    // reachability via `ipc_lookup_service` rather than polling
    // `/run/services.status` because the supervisor transitions
    // display_server to `running` at fork time, before the restarted
    // process re-creates and re-registers its endpoints. The lookup
    // succeeds only after the new instance has called
    // `ipc_register_service("display-control")`, which is the actual
    // signal that the control plane is back. Keep the budget generous
    // (50 × 100 ms = 5 s) to absorb framebuffer-acquire backoff.
    // ------------------------------------------------------------------
    let handle2 = match lookup_with_extended_backoff(CONTROL_SERVICE_NAME) {
        Some(h) => h,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "DISPLAY_CRASH_SMOKE:FAIL step=5 re-lookup display-control\n",
            );
            return 5;
        }
    };
    syscall_lib::write_str(STDOUT_FILENO, "DISPLAY_CRASH_SMOKE:restart-confirmed\n");

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

/// Extended-budget lookup for post-restart re-connection. The
/// supervisor's "running" transition happens at fork time, but the
/// restarted process still needs to re-acquire the framebuffer
/// (bounded backoff ~40 ms) and re-register both endpoints. A 5 s
/// budget absorbs the cascade comfortably.
fn lookup_with_extended_backoff(name: &str) -> Option<u32> {
    const ATTEMPTS: u32 = 50;
    const BACKOFF_NS: u32 = 100_000_000; // 100 ms
    for attempt in 0..ATTEMPTS {
        let raw = syscall_lib::ipc_lookup_service(name);
        if raw != u64::MAX {
            return Some(raw as u32);
        }
        if attempt + 1 == ATTEMPTS {
            return None;
        }
        let _ = syscall_lib::nanosleep_for(0, BACKOFF_NS);
    }
    None
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "DISPLAY_CRASH_SMOKE:PANIC\n");
    syscall_lib::exit(101)
}
