#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod arch;
mod fb;
mod fs;
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

    // P9-T001: parse framebuffer info before mm::init consumes boot_info.
    // `mm::init` takes `&'static mut BootInfo` which borrows the whole struct
    // for 'static, so we must extract the raw pointer + layout first.
    let fb_parts: Option<(*mut u8, bootloader_api::info::FrameBufferInfo)> =
        boot_info.framebuffer.as_mut().map(|fb| {
            let info = fb.info();
            // SAFETY: boot_info is &'static mut so the framebuffer memory is
            // valid for the kernel lifetime.  We extract a raw pointer here
            // and hand it to fb::init_from_parts after mm::init returns;
            // no other code accesses the framebuffer between these two points.
            let ptr: *mut u8 = fb.buffer_mut().as_mut_ptr();
            (ptr, info)
        });

    mm::init(boot_info);

    // P9-T002: initialise framebuffer text console (fixed-font renderer).
    if let Some((buf_ptr, info)) = fb_parts {
        // SAFETY: buf_ptr is derived from boot_info.framebuffer which is
        // &'static mut; the mapping outlives the kernel.  mm::init does not
        // touch the framebuffer region.
        unsafe { fb::init_from_parts(buf_ptr, info) };
        log::info!("[fb] framebuffer console initialised");
    } else {
        log::warn!("[fb] no framebuffer provided by bootloader");
    }

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
    // init_task creates a console IPC endpoint, registers it in the service
    // registry, spawns the server tasks, and then yields.
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
    // Phase 7: console service endpoint.
    let console_ep = ipc::endpoint::ENDPOINTS.lock().create();
    ipc::registry::register("console", console_ep)
        .expect("[init] failed to register console service");
    log::info!("[init] service registry: console={:?}", console_ep);

    // Phase 9: kbd endpoint — registered before spawning kbd_server_task so the
    // server can look it up via the registry on startup.
    let kbd_ep = ipc::endpoint::ENDPOINTS.lock().create();
    ipc::registry::register("kbd", kbd_ep).expect("[init] failed to register kbd service");
    log::info!("[init] service registry: kbd={:?}", kbd_ep);

    // Phase 8: fat_server endpoint — must be registered before vfs_server
    // spawns because vfs_server calls lookup("fat") during its startup.
    let fat_ep = ipc::endpoint::ENDPOINTS.lock().create();
    ipc::registry::register("fat", fat_ep).expect("[init] failed to register fat service");
    log::info!("[init] service registry: fat={:?}", fat_ep);

    // Phase 8: vfs_server endpoint.
    let vfs_ep = ipc::endpoint::ENDPOINTS.lock().create();
    ipc::registry::register("vfs", vfs_ep).expect("[init] failed to register vfs service");
    log::info!("[init] service registry: vfs={:?}", vfs_ep);

    // Spawn Phase 7 service tasks.
    // kbd_server_task creates its own notification internally.
    task::spawn(console_server_task, "console");
    task::spawn(kbd_server_task, "kbd");
    task::spawn(console_client_task, "console-client");

    // Spawn Phase 8 storage tasks.
    task::spawn(fat_server_task, "fat");
    task::spawn(vfs_server_task, "vfs");
    task::spawn(fs_client_task, "fs-client");

    // Spawn Phase 9 shell task.
    task::spawn(shell_task, "shell");

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
                        // P9-T003: mirror output to framebuffer console.
                        // Write text exactly as provided — no extra newline added here;
                        // callers are responsible for including '\n' when desired.
                        fb::write_str(text);
                        ipc::Message::new(0)
                    } else {
                        log::warn!("[console] received invalid UTF-8; rejecting write request");
                        ipc::Message::new(u64::MAX)
                    }
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
            _ => {
                // Sender used send() rather than call() — no reply cap was inserted.
                // Log a warning and recv the next message without replying.
                log::warn!("[console] no reply cap at handle 1; sender used send rather than call");
                msg = ipc::endpoint::recv_msg(my_id, ep_id);
                continue;
            }
        };
        let _ = task::remove_task_cap(my_id, reply_cap_handle);

        // Reply and immediately wait for the next message.
        msg = ipc::endpoint::reply_recv_msg(my_id, caller_id, ep_id, reply_msg);
    }
}

