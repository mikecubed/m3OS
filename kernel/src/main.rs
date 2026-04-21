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
mod iommu;
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
mod syscall;
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
    // `tmpfs::init()` is deferred to after this block so its heap / frame
    // usage doesn't perturb the frame-allocator baseline that some tests
    // snapshot.
    #[cfg(test)]
    test_main();

    // Phase 54: populate tmpfs with /tmp and /run top-level directories.
    // Must run after heap init so tmpfs allocations succeed, before any
    // task that opens files under those paths.
    fs::tmpfs::init();

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

    // Phase 55a (B): IOMMU discovery — consume decoded DMAR / IVRS tables,
    // build unit descriptor list, device-to-unit map, and reserved-region
    // set. No hardware bring-up yet (Tracks C / D / E follow).
    iommu::init();

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

    // Phase 16: Initialize NIC drivers.  Phase 55b E.5: the in-kernel e1000
    // driver has been deleted; device-specific 82540EM code now lives in
    // `userspace/drivers/e1000`. The kernel registers only virtio-net here;
    // the ring-3 e1000 driver registers its `RemoteNic` facade via IPC on
    // startup.
    net::virtio_net::init();

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

    // Phase 54: fat_server and vfs_server are now userspace processes.
    // Ring-0 endpoint pre-registration and task spawning removed —
    // the userspace crates register themselves on startup via IPC.

    // Spawn Phase 7 service tasks.
    task::spawn(console_server_task, "console");
    // Phase 52: kbd_server_task removed — userspace kbd_server handles IRQ1.

    // Spawn Phase 16 network processing task.  VirtIO-net ready is enough to
    // justify the task; the ring-3 e1000 driver (`userspace/drivers/e1000`)
    // delivers RX frames via RemoteNic IPC and does not require the task to
    // spin on a kernel-side IRQ flag.
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

// Phase 54: ring-0 fat_server_task and vfs_server_task removed.
// These are now userspace processes (userspace/fat_server, userspace/vfs_server).

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

