//! Userspace mouse service for m3OS (Phase 56 Track D.2).
//!
//! Drains the kernel PS/2 mouse-packet ring (via `SYS_READ_MOUSE_PACKET =
//! 0x1015`, exposed as `syscall_lib::read_mouse_packet`) and lifts each 8-byte
//! wire packet into a `kernel_core::input::events::PointerEvent` with stable
//! relative deltas, button-edge tracking via the pure-logic `ButtonTracker`,
//! and (when IntelliMouse mode is active in the kernel) wheel deltas.
//!
//! ## Endpoint design choice
//!
//! Phase 56 D.2 mirrors D.1's **second-label-on-existing-endpoint** approach:
//! `mouse_server` registers exactly one IPC endpoint as service `"mouse"` and
//! dispatches by label. Today the only label is [`MOUSE_EVENT_PULL = 1`] —
//! clients pull, the server replies with `label = MOUSE_EVENT_PULL` and a
//! 37-byte `PointerEvent` wire payload as bulk data, or with the sentinel
//! `label = u64::MAX` on bounded-wait expiry. This keeps the wire shape
//! symmetric with `kbd`'s `KBD_EVENT_PULL = 2`.
//!
//! ## Phase 56 D.2 banner-only scaffold
//!
//! This is the initial scaffold commit: it brings up the IPC endpoint and
//! `"mouse"` registration, logs the startup banner / IRQ12 attach / ready
//! lines, and replies with the timeout sentinel to every incoming pull.
//! The follow-up commit wires up the full PS/2 drain → ButtonTracker →
//! PointerEvent encode pipeline.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "mouse_server: alloc error\n");
    syscall_lib::exit(99)
}

// ---------------------------------------------------------------------------
// Wire labels
// ---------------------------------------------------------------------------

/// Phase 56 Track D.2 label. Reply carries a 37-byte
/// `PointerEvent` wire payload as bulk data (zero-byte data0/label).
/// The `display_server` (D.3) is the expected consumer.
const MOUSE_EVENT_PULL: u64 = 1;

/// Reply-cap slot is fixed at 1 by the kernel's IPC ABI.
const REPLY_CAP_HANDLE: u32 = 1;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(
        STDOUT_FILENO,
        "mouse_server: starting (Phase 56 D.2 — PointerEvent pipeline online)\n",
    );

    // Phase 56 F.1 acceptance: input services emit a one-time log on startup
    // identifying which input source they will target. PS/2 AUX is IRQ 12 in
    // the legacy 8259 / IOAPIC mapping; the kernel's ps2.rs handler drains
    // bytes into the ring this server reads via `read_mouse_packet`.
    syscall_lib::write_str(
        STDOUT_FILENO,
        "mouse_server: attached to PS/2 AUX (IRQ 12) — kernel-decoded packets\n",
    );

    // 1. Create the IPC endpoint that backs the `mouse` service.
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "mouse_server: failed to create endpoint\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    // 2. Register as `"mouse"` so display_server can find us via a single
    //    service lookup. Label-based dispatch decides the request shape.
    let ret = syscall_lib::ipc_register_service(ep_handle, "mouse");
    if ret == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "mouse_server: failed to register 'mouse'\n");
        return 1;
    }

    syscall_lib::write_str(STDOUT_FILENO, "mouse_server: ready\n");

    // Phase 56 D.2 scaffold: dispatch loop replies with the timeout sentinel
    // for every request until the follow-up commit wires up the real
    // PS/2 drain pipeline. Unknown labels also get the sentinel so clients
    // can distinguish.
    let mut label = syscall_lib::ipc_recv(ep_handle);

    loop {
        match label {
            MOUSE_EVENT_PULL => {
                // Scaffold: respond with timeout sentinel until the real
                // pipeline lands.
                syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
            }
            _ => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "mouse_server: warn: unknown IPC label; replying with sentinel\n",
                );
                syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
            }
        }
        label = syscall_lib::ipc_recv(ep_handle);
    }
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "mouse_server: PANIC\n");
    syscall_lib::exit(101)
}