/// Keyboard server: serves KBD_READ IPC requests, blocking on IRQ1 when no
/// scancode is immediately available.
///
/// Capability table layout:
///   handle 0 — Notification(notif_id)  inserted by kbd_server itself
///   handle 1 — Endpoint(ep_id)         inserted by kbd_server itself
///   handle 2 — Reply(caller_id)        inserted by recv_msg / call_msg on each client call
fn kbd_server_task() -> ! {
    let my_id = task::current_task_id().expect("[kbd] no task id");

    // Create a notification and register it for IRQ1 with interrupts disabled
    // to avoid a race between create() and register_irq().
    let notif_id = x86_64::instructions::interrupts::without_interrupts(|| {
        let id = ipc::notification::create();
        ipc::notification::register_irq(1, id);
        id
    });

    // Handle 0: notification capability.
    task::insert_cap(my_id, ipc::Capability::Notification(notif_id))
        .expect("[kbd] failed to insert notification cap");

    // Look up the kbd endpoint registered by init_task.
    let ep_id = ipc::registry::lookup("kbd").expect("[kbd] endpoint not in registry");
    task::set_server_endpoint(my_id, ep_id);

    // Handle 1: endpoint capability.
    let _ep_handle = task::insert_cap(my_id, ipc::Capability::Endpoint(ep_id))
        .expect("[kbd] failed to insert endpoint cap");

    log::info!("[kbd] ready, waiting for KBD_READ requests");

    // Reply cap is inserted at handle 2 by recv_msg each time a client calls.
    let reply_cap_handle: ipc::CapHandle = 2;

    // First receive — blocks until a client sends KBD_READ.
    let mut msg = ipc::endpoint::recv_msg(my_id, ep_id);

    loop {
        let reply_msg = match msg.label {
            KBD_READ => {
                // Poll the ring buffer; if empty, sleep on IRQ notification.
                let scancode = loop {
                    if let Some(sc) = crate::arch::x86_64::interrupts::read_scancode() {
                        break sc;
                    }
                    // Block until the keyboard ISR fires.
                    ipc::notification::wait(my_id, notif_id);
                    // After waking, drain will happen on next iteration.
                };
                log::info!("[kbd] scancode={:#04x}", scancode);
                let mut r = ipc::Message::new(0);
                r.data[0] = scancode as u64;
                r
            }
            _ => ipc::Message::new(u64::MAX),
        };

        let caller_id = match task::task_cap(my_id, reply_cap_handle) {
            Ok(ipc::Capability::Reply(id)) => id,
            _ => {
                log::warn!("[kbd] no reply cap at handle 2; sender used send rather than call");
                msg = ipc::endpoint::recv_msg(my_id, ep_id);
                continue;
            }
        };
        let _ = task::remove_task_cap(my_id, reply_cap_handle);
        msg = ipc::endpoint::reply_recv_msg(my_id, caller_id, ep_id, reply_msg);
    }
}

/// Console IPC operation label: write a UTF-8 string to the serial console.
///
/// data[0] = kernel pointer to string bytes, data[1] = byte length (max 4096).
const CONSOLE_WRITE: u64 = 0;

/// Keyboard server IPC operation label: read one scancode.
///
/// Request: no data fields.
/// Reply:   data[0] = scancode (u8 as u64).  The server blocks on IRQ1 if no
///          scancode is available, so this call always returns a real scancode.
const KBD_READ: u64 = 1;

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

// ---------------------------------------------------------------------------
// Phase 8 storage tasks
// ---------------------------------------------------------------------------

/// Ramdisk filesystem server: serves FILE_OPEN / FILE_READ / FILE_CLOSE
/// requests by delegating to the static embedded ramdisk in `fs::ramdisk`.
fn fat_server_task() -> ! {
    let my_id = task::current_task_id().expect("[fat] no task id");

    // Look up this server's endpoint via the service registry.
    let ep_id = ipc::registry::lookup("fat").expect("[fat] endpoint not in registry");
    task::set_server_endpoint(my_id, ep_id);

    // Insert an endpoint capability at handle 0.
    let ep_handle = task::insert_cap(my_id, ipc::Capability::Endpoint(ep_id))
        .expect("[fat] failed to insert endpoint cap");
    assert_eq!(ep_handle, 0, "[fat] endpoint cap not at expected handle 0");

    log::info!("[fat] ready");

    let reply_cap_handle: ipc::CapHandle = 1;
    let mut msg = ipc::endpoint::recv_msg(my_id, ep_id);

    loop {
        // Delegate to the ramdisk handler (T003, T005: read-only, no mutations).
        let reply_msg = crate::fs::ramdisk::handle(&msg);

        // Consume the one-shot reply cap inserted by recv_msg.
        let caller_id = match task::task_cap(my_id, reply_cap_handle) {
            Ok(ipc::Capability::Reply(id)) => id,
            _ => {
                log::warn!("[fat] no reply cap at handle 1; sender used send rather than call");
                msg = ipc::endpoint::recv_msg(my_id, ep_id);
                continue;
            }
        };
        let _ = task::remove_task_cap(my_id, reply_cap_handle);

        msg = ipc::endpoint::reply_recv_msg(my_id, caller_id, ep_id, reply_msg);
    }
}

