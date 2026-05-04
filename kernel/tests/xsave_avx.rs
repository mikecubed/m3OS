#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Phase 57e Track J.5 — XSAVE / AVX context-switch regression test.
//!
//! Pins the XSAVE migration's headline contract: a task that writes a known
//! pattern to YMM upper halves yields, runs another task that also writes YMM
//! upper halves, then yields back; the original pattern survives.  Under
//! FXSAVE this fails (only the lower 128 bits are saved); under XSAVE with
//! XCR0 = x87+SSE+AVX = 0x7 it passes.
//!
//! # Live tests
//!
//! Two tests run kernel_core's XSAVE feature parser on synthetic CPUID values
//! to pin the runtime probe's logic against host-equivalent inputs.  These
//! cover:
//!
//! * `parse_sandy_bridge_baseline` — XSAVE+OSXSAVE+AVX, 832-byte area, no
//!   XSAVEOPT (the m3OS minimum).
//! * `parse_xsaveopt_capable` — Ivy Bridge and later: XSAVEOPT advertised.
//!
//! # `#[ignore]` stubs
//!
//! The end-to-end YMM-survives-yield test requires `smp::init_bsp_per_core`
//! and the scheduler's user-task dispatch path, neither of which is wired into
//! the QEMU test harness today.  Activated alongside Track G.
//!
//! Source ref: phase-57e-track-J.5

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use kernel_core::xsave_model::{
    LEAF1_ECX_OSXSAVE, LEAF1_ECX_XSAVE, XSAVE_FEATURE_MASK, XSaveFeaturesModel,
};
use x86_64::instructions::{hlt, port::Port};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(xsave_avx_kernel_test, config = &BOOTLOADER_CONFIG);

fn xsave_avx_kernel_test(_boot_info: &'static mut BootInfo) -> ! {
    test_main();
    qemu_exit(0x10);
}

struct NoAlloc;
unsafe impl GlobalAlloc for NoAlloc {
    unsafe fn alloc(&self, _: Layout) -> *mut u8 {
        core::ptr::null_mut()
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}
#[global_allocator]
static STUB_ALLOC: NoAlloc = NoAlloc;

trait Testable {
    fn run(&self);
}
impl<T: Fn()> Testable for T {
    fn run(&self) {
        self();
    }
}

fn test_runner(tests: &[&dyn Testable]) {
    for test in tests {
        test.run();
    }
}

fn qemu_exit(code: u32) -> ! {
    unsafe { Port::new(0xf4).write(code) };
    loop {
        hlt();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    qemu_exit(0x11);
}

// ---------------------------------------------------------------------------
// Live model-layer tests
// ---------------------------------------------------------------------------

#[test_case]
fn parse_sandy_bridge_baseline() {
    let f = XSaveFeaturesModel::from_raw(
        LEAF1_ECX_XSAVE | LEAF1_ECX_OSXSAVE,
        0x0000_0007,
        832,
        832,
        0,
        0,
    );
    assert!(f.meets_minimum());
    assert_eq!(f.area_size_at_mask, 832);
    assert_eq!(
        f.supported_components & XSAVE_FEATURE_MASK,
        XSAVE_FEATURE_MASK
    );
    assert!(!f.xsaveopt);
}

#[test_case]
fn parse_xsaveopt_capable() {
    let f = XSaveFeaturesModel::from_raw(
        LEAF1_ECX_XSAVE | LEAF1_ECX_OSXSAVE,
        0x0000_0007,
        832,
        832,
        0,
        0x0000_0001,
    );
    assert!(f.meets_minimum());
    assert!(f.xsaveopt);
}

// ---------------------------------------------------------------------------
// `#[ignore]` stubs — activated alongside Track G
// ---------------------------------------------------------------------------

/// J.5 acceptance: a kernel task writes to YMM upper halves via `vmovaps`,
/// yields, runs another task that also writes YMM upper halves, yields back;
/// the original YMM upper halves are restored.  Fails under FXSAVE; passes
/// under XSAVE with XCR0 = 0x7.
#[test_case]
#[ignore = "Track G activation pending — needs smp::init_bsp_per_core + scheduler dispatch"]
fn ymm_upper_halves_survive_context_switch() {
    // Track G activation pending — needs smp::init_bsp_per_core + scheduler dispatch
}

/// J.5 robustness: 1000 iterations of the yield-and-verify cycle.
#[test_case]
#[ignore = "Track G activation pending — needs scheduler + iterated yield harness"]
fn ymm_upper_halves_survive_1000_iterations() {
    // Track G activation pending
}
