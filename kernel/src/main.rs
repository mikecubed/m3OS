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

    // Phase 7: Core Servers demo
    //
    // init_task creates IPC endpoints for console and kbd services, registers
    // them in the service registry, spawns the server tasks, and then yields.
    // console_client_task demonstrates service discovery and IPC-based output.
    task::spawn(init_task, "init");
    task::spawn_idle(idle_task);

    log::info!("[kernel] entering scheduler — init will start service set");
    task::run()
}

// ---------------------------------------------------------------------------
// Phase 7 service tasks
// ---------------------------------------------------------------------------

/// init task: creates service endpoints, registers them, spawns servers.
fn init_task() -> ! {
    // Create IPC endpoint for the console service.
    let console_ep = ipc::endpoint::ENDPOINTS.lock().create();

    // Register in the service registry so clients can look it up by name.
    ipc::registry::register("console", console_ep)
        .expect("[init] failed to register console service");

    log::info!("[init] service registry: console={:?}", console_ep);

    // Spawn service tasks and a demo client.
    // kbd_server_task creates its own notification internally and does not
    // serve IPC clients in Phase 7 — no endpoint registration needed.
    task::spawn(console_server_task, "console");
    task::spawn(kbd_server_task, "kbd");
    task::spawn(console_client_task, "console-client");

    log::info!("[init] service set started — yielding");
    loop {
        task::yield_now();
    }
}

/// Console server: receives IPC write requests, logs to serial, replies with ack.
///
/// IPC protocol (label=0, CONSOLE_WRITE):
///   data[0] = pointer to UTF-8 string bytes (kernel address)
///   data[1] = byte length (capped at 4096)
/// Reply: label=0 (ack)
fn console_server_task() -> ! {
    let my_id = task::current_task_id().expect("[console] no task id");

    // Look up this server's endpoint via the service registry.
    let ep_id = ipc::registry::lookup("console").expect("[console] endpoint not in registry");

    task::set_server_endpoint(my_id, ep_id);

    // Insert an endpoint capability at handle 0.
    let ep_handle = task::insert_cap(my_id, ipc::Capability::Endpoint(ep_id))
        .expect("[console] failed to insert endpoint cap");
    debug_assert_eq!(
        ep_handle, 0,
        "[console] endpoint cap not at expected handle 0"
    );

    log::info!("[console] ready");

    // First receive.
    let reply_cap_handle: ipc::CapHandle = 1;
    let mut msg = ipc::endpoint::recv_msg(my_id, ep_id);

    loop {
        let reply_msg = match msg.label {
            CONSOLE_WRITE => {
                // Handle the write request: data[0]=ptr, data[1]=len.
                let ptr = msg.data[0] as *const u8;
                let len = msg.data[1] as usize;
                if ptr.is_null() || len == 0 || len > 4096 {
                    // Bad request — reply with error label.
                    ipc::Message::new(u64::MAX)
                } else {
                    // Safety: In Phase 7, kernel tasks share the kernel address space.
                    // The pointer is a kernel static string address provided by the client.
                    // ptr is non-null (checked above) and len is in 1..=4096.
                    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
                    if let Ok(text) = core::str::from_utf8(bytes) {
                        log::info!("[console] {}", text.trim_end_matches('\n'));
                    }
                    ipc::Message::new(0)
                }
            }
            _ => {
                // Unknown operation — reply with error label.
                ipc::Message::new(u64::MAX)
            }
        };

        // Consume the one-shot reply cap inserted by recv_msg.
        let caller_id = match task::task_cap(my_id, reply_cap_handle) {
            Ok(ipc::Capability::Reply(id)) => id,
            _ => panic!("[console] expected reply cap at handle 1"),
        };
        let _ = task::remove_task_cap(my_id, reply_cap_handle);

        // Reply and immediately wait for the next message.
        msg = ipc::endpoint::reply_recv_msg(my_id, caller_id, ep_id, reply_msg);
    }
}

/// Keyboard server: waits for IRQ1 notification, logs each keypress.
///
/// Creates its own notification object and registers it for IRQ1.
/// In Phase 7 this server logs events; Phase 8+ will forward them to subscribed clients.
fn kbd_server_task() -> ! {
    let my_id = task::current_task_id().expect("[kbd] no task id");

    // Create a notification and register it for IRQ1 with interrupts disabled
    // to avoid a race between create() and register_irq().
    let notif_id = x86_64::instructions::interrupts::without_interrupts(|| {
        let id = ipc::notification::create();
        ipc::notification::register_irq(1, id);
        id
    });

    // Insert a notification capability at handle 0.
    task::insert_cap(my_id, ipc::Capability::Notification(notif_id))
        .expect("[kbd] failed to insert notification cap");

    log::info!("[kbd] ready, waiting for keyboard IRQ");

    loop {
        let bits = ipc::notification::wait(my_id, notif_id);
        log::info!("[kbd] keypress received (notification bits={:#b})", bits);
        // Drain the scancode ring buffer to avoid dropping events on rapid keypresses.
        while let Some(scancode) = crate::arch::x86_64::interrupts::read_scancode() {
            log::info!("[kbd] scancode={:#04x}", scancode);
        }
        // Phase 8+: forward to subscribed clients via IPC.
    }
}

/// Console IPC operation label: write a UTF-8 string to the serial console.
///
/// data[0] = kernel pointer to string bytes, data[1] = byte length (max 4096).
const CONSOLE_WRITE: u64 = 0;

/// Demo client: looks up the console service and sends one write request.
///
/// Demonstrates that a client task can discover the console service via the
/// registry and send output through it without knowing the endpoint ID up front.
static CONSOLE_MSG: &str = "Hello from console_client!";

fn console_client_task() -> ! {
    let my_id = task::current_task_id().expect("[console-client] no task id");

    // Discover the console service endpoint via the registry.
    let ep_id =
        ipc::registry::lookup("console").expect("[console-client] console service not found");

    // Insert an endpoint capability so we can call it.
    task::insert_cap(my_id, ipc::Capability::Endpoint(ep_id))
        .expect("[console-client] failed to insert endpoint cap");

    log::info!("[console-client] sending write request to console service");

    // Send: label=0 (CONSOLE_WRITE), data[0]=ptr, data[1]=len.
    let msg = ipc::Message::with2(0, CONSOLE_MSG.as_ptr() as u64, CONSOLE_MSG.len() as u64);
    let reply_label = ipc::endpoint::call(my_id, ep_id, msg);
    log::info!("[console-client] got ack (label={:#x})", reply_label);

    log::info!("[console-client] Phase 7 core-servers demo complete");

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
