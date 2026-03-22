#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod arch;
mod ipc;
mod mm;
mod process;
mod serial;
mod task;

use alloc::{boxed::Box, string::String, vec, vec::Vec};
use bootloader_api::{config::Mapping, entry_point, BootInfo, BootloaderConfig};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    serial::init_logger();

    serial_println!("[ostest] Hello from kernel!");
    log::info!("Kernel initialized");

    // Load GDT/IDT — no IRQs yet.
    arch::init();

    mm::init(boot_info);

    // Smoke-test heap allocations (P2-T007)
    let boxed = Box::new(42u64);
    log::info!("[mm] Box::new(42) = {}", *boxed);

    let v: Vec<u32> = vec![1, 2, 3];
    log::info!("[mm] Vec alloc ok, len={}", v.len());

    let s = String::from("heap works");
    log::info!("[mm] String alloc ok: {}", s);

    // Enable PIC and unmask IRQs now that all subsystems are initialized.
    unsafe { arch::enable_interrupts() };
    log::info!("[arch] interrupts enabled");

    // Trigger a breakpoint to verify the IDT is working (P3-T007).
    if cfg!(debug_assertions) {
        x86_64::instructions::interrupts::int3();
        log::info!("[arch] breakpoint exception handled OK");
    }

    // Verify timer IRQ is firing (P3-T008) — debug builds only.
    if cfg!(debug_assertions) {
        let start = arch::x86_64::interrupts::tick_count();
        let mut ticked = false;
        for _ in 0..10_000_000u32 {
            core::hint::spin_loop();
            if arch::x86_64::interrupts::tick_count().wrapping_sub(start) >= 1 {
                ticked = true;
                break;
            }
        }
        let ticks = arch::x86_64::interrupts::tick_count();
        if ticked {
            log::info!("[arch] timer ticks after wait: {}", ticks);
        } else {
            log::warn!("[arch] no timer ticks observed — IRQs may not be firing");
        }
    }

    // ---------------------------------------------------------------------------
    // Phase 6: IPC Core demo
    //
    // Demonstrates synchronous rendezvous IPC between two kernel threads:
    //   - server_task: blocks on recv, processes two messages with reply_recv
    //   - client_task: calls the server twice and logs each reply
    //
    // The scheduler drives the exchange.  The server logs each call it handles;
    // the client logs each reply it receives.  After the second exchange the
    // client exits and the system halts via the idle loop.
    // ---------------------------------------------------------------------------

    // Create a global IPC endpoint.  The server will recv on it; the client
    // will call it.  The EndpointId is baked into each task's closure via
    // a static (no_std closures cannot capture from the stack easily, so we
    // use task::spawn's fn pointer API with a global static for the ID).
    let ep_id = ipc::endpoint::ENDPOINTS.lock().create();
    // Safety: single-CPU boot, no concurrent access.
    unsafe {
        DEMO_EP = ep_id;
    }

    // Allocate a notification object for the kbd_server demo (P6-T011).
    // The keyboard ISR will signal bit 1 each keypress; the kbd_notif_task
    // blocks on wait() and logs the first keypress it receives.
    //
    // Wrapped in without_interrupts: register_irq writes IRQ_MAP atomically,
    // but we want IRQ1 masked until the mapping is fully visible.
    let notif_id = x86_64::instructions::interrupts::without_interrupts(|| {
        let id = ipc::notification::create();
        ipc::notification::register_irq(1, id);
        id
    });
    unsafe {
        DEMO_NOTIF = notif_id;
    }

    // Spawn the server, client, and kbd_notif tasks.
    task::spawn(server_task, "ipc-server");
    task::spawn(client_task, "ipc-client");
    task::spawn(kbd_notif_task, "kbd-notif");
    task::spawn_idle(idle_task);

    log::info!("[ipc] demo starting — entering scheduler");
    task::run()
}

// ---------------------------------------------------------------------------
// Demo task state
// ---------------------------------------------------------------------------

/// The IPC endpoint used by the demo.
static mut DEMO_EP: ipc::EndpointId = ipc::EndpointId(0);
/// The notification ID used for keyboard IRQ delivery.
static mut DEMO_NOTIF: ipc::notification::NotifId = ipc::notification::NotifId(0);

// ---------------------------------------------------------------------------
// Demo tasks
// ---------------------------------------------------------------------------

