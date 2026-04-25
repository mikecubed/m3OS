//! Phase 56 Track C.1 — userspace display server (compositor) — scaffolding.
//!
//! This is the C.1 scaffolding: the binary builds, boots under init, and
//! registers itself in the service registry as `"display"`. Real graphical
//! behaviour (framebuffer acquisition, surface state machine, software
//! composition, client connection handshake, gfx-demo client) lands in
//! Tracks B.1–B.4 and C.2–C.6.
//!
//! Today's responsibilities:
//!   * Initialize the brk-backed userspace heap (needed because we depend on
//!     `kernel-core` and may use `alloc` types in later tracks).
//!   * Create an IPC endpoint and register it under the well-known service
//!     name `"display"`.
//!   * Idle in `ipc_recv` so init's supervisor sees a live, healthy daemon.
//!     Any client message is currently a no-op — the protocol dispatcher
//!     lands in C.5.
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
    syscall_lib::write_str(STDOUT_FILENO, "display_server: alloc error\n");
    syscall_lib::exit(99)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

/// Daemon entry point invoked by `syscall_lib`'s start-up shim.
///
/// Returns a process exit code. Under normal operation we never return —
/// the idle loop runs forever and termination only happens via signal
/// delivery or supervised restart from init.
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(
        STDOUT_FILENO,
        "display_server: starting (Phase 56 scaffolding)\n",
    );

    // Create our IPC endpoint and register as "display" so future graphical
    // clients can resolve us through the service registry. The actual
    // protocol-handling loop lands in C.5; here we just block on the
    // endpoint to keep the daemon alive and visible to init.
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "display_server: failed to create endpoint\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    let reg = syscall_lib::ipc_register_service(ep_handle, "display");
    if reg == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "display_server: failed to register 'display'\n",
        );
        return 1;
    }

    syscall_lib::write_str(STDOUT_FILENO, "display_server: registered as 'display'\n");

    // Idle loop: wait for any IPC. `ipc_recv` blocks until a message
    // arrives, so this does not spin. Tracks C.2–C.5 replace the no-op
    // body with the real client-protocol dispatcher.
    loop {
        let _label = syscall_lib::ipc_recv(ep_handle);
        // No-op for now; no protocol exists yet, so we silently drop the
        // message. The kernel does not require us to consume the reply
        // capability — it will be reaped when overwritten by the next
        // `ipc_recv`.
    }
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "display_server: PANIC\n");
    syscall_lib::exit(101)
}
