//! Phase 61 Track 0b — test prelude for `kernel/tests/*.rs` integration tests.
//!
//! Provides a minimum-viable boot sequence so live-scheduler / SMP / pipe /
//! IPC integration tests can exercise the real kernel state machine instead
//! of relying on `kernel-core` model mirrors alone.
//!
//! # Why a prelude?
//!
//! The kernel's full boot sequence (`kernel::kernel_main_entry`) does much
//! more than tests need: framebuffer init, RTC, network stack bring-up,
//! userspace `init` spawn, the entire service set, and so on. It also ends
//! with `task::run()` which never returns — so tests cannot run code after
//! the boot sequence completes.
//!
//! `init_minimal_smp` runs the strict subset of `kernel_main_entry` that the
//! scheduler / SMP / pipe / IPC subsystems require, then **returns**. After
//! it returns, integration tests can:
//!
//!   - call `kernel::smp::get_core_data(id).with_run_queue(|q| q.len())`;
//!   - call `kernel::task::spawn` / `kernel::task::yield_now`;
//!   - exercise `kernel::pipe::PIPE_WAITQUEUES`, `kernel::ipc::endpoint::*`;
//!   - run `test_main()` inside a kernel task and signal QEMU exit when
//!     all `#[test_case]` blocks pass.
//!
//! The prelude does **not** start the userspace `init` task or the network
//! / display / audio / session services — those are out of scope for SMP
//! correctness tests, take significant boot time, and would couple the
//! tests to disk image contents.
//!
//! # Usage pattern
//!
//! ```ignore
//! use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
//!
//! const BOOTLOADER_CONFIG: BootloaderConfig = {
//!     let mut c = BootloaderConfig::new_default();
//!     c.mappings.physical_memory = Some(Mapping::Dynamic);
//!     c
//! };
//!
//! entry_point!(my_test, config = &BOOTLOADER_CONFIG);
//!
//! fn my_test(boot_info: &'static mut BootInfo) -> ! {
//!     kernel::test_prelude::init_minimal_smp(boot_info);
//!     // boot APs only if the test exercises cross-core behavior
//!     kernel::test_prelude::boot_aps_if_available();
//!     test_main();
//!     kernel::test_prelude::qemu_exit_success()
//! }
//! ```
//!
//! Tests that import `kernel` automatically inherit the kernel's
//! `#[global_allocator]` (`mm::heap::ALLOCATOR`), so they must NOT declare
//! their own — doing so produces a linker error.

use bootloader_api::BootInfo;

