//! `clip-smoke` — the two-client clipboard round-trip helper for the
//! `clipboard-smoke` gate (Phase 105 Track B.4).
//!
//! A serial `Wait` can't see a clipboard transfer, and the round-trip's
//! whole point is that it crosses a process boundary — so this helper
//! runs it end to end between two *independent* `desktop_client`
//! connections (distinct client tokens):
//!
//! 1. The parent connects, copies `M3OS_CLIP_OK` (`set_clipboard`), and
//!    prints `CLIP:set`. It stays connected (so the offer isn't dropped).
//! 2. It `fork()`s a child, which connects as a fresh client, pastes
//!    (`get_clipboard`), and compares the bytes.
//! 3. The child prints `CLIP_ROUNDTRIP_OK` on an exact match, else
//!    `CLIP_ROUNDTRIP_FAIL`. The parent reaps it and exits.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use desktop_client::DisplayConnection;
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{STDOUT_FILENO, write_str};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "clip-smoke: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "clip-smoke: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const PAYLOAD: &str = "M3OS_CLIP_OK";

fn log(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    syscall_lib::serial_print(msg);
}

/// Phase 112 Track C.2 — sentinel prefix for the paste-only mode. The
/// gate greps for this on the serial console, so the exact bytes matter.
const PASTE_PREFIX: &str = "CLIP_PASTE:";

fn program_main(args: &[&str]) -> i32 {
    // Phase 112 Track C.2 — `clip-smoke --paste` is a *read-only* second
    // client: it connects, reads whatever offer the compositor currently
    // holds, and prints it. The Phase 112 gate uses this to verify that a
    // copy performed in `term` really landed in the compositor's
    // `ClipboardStore` — read back by an independent client, not by the
    // same one that wrote it (which would prove nothing about the broker).
    if args.iter().any(|a| *a == "--paste") {
        let conn = match DisplayConnection::connect_auto() {
            Some(c) => c,
            None => {
                log("CLIP_PASTE_FAIL reason=connect\n");
                return 1;
            }
        };
        let result = conn.get_clipboard();
        conn.goodbye();
        return match result {
            Some(bytes) => {
                // Print `CLIP_PASTE:<text>` on one line. Non-UTF-8 is not
                // expected (the only MIME tag is text/plain;utf-8) but is
                // reported rather than panicking.
                match core::str::from_utf8(&bytes) {
                    Ok(text) => {
                        log(PASTE_PREFIX);
                        log(text);
                        log("\n");
                        0
                    }
                    Err(_) => {
                        log("CLIP_PASTE_FAIL reason=not-utf8\n");
                        1
                    }
                }
            }
            None => {
                log("CLIP_PASTE_FAIL reason=request-failed\n");
                1
            }
        };
    }

    // ---- Client A (parent): copy the payload -------------------------
    let conn_a = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => {
            log("clip-smoke: client A cannot connect\n");
            return 1;
        }
    };
    if !conn_a.set_clipboard(PAYLOAD) {
        log("CLIP_ROUNDTRIP_FAIL reason=set\n");
        return 1;
    }
    log("CLIP:set\n");

    // ---- Client B (child): paste + compare ---------------------------
    let pid = syscall_lib::fork();
    if pid < 0 {
        log("clip-smoke: fork failed\n");
        return 1;
    }
    if pid == 0 {
        // Child is a distinct client (distinct token/surface via its own
        // pid). The parent stays alive so its offer is still held.
        let conn_b = match DisplayConnection::connect_auto() {
            Some(c) => c,
            None => {
                log("CLIP_ROUNDTRIP_FAIL reason=connect-b\n");
                syscall_lib::exit(1);
            }
        };
        match conn_b.get_clipboard() {
            Some(bytes) if bytes == PAYLOAD.as_bytes() => {
                log("CLIP_ROUNDTRIP_OK\n");
                conn_b.goodbye();
                syscall_lib::exit(0);
            }
            Some(_) => {
                log("CLIP_ROUNDTRIP_FAIL reason=mismatch\n");
                conn_b.goodbye();
                syscall_lib::exit(1);
            }
            None => {
                log("CLIP_ROUNDTRIP_FAIL reason=empty\n");
                conn_b.goodbye();
                syscall_lib::exit(1);
            }
        }
    }

    // ---- Parent: reap the child, then disconnect ----------------------
    let mut status = 0i32;
    let _ = syscall_lib::waitpid(pid as i32, &mut status, 0);
    conn_a.goodbye();
    let child_ok = status & 0x7f == 0 && (status >> 8) & 0xff == 0;
    if child_ok { 0 } else { 1 }
}
