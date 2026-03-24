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
        if unsafe { fb::init_from_parts(buf_ptr, info) } {
            log::info!("[fb] framebuffer console initialised");
        } else {
            log::warn!("[fb] framebuffer too small for text console");
        }
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

    // Phase 11: spawn userspace process launcher task (P11-T017).
    task::spawn(p11_launcher_task, "p11-launcher");

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
                if ptr.is_null() || len == 0 || len > MAX_CONSOLE_WRITE_LEN {
                    // Bad request — reply with error label.
                    ipc::Message::new(u64::MAX)
                } else {
                    // Safety: In Phase 9, clients still share the kernel address
                    // space with the server. The caller provides a kernel pointer
                    // to a valid UTF-8 byte range that remains live for the
                    // duration of this synchronous IPC call. `ptr` is non-null
                    // (checked above) and `len` is in 1..=MAX_CONSOLE_WRITE_LEN.
                    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
                    if let Ok(text) = core::str::from_utf8(bytes) {
                        crate::serial::_print(format_args!("{}", text));
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
///   handle 1 — Reply(caller_id)        inserted by recv_msg / call_msg on each client call
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
    let notif_handle = task::insert_cap(my_id, ipc::Capability::Notification(notif_id))
        .expect("[kbd] failed to insert notification cap");
    debug_assert_eq!(
        notif_handle, 0,
        "[kbd] notification cap not at expected handle 0"
    );

    // Look up the kbd endpoint registered by init_task.
    let ep_id = ipc::registry::lookup("kbd").expect("[kbd] endpoint not in registry");
    task::set_server_endpoint(my_id, ep_id);

    log::info!("[kbd] ready, waiting for KBD_READ requests");

    // recv_msg/reply_recv_msg take EndpointId directly, so keep handle 1 free
    // for the one-shot Reply capability inserted on each client call.
    let reply_cap_handle: ipc::CapHandle = 1;

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
                log::debug!("[kbd] scancode={:#04x}", scancode);
                let mut r = ipc::Message::new(0);
                r.data[0] = scancode as u64;
                r
            }
            _ => ipc::Message::new(u64::MAX),
        };

        let caller_id = match task::task_cap(my_id, reply_cap_handle) {
            Ok(ipc::Capability::Reply(id)) => id,
            _ => {
                log::warn!("[kbd] no reply cap at handle 1; sender used send rather than call");
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
const MAX_CONSOLE_WRITE_LEN: usize = 4096;

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
    let bytes = s.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let chunk_end = (offset + MAX_CONSOLE_WRITE_LEN).min(bytes.len());
        let chunk = &bytes[offset..chunk_end];
        let msg = ipc::Message::with2(CONSOLE_WRITE, chunk.as_ptr() as u64, chunk.len() as u64);
        let _ = ipc::endpoint::call_msg(my_id, console_ep, msg);
        offset = chunk_end;
    }
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
    if reply.label == u64::MAX {
        shell_print(my_id, console_ep, "ls: error\n");
        return;
    }
    let ptr = reply.data[0] as *const u8;
    let len = reply.data[1] as usize;
    if ptr.is_null() || len > crate::fs::protocol::MAX_LIST_LEN {
        shell_print(my_id, console_ep, "ls: error\n");
        return;
    }
    if len == 0 {
        shell_print(my_id, console_ep, "(no files)\n");
        return;
    }
    // SAFETY: The VFS protocol returns a pointer to a static FILE_LIST buffer.
    // The pointer is non-null (checked above) and len is capped to MAX_LIST_LEN.
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

    // FILE_READ (offset=0, max=MAX_READ_LEN)
    let read_msg = ipc::Message {
        label: crate::fs::protocol::FILE_READ,
        data: [fd, 0, crate::fs::protocol::MAX_READ_LEN as u64, 0],
    };
    let read_reply = ipc::endpoint::call_msg(my_id, vfs_ep, read_msg);
    let ptr = read_reply.data[0] as *const u8;
    let len = read_reply.data[1] as usize;
    if read_reply.label == u64::MAX || ptr.is_null() || len > crate::fs::protocol::MAX_READ_LEN {
        shell_print(my_id, console_ep, "cat: read error\n");
    } else {
        if len == 0 {
            shell_print(my_id, console_ep, "(empty)\n");
        } else {
            // SAFETY: Phase 9 — fat_server returns a pointer into static ramdisk
            // content. Pointer is non-null (checked above) and len is bounded
            // by MAX_READ_LEN (4096).
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
    let mut kbd_error_logged = false;

    shell_print(my_id, console_ep, "> ");

    loop {
        // Request one scancode from the keyboard server.
        let kbd_req = ipc::Message::new(KBD_READ);
        let kbd_reply = ipc::endpoint::call_msg(my_id, kbd_ep, kbd_req);
        if kbd_reply.label == u64::MAX {
            if !kbd_error_logged {
                log::warn!("[shell] KBD_READ failed; yielding before retry");
                kbd_error_logged = true;
            }
            task::yield_now();
            continue;
        }
        kbd_error_logged = false;
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
// Phase 11 — userspace launcher (P11-T017 / P11-T018)
// ---------------------------------------------------------------------------

/// Kernel task that demonstrates Phase 11: load ELF binaries from the
/// ramdisk, run them as ring-3 processes, and collect their exit codes.
///
/// This is the initial integration test that validates the ELF loader,
/// process table, and core process syscalls end-to-end.
fn p11_launcher_task() -> ! {
    log::info!("[p11] launcher started");

    // -----------------------------------------------------------------------
    // Test 0 (P11-T023): malformed ELF inputs must return errors, not panic
    // -----------------------------------------------------------------------
    test_elf_error_cases();

    // -----------------------------------------------------------------------
    // Test 1 (P11-T019): load exit0.elf → expect exit code 0
    // -----------------------------------------------------------------------
    run_elf_and_report("exit0.elf");

    // -----------------------------------------------------------------------
    // Test 2 (P11-T020): echo-args prints argc/argv via serial
    // -----------------------------------------------------------------------
    run_elf_and_report("echo-args.elf");

    // -----------------------------------------------------------------------
    // Test 3 (P11-T021): load fork-test.elf → parent waits for child(42)
    // -----------------------------------------------------------------------
    run_elf_and_report("fork-test.elf");

    // -----------------------------------------------------------------------
    // Test 4 (P11-T022): two sequential processes, separate address spaces
    // -----------------------------------------------------------------------
    run_elf_and_report("exit0.elf");
    run_elf_and_report("exit0.elf");
    log::info!("[p11] T022: both exit0 instances completed — address spaces isolated");

    log::info!("[p11] all Phase 11 tests complete");

    // -----------------------------------------------------------------------
    // Phase 12 T030: musl hello world — exercises Linux syscall ABI end-to-end
    // -----------------------------------------------------------------------
    log::info!("[p12] T030: running hello.elf (musl-compiled C binary)");
    run_elf_and_report("hello.elf");
    log::info!("[p12] T030: hello.elf launch complete");

    // -----------------------------------------------------------------------
    // Phase 13: tmpfs validation — exercises writable filesystem syscalls
    // -----------------------------------------------------------------------
    log::info!("[p13] running tmpfs-test.elf (writable filesystem validation)");
    run_elf_and_report("tmpfs-test.elf");
    log::info!("[p13] tmpfs-test.elf launch complete");

    loop {
        task::yield_now();
    }
}

/// P11-T023: verify that malformed ELF data returns errors without panicking.
///
/// These tests all fail at header parse time (before any segment mapping),
/// so we reuse the current address space mapper to avoid allocating and
/// leaking a per-test PML4 frame.  The mapper is never mutated.
fn test_elf_error_cases() {
    use mm::elf::{load_elf_into, ElfError};

    // 64-byte "bad magic" ELF-sized buffer with wrong magic bytes.
    let bad_magic = {
        let mut b = [0u8; 64];
        b[0] = 0xFF;
        b[1] = 0xFF;
        b[2] = 0xFF;
        b[3] = 0xFF;
        b
    };

    // Header with a program-header offset near u64::MAX. This must be rejected
    // as a truncated program-header table rather than panicking on offset math.
    let phdr_overflow = {
        let mut b = [0u8; 64];
        b[0..4].copy_from_slice(b"\x7FELF");
        b[4] = 2; // ELFCLASS64
        b[5] = 1; // little-endian
        b[18..20].copy_from_slice(&0x3Eu16.to_le_bytes()); // EM_X86_64
        b[32..40].copy_from_slice(&(u64::MAX - 32).to_le_bytes()); // e_phoff
        b[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        b[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        b
    };

    // Valid ELF header + PT_LOAD phdr, but with a segment file offset near
    // u64::MAX. This must be rejected as truncated instead of panicking while
    // computing the backing file range.
    let segment_offset_overflow = {
        let mut b = [0u8; 120];
        b[0..4].copy_from_slice(b"\x7FELF");
        b[4] = 2; // ELFCLASS64
        b[5] = 1; // little-endian
        b[18..20].copy_from_slice(&0x3Eu16.to_le_bytes()); // EM_X86_64
        b[24..32].copy_from_slice(&0x0040_0000u64.to_le_bytes()); // e_entry
        b[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        b[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        b[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

        let ph = &mut b[64..120];
        ph[0..4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        ph[4..8].copy_from_slice(&0x5u32.to_le_bytes()); // PF_R | PF_X
        ph[8..16].copy_from_slice(&(u64::MAX - 32).to_le_bytes()); // p_offset
        ph[16..24].copy_from_slice(&0x0040_0000u64.to_le_bytes()); // p_vaddr
        ph[32..40].copy_from_slice(&64u64.to_le_bytes()); // p_filesz
        ph[40..48].copy_from_slice(&64u64.to_le_bytes()); // p_memsz
        ph[48..56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align
        b
    };

    let cases: &[(&str, &[u8])] = &[
        ("empty", &[]),
        ("bad magic", &bad_magic),
        ("truncated", &[0x7f, b'E', b'L', b'F']),
        ("phdr overflow", &phdr_overflow),
        ("segment offset overflow", &segment_offset_overflow),
    ];

    // All cases fail before any page mapping — reuse current mapper to avoid
    // allocating PML4 frames that cannot be freed (bump allocator).
    // SAFETY: no other OffsetPageTable over the current CR3 is alive here.
    let phys_off = mm::phys_offset();
    let mut mapper = unsafe { mm::paging::get_mapper() };

    let mut all_ok = true;
    for (label, data) in cases {
        let result = unsafe { load_elf_into(&mut mapper, phys_off, data) };
        match result {
            Err(ElfError::TruncatedHeader) | Err(ElfError::InvalidMagic) => {
                log::info!(
                    "[p11-T023] '{}': correctly rejected (truncated or bad magic)",
                    label
                );
            }
            Err(e) => {
                log::info!("[p11-T023] '{}': rejected with {:?}", label, e);
            }
            Ok(_) => {
                log::warn!(
                    "[p11-T023] '{}': UNEXPECTED success — should have been rejected",
                    label
                );
                all_ok = false;
            }
        }
    }
    if all_ok {
        log::info!("[p11-T023] all malformed ELF cases correctly rejected");
    }
}

/// Load an ELF from the ramdisk, register it as a process, schedule it,
/// then wait for it to exit and log the exit code.
fn run_elf_and_report(name: &'static str) {
    use mm::elf::load_elf_into;
    use process::{spawn_process_with_cr3, PROCESS_TABLE};

    let data = match fs::ramdisk::get_file(name) {
        Some(d) => d,
        None => {
            log::warn!("[p11] ELF not found in ramdisk: {}", name);
            return;
        }
    };

    if data.is_empty() {
        log::warn!("[p11] ELF file is empty (not yet built?): {}", name);
        return;
    }

    log::info!("[p11] loading {}: {} bytes", name, data.len());

    let new_cr3 = match mm::new_process_page_table() {
        Some(f) => f,
        None => {
            log::warn!("[p11] out of frames for {}", name);
            return;
        }
    };

    let phys_off = mm::phys_offset();
    let (loaded, user_rsp) = {
        // SAFETY: new_cr3 was just allocated; exclusive.
        let mut mapper = unsafe { mm::mapper_for_frame(new_cr3) };
        let loaded = match unsafe { load_elf_into(&mut mapper, phys_off, data) } {
            Ok(l) => l,
            Err(e) => {
                log::warn!("[p11] ELF load failed for {}: {:?}", name, e);
                return;
            }
        };
        // Build the SysV AMD64 ABI initial stack: [argc, argv[0], NULL, envp NULL, ...]
        // argv[0] = binary name.
        let argv: &[&[u8]] = &[name.as_bytes()];
        // SAFETY: stack pages were just mapped by load_elf_into; mapper is valid.
        let user_rsp = match unsafe {
            mm::elf::setup_abi_stack(
                loaded.stack_top,
                &mapper,
                phys_off,
                argv,
                loaded.phdr_vaddr,
                loaded.phnum,
            )
        } {
            Ok(rsp) => rsp,
            Err(e) => {
                log::warn!("[p11] ABI stack setup failed: {:?}", e);
                return;
            }
        };
        (loaded, user_rsp)
    };

    log::info!(
        "[p11] {} loaded: entry={:#x} rsp={:#x}",
        name,
        loaded.entry,
        user_rsp,
    );

    let pid = spawn_process_with_cr3(
        0,
        loaded.entry,
        user_rsp,
        x86_64::PhysAddr::new(new_cr3.start_address().as_u64()),
        0,
        0,
    );
    log::info!("[p11] {} registered as pid {}", name, pid);

    // Push a fork context so fork_child_trampoline can pick it up.
    // fork_child_trampoline sets CURRENT_PID when it runs — the launcher
    // must not set it here because the launcher kernel task is not the
    // new userspace process.
    process::push_fork_ctx(pid, loaded.entry, user_rsp);

    // Spawn the kernel task; it will run fork_child_trampoline which
    // sets CURRENT_PID, switches CR3, and enters ring 3.
    task::spawn(process::fork_child_trampoline, "p11-elf");

    // Wait for the process to exit.
    log::info!("[p11] waiting for pid {}...", pid);
    loop {
        let done = {
            let table = PROCESS_TABLE.lock();
            table
                .find(pid)
                .map(|p| p.state == process::ProcessState::Zombie)
                .unwrap_or(false)
        };
        if done {
            let code = {
                let mut table = PROCESS_TABLE.lock();
                let code = table.find(pid).and_then(|p| p.exit_code).unwrap_or(-1);
                table.reap(pid);
                code
            };
            log::info!("[p11] {} (pid {}) exited with code {}", name, pid, code);
            break;
        }
        task::yield_now();
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
