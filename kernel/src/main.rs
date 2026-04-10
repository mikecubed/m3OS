#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(crate::testing::test_runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]

extern crate alloc;

mod acpi;
mod arch;
mod blk;
mod fb;
mod fs;
mod ipc;
mod mm;
mod net;
mod panic_diag;
mod pci;
mod pipe;
mod process;
mod pty;
mod rtc;
mod serial;
mod signal;
mod smp;
mod stdin;
mod task;
#[cfg(test)]
mod testing;
mod trace;
mod tty;

use alloc::{boxed::Box, string::String, vec, vec::Vec};
use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    serial::init_logger();

    serial_println!("[m3os] Hello from kernel!");
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

    // P15-T001: extract RSDP address before mm::init consumes boot_info.
    let rsdp_addr: Option<u64> = boot_info.rsdp_addr.into_option();

    mm::init(boot_info);

    // When built with `cargo test`, run the generated test harness and exit.
    // Placed after mm::init so that tests can use heap allocations.
    #[cfg(test)]
    test_main();

    // P9-T002: initialise framebuffer text console (fixed-font renderer).
    if let Some((buf_ptr, info)) = fb_parts {
        // SAFETY: buf_ptr is derived from boot_info.framebuffer which is
        // &'static mut; the mapping outlives the kernel.  mm::init does not
        // touch the framebuffer region.
        if unsafe { fb::init_from_parts(buf_ptr, info) } {
            log::info!("[fb] framebuffer console initialised");
            // Update TTY0 winsize to match the actual framebuffer dimensions.
            if let Some((rows, cols)) = fb::console_text_size() {
                let mut tty = tty::TTY0.lock();
                tty.winsize.ws_row = rows;
                tty.winsize.ws_col = cols;
                log::info!("[fb] TTY winsize set to {}x{}", rows, cols);
            }
        } else {
            log::warn!("[fb] framebuffer too small for text console");
        }
    } else {
        log::warn!("[fb] no framebuffer provided by bootloader");
    }

    // P15: ACPI table discovery — parse RSDP, RSDT/XSDT, MADT, FADT.
    acpi::init(rsdp_addr);

    // Phase 34: Read RTC and establish boot wall-clock time.
    rtc::init_rtc();

    // Smoke-test heap allocations (P2-T007)
    let boxed = Box::new(42u64);
    log::info!("[mm] Box::new(42) = {}", *boxed);

    let v: Vec<u32> = vec![1, 2, 3];
    log::info!("[mm] Vec alloc ok, len={}", v.len());

    let s = String::from("heap works");
    log::info!("[mm] String alloc ok: {}", s);

    // P15: Enumerate PCI buses and log discovered devices.
    pci::init();

    // Phase 24: Initialize virtio-blk driver.
    blk::init();

    // Enable PIC and unmask IRQs now that all subsystems are initialized.
    unsafe { arch::enable_interrupts() };
    log::info!("[arch] interrupts enabled");

    // Phase 15: switch from PIC to APIC interrupt routing.
    // Only attempt APIC init if ACPI MADT data is available; otherwise the
    // kernel falls back to the legacy PIC (which is already running).
    if acpi::io_apic_address().is_some() {
        arch::x86_64::apic::init();
    } else {
        log::warn!("[apic] MADT/I/O APIC not found — staying on legacy PIC");
    }

    // Phase 25: Initialize per-core data structures for the BSP.
    // Always called — gs_base must be set for the scheduler. If no MADT is
    // available, init_bsp_per_core() falls back to single-core BSP-only mode.
    smp::init_bsp_per_core();

    // Phase 16: Initialize virtio-net driver and route its IRQ.
    net::virtio_net::init();
    if net::virtio_net::VIRTIO_NET_READY.load(core::sync::atomic::Ordering::Acquire) {
        // Route the virtio-net PCI interrupt through the I/O APIC.
        let mut irq_routed = false;
        if let Some(dev) = net::virtio_net::find_virtio_net_device()
            && acpi::io_apic_address().is_some()
            && dev.interrupt_line != 0xFF
        {
            arch::x86_64::apic::route_pci_irq(
                dev.interrupt_line,
                arch::x86_64::interrupts::InterruptIndex::VirtioNet as u8,
            );
            irq_routed = true;
        }
        VIRTIO_NET_IRQ_ROUTED.store(irq_routed, core::sync::atomic::Ordering::Release);
        if !irq_routed {
            log::warn!("[net] virtio-net IRQ not routed — net_task will use periodic polling");
        }
    }

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

    // Phase 25: Boot Application Processors.
    // Only if SMP was initialized and there are APs to boot.
    if smp::is_per_core_ready() && smp::core_count() > 1 {
        smp::boot::boot_aps();
    }

    task::spawn(init_task, "init");
    task::spawn_idle(idle_task);

    log::info!("[kernel] entering scheduler — init will start service set");
    task::run()
}

