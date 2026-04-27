//! Phase 56 close-out (G.2 regression) — keybind grab-hook smoke.
//!
//! 1. Looks up the `display-control` IPC service.
//! 2. Registers a bind for `MOD_SUPER + 'q'` via `RegisterBind`.
//! 3. Injects a synthetic `KeyDown` for `MOD_SUPER + 'q'` via the
//!    test-only `InjectKey` verb (gated by
//!    `M3OS_DISPLAY_SERVER_INJECT_KEY=1`).
//! 4. Asserts via the regression's serial-log pattern that
//!    `display_server` logs `bind triggered id=...` — proving the
//!    grab fired and no client received the matching `KeyEvent`.
//! 5. Exits 0 on PASS, non-zero on FAIL.
//!
//! # Output signals
//!
//! - `GRAB_HOOK_SMOKE:BEGIN`
//! - `GRAB_HOOK_SMOKE:bind-registered`
//! - `GRAB_HOOK_SMOKE:key-injected`
//! - `GRAB_HOOK_SMOKE:PASS`
//! - `GRAB_HOOK_SMOKE:FAIL step=N`
//!
//! `display_server` itself prints `display_server: bind triggered id=N`
//! when the dispatcher's grab arm fires; the regression's xtask waits
//! for that line as the load-bearing assertion.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use kernel_core::display::control::{ControlCommand, ControlEvent, decode_event, encode_command};
use kernel_core::input::events::MOD_SUPER;
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "grab-hook-smoke: alloc error\n");
    syscall_lib::exit(99)
}

const CONTROL_SERVICE_NAME: &str = "display-control";
const LABEL_CTL_CMD: u64 = 1;
const ENCODE_BUF_LEN: usize = 64;
const REPLY_BUF_LEN: usize = 256;
const SERVICE_LOOKUP_ATTEMPTS: u32 = 8;
const SERVICE_LOOKUP_BACKOFF_NS: u32 = 5_000_000;

const KEYCODE_Q: u32 = b'q' as u32;
const KEY_KIND_DOWN: u8 = 0;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "GRAB_HOOK_SMOKE:BEGIN\n");

    let handle = match lookup_with_backoff(CONTROL_SERVICE_NAME) {
        Some(h) => h,
        None => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "GRAB_HOOK_SMOKE:FAIL step=1 lookup display-control\n",
            );
            return 1;
        }
    };

    if !send_command(
        handle,
        &ControlCommand::RegisterBind {
            modifier_mask: MOD_SUPER,
            keycode: KEYCODE_Q,
        },
    ) {
        syscall_lib::write_str(STDOUT_FILENO, "GRAB_HOOK_SMOKE:FAIL step=2 register-bind\n");
        return 2;
    }
    syscall_lib::write_str(STDOUT_FILENO, "GRAB_HOOK_SMOKE:bind-registered\n");

    if !send_command(
        handle,
        &ControlCommand::InjectKey {
            modifier_mask: MOD_SUPER,
            keycode: KEYCODE_Q,
            kind: KEY_KIND_DOWN,
        },
    ) {
        syscall_lib::write_str(STDOUT_FILENO, "GRAB_HOOK_SMOKE:FAIL step=3 inject-key\n");
        return 3;
    }
    syscall_lib::write_str(STDOUT_FILENO, "GRAB_HOOK_SMOKE:key-injected\n");

    // Wait briefly so display_server's main loop drains the injected
    // key, routes it through the dispatcher, and logs `bind triggered
    // id=...`. The regression's xtask waits for that log line; this
    // smoke client just exits cleanly once the inject Ack returns.
    let _ = syscall_lib::nanosleep_for(0, 200_000_000);

    syscall_lib::write_str(STDOUT_FILENO, "GRAB_HOOK_SMOKE:PASS\n");
    0
}

fn send_command(handle: u32, cmd: &ControlCommand) -> bool {
    let mut req_buf = [0u8; ENCODE_BUF_LEN];
    let req_len = match encode_command(cmd, &mut req_buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let reply = syscall_lib::ipc_call_buf(handle, LABEL_CTL_CMD, 0, &req_buf[..req_len]);
    if reply == u64::MAX {
        return false;
    }
    // Drain the reply bulk and confirm Ack (or any non-Error event
    // for verbs that don't reply with a payload — the test's load-
    // bearing assertion is the `bind triggered` log, not the reply).
    let mut reply_buf = [0u8; REPLY_BUF_LEN];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX {
        return false;
    }
    if n == 0 {
        // Void reply — accept (some verbs are fire-and-forget Ack).
        return true;
    }
    let used = n as usize;
    match decode_event(&reply_buf[..used]) {
        Ok((ControlEvent::Ack, _)) => true,
        // Any non-Ack reply (Error, etc.) counts as failure.
        Ok((ControlEvent::Error { .. }, _)) | Err(_) => false,
        Ok((_, _)) => true,
    }
}

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

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "GRAB_HOOK_SMOKE:PANIC\n");
    syscall_lib::exit(101)
}