/// Run the strict subset of `kernel_main_entry` that the SMP / scheduler /
/// pipe / IPC subsystems require. After this returns, integration tests can
/// exercise the live kernel state machine.
///
/// Initializes (in order):
///
/// 1. Serial port + structured logger.
/// 2. GDT + IDT (`arch::init`).
/// 3. Frame allocator + heap (`mm::init`) — boot_info is consumed here.
/// 4. PCI enumeration (required so APIC discovery sees IO-APIC entries).
/// 5. ACPI table discovery (RSDP → MADT) for AP and IO-APIC topology.
/// 6. Local APIC + IO-APIC bring-up (skipped if MADT absent — uniprocessor).
/// 7. Per-core data for the BSP (`gs_base` set so `per_core()` does not panic).
/// 8. Hardware interrupts enabled.
/// 9. CPUID probe + XSAVE state enable (required for context switch).
///
/// `acpi::init` runs **before** `enable_interrupts` so that the timer-IRQ
/// path's `tick_account_current_task` → `is_bsp()` → `local_apic_address()`
/// chain cannot panic on the very first tick (the MADT must be parsed
/// before any LAPIC register read). This matches the order used by the
/// production `kernel_main_entry`.
///
/// Does **not** boot Application Processors. Tests that need cross-core
/// behavior should call [`boot_aps_if_available`] after this returns.
pub fn init_minimal_smp(boot_info: &'static mut BootInfo) {
    crate::serial::init();
    crate::serial::init_logger();
    log::info!("[test_prelude] init_minimal_smp entered");

    crate::arch::init();

    let rsdp_addr = boot_info.rsdp_addr.into_option();
    crate::mm::init(boot_info);

    // Map the kernel-stack pool with guard pages. Required before any AP
    // boot or task spawn since both go through `kstack::alloc_leaked_top` /
    // `KernelStack::alloc`.
    crate::task::kstack::init();

    crate::pci::init();

    crate::acpi::init(rsdp_addr);
    if crate::acpi::io_apic_address().is_some() {
        crate::arch::x86_64::apic::init();
    }

    crate::smp::init_bsp_per_core();

    // SAFETY: arch::init() loaded the IDT; mm::init() set up the heap; ACPI
    // and APIC are now up so the timer ISR's `is_bsp()` call can read the
    // LAPIC ID safely. This is the canonical point for unmasking IRQs
    // (matches the production boot sequence in kernel_main_entry).
    unsafe { crate::arch::enable_interrupts() };

    let _xsave = crate::arch::x86_64::cpuid::probe();
    // SAFETY: enable_xsave_state writes CR4.OSXSAVE and XCR0 via xsetbv.
    // Its safety contract requires single-threaded execution OR IRQs masked;
    // boot_aps has not yet run so we are single-threaded, but interrupts are
    // already enabled — wrap in without_interrupts to honor the contract
    // unconditionally (mirrors kernel_main_entry).
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        crate::arch::x86_64::cpuid::enable_xsave_state()
    });

    log::info!(
        "[test_prelude] init_minimal_smp complete: cores={} per_core_ready={}",
        crate::smp::core_count(),
        crate::smp::is_per_core_ready()
    );
}

/// Boot Application Processors if the system has more than one core (per
/// ACPI MADT). Call after [`init_minimal_smp`] and before launching test
/// logic that needs cross-core behavior.
///
/// Safe to call on uniprocessor systems — it returns without doing anything.
pub fn boot_aps_if_available() {
    if crate::smp::is_per_core_ready() && crate::smp::core_count() > 1 {
        crate::smp::boot::boot_aps();
        log::info!(
            "[test_prelude] APs booted (cores={})",
            crate::smp::core_count()
        );
    } else {
        log::info!("[test_prelude] uniprocessor — APs not booted");
    }
}

/// Default idle task body for the BSP scheduler. Mirrors the production
/// `idle_task` in `kernel/src/lib.rs` — `enable_and_hlt` to park until the
/// next interrupt, then `yield_now` to let other Ready tasks dispatch.
///
/// Tests that enter the scheduler via `kernel::task::run()` must spawn an
/// idle task on each core (use [`spawn_idle`] convenience helper).
pub fn idle_task() -> ! {
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
        crate::task::yield_now();
    }
}

/// Convenience: spawn `test_prelude::idle_task` on the BSP. Required before
/// `kernel::task::run()` is entered so the scheduler always has something
/// to dispatch.
pub fn spawn_idle() {
    crate::task::spawn_idle(idle_task);
}

/// Write the QEMU `isa-debug-exit` device with the success code (`0x10`,
/// which QEMU reports as exit status `0x21`). Halts forever afterward — the
/// device write triggers the actual VM exit.
pub fn qemu_exit_success() -> ! {
    qemu_exit(0x10)
}

/// Write the QEMU `isa-debug-exit` device with the failure code (`0x11`,
/// reported as exit status `0x23`). Used by integration-test panic handlers.
pub fn qemu_exit_failure() -> ! {
    qemu_exit(0x11)
}

fn qemu_exit(code: u32) -> ! {
    use x86_64::instructions::port::Port;
    // SAFETY: 0xf4 is the standard QEMU isa-debug-exit IO port; no observable
    // CPU state mutation beyond the VM-exit it triggers.
    unsafe {
        Port::new(0xf4).write(code);
    }
    loop {
        x86_64::instructions::hlt();
    }
}