// ---------------------------------------------------------------------------
// Phase 7 service tasks
// ---------------------------------------------------------------------------

/// init task: creates service endpoints, registers them, spawns servers,
/// then loads the userspace `/sbin/init` as PID 1.
fn init_task() -> ! {
    // Phase 7: console service endpoint.
    let console_ep = ipc::endpoint::ENDPOINTS.lock().create();
    ipc::registry::register("console", console_ep)
        .expect("[init] failed to register console service");
    log::info!("[init] service registry: console={:?}", console_ep);

    // Phase 52: kbd endpoint creation and registration moved to the userspace
    // kbd_server service (kernel/initrd/etc/services.d/kbd.conf).  The kernel
    // no longer pre-registers or spawns a ring-0 kbd_server_task.

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
    task::spawn(console_server_task, "console");
    // Phase 52: kbd_server_task removed — userspace kbd_server handles IRQ1.

    // Spawn Phase 8 storage tasks.
    task::spawn(fat_server_task, "fat");
    task::spawn(vfs_server_task, "vfs");

    // Spawn Phase 16 network processing task.
    if net::virtio_net::VIRTIO_NET_READY.load(core::sync::atomic::Ordering::Acquire) {
        task::spawn(net_task, "net");
    }

    // Phase 52: stdin_feeder_task removed — userspace stdin_feeder reads from
    // the userspace kbd_server via IPC and pushes to stdin via stdin_push syscall.

    // Phase 21: serial stdin feeder — reads bytes from COM1, feeds stdin buffer.
    // Allows testing ion interactively via `cargo xtask run` with piped input.
    task::spawn(serial_stdin_feeder_task, "serial-stdin");

    // Phase 20: load /sbin/init from ramdisk as userspace PID 1.
    spawn_userspace_init();

    log::info!("[init] service set started — yielding");
    loop {
        task::yield_now();
    }
}

/// Load `/sbin/init` from the ramdisk and launch it as userspace PID 1.
fn spawn_userspace_init() {
    use mm::elf::load_elf_into;

    let data = fs::ramdisk::get_file("sbin/init").expect("[init] /sbin/init not found in ramdisk");

    if data.is_empty() {
        panic!("[init] /sbin/init is empty — not built?");
    }

    log::info!("[init] loading /sbin/init: {} bytes", data.len());

    let new_cr3 = mm::new_process_page_table().expect("[init] out of frames for /sbin/init");
    let phys_off = mm::phys_offset();

    let argv: &[&[u8]] = &[b"/sbin/init"];
    let envp: &[&[u8]] = &[b"PATH=/bin:/sbin:/usr/bin", b"HOME=/", b"TERM=m3os"];

    let (loaded, user_rsp) = {
        let mut mapper = unsafe { mm::mapper_for_frame(new_cr3) };
        let loaded = unsafe { load_elf_into(&mut mapper, phys_off, data) }
            .expect("[init] ELF load failed for /sbin/init");
        let user_rsp = unsafe {
            mm::elf::setup_abi_stack_with_envp(
                loaded.stack_top,
                &mapper,
                phys_off,
                argv,
                envp,
                loaded.phdr_vaddr,
                loaded.phnum,
            )
        }
        .expect("[init] ABI stack setup failed for /sbin/init");
        (loaded, user_rsp)
    };

    log::info!(
        "[init] /sbin/init loaded: entry={:#x} rsp={:#x}",
        loaded.entry,
        user_rsp,
    );

    let pid = process::spawn_process_with_cr3(
        0,
        loaded.entry,
        user_rsp,
        x86_64::PhysAddr::new(new_cr3.start_address().as_u64()),
        0,
        0,
    );
    log::info!("[init] /sbin/init registered as pid {}", pid);

    task::spawn_fork_task(
        process::make_fork_ctx_zeroed(pid, loaded.entry, user_rsp),
        "userspace-init",
    );
}