/// VFS routing server: accepts file requests from clients and forwards them
/// to the fat_server backend via IPC.
///
/// In Phase 8 there is one backend (fat_server). Phase 9+ will consult a
/// mount table to select the backend for each path prefix.
fn vfs_server_task() -> ! {
    let my_id = task::current_task_id().expect("[vfs] no task id");

    // Look up this server's own endpoint.
    let ep_id = ipc::registry::lookup("vfs").expect("[vfs] endpoint not in registry");
    task::set_server_endpoint(my_id, ep_id);

    let ep_handle = task::insert_cap(my_id, ipc::Capability::Endpoint(ep_id))
        .expect("[vfs] failed to insert endpoint cap");
    assert_eq!(ep_handle, 0, "[vfs] endpoint cap not at expected handle 0");

    // Find the fat_server backend endpoint — it must already be registered
    // (init_task registers "fat" before spawning vfs_server_task).
    //
    // NOTE: call_msg() takes EndpointId directly; no capability insert is
    // needed here.  Inserting a cap would occupy handle 1, which this server
    // reserves for incoming Reply caps from clients — causing a permanent
    // block on the first client call.
    let fat_ep_id = ipc::registry::lookup("fat").expect("[vfs] fat backend not in registry");

    log::info!("[vfs] ready, backend={:?}", fat_ep_id);

    let reply_cap_handle: ipc::CapHandle = 1;
    let mut msg = ipc::endpoint::recv_msg(my_id, ep_id);

    loop {
        // Check for the Reply cap before forwarding to the backend.  A client
        // using send() rather than call() inserts no Reply cap; forwarding via
        // call_msg() in that case would block the VFS task waiting for a fat
        // reply that will be discarded.  Skip the backend call entirely when
        // no reply cap is present.
        let caller_id = match task::task_cap(my_id, reply_cap_handle) {
            Ok(ipc::Capability::Reply(id)) => id,
            _ => {
                log::warn!("[vfs] no reply cap at handle 1; sender used send rather than call");
                msg = ipc::endpoint::recv_msg(my_id, ep_id);
                continue;
            }
        };

        // Forward the request to the fat_server backend and collect the full reply.
        let reply_msg = ipc::endpoint::call_msg(my_id, fat_ep_id, msg);

        let _ = task::remove_task_cap(my_id, reply_cap_handle);
        msg = ipc::endpoint::reply_recv_msg(my_id, caller_id, ep_id, reply_msg);
    }
}

