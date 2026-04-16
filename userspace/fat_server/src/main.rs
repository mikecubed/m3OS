//! Userspace FAT storage service for m3OS (Phase 54).
//!
//! Minimal supervised service: creates an IPC endpoint, registers as "fat",
//! and enters a recv loop.  This first slice does not serve actual file reads
//! — it exists to keep the "fat" service supervised under init so that later
//! phases can migrate FAT32 file I/O from ring 0 to ring 3 incrementally.
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
    syscall_lib::write_str(STDOUT_FILENO, "fat_server: alloc error\n");
    syscall_lib::exit(99)
}

syscall_lib::entry_point!(program_main);

/// Reply cap handle — kernel inserts the one-shot reply cap at handle 1
/// after each successful recv.
const REPLY_CAP_HANDLE: u32 = 1;

/// Negative `ENOSYS` as a reply label — signals "service exists but this
/// operation is not implemented", distinct from `u64::MAX` which callers
/// already use as a transport-level failure sentinel.
const NEG_ENOSYS: u64 = (-38i64) as u64;

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "fat_server: starting\n");

    // 1. Create an IPC endpoint.
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "fat_server: create_endpoint failed\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    // 2. Register as "fat" service.
    let ret = syscall_lib::ipc_register_service(ep_handle, "fat");
    if ret == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "fat_server: register_service failed\n");
        return 1;
    }

    syscall_lib::write_str(
        STDOUT_FILENO,
        "fat_server: registered, entering recv loop\n",
    );

    // 3. Supervised recv loop — reply with "not implemented" to any request.
    let mut msg = syscall_lib::IpcMessage::new(0);
    let mut buf = [0u8; 64];

    syscall_lib::ipc_recv_msg(ep_handle, &mut msg, &mut buf);

    loop {
        // Reply with -ENOSYS — no operations implemented in this slice.
        // Using a specific errno (not the u64::MAX transport sentinel) lets
        // callers tell "service up, op not implemented" from "IPC failure".
        msg = syscall_lib::IpcMessage::new(0);
        syscall_lib::ipc_reply_recv_msg(
            REPLY_CAP_HANDLE,
            NEG_ENOSYS,
            ep_handle,
            &mut msg,
            &mut buf,
        );
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "fat_server: PANIC\n");
    syscall_lib::exit(101)
}
