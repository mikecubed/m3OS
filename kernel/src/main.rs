#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod arch;
mod mm;
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
    // Gated on debug builds so production boots don't always trap.
    if cfg!(debug_assertions) {
        x86_64::instructions::interrupts::int3();
        log::info!("[arch] breakpoint exception handled OK");
    }

    // Verify timer IRQ is firing (P3-T008) — debug builds only so normal boots
    // are not slowed by the busy-wait on release hardware.
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

    // Register the idle task (selected only when no other task is Ready, P4-T009).
    task::spawn_idle(idle_task);
    // Spawn two demo tasks to verify interleaved execution (P4-T007).
    task::spawn(task_a, "task-a");
    task::spawn(task_b, "task-b");
    log::info!("[task] scheduler starting");
    task::run() // never returns
}

/// Idle task: runs only when no other task is Ready (P4-T009).
///
/// Tasks execute with interrupts disabled (inside the scheduler's
/// `without_interrupts` critical section that protects `switch_context`).
/// Plain `hlt` with IF=0 would never wake; `enable_and_hlt` atomically
/// re-enables interrupts and halts, allowing the timer IRQ to fire and set
/// `RESCHEDULE` before yielding back to the scheduler.
fn idle_task() -> ! {
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
        task::yield_now();
    }
}

/// Demo task A — logs a monotonically increasing counter (P4-T007, P4-T008).
fn task_a() -> ! {
    let mut n = 0u32;
    loop {
        log::info!("[task A] tick {}", n);
        n += 1;
        task::yield_now();
    }
}

/// Demo task B — logs a monotonically increasing counter (P4-T007, P4-T008).
fn task_b() -> ! {
    let mut n = 0u32;
    loop {
        log::info!("[task B] tick {}", n);
        n += 1;
        task::yield_now();
    }
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Use _panic_print to avoid deadlock if panic occurs while serial mutex is held
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