/// Server task: handles two consecutive IPC calls and then exits.
///
/// Uses `recv` for the first message, then `reply_recv` for the second,
/// demonstrating the standard server loop pattern.
fn server_task() -> ! {
    // Safety: written once before spawn, read-only from here.
    let ep_id = unsafe { DEMO_EP };
    let my_id = task::current_task_id().expect("server: no task id");

    // Register ourselves as the server of this endpoint so reply_recv can
    // find it.
    task::set_server_endpoint(my_id, ep_id);

    // Pre-insert an endpoint capability at handle 0.
    task::insert_cap(my_id, ipc::Capability::Endpoint(ep_id))
        .expect("server: failed to insert endpoint cap");

    log::info!("[ipc-server] waiting for first call");

    // First message: plain recv.
    let label = ipc::endpoint::recv(my_id, ep_id);
    log::info!("[ipc-server] received call label={}", label);

    // Reply cap was inserted into our table by recv() on behalf of the client.
    // Find it: it's at handle 1 (handle 0 is our endpoint cap).
    let reply_cap_handle: ipc::CapHandle = 1;
    let caller_id = match task::task_cap(my_id, reply_cap_handle) {
        Ok(ipc::Capability::Reply(id)) => id,
        _ => panic!("server: expected reply cap at handle 1"),
    };
    let _ = task::remove_task_cap(my_id, reply_cap_handle);

    // Reply + immediately wait for next message.
    log::info!("[ipc-server] replying and waiting for next call");
    let reply = ipc::Message::with1(0xBEEF, 42);
    let label2 = ipc::endpoint::reply_recv(my_id, caller_id, ep_id, reply);
    log::info!("[ipc-server] received second call label={}", label2);

    // Find the second reply cap (also at handle 1, re-inserted by reply_recv).
    let caller2_id = match task::task_cap(my_id, reply_cap_handle) {
        Ok(ipc::Capability::Reply(id)) => id,
        _ => panic!("server: expected reply cap at handle 1 for second call"),
    };
    let _ = task::remove_task_cap(my_id, reply_cap_handle);

    let reply2 = ipc::Message::with1(0xCAFE, 99);
    ipc::endpoint::reply(caller2_id, reply2);
    log::info!("[ipc-server] second reply sent — server done");

    // Yield forever; the idle task will keep the system alive.
    loop {
        task::yield_now();
    }
}

/// Client task: sends two IPC calls to the server and logs the replies.
fn client_task() -> ! {
    let ep_id = unsafe { DEMO_EP };
    let my_id = task::current_task_id().expect("client: no task id");

    // Insert an endpoint capability at handle 0.
    task::insert_cap(my_id, ipc::Capability::Endpoint(ep_id))
        .expect("server: failed to insert endpoint cap");

    log::info!("[ipc-client] sending first call");
    let reply_label = ipc::endpoint::call(my_id, ep_id, ipc::Message::new(0x1234));
    log::info!("[ipc-client] got first reply label={:#x}", reply_label);

    log::info!("[ipc-client] sending second call");
    let reply_label2 = ipc::endpoint::call(my_id, ep_id, ipc::Message::new(0x5678));
    log::info!("[ipc-client] got second reply label={:#x}", reply_label2);

    log::info!("[ipc-client] IPC demo complete");

    loop {
        task::yield_now();
    }
}

/// Keyboard notification task: blocks until the first keypress, then logs it.
///
/// Demonstrates IRQ delivery via notification objects (P6-T007, P6-T011).
fn kbd_notif_task() -> ! {
    let notif_id = unsafe { DEMO_NOTIF };
    let my_id = task::current_task_id().expect("kbd-notif: no task id");

    // Insert a notification capability at handle 0.
    task::insert_cap(my_id, ipc::Capability::Notification(notif_id))
        .expect("kbd-notif: failed to insert notification cap");

    log::info!("[kbd-notif] waiting for keyboard IRQ via notification");
    let bits = ipc::notification::wait(my_id, notif_id);
    log::info!(
        "[kbd-notif] keyboard notification received, bits={:#b}",
        bits
    );

    loop {
        task::yield_now();
    }
}

/// Idle task: halts the CPU between timer ticks.
fn idle_task() -> ! {
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

// ---------------------------------------------------------------------------
// Kernel utilities
// ---------------------------------------------------------------------------

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    if let Some(location) = info.location() {
        serial::_panic_print(format_args!(
            "KERNEL PANIC at {}:{}\n",
            location.file(),
            location.line()
        ));
    } else {
        serial::_panic_print(format_args!("KERNEL PANIC at unknown location\n"));
    }
    serial::_panic_print(format_args!("  {}\n", info.message()));
    hlt_loop();
}

#[alloc_error_handler]
fn alloc_error_handler(layout: alloc::alloc::Layout) -> ! {
    panic!("allocation error: {:?}", layout)
}
