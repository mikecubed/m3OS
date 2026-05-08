#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Phase 61 Track C.2 — cross-core TLB-shootdown regression test.
//!
//! Phase 25 P25-T045 was the acceptance line "a TLB shootdown
//! triggered by `munmap` does not leave stale mappings on another
//! core". The full userspace fork → mmap → cross-core access →
//! munmap → cross-core re-access path requires fork-and-exec test
//! infrastructure that doesn't exist in `kernel/tests/*`. This test
//! is the kernel-side equivalent: it pins the cross-core TLB
//! invalidation IPI mechanism that `sys_linux_munmap` calls into via
//! `crate::smp::tlb::tlb_shootdown_range` (Track C.1 verified the
//! call site).
//!
//! The test boots APs, then from the BSP calls `tlb_shootdown(addr)`
//! — the single-address broadcast path that uses the same
//! `IPI_TLB_SHOOTDOWN` vector and `SHOOTDOWN_PENDING` ack discipline
//! as the range-based path. A successful return proves:
//!
//!   1. The IPI was delivered to every online AP.
//!   2. Each AP's IDT vector ran `handle_tlb_shootdown_ipi`.
//!   3. Each AP ack'd via `SHOOTDOWN_PENDING.fetch_sub(1)`.
//!
//! `tlb_shootdown` spin-waits on `SHOOTDOWN_PENDING == 0`, so a
//! missing ack hangs the test rather than silently passing.
//! Defense-in-depth: the runner caps wall-clock latency and reports
//! the actual tick count.

extern crate alloc;

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::panic::PanicInfo;

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(munmap_tlb_smp_test, config = &BOOTLOADER_CONFIG);

fn munmap_tlb_smp_test(boot_info: &'static mut BootInfo) -> ! {
    kernel::test_prelude::init_minimal_smp(boot_info);
    kernel::test_prelude::boot_aps_if_available();

    kernel::task::spawn(test_runner_task, "munmap-tlb-runner");
    kernel::test_prelude::spawn_idle();
    kernel::task::run()
}

fn test_runner_task() -> ! {
    test_main();
    kernel::test_prelude::qemu_exit_success()
}

trait Testable {
    fn run(&self);
}
impl<T: Fn()> Testable for T {
    fn run(&self) {
        self();
    }
}
fn test_runner(tests: &[&dyn Testable]) {
    for t in tests {
        t.run();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(loc) = info.location() {
        kernel::serial_println!(
            "[munmap-tlb-test] PANIC at {}:{}: {}",
            loc.file(),
            loc.line(),
            info.message()
        );
    } else {
        kernel::serial_println!("[munmap-tlb-test] PANIC: {}", info.message());
    }
    kernel::test_prelude::qemu_exit_failure()
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test_case]
fn cross_core_tlb_shootdown_ipi_completes() {
    let cores = kernel::smp::core_count() as usize;
    assert!(
        cores >= 2,
        "cross-core TLB shootdown test requires at least 2 cores; got {cores}"
    );
    kernel::serial_println!("[munmap-tlb-test] cores={}", cores);

    // Wait briefly so APs are demonstrably running their idle/scheduler
    // loops before we exercise the IPI mechanism. APs entering their
    // scheduler is logged as "[smp] AP core_id=N fully initialized" by
    // `boot_aps`; this loop is a defense-in-depth observable.
    let warmup_deadline = kernel::arch::x86_64::interrupts::tick_count().saturating_add(20);
    while kernel::arch::x86_64::interrupts::tick_count() < warmup_deadline {
        kernel::task::yield_now();
    }

    // Issue the cross-core TLB shootdown for a kernel virtual address.
    // The single-address broadcast path matches the post-batch IPI
    // `sys_linux_munmap` uses (the range version is exercised when there
    // is more than one page; the mechanism is identical — same IPI
    // vector, same SHOOTDOWN_PENDING ack discipline, same IRQ-handler
    // invalidation).
    //
    // The address chosen is in the kernel-half virtual range (PML4[256])
    // but is unmapped; `tlb::flush` / `invlpg` on an unmapped address
    // is a harmless no-op at the hardware level.
    const SHOOTDOWN_ADDR: u64 = 0xFFFF_8000_DEAD_0000;
    let start = kernel::arch::x86_64::interrupts::tick_count();
    kernel::serial_println!(
        "[munmap-tlb-test] calling tlb_shootdown(0x{:x}) at tick {}",
        SHOOTDOWN_ADDR,
        start
    );
    kernel::smp::tlb::tlb_shootdown(SHOOTDOWN_ADDR);
    let end = kernel::arch::x86_64::interrupts::tick_count();
    let latency = end.saturating_sub(start);
    kernel::serial_println!(
        "[munmap-tlb-test] tlb_shootdown returned at tick {} (latency {} ticks)",
        end,
        latency
    );

    // The fact that `tlb_shootdown` returned is itself the assertion:
    // the function spin-waits on `SHOOTDOWN_PENDING` until every
    // targeted remote core has run the IPI handler and ack'd via
    // `fetch_sub(1)`. If any AP failed to handle the IPI, the function
    // would spin forever and the test harness would report timeout.
    //
    // Defense-in-depth: bound the wall-clock latency. With `cores - 1`
    // targets, IPI delivery + IRQ-handler runtime is well under a
    // millisecond per core on QEMU TCG; allow 100 ticks total budget.
    assert!(
        latency <= 100,
        "tlb_shootdown latency {latency} ticks exceeds budget 100 \
         (cores={cores}, addr=0x{SHOOTDOWN_ADDR:x})",
    );

    kernel::serial_println!(
        "[munmap-tlb-test] PASSED: cross-core TLB shootdown IPI completes \
         (latency {} ticks, cores={})",
        latency,
        cores
    );
}
