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

    // 3. Create an IRQ1 notification.
    let notif_cap = syscall_lib::create_irq_notification(1);
    if notif_cap == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "kbd_server: failed to create IRQ1 notification\n",
        );
        return 1;
    }
    let notif_cap = notif_cap as u32;

    syscall_lib::write_str(STDOUT_FILENO, "kbd_server: ready\n");

    // 4. Service loop: wait for IPC requests.
    //
    // The reply capability is a one-shot handle inserted by the kernel into
    // our cap table when a client uses `ipc_call`. We track the handle slot
    // that the kernel uses (conventionally the next free slot after our
    // existing caps). Since ipc_recv returns the label, the reply cap is
    // implicitly available.
    //
    // First recv — blocks until a client sends a KBD_READ request.
    let mut label = syscall_lib::ipc_recv(ep_handle);

    loop {
        if label == KBD_READ {
            // Poll the scancode buffer; if empty, wait on IRQ1 notification.
            let scancode = loop {
                let sc = syscall_lib::read_kbd_scancode();
                if sc != 0 {
                    break sc;
                }
                // Block until the keyboard ISR fires.
                syscall_lib::notify_wait(notif_cap);
            };

            // The kernel inserts a Reply capability when a client uses
            // ipc_call. We need to find it. The convention for userspace
            // servers: the reply cap is at a known slot. Since we have
            // ep_handle and notif_cap already allocated, the reply cap
            // will be inserted at the next available slot.
            //
            // We use ipc_reply + ipc_recv (separate calls) because
            // ipc_reply supports data0 (the scancode) while ipc_reply_recv
            // does not carry data in the syscall ABI.
            //
            // The reply cap handle: the kernel inserts it after existing
            // caps. We need to figure out which slot. Looking at the kernel
            // code, recv/call inserts a Reply cap. For userspace, after
            // ipc_recv the reply cap is in the next free slot.
            //
            // With ep_handle (slot 0) and notif_cap (slot 1), the reply
            // cap will be at slot 2 on the first call. After we consume it
            // via ipc_reply the slot is freed, so it stays at slot 2.
            let reply_cap: u32 = 2;

            // Reply with label=scancode so ipc_call callers (which only
            // receive the reply label) can read it directly.
            syscall_lib::ipc_reply(reply_cap, scancode as u64, 0);

            // Wait for next request.
            label = syscall_lib::ipc_recv(ep_handle);
        } else {
            // Unknown label — reply with error and continue.
            let reply_cap: u32 = 2;
            syscall_lib::ipc_reply(reply_cap, u64::MAX, 0);
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