/// Background task that processes incoming network frames.
///
/// Phase 55b E.5: the virtio-net driver installs its RX IRQ through the HAL
/// (`install_msi_irq` / `install_intx_irq`); the ISR sets
/// [`net::virtio_net::NET_IRQ_WOKEN`] and wakes this task. The ring-3 e1000
/// driver (`userspace/drivers/e1000`) delivers frames via `RemoteNic::inject_rx_frame`
/// which also sets [`net::NIC_WOKEN`]. Between IRQs the task parks via
/// [`task::scheduler::block_current_unless_woken`]; on wake it drains all
/// pending frames through the network dispatch stack.
fn net_task() -> ! {
    // Register this task's id with the virtio-net ISR so it can wake us.
    // The ring-3 e1000 driver wakes the task via RemoteNic IPC — no kernel
    // task-id registration is needed for it.
    if let Some(id) = task::scheduler::current_task_id() {
        net::virtio_net::set_net_task_id(id);
    }
    if let Some((
        id,
        pid,
        name,
        state,
        assigned_core,
        affinity_mask,
        last_ready_tick,
        last_migrated_tick,
    )) = task::scheduler::current_task_debug_snapshot()
    {
        log::info!(
            "[net] task snapshot: id={} pid={} name={} state={:?} assigned_core={} affinity={:#x} ready_at={} migrated_at={}",
            id.0,
            pid,
            name,
            state,
            assigned_core,
            affinity_mask,
            last_ready_tick,
            last_migrated_tick
        );
    }
    log::info!("[net] network processing task started");

    let mut wake_summary_seq: u64 = 0;
    let mut last_wake_attempts = 0;
    let mut last_wake_successes = 0;
    let mut last_wake_failures = 0;
    let mut last_wake_missing_id = 0;
    loop {
        // Clear the unified wake flag up front so any edge set between now
        // and park is still observable.
        net::NIC_WOKEN.store(false, core::sync::atomic::Ordering::Release);
        let mut any =
            net::virtio_net::NET_IRQ_WOKEN.swap(false, core::sync::atomic::Ordering::Acquire);
        while any {
            net::dispatch::process_rx();
            any = net::virtio_net::NET_IRQ_WOKEN.swap(false, core::sync::atomic::Ordering::Acquire);
        }
        let (wake_attempts, wake_successes, wake_failures, wake_missing_id) =
            net::virtio_net::wake_debug_counters();
        if wake_attempts != last_wake_attempts
            || wake_successes != last_wake_successes
            || wake_failures != last_wake_failures
            || wake_missing_id != last_wake_missing_id
        {
            wake_summary_seq = wake_summary_seq.wrapping_add(1);
            if let Some((
                id,
                pid,
                name,
                state,
                assigned_core,
                affinity_mask,
                last_ready_tick,
                last_migrated_tick,
            )) = task::scheduler::current_task_debug_snapshot()
            {
                log::info!(
                    "[net] wake-summary#{}: id={} pid={} name={} state={:?} assigned_core={} affinity={:#x} ready_at={} migrated_at={} attempts={} successes={} failures={} missing_task_id={}",
                    wake_summary_seq,
                    id.0,
                    pid,
                    name,
                    state,
                    assigned_core,
                    affinity_mask,
                    last_ready_tick,
                    last_migrated_tick,
                    wake_attempts,
                    wake_successes,
                    wake_failures,
                    wake_missing_id
                );
            }
            last_wake_attempts = wake_attempts;
            last_wake_successes = wake_successes;
            last_wake_failures = wake_failures;
            last_wake_missing_id = wake_missing_id;
        }
        // Park on the unified flag: the virtio-net ISR and RemoteNic both set
        // it, so a wake from either path reliably unblocks the task. If an IRQ
        // fires between the drain-loop exit and the park,
        // `block_current_unless_woken` observes `NIC_WOKEN` set and returns
        // immediately without sleeping.
        task::scheduler::block_current_unless_woken(&net::NIC_WOKEN);
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
    // The bootstrap/size-class allocator already attempted allocator-local
    // reclaim and any eligible heap-growth retry before this handler is
    // reached. If we get here, the allocation really is out of options.
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

    /// C.1/C.2: Verify the post-cutover allocator can satisfy large runtime
    /// allocations without disturbing the bootstrap heap accounting.
    #[test_case]
    fn heap_grows_on_oom() {
        use crate::mm::{
            frame_allocator::frame_stats,
            heap::{HEAP_INITIAL_SIZE, heap_stats},
        };
        use alloc::vec::Vec;

        let before = heap_stats();
        let frames_before = frame_stats();
        assert!(before.total_size >= HEAP_INITIAL_SIZE);
        assert!(
            before.size_class_active,
            "size-class allocator was not activated before runtime tests",
        );

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
        let frames_after = frame_stats();
        // Runtime allocations should increase allocator activity and consume
        // backing pages without requiring the bootstrap heap itself to grow.
        assert!(
            after.alloc_count > before.alloc_count,
            "allocator did not record new allocations: before={} after={}",
            before.alloc_count,
            after.alloc_count
        );
        assert!(
            after.page_backed_pages > before.page_backed_pages,
            "page-backed allocation count did not increase: before={} after={}",
            before.page_backed_pages,
            after.page_backed_pages
        );
        assert!(
            frames_after.allocated_frames > frames_before.allocated_frames,
            "frame usage did not increase: before={} after={}",
            frames_before.allocated_frames,
            frames_after.allocated_frames
        );
        serial_println!(
            "allocator grew via page-backed path: bootstrap={} KiB large_pages={} alloc_delta={}",
            after.total_size / 1024,
            after.page_backed_pages,
            after.alloc_count - before.alloc_count
        );

        // Drop all blocks — backing pages should return to the frame allocator.
        drop(blocks);
        let final_stats = heap_stats();
        let frames_final = frame_stats();
        assert!(
            frames_final.allocated_frames < frames_after.allocated_frames,
            "dropping blocks did not release backing pages: after={} final={}",
            frames_after.allocated_frames,
            frames_final.allocated_frames
        );
        assert_eq!(
            final_stats.page_backed_pages, before.page_backed_pages,
            "dropping blocks did not restore the page-backed allocation count: before={} final={}",
            before.page_backed_pages, final_stats.page_backed_pages
        );
        assert!(final_stats.free_bytes > 0);
    }

    /// B: Verify buddy allocator manages frames correctly — alloc and free
    /// cycle doesn't leak.
    #[test_case]
    fn buddy_alloc_free_no_leak() {
        use crate::mm::frame_allocator;

        // Pre-allocate the storage Vec so that its slab page consumption
        // happens before we snapshot the free count.  Without this, the
        // size-class allocator's first slab page allocation would appear
        // as a frame leak.
        let mut frames = alloc::vec::Vec::with_capacity(16);

        let before = frame_allocator::available_count();

        // Allocate 16 frames.
        for _ in 0..16 {
            let frame = frame_allocator::allocate_frame().expect("frame alloc failed");
            frames.push(frame.start_address().as_u64());
        }

        let during = frame_allocator::available_count();
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

        let after = frame_allocator::available_count();
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

        let before = frame_allocator::available_count();

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
        let after = frame_allocator::available_count();
        assert_eq!(
            after, before,
            "contiguous frame leak: before={} after={}",
            before, after
        );
    }

    /// D.4: Verify allocate_frame_zeroed returns a fully zeroed frame.
    #[test_case]
    fn allocate_frame_zeroed_returns_zeros() {
        use crate::mm::frame_allocator;

        // First, allocate a raw frame, write a non-zero pattern, and free it
        // so the buddy pool contains a "dirty" frame.
        let dirty = frame_allocator::allocate_frame().expect("alloc dirty frame");
        let phys = dirty.start_address().as_u64();
        let phys_off = crate::mm::phys_offset();
        let ptr = (phys_off + phys) as *mut u8;
        unsafe { core::ptr::write_bytes(ptr, 0xAB, 4096) };
        frame_allocator::free_frame(phys);

        // Now allocate via the zeroed path.
        let zeroed = frame_allocator::allocate_frame_zeroed().expect("alloc zeroed frame");
        let z_phys = zeroed.start_address().as_u64();
        let z_ptr = (phys_off + z_phys) as *const u8;
        let data = unsafe { core::slice::from_raw_parts(z_ptr, 4096) };
        assert!(
            data.iter().all(|&b| b == 0),
            "allocate_frame_zeroed returned non-zero content at frame {:#x}",
            z_phys
        );
        frame_allocator::free_frame(z_phys);
    }

    /// D.4: Stale-mapping reuse — dirty frames recycled through multiple
    /// alloc/free cycles must still be zeroed by allocate_frame_zeroed.
    /// Catches regressions where a new allocator path skips zeroing.
    #[test_case]
    fn zero_exposure_stale_reuse_cycles() {
        use crate::mm::frame_allocator;
        let phys_off = crate::mm::phys_offset();

        // Run 4 rounds with different poison patterns to defeat coincidence.
        let patterns: [u8; 4] = [0xDE, 0x55, 0xFF, 0x01];
        for (round, &pattern) in patterns.iter().enumerate() {
            let dirty = frame_allocator::allocate_frame().expect("alloc dirty");
            let phys = dirty.start_address().as_u64();
            unsafe {
                core::ptr::write_bytes((phys_off + phys) as *mut u8, pattern, 4096);
            }
            frame_allocator::free_frame(phys);

            let zeroed = frame_allocator::allocate_frame_zeroed().expect("alloc zeroed");
            let z_phys = zeroed.start_address().as_u64();
            let data =
                unsafe { core::slice::from_raw_parts((phys_off + z_phys) as *const u8, 4096) };
            assert!(
                data.iter().all(|&b| b == 0),
                "round {}: stale reuse leak at frame {:#x} (pattern {:#x})",
                round,
                z_phys,
                pattern
            );
            frame_allocator::free_frame(z_phys);
        }
    }

    /// D.4: map_user_pages end-to-end — exercises the real `map_user_pages`
    /// function (which calls `allocate_frame_zeroed` internally).  Poisons
    /// frames first so that any failure to zero would leave stale data.
    /// Verifies the mapped physical frames are clean via the physical offset.
    #[test_case]
    fn zero_exposure_map_user_pages_e2e() {
        use crate::mm::frame_allocator;
        use x86_64::structures::paging::{Mapper, PageTableFlags, Translate};
        let phys_off = crate::mm::phys_offset();

        // Poison 4 frames and return them to the pool.
        for pattern in [0xCC_u8, 0xDD, 0xEE, 0xFF] {
            let f = frame_allocator::allocate_frame().expect("alloc poison");
            let phys = f.start_address().as_u64();
            unsafe { core::ptr::write_bytes((phys_off + phys) as *mut u8, pattern, 4096) };
            frame_allocator::free_frame(phys);
        }

        // Call the real map_user_pages which allocates via allocate_frame_zeroed.
        const TEST_VBASE: u64 = 0x0000_7FFE_0000_0000;
        const N_PAGES: u64 = 4;
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;

        let mut mapper = unsafe { crate::mm::paging::get_mapper() };
        unsafe {
            crate::mm::user_space::map_user_pages(&mut mapper, TEST_VBASE, N_PAGES, flags)
                .expect("map_user_pages failed");
        }

        // Read back each mapped frame via physical offset and verify zero.
        let mut frame_addrs = [0u64; N_PAGES as usize];
        for i in 0..N_PAGES {
            let vaddr = x86_64::VirtAddr::new(TEST_VBASE + i * 4096);
            let paddr = mapper
                .translate_addr(vaddr)
                .expect("page not mapped after map_user_pages");
            frame_addrs[i as usize] = paddr.as_u64() & !0xFFF;
            let data = unsafe {
                core::slice::from_raw_parts((phys_off + frame_addrs[i as usize]) as *const u8, 4096)
            };
            assert!(
                data.iter().all(|&b| b == 0),
                "map_user_pages: stale data in page {} (frame {:#x})",
                i,
                frame_addrs[i as usize]
            );
        }

        // Cleanup: unmap and free.
        for i in 0..N_PAGES {
            let vaddr = x86_64::VirtAddr::new(TEST_VBASE + i * 4096);
            let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(vaddr);
            if let Ok((_frame, flush)) = mapper.unmap(page) {
                flush.flush();
            }
            frame_allocator::free_frame(frame_addrs[i as usize]);
        }
    }

    /// D.4: resolve_cow_fault end-to-end — sets up a real CoW-marked page in
    /// the current address space, calls the real `resolve_cow_fault`, and
    /// verifies that the new frame contains the parent's data with no stale
    /// content leakage.
    #[test_case]
    fn zero_exposure_resolve_cow_e2e() {
        use crate::mm::frame_allocator;
        use x86_64::structures::paging::{Mapper, PageTableFlags, Translate};
        let phys_off = crate::mm::phys_offset();

        // Poison a frame and return it so the pool has stale data for the CoW
        // destination.
        let stale = frame_allocator::allocate_frame().expect("alloc stale");
        let stale_phys = stale.start_address().as_u64();
        unsafe { core::ptr::write_bytes((phys_off + stale_phys) as *mut u8, 0xBE, 4096) };
        frame_allocator::free_frame(stale_phys);

        // Allocate the "parent" frame and fill with a distinguishable pattern.
        let parent = frame_allocator::allocate_frame().expect("alloc parent");
        let parent_phys = parent.start_address().as_u64();
        let parent_ptr = (phys_off + parent_phys) as *mut u8;
        for i in 0u16..4096 {
            unsafe { parent_ptr.add(i as usize).write((i & 0xFF) as u8) };
        }

        // Bump refcount to 2 so resolve_cow_fault takes the copy path.
        frame_allocator::refcount_inc(parent_phys);

        // Map the parent frame at a test user-space address with CoW flags:
        // PRESENT | USER_ACCESSIBLE | BIT_9 (CoW marker) | !WRITABLE.
        const COW_TEST_VADDR: u64 = 0x0000_7FFD_0000_0000;
        let cow_flags = PageTableFlags::PRESENT
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::BIT_9;
        let vaddr = x86_64::VirtAddr::new(COW_TEST_VADDR);
        unsafe {
            crate::mm::paging::map_current_user_page_locked(vaddr, parent, cow_flags)
                .expect("map CoW page failed");
        }

        // Call the real resolve_cow_fault — this allocates a new frame, copies
        // parent data, and remaps the PTE as writable.
        let resolved = crate::arch::x86_64::interrupts::resolve_cow_fault(COW_TEST_VADDR);
        assert!(resolved, "resolve_cow_fault returned false");

        // Find the new physical frame via translation.
        let mapper = unsafe { crate::mm::paging::get_mapper() };
        let new_paddr = mapper
            .translate_addr(vaddr)
            .expect("page not mapped after resolve_cow_fault");
        let new_phys = new_paddr.as_u64() & !0xFFF;

        // The new frame must differ from the parent (a copy was made).
        assert_ne!(
            new_phys, parent_phys,
            "resolve_cow_fault should have allocated a new frame"
        );

        // Verify every byte in the new frame matches the parent's pattern.
        let new_data =
            unsafe { core::slice::from_raw_parts((phys_off + new_phys) as *const u8, 4096) };
        for (i, &byte) in new_data.iter().enumerate() {
            let expected = (i & 0xFF) as u8;
            assert_eq!(
                byte, expected,
                "CoW copy mismatch at offset {}: got {:#x}, expected {:#x} (new frame {:#x})",
                i, byte, expected, new_phys
            );
        }

        // Cleanup: unmap the page and free both frames.
        let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(vaddr);
        drop(mapper);
        let mut mapper = unsafe { crate::mm::paging::get_mapper() };
        if let Ok((_f, flush)) = mapper.unmap(page) {
            flush.flush();
        }
        frame_allocator::free_frame(new_phys);
        // Parent frame had refcount bumped to 2; resolve_cow_fault decremented
        // to 1 via free_frame.  Decrement once more to actually free.
        frame_allocator::free_frame(parent_phys);
    }

    /// D.4: munmap + reuse — after freeing a batch of dirty frames
    /// (simulating munmap), every subsequent zeroed allocation must be clean.
    #[test_case]
    fn zero_exposure_munmap_reuse_batch() {
        use crate::mm::frame_allocator;
        let phys_off = crate::mm::phys_offset();

        const BATCH: usize = 8;
        let mut freed_addrs = [0u64; BATCH];

        // Allocate BATCH frames, poison each with a distinct pattern, free all.
        for (i, slot) in freed_addrs.iter_mut().enumerate() {
            let f = frame_allocator::allocate_frame().expect("alloc batch");
            let phys = f.start_address().as_u64();
            unsafe {
                core::ptr::write_bytes((phys_off + phys) as *mut u8, (0xA0 + i as u8), 4096);
            }
            *slot = phys;
        }
        for &phys in &freed_addrs {
            frame_allocator::free_frame(phys);
        }

        // Re-allocate BATCH frames via the zeroed path and verify each.
        for i in 0..BATCH {
            let z = frame_allocator::allocate_frame_zeroed().expect("alloc zeroed batch");
            let z_phys = z.start_address().as_u64();
            let data =
                unsafe { core::slice::from_raw_parts((phys_off + z_phys) as *const u8, 4096) };
            assert!(
                data.iter().all(|&b| b == 0),
                "munmap reuse batch[{}]: stale data at frame {:#x}",
                i,
                z_phys
            );
            frame_allocator::free_frame(z_phys);
        }
    }

    /// D.4: Contiguous-block zeroed allocation — multi-page allocations
    /// via allocate_contiguous_zeroed must zero every page in the block,
    /// even when backing frames previously held data.
    #[test_case]
    fn zero_exposure_contiguous_zeroed() {
        use crate::mm::frame_allocator;
        let phys_off = crate::mm::phys_offset();
        let page_size = 4096u64;

        // Allocate and poison a 4-page contiguous block (order 2), then free it.
        let dirty = frame_allocator::allocate_contiguous(2).expect("alloc dirty contig");
        let base = dirty.start_address().as_u64();
        for i in 0..4u64 {
            unsafe {
                core::ptr::write_bytes(
                    (phys_off + base + i * page_size) as *mut u8,
                    0xFE,
                    page_size as usize,
                );
            }
        }
        frame_allocator::free_contiguous(base, 2);

        // Re-allocate via the zeroed path.
        let zeroed = frame_allocator::allocate_contiguous_zeroed(2).expect("alloc zeroed contig");
        let z_base = zeroed.start_address().as_u64();
        for i in 0..4u64 {
            let data = unsafe {
                core::slice::from_raw_parts(
                    (phys_off + z_base + i * page_size) as *const u8,
                    page_size as usize,
                )
            };
            assert!(
                data.iter().all(|&b| b == 0),
                "contiguous zeroed: stale data in page {} of block at {:#x}",
                i,
                z_base
            );
        }
        frame_allocator::free_contiguous(z_base, 2);
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

        // Linux-like accounting: total = available + allocated,
        // where available = free (buddy) + per_cpu_cached.
        assert_eq!(
            stats.total_frames,
            stats.available_frames + stats.allocated_frames,
            "frame count mismatch: total={} available={} alloc={}",
            stats.total_frames,
            stats.available_frames,
            stats.allocated_frames
        );
        assert_eq!(
            stats.available_frames,
            stats.free_frames + stats.per_cpu_cached,
            "available mismatch: available={} free={} cached={}",
            stats.available_frames,
            stats.free_frames,
            stats.per_cpu_cached
        );

        // Per-order free counts should sum to free_frames (buddy-only, no per-CPU).
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
            "frame stats: total={} free={} available={} allocated={} per_cpu_cached={}",
            stats.total_frames,
            stats.free_frames,
            stats.available_frames,
            stats.allocated_frames,
            stats.per_cpu_cached
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

    // -----------------------------------------------------------------------
    // Phase 55b Track B.1 — sys_device_claim integration tests
    // -----------------------------------------------------------------------
    //
    // These run in the pre-`kernel_main` test harness, so `test_main()` is
    // invoked before `pci::init()`. The test forces PCI enumeration itself
    // so that a real BDF is available for the claim path. The assertions
    // cover first-claim success, duplicate-claim returns `Busy`, and
    // release re-opens the slot for a new PID.

    /// Track B.1: first claim succeeds; second claim on the same BDF by a
    /// different PID returns `Busy`; releasing for the owning PID restores
    /// the slot so a third PID can claim it.
    ///
    /// Cross-references the pure-logic assertions in
    /// `kernel_core::device_host::registry_logic::tests` — this test adds
    /// the kernel-side invariant that the `PciDeviceHandle` (and its
    /// IOMMU domain) round-trips through the registry correctly.
    #[test_case]
    fn device_host_claim_first_succeeds_duplicate_returns_busy() {
        use crate::syscall::device_host::{
            TestClaimError, test_owner_of, test_release_for_pid, test_try_claim_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        // Ensure the PCI bus has been scanned so a real device is available.
        // `pci::init` is idempotent on repeat calls — the second scan finds
        // the already-populated static list and logs the same devices.
        crate::pci::init();

        // Find the first unclaimed device so the test stays decoupled from
        // whatever QEMU happens to attach. If QEMU produces no PCI device
        // at all (very unusual), skip the test rather than fail.
        let mut key: Option<DeviceCapKey> = None;
        let mut idx = 0;
        while let Some(dev) = crate::pci::pci_device(idx) {
            let k = DeviceCapKey::new(0, dev.bus, dev.device, dev.function);
            if test_owner_of(k).is_none() {
                // Also check that it's not already claimed by an in-kernel
                // driver — if it is, claim_pci_device_by_bdf would return
                // `AlreadyClaimed` which the test would interpret as Busy.
                key = Some(k);
                break;
            }
            idx += 1;
        }
        let Some(key) = key else {
            serial_println!("device_host test skipped: no free PCI device in QEMU");
            return;
        };

        serial_println!(
            "device_host test using BDF {:04x}:{:02x}:{:02x}.{}",
            key.segment,
            key.bus,
            key.dev,
            key.func
        );

        // Use PID values in a range the kernel does not actually schedule —
        // current_pid() for the test runner is 0, so picking high sentinels
        // avoids any collision with real PIDs.
        const PID_A: crate::process::Pid = 0xC0FF_EE01;
        const PID_B: crate::process::Pid = 0xC0FF_EE02;
        const PID_C: crate::process::Pid = 0xC0FF_EE03;

        // Pre-clean in case a prior test left state (should not happen, but
        // defensive since the registry is a static global).
        let _ = test_release_for_pid(PID_A);
        let _ = test_release_for_pid(PID_B);
        let _ = test_release_for_pid(PID_C);

        // 1) First claim succeeds, recorded under PID_A.
        match test_try_claim_for_pid(PID_A, key) {
            Ok(()) => {}
            Err(e) => {
                // `AlreadyClaimed` here means an in-kernel driver beat us
                // to the slot during the pre-scan race — skip gracefully.
                if matches!(e, TestClaimError::Busy) {
                    serial_println!(
                        "device_host test skipped: BDF {:02x}:{:02x}.{} already claimed in kernel",
                        key.bus,
                        key.dev,
                        key.func,
                    );
                    return;
                }
                panic!(
                    "first claim failed unexpectedly: {:?} for BDF {:02x}:{:02x}.{}",
                    e, key.bus, key.dev, key.func
                );
            }
        }
        assert_eq!(
            test_owner_of(key),
            Some(PID_A),
            "ownership should track PID_A after first claim",
        );

        // 2) A second claim on the same BDF — whether by PID_A or PID_B —
        //    returns Busy. B.1 acceptance race: "exactly one succeeds".
        assert_eq!(
            test_try_claim_for_pid(PID_A, key),
            Err(TestClaimError::Busy),
            "same-PID duplicate claim must be Busy",
        );
        assert_eq!(
            test_try_claim_for_pid(PID_B, key),
            Err(TestClaimError::Busy),
            "cross-PID duplicate claim must be Busy",
        );
        assert_eq!(
            test_owner_of(key),
            Some(PID_A),
            "original owner's claim must survive the duplicate attempt",
        );

        // 3) PID_A exits (simulate via release_for_pid). Slot is now free
        //    and a fresh PID_C can claim it — this is the Phase 46 / 51
        //    supervisor-restart path exercised at the registry level.
        let freed = test_release_for_pid(PID_A);
        assert_eq!(freed, 1, "release_for_pid must free exactly one entry");
        assert_eq!(test_owner_of(key), None, "slot must be free after release");

        match test_try_claim_for_pid(PID_C, key) {
            Ok(()) => {}
            Err(e) => panic!("reclaim by PID_C failed: {:?}", e),
        }
        assert_eq!(test_owner_of(key), Some(PID_C));

        // 4) Double-release of an already-released PID must not panic; it
        //    returns zero freed slots (tests the -EBADF acceptance clause
        //    at the registry level).
        let double = test_release_for_pid(PID_A);
        assert_eq!(double, 0, "double-release must be safe and return 0");

        // Cleanup for a tidy global registry — the next test in the suite
        // should see the state it started with.
        let _ = test_release_for_pid(PID_C);
        assert_eq!(test_owner_of(key), None);

        serial_println!("device_host B.1 integration test passed");
    }

    // -----------------------------------------------------------------------
    // Phase 55b Tracks B.2 / B.3 / B.4 — device-host syscall integration tests
    // -----------------------------------------------------------------------

    /// Pick a free PCI BDF for the test. Returns `None` when no free device
    /// is available (test is skipped in that case).
    #[cfg(test)]
    fn pick_free_pci_bdf() -> Option<kernel_core::device_host::DeviceCapKey> {
        use crate::syscall::device_host::test_owner_of;
        use kernel_core::device_host::DeviceCapKey;
        crate::pci::init();
        let mut idx = 0;
        while let Some(dev) = crate::pci::pci_device(idx) {
            let k = DeviceCapKey::new(0, dev.bus, dev.device, dev.function);
            if test_owner_of(k).is_none() {
                return Some(k);
            }
            idx += 1;
        }
        None
    }

    // -- Track B.2 — sys_device_mmio_map integration tests -------------------

    /// Track B.2: recording an MMIO mapping under a claimed device, then
    /// calling `test_release_for_pid`, must clear both the claim slot and
    /// the MMIO registry entries it owned.
    #[test_case]
    fn device_host_mmio_release_cascades_to_mmio_entries() {
        use crate::syscall::device_host::{
            TestClaimError, test_mmio_count_for_pid, test_owner_of, test_record_mmio,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("device_host B.2 cascade test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE10;
        let _ = test_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "device_host B.2 cascade test skipped: BDF already in use by kernel driver"
                );
                return;
            }
            Err(e) => panic!("claim failed: {:?}", e),
        }

        // Record two MMIO entries under the same device — mimics a driver
        // that maps BAR0 and BAR2. Neither needs a real page table for the
        // cascade assertion.
        test_record_mmio(PID, key, 0, 0x1000, 0xdead_0000).expect("BAR0 mmio recorded");
        test_record_mmio(PID, key, 2, 0x2000, 0xdead_2000).expect("BAR2 mmio recorded");
        assert_eq!(
            test_mmio_count_for_pid(PID),
            2,
            "two MMIO entries should be present after recording"
        );

        // Release the claim — cleanup cascade must wipe both MMIO entries.
        let freed = test_release_for_pid(PID);
        assert_eq!(freed, 1, "expected 1 claim released");
        assert_eq!(
            test_mmio_count_for_pid(PID),
            0,
            "MMIO entries must be cleared by the cascade",
        );
        assert_eq!(test_owner_of(key), None);

        serial_println!("device_host B.2 cascade test passed");
    }

    /// Track B.2: the 33rd MMIO-map request against a single device-cap
    /// returns `CapacityExceeded` without corrupting the registry.
    #[test_case]
    fn device_host_mmio_capacity_cap_is_enforced() {
        use crate::syscall::device_host::{
            MAX_MMIO_PER_DEVICE, TestClaimError, TestMmioError, test_mmio_count_for_pid,
            test_record_mmio, test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("device_host B.2 capacity test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE11;
        let _ = test_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "device_host B.2 capacity test skipped: BDF already in use by kernel driver"
                );
                return;
            }
            Err(e) => panic!("claim failed: {:?}", e),
        }

        // Fill the per-device MMIO slot cap. BAR indices wrap 0..6 to stay
        // valid — the registry key is (pid, key, bar_index, user_va), so
        // the synthetic `user_va` values keep entries distinct.
        for i in 0..MAX_MMIO_PER_DEVICE {
            let bar_index = (i % 6) as u8;
            let user_va = 0xdead_0000 + (i as u64) * 0x1000;
            test_record_mmio(PID, key, bar_index, 0x1000, user_va)
                .unwrap_or_else(|e| panic!("record {i} failed: {:?}", e));
        }
        assert_eq!(test_mmio_count_for_pid(PID), MAX_MMIO_PER_DEVICE);

        // One more should be rejected with CapacityExceeded.
        let one_over = test_record_mmio(PID, key, 0, 0x1000, 0xbeef_0000);
        assert_eq!(one_over, Err(TestMmioError::CapacityExceeded));
        // Registry unchanged.
        assert_eq!(test_mmio_count_for_pid(PID), MAX_MMIO_PER_DEVICE);

        let freed = test_release_for_pid(PID);
        assert_eq!(freed, 1);
        assert_eq!(test_mmio_count_for_pid(PID), 0);

        serial_println!("device_host B.2 capacity test passed");
    }

    /// Track B.2: MMIO entry recorded against a device not claimed by the
    /// caller returns a `NotClaimed` error. This is the registry-level
    /// analogue of the cross-device negative test in F.3.
    #[test_case]
    fn device_host_mmio_record_without_claim_fails() {
        use crate::syscall::device_host::{TestMmioError, test_record_mmio, test_release_for_pid};
        use kernel_core::device_host::DeviceCapKey;

        // Use a deliberately-bogus BDF that no real PCI device should occupy
        // (b:d.f = FF:1F.7 on segment 0xFFFF).
        let key = DeviceCapKey::new(0xFFFF, 0xFF, 0x1F, 7);

        const PID: crate::process::Pid = 0xC0FF_EE12;
        let _ = test_release_for_pid(PID);

        let err = test_record_mmio(PID, key, 0, 0x1000, 0xdead_3000);
        assert_eq!(
            err,
            Err(TestMmioError::NotClaimed),
            "recording MMIO without a prior claim must fail with NotClaimed",
        );

        serial_println!("device_host B.2 no-claim test passed");
    }

    /// Track B.2: pure-logic bounds checks are host-tested in `kernel-core`,
    /// but this smoke test asserts the re-export surface is reachable from
    /// the kernel crate so downstream drivers see the same API.
    #[test_case]
    fn device_host_mmio_bounds_helpers_reachable_from_kernel() {
        use kernel_core::device_host::{
            MAX_MMIO_BAR_BYTES, MmioBoundsError, MmioCacheMode, build_mmio_window,
            cache_mode_for_bar, validate_mmio_bar_size,
        };

        assert_eq!(
            validate_mmio_bar_size(6, 0x1000),
            Err(MmioBoundsError::BarIndexOutOfRange)
        );
        assert_eq!(
            validate_mmio_bar_size(0, 0),
            Err(MmioBoundsError::ZeroSizedBar)
        );
        assert_eq!(
            validate_mmio_bar_size(0, MAX_MMIO_BAR_BYTES + 1),
            Err(MmioBoundsError::BarTooLarge),
        );
        assert_eq!(cache_mode_for_bar(true), MmioCacheMode::WriteCombining);
        assert_eq!(cache_mode_for_bar(false), MmioCacheMode::Uncacheable);

        let desc = build_mmio_window(0, 0xfebf_0000, 0x1000, false).expect("valid BAR");
        assert_eq!(desc.len, 0x1000);
        assert_eq!(desc.cache_mode, MmioCacheMode::Uncacheable);

        serial_println!("device_host B.2 bounds-reexport test passed");
    }

    // -- Track B.3 — sys_device_dma_alloc integration tests ------------------

    /// B.3: sys_device_dma_alloc returns a (user_va, iova, len) handle whose
    /// views of the backing frame are consistent — write via user_va, read
    /// via iova-equivalent kernel view, get the same byte.
    #[test_case]
    fn device_host_dma_alloc_yields_consistent_user_and_iova_views() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_count, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("B.3 dma_alloc test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE40;
        let _ = test_release_for_pid(PID);
        let _ = test_dma_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "B.3 dma_alloc test skipped: BDF {:02x}:{:02x}.{} busy",
                    key.bus,
                    key.dev,
                    key.func,
                );
                return;
            }
            Err(e) => panic!("unexpected claim error: {:?}", e),
        }

        let before = test_dma_count();
        let snap = test_dma_alloc_for_pid(PID, key, 4096, 4096)
            .expect("dma_alloc must succeed for a claimed device");
        assert_eq!(snap.len, 4096, "len must be rounded to the request");
        assert_ne!(snap.iova, 0, "iova must be non-zero");
        assert_ne!(snap.user_va, 0, "user_va must be non-zero");
        assert_eq!(
            test_dma_count(),
            before + 1,
            "registry must record the new allocation"
        );

        let sentinel: u8 = 0xA5;
        unsafe {
            core::ptr::write_volatile(snap.user_va as *mut u8, sentinel);
        }

        let kvirt_of_iova = (crate::mm::phys_offset() + snap.iova) as *const u8;
        let read_back = unsafe { core::ptr::read_volatile(kvirt_of_iova) };
        assert_eq!(
            read_back, sentinel,
            "user VA and IOVA must alias the same frame",
        );

        let flip: u8 = 0x5A;
        unsafe {
            core::ptr::write_volatile(kvirt_of_iova as *mut u8, flip);
        }
        let read_back_user = unsafe { core::ptr::read_volatile(snap.user_va as *const u8) };
        assert_eq!(
            read_back_user, flip,
            "IOVA-view write must be visible through user VA",
        );

        let freed = test_dma_release_for_pid(PID);
        assert_eq!(freed, 1, "release must free exactly one allocation");
        let _ = test_release_for_pid(PID);
        serial_println!("device_host B.3 dma_alloc integration test passed");
    }

    /// B.3: handle-info returns the registered `(user_va, iova, len)` triple
    /// verbatim.
    #[test_case]
    fn device_host_dma_handle_info_returns_registered_triple() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_handle_info, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("B.3 handle_info test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE41;
        let _ = test_release_for_pid(PID);
        let _ = test_dma_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => return,
            Err(e) => panic!("claim failed: {:?}", e),
        }

        let alloc_snap = test_dma_alloc_for_pid(PID, key, 8192, 0).expect("dma_alloc must succeed");
        let info_snap = test_dma_handle_info(PID, alloc_snap.id)
            .expect("handle_info must find the live allocation");
        assert_eq!(alloc_snap, info_snap);

        const OTHER: crate::process::Pid = 0xC0FF_EE42;
        assert!(test_dma_handle_info(OTHER, alloc_snap.id).is_none());

        let _ = test_dma_release_for_pid(PID);
        let _ = test_release_for_pid(PID);
        serial_println!("device_host B.3 handle_info integration test passed");
    }

    /// B.3: dma_alloc against a non-claimed BDF returns NoDevice.
    #[test_case]
    fn device_host_dma_alloc_rejects_unclaimed_device() {
        use crate::syscall::device_host::{TestDmaError, test_dma_alloc_for_pid};
        use kernel_core::device_host::DeviceCapKey;

        let key = DeviceCapKey::new(0, 0xFF, 0x1F, 7);
        const PID: crate::process::Pid = 0xC0FF_EE43;
        let err = test_dma_alloc_for_pid(PID, key, 4096, 4096)
            .expect_err("alloc must fail without a prior claim");
        assert_eq!(err, TestDmaError::NoDevice);
    }

    /// B.3: allocation-rollback discipline — bad size returns InvalidArg and
    /// leaves no state in the registry.
    #[test_case]
    fn device_host_dma_alloc_rollback_on_validation_error() {
        use crate::syscall::device_host::{
            TestClaimError, TestDmaError, test_dma_alloc_for_pid, test_dma_count,
            test_dma_release_for_pid, test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("B.3 rollback test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE44;
        let _ = test_release_for_pid(PID);
        let _ = test_dma_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => return,
            Err(e) => panic!("claim failed: {:?}", e),
        }

        let before_count = test_dma_count();
        let before_frames = crate::mm::frame_allocator::available_count();

        assert_eq!(
            test_dma_alloc_for_pid(PID, key, 0, 4096),
            Err(TestDmaError::InvalidArg)
        );
        assert_eq!(
            test_dma_alloc_for_pid(PID, key, 4096, 3),
            Err(TestDmaError::InvalidArg)
        );
        assert_eq!(
            test_dma_alloc_for_pid(PID, key, 4096, 8192),
            Err(TestDmaError::InvalidArg)
        );

        assert_eq!(test_dma_count(), before_count, "no registry entries added");
        crate::mm::frame_allocator::drain_per_cpu_caches();
        let after_frames = crate::mm::frame_allocator::available_count();
        assert_eq!(
            after_frames, before_frames,
            "no frames leaked on validation error (before={} after={})",
            before_frames, after_frames,
        );

        let _ = test_release_for_pid(PID);
        serial_println!("device_host B.3 rollback integration test passed");
    }

    /// B.3: cross-device negative — two distinct BDFs each get their own
    /// DMA allocation; a driver cannot introspect another driver's handle.
    #[test_case]
    fn device_host_dma_alloc_cross_device_is_independent() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_handle_info, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        crate::pci::init();
        let mut keys: alloc::vec::Vec<DeviceCapKey> = alloc::vec::Vec::new();
        let mut idx = 0;
        while let Some(dev) = crate::pci::pci_device(idx) {
            let k = DeviceCapKey::new(0, dev.bus, dev.device, dev.function);
            if crate::syscall::device_host::test_owner_of(k).is_none() {
                keys.push(k);
                if keys.len() == 2 {
                    break;
                }
            }
            idx += 1;
        }
        if keys.len() < 2 {
            serial_println!("B.3 cross-device test skipped: <2 free PCI devices");
            return;
        }
        let key_a = keys[0];
        let key_b = keys[1];

        const PID_A: crate::process::Pid = 0xC0FF_EE50;
        const PID_B: crate::process::Pid = 0xC0FF_EE51;
        let _ = test_release_for_pid(PID_A);
        let _ = test_release_for_pid(PID_B);
        let _ = test_dma_release_for_pid(PID_A);
        let _ = test_dma_release_for_pid(PID_B);

        if test_try_claim_for_pid(PID_A, key_a).is_err() {
            return;
        }
        if test_try_claim_for_pid(PID_B, key_b).is_err() {
            let _ = test_release_for_pid(PID_A);
            return;
        }

        let snap_a =
            test_dma_alloc_for_pid(PID_A, key_a, 4096, 4096).expect("PID_A dma_alloc on key_a");
        let snap_b =
            test_dma_alloc_for_pid(PID_B, key_b, 4096, 4096).expect("PID_B dma_alloc on key_b");

        assert_ne!(snap_a.id, snap_b.id);

        assert!(
            test_dma_handle_info(PID_A, snap_b.id).is_none(),
            "PID_A must not observe PID_B's allocation"
        );
        assert!(
            test_dma_handle_info(PID_B, snap_a.id).is_none(),
            "PID_B must not observe PID_A's allocation"
        );

        assert!(test_dma_handle_info(PID_A, snap_a.id).is_some());
        assert!(test_dma_handle_info(PID_B, snap_b.id).is_some());

        let _ = test_dma_release_for_pid(PID_A);
        let _ = test_dma_release_for_pid(PID_B);
        let _ = test_release_for_pid(PID_A);
        let _ = test_release_for_pid(PID_B);
        serial_println!("device_host B.3 cross-device test passed");
    }

    /// B.3: process-exit cleanup — every live DMA entry owned by the exiting
    /// PID is freed (registry entry gone, frames returned to buddy).
    #[test_case]
    fn device_host_dma_release_on_exit_is_clean() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_count, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("B.3 on-exit cleanup test skipped: no free PCI device");
            return;
        };

        const PID: crate::process::Pid = 0xC0FF_EE60;
        let _ = test_release_for_pid(PID);
        let _ = test_dma_release_for_pid(PID);

        match test_try_claim_for_pid(PID, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => return,
            Err(e) => panic!("claim failed: {:?}", e),
        }

        crate::mm::frame_allocator::drain_per_cpu_caches();
        let frames_before = crate::mm::frame_allocator::available_count();

        let _ = test_dma_alloc_for_pid(PID, key, 4096, 4096).expect("alloc 1");
        let _ = test_dma_alloc_for_pid(PID, key, 8192, 4096).expect("alloc 2");
        let _ = test_dma_alloc_for_pid(PID, key, 4096, 4096).expect("alloc 3");
        assert_eq!(test_dma_count(), 3, "three live allocations");

        let freed = test_dma_release_for_pid(PID);
        assert_eq!(freed, 3, "release_for_pid freed all allocations");
        assert_eq!(test_dma_count(), 0, "registry empty after release");

        crate::mm::frame_allocator::drain_per_cpu_caches();
        let frames_after = crate::mm::frame_allocator::available_count();
        assert_eq!(
            frames_after, frames_before,
            "all DMA frames must be returned to the buddy allocator \
             (before={} after={})",
            frames_before, frames_after,
        );

        let _ = test_release_for_pid(PID);
        serial_println!("device_host B.3 on-exit cleanup integration test passed");
    }

    // -- Track B.4 — sys_device_irq_subscribe integration test ---------------

    /// Track B.4: a synthetic device IRQ delivered through the device-IRQ
    /// dispatch table (the same path a real MSI vector would take) sets the
    /// requested bit atomically on the bound notification. `release_for_pid`
    /// tears the binding down so the vector is reusable.
    #[test_case]
    fn device_host_irq_subscribe_signals_notification_bit() {
        use crate::syscall::device_host::{
            TestClaimError, test_release_for_pid, test_synthetic_irq_subscribe_and_signal,
            test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("device_host B.4 test skipped: no free PCI device in QEMU");
            return;
        };

        const PID_D: crate::process::Pid = 0xC0FF_EE04;
        let _ = test_release_for_pid(PID_D);

        match test_try_claim_for_pid(PID_D, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "device_host B.4 test skipped: BDF {:02x}:{:02x}.{} already claimed",
                    key.bus,
                    key.dev,
                    key.func,
                );
                return;
            }
            Err(e) => panic!("B.4 claim failed: {:?}", e),
        }

        // Bind bit 3 to vector offset 0.
        let pending = match test_synthetic_irq_subscribe_and_signal(PID_D, key, 3, 0) {
            Ok(p) => p,
            Err(e) => panic!("B.4 synthetic bind/signal failed: {:?}", e),
        };
        assert_eq!(
            pending,
            1u64 << 3,
            "ISR shim must have set exactly bit 3 on the bound notification (got {:#x})",
            pending,
        );

        // Re-arm with a different bit/vector.
        let pending_bit7 = match test_synthetic_irq_subscribe_and_signal(PID_D, key, 7, 1) {
            Ok(p) => p,
            Err(e) => panic!("B.4 second synthetic bind failed: {:?}", e),
        };
        assert_eq!(pending_bit7, 1u64 << 7);

        let freed = test_release_for_pid(PID_D);
        assert_eq!(freed, 1, "exactly one claim freed on exit");

        serial_println!("device_host B.4 integration test passed");
    }

    // -- Track B.4b — caller-provided NotifId path ----------------------------

    /// Track B.4b: the caller-provided notification path.
    ///
    /// The test pre-allocates a `Notification`, passes it to the synthetic
    /// IRQ bind helper (simulating what `sys_device_irq_subscribe` does when
    /// `notification_arg != SENTINEL_NEW`), verifies the ISR shim delivers
    /// to the correct bit, and confirms that the process-exit teardown does
    /// NOT free the caller-owned notification slot (pool count unchanged after
    /// the binding is torn down).
    #[test_case]
    fn device_host_irq_subscribe_caller_provided_notif() {
        use crate::syscall::device_host::{
            TestClaimError, test_release_for_pid,
            test_synthetic_irq_subscribe_and_signal_with_existing_notif, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("device_host B.4b test skipped: no free PCI device in QEMU");
            return;
        };

        const PID_E: crate::process::Pid = 0xC0FF_EE05;
        let _ = test_release_for_pid(PID_E);

        match test_try_claim_for_pid(PID_E, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!(
                    "device_host B.4b test skipped: BDF {:02x}:{:02x}.{} already claimed",
                    key.bus,
                    key.dev,
                    key.func,
                );
                return;
            }
            Err(e) => panic!("B.4b claim failed: {:?}", e),
        }

        // Pre-allocate a notification the "caller" owns.
        let caller_notif = crate::ipc::notification::try_create()
            .expect("notification pool must have a free slot");
        let pool_before = crate::ipc::notification::allocated_count();

        // Bind bit 5 to vector offset 2 using the caller-provided notification.
        let pending = match test_synthetic_irq_subscribe_and_signal_with_existing_notif(
            PID_E,
            key,
            caller_notif,
            5,
            2,
        ) {
            Ok(p) => p,
            Err(e) => {
                crate::ipc::notification::release(caller_notif);
                let _ = test_release_for_pid(PID_E);
                panic!("B.4b synthetic bind/signal (caller-notif) failed: {:?}", e);
            }
        };

        assert_eq!(
            pending,
            1u64 << 5,
            "ISR shim must have set exactly bit 5 (got {:#x})",
            pending,
        );

        // The notification must still be allocated — the helper did NOT release it.
        let pool_after_unbind = crate::ipc::notification::allocated_count();
        assert_eq!(
            pool_after_unbind, pool_before,
            "caller-owned notification must not be freed on IRQ unbind \
             (before={}, after={})",
            pool_before, pool_after_unbind,
        );

        // Cleanup: release the claim and then manually free the notification
        // (simulating the caller's own cap table teardown).
        let _ = test_release_for_pid(PID_E);

        // After release_for_pid on a caller-owned notif, the pool count must
        // still be the same (the process-exit path must not have freed it).
        let pool_after_exit = crate::ipc::notification::allocated_count();
        assert_eq!(
            pool_after_exit, pool_before,
            "caller-owned notification must not be freed by process-exit sweep \
             (before={}, after={})",
            pool_before, pool_after_exit,
        );

        // Now the caller explicitly frees it (cap table cleared).
        crate::ipc::notification::release(caller_notif);

        serial_println!("device_host B.4b caller-provided notif test passed");
    }

    // -- Track F.3 — Cross-device negative tests ------------------------------
    //
    // These four tests prove the central isolation invariant: a driver process
    // cannot access a BAR or DMA region belonging to a device it did not claim,
    // and forged / stale CapHandle values are rejected unconditionally.
    //
    // Tests 1, 2, 3 operate entirely through the test-harness helpers already
    // used by B.2 / B.3 (no live ring-3 process needed). Test 4 simulates
    // the post-crash handle-invalidation lifecycle using `test_release_for_pid`
    // + `test_try_claim_for_pid` as the stand-in for the supervisor kill+restart
    // cycle (the real end-to-end path is covered by F.2's process-restart
    // regression; we validate the handle-space invariant here at the registry
    // level).

    /// F.3 Test 1: cross-device MMIO denied.
    ///
    /// Simulates an NVMe driver (PID_NVME) holding a valid `Capability::Device`
    /// for its own BDF, then attempting to record an MMIO entry against a
    /// *different* BDF (which it has not claimed). The registry must reject
    /// this with `NotClaimed` — the same error the syscall boundary returns as
    /// `-EBADF` to the caller. No MMIO mapping is installed.
    #[test_case]
    fn cross_device_mmio_denied() {
        use crate::syscall::device_host::{
            TestClaimError, TestMmioError, test_mmio_count_for_pid, test_record_mmio,
            test_release_for_pid, test_try_claim_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        // Use a real PCI device for the NVMe driver's legitimate claim.
        let Some(nvme_key) = pick_free_pci_bdf() else {
            serial_println!("F.3 Test 1 skipped: no free PCI device for NVMe driver");
            return;
        };

        // e1000 BDF: fabricate a key the NVMe driver does NOT own.
        // We use a sentinel that is guaranteed to differ from nvme_key.
        let e1000_key = DeviceCapKey::new(0, 0xFE, 0x1F, 6);

        const PID_NVME: crate::process::Pid = 0xF3_0001;
        let _ = test_release_for_pid(PID_NVME);

        match test_try_claim_for_pid(PID_NVME, nvme_key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!("F.3 Test 1 skipped: nvme BDF busy");
                return;
            }
            Err(e) => panic!("F.3 Test 1 claim failed: {:?}", e),
        }

        // NVMe driver attempts to record an MMIO mapping against the e1000 BDF.
        let mmio_before = test_mmio_count_for_pid(PID_NVME);
        let result = test_record_mmio(PID_NVME, e1000_key, 0, 0x1000, 0xdead_4000);
        assert_eq!(
            result,
            Err(TestMmioError::NotClaimed),
            "F.3 Test 1: cross-device MMIO must be rejected with NotClaimed (-EBADF)",
        );
        assert_eq!(
            test_mmio_count_for_pid(PID_NVME),
            mmio_before,
            "F.3 Test 1: no MMIO entry installed after rejected cross-device attempt",
        );

        let _ = test_release_for_pid(PID_NVME);
        serial_println!("device_host F.3 Test 1 (cross_device_mmio_denied) passed");
    }

    /// F.3 Test 2: cross-device DMA denied.
    ///
    /// Simulates an NVMe driver (PID_NVME) attempting to allocate DMA against a
    /// BDF it has *not* claimed (the e1000's sentinel key). The DMA registry
    /// must reject this with `NoDevice` (the typed analogue of `-EBADF`). The
    /// driver's own claimed device is untouched; no allocation is recorded.
    ///
    /// IOMMU note: when the platform exposes an active IOMMU the `NoDevice`
    /// rejection happens before any IOMMU domain lookup, so the e1000's domain
    /// is never consulted. In identity-fallback mode the same registry check
    /// fires — the IOMMU layer is transparent to this test. Both paths return
    /// the same typed error.
    #[test_case]
    fn cross_device_dma_denied() {
        use crate::syscall::device_host::{
            TestClaimError, TestDmaError, test_dma_alloc_for_pid, test_dma_count,
            test_dma_release_for_pid, test_release_for_pid, test_try_claim_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        let Some(nvme_key) = pick_free_pci_bdf() else {
            serial_println!("F.3 Test 2 skipped: no free PCI device for NVMe driver");
            return;
        };

        // Sentinel BDF for the unclaimed "e1000" device.
        let e1000_key = DeviceCapKey::new(0, 0xFE, 0x1F, 5);

        const PID_NVME: crate::process::Pid = 0xF3_0002;
        let _ = test_release_for_pid(PID_NVME);
        let _ = test_dma_release_for_pid(PID_NVME);

        match test_try_claim_for_pid(PID_NVME, nvme_key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!("F.3 Test 2 skipped: nvme BDF busy");
                return;
            }
            Err(e) => panic!("F.3 Test 2 claim failed: {:?}", e),
        }

        let count_before = test_dma_count();

        // NVMe driver attempts DMA against e1000's unclaimed BDF.
        let result = test_dma_alloc_for_pid(PID_NVME, e1000_key, 4096, 4096);
        assert_eq!(
            result,
            Err(TestDmaError::NoDevice),
            "F.3 Test 2: DMA against unclaimed BDF must return NoDevice (-EBADF)",
        );
        assert_eq!(
            test_dma_count(),
            count_before,
            "F.3 Test 2: no DMA entry recorded after cross-device rejection",
        );

        let _ = test_dma_release_for_pid(PID_NVME);
        let _ = test_release_for_pid(PID_NVME);
        serial_println!("device_host F.3 Test 2 (cross_device_dma_denied) passed");
    }

    /// F.3 Test 3: forged CapHandle denied.
    ///
    /// A driver fabricates an arbitrary `CapHandle` value it never received from
    /// the kernel. Any device-host operation that validates ownership against the
    /// claim registry must reject it. We exercise this at the registry level by
    /// calling `test_record_mmio` and `test_dma_alloc_for_pid` under a PID that
    /// has no claim at all (never registered), passing plausible-looking BDF
    /// and handle values. Both operations must return the typed `NotClaimed` /
    /// `NoDevice` error with no side-effects.
    #[test_case]
    fn capability_forge_denied() {
        use crate::syscall::device_host::{
            TestDmaError, TestMmioError, test_dma_alloc_for_pid, test_dma_count,
            test_mmio_count_for_pid, test_record_mmio, test_release_for_pid,
        };
        use kernel_core::device_host::DeviceCapKey;

        // This PID has never claimed anything — simulates a driver that
        // fabricated a CapHandle out of thin air.
        const PID_FORGER: crate::process::Pid = 0xF3_0003;
        let _ = test_release_for_pid(PID_FORGER);

        // Use two arbitrary BDF keys the forger never claimed.
        let forge_key_a = DeviceCapKey::new(0, 0xFE, 0x1F, 4);
        let forge_key_b = DeviceCapKey::new(0, 0xFE, 0x1F, 3);

        let mmio_before = test_mmio_count_for_pid(PID_FORGER);
        let dma_before = test_dma_count();

        // Attempt forged MMIO record.
        let mmio_result = test_record_mmio(PID_FORGER, forge_key_a, 0, 0x2000, 0xcafe_0000);
        assert_eq!(
            mmio_result,
            Err(TestMmioError::NotClaimed),
            "F.3 Test 3: forged MMIO cap must be rejected with NotClaimed (-EBADF)",
        );

        // Attempt forged DMA alloc.
        let dma_result = test_dma_alloc_for_pid(PID_FORGER, forge_key_b, 4096, 4096);
        assert_eq!(
            dma_result,
            Err(TestDmaError::NoDevice),
            "F.3 Test 3: forged DMA cap must be rejected with NoDevice (-EBADF)",
        );

        // Verify no side-effects.
        assert_eq!(
            test_mmio_count_for_pid(PID_FORGER),
            mmio_before,
            "F.3 Test 3: MMIO count unchanged after forged MMIO attempt",
        );
        assert_eq!(
            test_dma_count(),
            dma_before,
            "F.3 Test 3: DMA count unchanged after forged DMA attempt",
        );

        serial_println!("device_host F.3 Test 3 (capability_forge_denied) passed");
    }

    /// F.3 Test 4: post-crash CapHandle values are invalid in the restarted
    /// process.
    ///
    /// Simulates the driver supervisor kill-and-restart lifecycle at the
    /// registry level:
    ///
    /// 1. Phase A (pre-crash): PID_PRE claims a BDF, records MMIO and DMA
    ///    entries, and captures the registry state.
    /// 2. Crash simulation: `test_release_for_pid(PID_PRE)` tears down all
    ///    claim, MMIO, and DMA state — exactly what the kernel does on process
    ///    exit (Phase 55b Track B.1 / B.2 / B.3 cleanup cascade).
    /// 3. Phase B (post-crash): a new PID (PID_POST, simulating the restarted
    ///    driver) claims the same BDF and receives fresh allocations. The handle
    ///    IDs from Phase A must not be visible to PID_POST.
    ///
    /// This validates the "handle-space is per-PID and non-transferable"
    /// invariant required by F.3 Acceptance item 4.
    #[test_case]
    fn post_crash_handles_invalid_in_restarted_process() {
        use crate::syscall::device_host::{
            TestClaimError, test_dma_alloc_for_pid, test_dma_handle_info, test_dma_release_for_pid,
            test_release_for_pid, test_try_claim_for_pid,
        };

        let Some(key) = pick_free_pci_bdf() else {
            serial_println!("F.3 Test 4 skipped: no free PCI device");
            return;
        };

        // --- Phase A: pre-crash driver ---
        const PID_PRE: crate::process::Pid = 0xF3_0004;
        let _ = test_release_for_pid(PID_PRE);
        let _ = test_dma_release_for_pid(PID_PRE);

        match test_try_claim_for_pid(PID_PRE, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                serial_println!("F.3 Test 4 skipped: BDF busy");
                return;
            }
            Err(e) => panic!("F.3 Test 4 pre-crash claim failed: {:?}", e),
        }

        let pre_snap = test_dma_alloc_for_pid(PID_PRE, key, 4096, 4096)
            .expect("F.3 Test 4: pre-crash DMA alloc must succeed");
        let pre_crash_id = pre_snap.id;

        // Pre-crash handle is visible to PID_PRE.
        assert!(
            test_dma_handle_info(PID_PRE, pre_crash_id).is_some(),
            "F.3 Test 4: pre-crash handle must be visible before crash",
        );

        // --- Crash simulation: supervisor calls release_for_pid ---
        let _ = test_dma_release_for_pid(PID_PRE);
        let released = test_release_for_pid(PID_PRE);
        assert_eq!(
            released, 1,
            "F.3 Test 4: exactly one claim must be freed on crash"
        );

        // Pre-crash handle is now gone even for PID_PRE.
        assert!(
            test_dma_handle_info(PID_PRE, pre_crash_id).is_none(),
            "F.3 Test 4: pre-crash handle must be invisible after crash teardown",
        );

        // --- Phase B: restarted driver with a fresh PID ---
        const PID_POST: crate::process::Pid = 0xF3_0005;
        let _ = test_release_for_pid(PID_POST);
        let _ = test_dma_release_for_pid(PID_POST);

        match test_try_claim_for_pid(PID_POST, key) {
            Ok(()) => {}
            Err(TestClaimError::Busy) => {
                panic!("F.3 Test 4: restarted driver must be able to re-claim BDF")
            }
            Err(e) => panic!("F.3 Test 4 post-crash claim failed: {:?}", e),
        }

        let post_snap = test_dma_alloc_for_pid(PID_POST, key, 4096, 4096)
            .expect("F.3 Test 4: post-crash DMA alloc must succeed");

        // The restarted driver must NOT see the pre-crash handle ID.
        assert!(
            test_dma_handle_info(PID_POST, pre_crash_id).is_none(),
            "F.3 Test 4: pre-crash CapHandle ID must be opaque to the restarted process",
        );

        // The restarted driver sees its own fresh allocation.
        assert!(
            test_dma_handle_info(PID_POST, post_snap.id).is_some(),
            "F.3 Test 4: restarted driver must see its own fresh allocation",
        );

        let _ = test_dma_release_for_pid(PID_POST);
        let _ = test_release_for_pid(PID_POST);
        serial_println!(
            "device_host F.3 Test 4 (post_crash_handles_invalid_in_restarted_process) passed"
        );
    }
}