/// Demo client: exercises the full VFS stack — open, read, and close two files
/// through vfs_server → fat_server → ramdisk.
///
/// Validates P8-T006 (open and read a known file), P8-T007 (missing file
/// returns predictable error), and P8-T008 (ownership boundary is exercised
/// across two IPC hops).
fn fs_client_task() -> ! {
    let my_id = task::current_task_id().expect("[fs-client] no task id");

    // Discover the VFS service endpoint.
    // call_msg() takes EndpointId directly; no cap insert is needed here.
    let vfs_ep_id = ipc::registry::lookup("vfs").expect("[fs-client] vfs service not found");

    // --- Open hello.txt (P8-T006) ---
    let name = "hello.txt";
    let open_msg = ipc::Message::with2(
        crate::fs::protocol::FILE_OPEN,
        name.as_ptr() as u64,
        name.len() as u64,
    );
    let open_reply = ipc::endpoint::call_msg(my_id, vfs_ep_id, open_msg);
    // Check the IPC label first: call_msg() returns label=u64::MAX with zeroed
    // data on IPC-level failure, which would be silently misread as fd=0.
    let fd = open_reply.data[0];
    if open_reply.label == u64::MAX || fd == u64::MAX {
        log::error!("[fs-client] FILE_OPEN(hello.txt) failed — unexpected");
    } else {
        log::info!("[fs-client] opened {} → fd={}", name, fd);

        // Read up to 256 bytes from offset 0.
        let read_msg = ipc::Message {
            label: crate::fs::protocol::FILE_READ,
            data: [fd, 0, 256, 0],
        };
        let read_reply = ipc::endpoint::call_msg(my_id, vfs_ep_id, read_msg);
        let content_ptr = read_reply.data[0] as *const u8;
        let content_len = read_reply.data[1] as usize;

        if read_reply.label == u64::MAX || content_ptr.is_null() {
            // IPC failure (label=u64::MAX) or protocol error (data[0]=null ptr).
            // content_len==0 alone is not an error — it indicates EOF.
            log::error!(
                "[fs-client] FILE_READ failed (label={:#x}, ptr={:?}) — unexpected",
                read_reply.label,
                content_ptr
            );
        } else if content_len == 0 {
            log::info!("[fs-client] FILE_READ returned 0 bytes (EOF or empty file)");
        } else {
            // SAFETY: Phase 8 — fat_server returns a pointer into 'static
            // ramdisk content. The pointer is valid for the lifetime of the
            // kernel, content_ptr is non-null (checked above), and
            // content_len is bounded by MAX_READ_LEN (4096).
            let bytes = unsafe { core::slice::from_raw_parts(content_ptr, content_len) };
            if let Ok(text) = core::str::from_utf8(bytes) {
                log::info!(
                    "[fs-client] read {} bytes: {:?}",
                    content_len,
                    text.trim_end_matches('\n')
                );
            } else {
                log::warn!("[fs-client] content is not valid UTF-8");
            }
        }

        // Close the fd (no-op in Phase 8, but exercises the close path).
        let close_msg = ipc::Message::with1(crate::fs::protocol::FILE_CLOSE, fd);
        let _ = ipc::endpoint::call_msg(my_id, vfs_ep_id, close_msg);
    }

    // --- Open a missing file (P8-T007: predictable error) ---
    let missing = "does-not-exist.txt";
    let open_missing = ipc::Message::with2(
        crate::fs::protocol::FILE_OPEN,
        missing.as_ptr() as u64,
        missing.len() as u64,
    );
    let missing_reply = ipc::endpoint::call_msg(my_id, vfs_ep_id, open_missing);
    if missing_reply.label == u64::MAX {
        // IPC-level failure (VFS or fat_server unreachable) — unexpected in a healthy system.
        log::error!(
            "[fs-client] FILE_OPEN({}) → IPC failure (unexpected)",
            missing
        );
    } else if missing_reply.data[0] == u64::MAX {
        // Protocol not-found sentinel — the expected outcome for a missing file.
        log::info!("[fs-client] FILE_OPEN({}) → not found (expected)", missing);
    } else {
        log::error!("[fs-client] FILE_OPEN missing file returned fd — unexpected");
    }

    log::info!("[fs-client] Phase 8 storage demo complete");

    loop {
        task::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Phase 9 shell tasks (T004–T009)
// ---------------------------------------------------------------------------

/// Translate a PS/2 scancode (make code, < 0x80) to an ASCII character.
///
/// Returns `None` for non-printable or unmapped scancodes.
fn scancode_to_char(sc: u8, shift: bool) -> Option<char> {
    // US-QWERTY layout.  Only make codes (< 0x80) are passed here.
    let (lo, hi): (Option<char>, Option<char>) = match sc {
        0x02 => (Some('1'), Some('!')),
        0x03 => (Some('2'), Some('@')),
        0x04 => (Some('3'), Some('#')),
        0x05 => (Some('4'), Some('$')),
        0x06 => (Some('5'), Some('%')),
        0x07 => (Some('6'), Some('^')),
        0x08 => (Some('7'), Some('&')),
        0x09 => (Some('8'), Some('*')),
        0x0A => (Some('9'), Some('(')),
        0x0B => (Some('0'), Some(')')),
        0x0C => (Some('-'), Some('_')),
        0x0D => (Some('='), Some('+')),
        0x10 => (Some('q'), Some('Q')),
        0x11 => (Some('w'), Some('W')),
        0x12 => (Some('e'), Some('E')),
        0x13 => (Some('r'), Some('R')),
        0x14 => (Some('t'), Some('T')),
        0x15 => (Some('y'), Some('Y')),
        0x16 => (Some('u'), Some('U')),
        0x17 => (Some('i'), Some('I')),
        0x18 => (Some('o'), Some('O')),
        0x19 => (Some('p'), Some('P')),
        0x1A => (Some('['), Some('{')),
        0x1B => (Some(']'), Some('}')),
        0x1E => (Some('a'), Some('A')),
        0x1F => (Some('s'), Some('S')),
        0x20 => (Some('d'), Some('D')),
        0x21 => (Some('f'), Some('F')),
        0x22 => (Some('g'), Some('G')),
        0x23 => (Some('h'), Some('H')),
        0x24 => (Some('j'), Some('J')),
        0x25 => (Some('k'), Some('K')),
        0x26 => (Some('l'), Some('L')),
        0x27 => (Some(';'), Some(':')),
        0x28 => (Some('\''), Some('"')),
        0x2B => (Some('\\'), Some('|')),
        0x2C => (Some('z'), Some('Z')),
        0x2D => (Some('x'), Some('X')),
        0x2E => (Some('c'), Some('C')),
        0x2F => (Some('v'), Some('V')),
        0x30 => (Some('b'), Some('B')),
        0x31 => (Some('n'), Some('N')),
        0x32 => (Some('m'), Some('M')),
        0x33 => (Some(','), Some('<')),
        0x34 => (Some('.'), Some('>')),
        0x35 => (Some('/'), Some('?')),
        0x39 => (Some(' '), Some(' ')),
        _ => (None, None),
    };
    if shift {
        hi
    } else {
        lo
    }
}

/// Send a string slice to the console server via CONSOLE_WRITE IPC.
fn shell_print(my_id: task::TaskId, console_ep: ipc::endpoint::EndpointId, s: &str) {
    if s.is_empty() {
        return;
    }
    let msg = ipc::Message::with2(CONSOLE_WRITE, s.as_ptr() as u64, s.len() as u64);
    let _ = ipc::endpoint::call_msg(my_id, console_ep, msg);
}

/// Dispatch a parsed command line to the appropriate built-in.
fn dispatch_command(
    my_id: task::TaskId,
    console_ep: ipc::endpoint::EndpointId,
    vfs_ep: ipc::endpoint::EndpointId,
    line: &str,
) {
    if line.is_empty() {
        return;
    }
    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let args = parts.next().unwrap_or("").trim();
    match cmd {
        "help" => {
            shell_print(
                my_id,
                console_ep,
                "commands: help  echo <text>  ls  cat <file>\n",
            );
        }
        "echo" => {
            shell_print(my_id, console_ep, args);
            shell_print(my_id, console_ep, "\n");
        }
        "ls" => cmd_ls(my_id, console_ep, vfs_ep),
        "cat" => cmd_cat(my_id, console_ep, vfs_ep, args),
        _ => {
            let err_msg = alloc::format!("unknown command: {}\n", cmd);
            shell_print(my_id, console_ep, &err_msg);
        }
    }
}

/// Built-in `ls`: list files via VFS FILE_LIST.
fn cmd_ls(
    my_id: task::TaskId,
    console_ep: ipc::endpoint::EndpointId,
    vfs_ep: ipc::endpoint::EndpointId,
) {
    let req = ipc::Message::new(crate::fs::protocol::FILE_LIST);
    let reply = ipc::endpoint::call_msg(my_id, vfs_ep, req);
    if reply.label == u64::MAX || reply.data[0] == 0 {
        shell_print(my_id, console_ep, "ls: error\n");
        return;
    }
    let ptr = reply.data[0] as *const u8;
    let len = reply.data[1] as usize;
    if len == 0 {
        shell_print(my_id, console_ep, "(no files)\n");
        return;
    }
    // SAFETY: Phase 9 — fat_server returns a pointer into static FILE_NAME_LIST
    // ramdisk data.  The pointer is non-null (checked above) and len is
    // bounded by the static buffer size.
    let list = unsafe { core::slice::from_raw_parts(ptr, len) };
    for name in list.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        if let Ok(s) = core::str::from_utf8(name) {
            shell_print(my_id, console_ep, s);
            shell_print(my_id, console_ep, "\n");
        }
    }
}

/// Built-in `cat`: open, read, and print a file via VFS.
fn cmd_cat(
    my_id: task::TaskId,
    console_ep: ipc::endpoint::EndpointId,
    vfs_ep: ipc::endpoint::EndpointId,
    filename: &str,
) {
    if filename.is_empty() {
        shell_print(my_id, console_ep, "usage: cat <file>\n");
        return;
    }

    // FILE_OPEN
    let open_msg = ipc::Message::with2(
        crate::fs::protocol::FILE_OPEN,
        filename.as_ptr() as u64,
        filename.len() as u64,
    );
    let open_reply = ipc::endpoint::call_msg(my_id, vfs_ep, open_msg);
    if open_reply.label == u64::MAX || open_reply.data[0] == u64::MAX {
        shell_print(my_id, console_ep, "cat: file not found\n");
        return;
    }
    let fd = open_reply.data[0];

    // FILE_READ (offset=0, max=4096)
    let read_msg = ipc::Message {
        label: crate::fs::protocol::FILE_READ,
        data: [fd, 0, 4096, 0],
    };
    let read_reply = ipc::endpoint::call_msg(my_id, vfs_ep, read_msg);
    if read_reply.label == u64::MAX || read_reply.data[0] == 0 {
        shell_print(my_id, console_ep, "cat: read error\n");
    } else {
        let ptr = read_reply.data[0] as *const u8;
        let len = read_reply.data[1] as usize;
        if len == 0 {
            shell_print(my_id, console_ep, "(empty)\n");
        } else {
            // SAFETY: Phase 9 — fat_server returns a pointer into static ramdisk
            // content.  Pointer is non-null (data[0] != 0 checked above) and len
            // is bounded by MAX_READ_LEN (4096).
            let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
            if let Ok(text) = core::str::from_utf8(bytes) {
                shell_print(my_id, console_ep, text);
                if !text.ends_with('\n') {
                    shell_print(my_id, console_ep, "\n");
                }
            } else {
                shell_print(my_id, console_ep, "(binary)\n");
            }
        }
    }

    // FILE_CLOSE
    let close_msg = ipc::Message::with1(crate::fs::protocol::FILE_CLOSE, fd);
    let _ = ipc::endpoint::call_msg(my_id, vfs_ep, close_msg);
}

/// Shell task: interactive line-oriented command interpreter (T005–T007).
///
/// Reads scancodes via KBD_READ IPC, echoes characters to the console,
/// and dispatches built-in commands (help, echo, ls, cat) on Enter.
fn shell_task() -> ! {
    let my_id = task::current_task_id().expect("[shell] no task id");

    let console_ep = ipc::registry::lookup("console").expect("[shell] console service not found");
    let kbd_ep = ipc::registry::lookup("kbd").expect("[shell] kbd service not found");
    let vfs_ep = ipc::registry::lookup("vfs").expect("[shell] vfs service not found");

    shell_print(my_id, console_ep, "[shell] ready — type 'help'\n");

    let mut line: Vec<u8> = Vec::new();
    let mut shift = false;

    shell_print(my_id, console_ep, "> ");

    loop {
        // Request one scancode from the keyboard server.
        let kbd_req = ipc::Message::new(KBD_READ);
        let kbd_reply = ipc::endpoint::call_msg(my_id, kbd_ep, kbd_req);
        let sc = kbd_reply.data[0] as u8;

        // Key-release (break) codes: bit 7 set.
        if sc >= 0x80 {
            let make = sc & 0x7F;
            if make == 0x2A || make == 0x36 {
                shift = false;
            }
            continue;
        }

        // Shift make codes.
        if sc == 0x2A || sc == 0x36 {
            shift = true;
            continue;
        }

        // Enter (0x1C): process line.
        if sc == 0x1C {
            shell_print(my_id, console_ep, "\n");
            let cmd_line =
                alloc::string::String::from(core::str::from_utf8(&line).unwrap_or("").trim());
            line.clear();
            dispatch_command(my_id, console_ep, vfs_ep, &cmd_line);
            shell_print(my_id, console_ep, "> ");
            continue;
        }

        // Backspace (0x0E): remove last character from buffer.
        if sc == 0x0E {
            if line.pop().is_some() {
                shell_print(my_id, console_ep, "\x08");
            }
            continue;
        }

        // Printable character.
        if let Some(c) = scancode_to_char(sc, shift) {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            line.extend_from_slice(s.as_bytes());
            shell_print(my_id, console_ep, s);
        }
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
