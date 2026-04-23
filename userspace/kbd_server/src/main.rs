//! Userspace keyboard service for m3OS (Phase 52).
//!
//! Handles `KBD_READ` IPC requests, draining scancodes from a
//! kernel-provided buffer after IRQ1 notification signals. This is the
//! ring-3 replacement for the kernel-resident `kbd_server_task`.
#![no_std]
#![no_main]

use syscall_lib::STDOUT_FILENO;

/// IPC operation label: read one scancode.
const KBD_READ: u64 = 1;
const REPLY_CAP_HANDLE: u32 = 1;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "kbd_server: starting\n");

    // 1. Create an IPC endpoint.
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "kbd_server: failed to create endpoint\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    // 2. Register as "kbd" in the service registry.
    let ret = syscall_lib::ipc_register_service(ep_handle, "kbd");
    if ret == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "kbd_server: failed to register 'kbd'\n");
        return 1;
    }

    syscall_lib::write_str(STDOUT_FILENO, "kbd_server: ready\n");

    // Service loop: wait for IPC requests.
    let mut label = syscall_lib::ipc_recv(ep_handle);

    loop {
        if label == KBD_READ {
            // Poll the scancode buffer until a key arrives.
            //
            // The original ring-3 keyboard server blocked in `notify_wait()` on
            // a dedicated IRQ1 notification capability. That path regressed
            // across the 55c notification changes and also forced the reply cap
            // to live at slot 2. Keeping the server on the simple endpoint-only
            // IPC pattern makes the reply cap stable at slot 1 and avoids
            // coupling keyboard input to the notification wake path.
            let scancode = loop {
                let sc = syscall_lib::read_kbd_scancode();
                if sc != 0 {
                    break sc;
                }
                let _ = syscall_lib::nanosleep_for(0, 5_000_000); // 5 ms
            };

            // Reply with label=scancode so ipc_call callers (which only
            // receive the reply label) can read it directly.
            syscall_lib::ipc_reply(REPLY_CAP_HANDLE, scancode as u64, 0);

            // Wait for next request.
            label = syscall_lib::ipc_recv(ep_handle);
        } else {
            // Unknown label — reply with error and continue.
            syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
            label = syscall_lib::ipc_recv(ep_handle);
        }
    }
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "kbd_server: PANIC\n");
    syscall_lib::exit(101)
}
