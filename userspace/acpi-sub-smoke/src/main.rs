//! `acpi-sub-smoke` — Phase 101 D.5/E.4 test subscriber.
//!
//! The `acpi-smoke` gate's Notify tail: registers its own event endpoint
//! in the service registry, subscribes to `acpid` by NAME (the
//! `ACPI_SUBSCRIBE` verb — acpid looks the name up to obtain a send
//! handle; a raw cap transfer would MOVE the endpoint cap out of this
//! process and orphan the receive side), then blocks until acpid pushes
//! one event and prints it. The gate launches it from the serial shell,
//! fires a QMP `system_powerdown`, and asserts the sentinels:
//!
//! - `ACPI_SUB:subscribed`                      — Subscribe round-tripped.
//! - `ACPI_SUB:event path=<asl-path> code=<c>`  — an event was pushed.
//! - `ACPI_SUB:error reason=<r>`                — any failure (gate fails).
//!
//! Protocol constants mirror `userspace/drivers/acpid/src/main.rs` (the
//! service's protocol doc is the contract).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use core::alloc::Layout;

use syscall_lib::heap::BrkAllocator;
use syscall_lib::{IpcMessage, STDOUT_FILENO, write_str};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "acpi-sub-smoke: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "acpi-sub-smoke: PANIC\n");
    syscall_lib::exit(101)
}

/// Mirror to serial (the gate oracle) and stdout.
fn log(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    syscall_lib::serial_print(msg);
}

/// `acpid` protocol (see the acpid module doc).
const ACPI_SERVICE_NAME: &str = "acpi";
const ACPI_SUBSCRIBE: u64 = 5;
const ACPI_EVENT: u64 = 6;
/// The event endpoint this subscriber registers; acpid resolves it via
/// `ipc_lookup_service` to obtain its send handle.
const SUB_SERVICE_NAME: &str = "acpi-sub-smoke.events";

fn program_main(_args: &[&str]) -> i32 {
    // The endpoint acpid will push events to — registered as a named
    // service so acpid can obtain its OWN send handle from the registry
    // (a cap transfer would MOVE this handle out of our table and orphan
    // the receive side; `grant_task_cap` deliberately never copies).
    let my_ep = syscall_lib::create_endpoint();
    if my_ep == u64::MAX {
        log("ACPI_SUB:error reason=create-endpoint\n");
        return 1;
    }
    let my_ep = my_ep as u32;
    if syscall_lib::ipc_register_service(my_ep, SUB_SERVICE_NAME) != 0 {
        log("ACPI_SUB:error reason=register-service\n");
        return 1;
    }

    // acpid registers "acpi" early in boot; retry briefly anyway.
    let mut svc = u64::MAX;
    for _ in 0..50 {
        svc = syscall_lib::ipc_lookup_service(ACPI_SERVICE_NAME);
        if svc != u64::MAX {
            break;
        }
        let _ = syscall_lib::nanosleep_for(0, 100_000_000);
    }
    let Ok(svc) = u32::try_from(svc) else {
        log("ACPI_SUB:error reason=no-acpi-service\n");
        return 1;
    };

    // Subscribe: bulk = our registered event-service name.
    let reply = syscall_lib::ipc_call_buf(
        svc,
        ACPI_SUBSCRIBE,
        SUB_SERVICE_NAME.len() as u64,
        SUB_SERVICE_NAME.as_bytes(),
    );
    if reply != 0 {
        log(&format!(
            "ACPI_SUB:error reason=subscribe-reply-{reply:#x}\n"
        ));
        return 1;
    }
    log("ACPI_SUB:subscribed\n");

    // Block until acpid pushes one event; the path rides the bulk (ASCII,
    // NUL-free — the buffer is zeroed so its end is the first NUL) and
    // the notify code rides data[0].
    let mut ev = IpcMessage::new(0);
    let mut bulk = [0u8; 256];
    loop {
        bulk.fill(0);
        let rc = syscall_lib::ipc_recv_msg(my_ep, &mut ev, &mut bulk);
        if rc == u64::MAX {
            continue;
        }
        if rc != ACPI_EVENT {
            continue;
        }
        let code = ev.data[0];
        let len = bulk.iter().position(|&b| b == 0).unwrap_or(bulk.len());
        let path = core::str::from_utf8(&bulk[..len]).unwrap_or("<non-utf8>");
        log(&format!("ACPI_SUB:event path={path} code={code:#x}\n"));
        return 0;
    }
}

syscall_lib::entry_point!(program_main);