/// Console server: receives IPC write requests, logs to serial, replies with ack.
///
/// # Data path
///
/// Callers pass a kernel-space pointer and length in the IPC message.  The
/// server **validates** the pointer range (non-null, bounded length, no
/// overflow) and then copies the bytes into a local buffer before use.
/// This eliminates the previous `from_raw_parts` shortcut that directly
/// aliased caller memory.
///
/// When this service is eventually extracted to a ring-3 process, the
/// validated copy will be replaced by `copy_from_user` which additionally
/// walks the caller's page tables.
///
/// # IPC protocol (label = CONSOLE_WRITE)
///
///   data\[0\] = pointer to UTF-8 string bytes (kernel address)
///   data\[1\] = byte length (must be 1..=4096)
///
/// Reply: label = 0 on success, `u64::MAX` on error.
///
/// # Service lifecycle (Phase 46)
///
/// Follows the standard service lifecycle: registers its endpoint via the
/// service registry, enters a recv/reply_recv loop, and is restart-safe
/// (the registry supports re-registration, and `cleanup_task_ipc` from
/// Track E cleans up endpoint/notification state if this task dies;
/// callers blocked in `BlockedOnReply` remain stuck — see docs/06-ipc.md).
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
                let ptr = msg.data[0];
                let len = msg.data[1] as usize;
                if ptr == 0
                    || len == 0
                    || len > MAX_CONSOLE_WRITE_LEN
                    || ptr.checked_add(len as u64).is_none()
                {
                    ipc::Message::new(u64::MAX)
                } else {
                    // Validated kernel-space copy: current callers are kernel tasks
                    // sharing the kernel address space. When this service moves to
                    // ring 3, callers will use copy_from_user instead.
                    let mut buf = alloc::vec![0u8; len];
                    unsafe {
                        core::ptr::copy_nonoverlapping(ptr as *const u8, buf.as_mut_ptr(), len);
                    }
                    if let Ok(text) = core::str::from_utf8(&buf) {
                        crate::serial::_print(format_args!("{}", text));
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

// Phase 52: kbd_server_task removed — the userspace kbd_server
// (kernel/initrd/etc/services.d/kbd.conf) now owns IRQ1, scancode translation,
// and KBD_READ IPC handling.

/// Console IPC operation label: write a UTF-8 string to the serial console.
///
/// data[0] = kernel pointer to string bytes, data[1] = byte length (max 4096).
const CONSOLE_WRITE: u64 = 0;
const MAX_CONSOLE_WRITE_LEN: usize = 4096;

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

/// Idle task: halts the CPU between timer ticks.
fn idle_task() -> ! {
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
        task::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Phase 21 — serial stdin feeder
// ---------------------------------------------------------------------------

/// Read bytes from the IRQ-driven serial ring buffer and feed them into the
/// kernel stdin buffer with canonical editing, echo, and signal support.
fn serial_stdin_feeder_task() -> ! {
    // Enable UART Receive Data Available interrupt (IER bit 0).
    unsafe {
        x86_64::instructions::port::Port::new(0x3F9u16).write(0x01u8);
    }

    log::info!("[serial-stdin] feeder ready (IRQ-driven, echo + signals)");

    loop {
        // Read from the lock-free ring buffer. If empty, disable interrupts,
        // re-check, and halt until the next IRQ — this closes the lost-wakeup
        // window without busy-polling.
        let byte = match crate::serial::serial_rx_pop() {
            Some(b) => b,
            None => {
                loop {
                    x86_64::instructions::interrupts::disable();
                    // Clear the pending flag and re-check the buffer while
                    // interrupts are disabled. If the IRQ fired between our
                    // pop() and disable(), the flag/buffer will be non-empty
                    // and we retry immediately instead of halting.
                    crate::serial::SERIAL_RX_PENDING
                        .store(false, core::sync::atomic::Ordering::SeqCst);
                    if let Some(b) = crate::serial::serial_rx_pop() {
                        x86_64::instructions::interrupts::enable();
                        break b;
                    }
                    // Atomically re-enable interrupts and halt. The next IRQ
                    // (serial or otherwise) will wake us.
                    x86_64::instructions::interrupts::enable_and_hlt();
                }
            }
        };

        // Delegate to the unified LineDiscipline in TTY0.
        let mut eof_signal = false;
        let result = {
            let mut t = tty::TTY0.lock();
            t.ldisc.process_byte(byte, &mut |data| {
                if data.is_empty() {
                    eof_signal = true;
                } else {
                    for &b in data {
                        stdin::push_char(b);
                    }
                }
            })
        };
        if eof_signal {
            stdin::signal_eof();
        }

        // Handle the result.
        match result {
            kernel_core::tty::LdiscResult::Consumed => {}
            kernel_core::tty::LdiscResult::Signal(sig) => {
                let fg = process::FG_PGID.load(core::sync::atomic::Ordering::Relaxed);
                if fg != 0 {
                    let name = match sig {
                        2 => "^C",
                        20 => "^Z",
                        3 => "^\\",
                        _ => "",
                    };
                    serial_echo(name);
                    serial_echo("\n");
                    process::send_signal_to_group(fg, sig as u32);
                } else {
                    stdin::push_char(byte);
                }
            }
            kernel_core::tty::LdiscResult::Pushed { ref echo }
            | kernel_core::tty::LdiscResult::LineComplete { ref echo } => {
                if let Some(count) = echo.erase_count() {
                    for _ in 0..count {
                        serial_echo("\x08 \x08");
                    }
                } else if !echo.is_empty() {
                    let echo_bytes = echo.as_slice();
                    if let Ok(s) = core::str::from_utf8(echo_bytes) {
                        serial_echo(s);
                    }
                }
            }
        }
    }
}

/// Echo a string back to the serial port (COM1).
fn serial_echo(s: &str) {
    for &b in s.as_bytes() {
        unsafe {
            // Wait for transmit holding register to be empty.
            while x86_64::instructions::port::Port::<u8>::new(0x3FD).read() & 0x20 == 0 {}
            x86_64::instructions::port::Port::new(0x3F8).write(b);
        }
    }
}

// ---------------------------------------------------------------------------
// Network task (P16-T055)
// ---------------------------------------------------------------------------

/// Whether the virtio-net IRQ was successfully routed through the I/O APIC.
/// Set during kernel_main init; read by net_task to choose IRQ vs polling mode.
static VIRTIO_NET_IRQ_ROUTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Background task that processes incoming network frames.
///
/// Polls the virtio-net driver for received frames and dispatches them through
/// the network stack (Ethernet → ARP/IPv4 → ICMP/UDP/TCP).
fn net_task() -> ! {
    let has_irq_routing = VIRTIO_NET_IRQ_ROUTED.load(core::sync::atomic::Ordering::Acquire);
    if !has_irq_routing {
        log::info!("[net] no APIC routing — using periodic poll mode");
    }
    log::info!("[net] network processing task started");

    let mut last_poll_tick: u64 = 0;

    loop {
        if has_irq_routing {
            // IRQ-driven path: drain work signaled by the IRQ flag.
            while arch::x86_64::interrupts::VIRTIO_NET_IRQ_PENDING
                .swap(false, core::sync::atomic::Ordering::Acquire)
            {
                net::dispatch::process_rx();
            }

            task::yield_now();

            // Race-free sleep.
            x86_64::instructions::interrupts::disable();
            if !arch::x86_64::interrupts::VIRTIO_NET_IRQ_PENDING
                .load(core::sync::atomic::Ordering::Acquire)
            {
                x86_64::instructions::interrupts::enable_and_hlt();
            } else {
                x86_64::instructions::interrupts::enable();
            }
        } else {
            // Legacy PIC fallback: the virtio-net IRQ is not routed, so poll
            // process_rx() every ~10 ticks (~100ms at 100 Hz).
            let now = arch::x86_64::interrupts::tick_count();
            if now.wrapping_sub(last_poll_tick) >= 10 {
                net::dispatch::process_rx();
                last_poll_tick = now;
            }
            task::yield_now();
        }
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
    // In test builds, delegate to the test panic handler which exits QEMU
    // with the failure code so `cargo xtask test` can detect the error.
    #[cfg(test)]
    testing::test_panic_handler(info);

    #[cfg(not(test))]
    {
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
        panic_diag::dump_crash_context();
        trace::dump_trace_rings();
        hlt_loop();
    }
}

#[alloc_error_handler]
fn alloc_error_handler(layout: alloc::alloc::Layout) -> ! {
    // The RetryAllocator already attempted heap growth and retry before this
    // handler is reached. If we get here, all growth attempts failed.
    panic!(
        "kernel OOM: failed to allocate {:?} after heap growth retry",
        layout
    );
}

// ---------------------------------------------------------------------------
// In-QEMU unit tests (run via `cargo xtask test`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::serial_println;

    #[test_case]
    fn trivial_assertion() {
        assert_eq!(1 + 1, 2);
    }

    #[test_case]
    fn serial_output_works() {
        serial_println!("serial output from test");
    }

    // -----------------------------------------------------------------------
    // Phase 33 — Memory subsystem tests
    // -----------------------------------------------------------------------

    /// A.4: Verify the kernel heap grows automatically via the RetryAllocator
    /// when the initial heap is exhausted.
    #[test_case]
    fn heap_grows_on_oom() {
        use crate::mm::heap::{HEAP_INITIAL_SIZE, heap_stats};
        use alloc::vec::Vec;

        let before = heap_stats();
        assert!(before.total_size >= HEAP_INITIAL_SIZE);

        // Allocate a series of 256 KiB blocks to push past initial heap.
        let block_size = 256 * 1024;
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        // Allocate enough to exceed the initial heap by 1 MiB.
        let target = HEAP_INITIAL_SIZE + (1024 * 1024);
        let mut total_allocated = 0usize;
        while total_allocated < target {
            let mut block = Vec::with_capacity(block_size);
            // Touch the memory to ensure it's actually mapped.
            block.resize(block_size, 0xAB);
            assert_eq!(block[0], 0xAB);
            assert_eq!(block[block_size - 1], 0xAB);
            blocks.push(block);
            total_allocated += block_size;
        }

        let after = heap_stats();
        // Heap must have grown beyond initial size.
        assert!(
            after.total_size > HEAP_INITIAL_SIZE,
            "heap did not grow: total_size={} initial={}",
            after.total_size,
            HEAP_INITIAL_SIZE
        );
        serial_println!(
            "heap grew: {} KiB → {} KiB ({} allocs)",
            before.total_size / 1024,
            after.total_size / 1024,
            after.alloc_count - before.alloc_count
        );

        // Drop all blocks — heap should have free space again.
        drop(blocks);
        let final_stats = heap_stats();
        assert!(final_stats.free_bytes > 0);
    }

    /// B: Verify buddy allocator manages frames correctly — alloc and free
    /// cycle doesn't leak.
    #[test_case]
    fn buddy_alloc_free_no_leak() {
        use crate::mm::frame_allocator;

        let before = frame_allocator::free_count();

        // Allocate 16 frames.
        let mut frames = alloc::vec::Vec::new();
        for _ in 0..16 {
            let frame = frame_allocator::allocate_frame().expect("frame alloc failed");
            frames.push(frame.start_address().as_u64());
        }

        let during = frame_allocator::free_count();
        assert!(
            during <= before - 16,
            "free count should have dropped by at least 16: before={} during={}",
            before,
            during
        );

        // Free all frames.
        for phys in frames {
            frame_allocator::free_frame(phys);
        }

        let after = frame_allocator::free_count();
        assert_eq!(
            after, before,
            "frame leak: before={} after={}",
            before, after
        );
    }

    /// B.4: Verify contiguous multi-page allocation works.
    #[test_case]
    fn contiguous_alloc_works() {
        use crate::mm::frame_allocator;

        let before = frame_allocator::free_count();

        // Allocate 4 contiguous pages (order 2).
        let frame = frame_allocator::allocate_contiguous(2).expect("contiguous alloc failed");
        let base = frame.start_address().as_u64();

        // Verify alignment: base must be 16 KiB aligned (4 pages).
        assert_eq!(
            base % (4096 * 4),
            0,
            "contiguous block not properly aligned"
        );

        // Free and verify no leak.
        frame_allocator::free_contiguous(base, 2);
        let after = frame_allocator::free_count();
        assert_eq!(
            after, before,
            "contiguous frame leak: before={} after={}",
            before, after
        );
    }

    /// C: Verify slab cache allocation and deallocation.
    #[test_case]
    fn slab_cache_alloc_free() {
        let caches = crate::mm::slab::caches();
        let mut fd_cache = caches.fd_cache.lock();

        let stats_before = fd_cache.stats();
        let mut page_counter = 0usize;

        // Allocate 10 objects from the FD cache (64-byte slots).
        let mut addrs = alloc::vec::Vec::new();
        for _ in 0..10 {
            let addr = fd_cache
                .allocate(&mut || {
                    // Page allocator callback: use frame allocator.
                    let frame = crate::mm::frame_allocator::allocate_frame()?;
                    page_counter += 1;
                    Some((crate::mm::phys_offset() + frame.start_address().as_u64()) as usize)
                })
                .expect("slab alloc failed");
            addrs.push(addr);
        }

        let stats_during = fd_cache.stats();
        assert_eq!(
            stats_during.active_objects,
            stats_before.active_objects + 10
        );

        // Free all objects.
        for addr in addrs {
            fd_cache.free(addr as usize);
        }

        let stats_after = fd_cache.stats();
        assert_eq!(stats_after.active_objects, stats_before.active_objects);
        serial_println!(
            "slab test: allocated {} objects using {} page(s)",
            10,
            page_counter
        );
    }

    /// F: Verify frame statistics are consistent.
    #[test_case]
    fn frame_stats_consistent() {
        let stats = crate::mm::frame_allocator::frame_stats();

        assert!(stats.total_frames > 0, "no frames reported");
        assert_eq!(
            stats.total_frames,
            stats.free_frames + stats.allocated_frames,
            "frame count mismatch: total={} free={} alloc={}",
            stats.total_frames,
            stats.free_frames,
            stats.allocated_frames
        );

        // Per-order free counts should sum to the total free count.
        let order_sum: usize = stats
            .free_by_order
            .iter()
            .enumerate()
            .map(|(order, &count)| count * (1 << order))
            .sum();
        assert_eq!(
            order_sum, stats.free_frames,
            "buddy order sum ({}) != free_frames ({})",
            order_sum, stats.free_frames
        );
        serial_println!(
            "frame stats: total={} free={} allocated={}",
            stats.total_frames,
            stats.free_frames,
            stats.allocated_frames
        );
    }

    /// F: Verify meminfo syscall returns non-empty data (heap stats).
    #[test_case]
    fn heap_stats_nonzero() {
        let stats = crate::mm::heap::heap_stats();
        assert!(stats.total_size > 0, "heap total_size is 0");
        assert!(stats.alloc_count > 0, "no allocations recorded");
        assert!(
            stats.total_size >= stats.free_bytes,
            "free > total: free={} total={}",
            stats.free_bytes,
            stats.total_size
        );
    }
}
