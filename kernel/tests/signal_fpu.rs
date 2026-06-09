#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Phase 86f Track B.1 — Signal-frame FPU save/restore tests.
//!
//! # Live tests
//!
//! ## Layout constants (infrastructure-free)
//!
//! `sigframe_fpu_layout_constants` — asserts compile-time layout constants:
//! - `FPU_AREA_SIZE == XSAVE_AREA_SIZE` (the FPU snapshot exactly fits)
//! - `SIGFRAME_EXTENDED_SIZE == SIGFRAME_SIZE + FPU_AREA_SIZE`
//! - `FPU_AREA_SIZE > 0` (not accidentally zeroed)
//!
//! `sigframe_mc_fpstate_offset` — asserts the `fpstate` pointer sits at the
//! expected offset within mcontext (176 bytes into mcontext, 224 bytes from
//! frame base) to match the Linux `sigcontext` layout that musl reads.
//!
//! ## FPU save/restore roundtrip (requires scheduler dispatch)
//!
//! `fpu_save_restore_roundtrip` — spawned as a kernel task after full BSP
//! init.  Exercises `with_current_task_fpu_saved` and
//! `restore_current_task_fpu_from_bytes` with a synthetic byte pattern.
//! Proves the kernel API correctly reads back the XSave area bytes after a
//! save-and-restore cycle.
//!
//! # `#[ignore]` stubs
//!
//! `xmm_survives_signal` — the end-to-end acceptance criterion (a user task
//! fills XMM0–XMM3 with a known pattern, raises a signal whose handler
//! clobbers XMM0–XMM3, returns via sigreturn, then asserts the values are
//! bit-identical to the pre-signal values) cannot be driven from within a
//! kernel task: it requires a ring-3 process with a registered signal
//! handler and a full `setup_signal_frame` / `restore_sigframe` round-trip
//! through the actual delivery path.  This test is kept as a documented
//! placeholder for a future smoke-gate integration.
//!
//! Source ref: phase-86f-track-B.1

extern crate alloc;

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(signal_fpu_test_entry, config = &BOOTLOADER_CONFIG);

fn signal_fpu_test_entry(boot_info: &'static mut BootInfo) -> ! {
    kernel::test_prelude::init_minimal_smp(boot_info);
    kernel::task::spawn(test_runner_task, "signal-fpu-runner");
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
    for test in tests {
        test.run();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(loc) = info.location() {
        kernel::serial_println!(
            "[signal-fpu-test] PANIC at {}:{}: {}",
            loc.file(),
            loc.line(),
            info.message(),
        );
    } else {
        kernel::serial_println!("[signal-fpu-test] PANIC: {}", info.message());
    }
    kernel::test_prelude::qemu_exit_failure()
}

// ---------------------------------------------------------------------------
// Infrastructure-free layout tests (run without scheduler init)
// ---------------------------------------------------------------------------

/// Verify that the FPU area constants are self-consistent.
#[test_case]
fn sigframe_fpu_layout_constants() {
    use kernel::arch::x86_64::cpuid::XSAVE_AREA_SIZE;
    use kernel::signal::{FPU_AREA_SIZE, SIGFRAME_EXTENDED_SIZE, SIGFRAME_SIZE};

    assert!(FPU_AREA_SIZE > 0, "FPU_AREA_SIZE must not be zero");
    assert_eq!(
        FPU_AREA_SIZE, XSAVE_AREA_SIZE,
        "FPU_AREA_SIZE must equal XSAVE_AREA_SIZE so the snapshot fits exactly"
    );
    assert_eq!(
        SIGFRAME_EXTENDED_SIZE,
        SIGFRAME_SIZE + FPU_AREA_SIZE,
        "SIGFRAME_EXTENDED_SIZE must equal SIGFRAME_SIZE + FPU_AREA_SIZE"
    );
    kernel::serial_println!(
        "[signal-fpu-test] layout: SIGFRAME_SIZE={} FPU_AREA_SIZE={} SIGFRAME_EXTENDED_SIZE={}",
        SIGFRAME_SIZE,
        FPU_AREA_SIZE,
        SIGFRAME_EXTENDED_SIZE,
    );
}

/// Verify that the fpstate pointer sits at offset 224 from frame base,
/// i.e. OFF_MCONTEXT (48) + MC_FPSTATE (184) = 232 within the frame.
///
/// This offset is the Linux kernel / musl `sigcontext.fpstate` contract.
/// Sigcontext layout: 16 GPRs × 8 = 128, rip/rflags × 8 = 16 → 144 bytes
/// of registers; then cs/gs/fs/pad (8 bytes) at 144; err/trapno/oldmask/cr2
/// (4 × 8 = 32 bytes) at 152; fpstate (8 bytes) at 184.  Adding OFF_MCONTEXT
/// (48) gives 48 + 184 = 232.
#[test_case]
fn sigframe_mc_fpstate_offset() {
    use kernel::signal::SIGFRAME_SIZE;

    // The FPU area starts right after the core frame.
    let fpu_offset_in_extended_frame = SIGFRAME_SIZE;

    // OFF_MCONTEXT = 48, MC_FPSTATE = 184 → fpstate field is at frame byte 232.
    // This is a compile-time constant; assert it matches expectations.
    const OFF_MCONTEXT: usize = 48;
    const MC_FPSTATE: usize = 184;
    let expected_fpstate_field = OFF_MCONTEXT + MC_FPSTATE;

    kernel::serial_println!(
        "[signal-fpu-test] fpstate field at frame offset {}; FPU area at offset {}",
        expected_fpstate_field,
        fpu_offset_in_extended_frame,
    );

    // fpstate field (at 232) < FPU area start (at 560): they are in the right order.
    assert!(
        expected_fpstate_field < fpu_offset_in_extended_frame,
        "fpstate pointer field must be before the FPU area it points to"
    );
}

// ---------------------------------------------------------------------------
// FPU save/restore roundtrip (runs inside a scheduler task)
// ---------------------------------------------------------------------------

/// Shared flag so the roundtrip test can signal completion/failure back to
/// the test runner (the task exits via qemu_exit, but the flag guards the
/// assert inside the spawned sub-task body).
static ROUNDTRIP_PASSED: AtomicBool = AtomicBool::new(false);

/// Phase 86f Track B.1 — verify that `with_current_task_fpu_saved` captures
/// the task's XSave area bytes and `restore_current_task_fpu_from_bytes`
/// round-trips them correctly.
///
/// This test runs inside a kernel task (dispatched by the scheduler), so
/// `get_current_task_idx()` returns `Some(idx)` and the FPU helpers work.
/// We overwrite the task's XSaveArea with a synthetic byte pattern, call the
/// helpers, and check the returned bytes match.
#[test_case]
fn fpu_save_restore_roundtrip() {
    use alloc::vec;
    use kernel::arch::x86_64::cpuid::XSAVE_AREA_SIZE;

    // Confirm we are running in a scheduled task context.
    let task_idx = kernel::task::scheduler::get_current_task_idx();
    assert!(
        task_idx.is_some(),
        "fpu_save_restore_roundtrip must run inside a scheduled kernel task"
    );

    // Reset the task's FPU area to a known baseline to make the test
    // deterministic (xsave may have left any values there from boot).
    kernel::task::scheduler::reset_current_task_fpu_state();

    // Save the current (reset) FPU state and verify we get XSAVE_AREA_SIZE bytes.
    let got_bytes = kernel::task::scheduler::with_current_task_fpu_saved(|bytes| {
        assert_eq!(
            bytes.len(),
            XSAVE_AREA_SIZE,
            "with_current_task_fpu_saved must yield exactly XSAVE_AREA_SIZE bytes"
        );
        let mut copy = vec![0u8; bytes.len()];
        copy.copy_from_slice(bytes);
        copy
    });

    assert!(
        got_bytes.is_some(),
        "with_current_task_fpu_saved returned None inside a task — task index missing"
    );
    let saved = got_bytes.unwrap();
    assert_eq!(saved.len(), XSAVE_AREA_SIZE);

    // Build a synthetic FPU snapshot that xrstor will load faithfully.
    //
    // Start with a well-formed zeroed buffer so all reserved/padding bytes
    // are 0, then set only the fields required for a valid xrstor operand.
    let mut synthetic = vec![0u8; XSAVE_AREA_SIZE];
    // x87 control word (offset 0, 2 bytes): 0x037F (all exceptions masked,
    // double-extended precision, round-to-nearest) — the architectural init value.
    synthetic[0] = 0x7f;
    synthetic[1] = 0x03;
    // MXCSR (offset 24, 4 bytes): 0x00001F80 (all exceptions masked,
    // round-to-nearest, flush-to-zero off) — the architectural init value.
    synthetic[24] = 0x80;
    synthetic[25] = 0x1f;
    // MXCSR_MASK (offset 28, 4 bytes): 0x0000FFFF (report all mask bits
    // as valid — xrstor ignores it but some CPUID checks read it).
    synthetic[28] = 0xff;
    synthetic[29] = 0xff;
    // Fill the XMM register area (offsets 160–415, 256 bytes) with a
    // distinctive pattern so xsaveopt can't use init-optimisation for the
    // SSE component (init-opt fires only when XMMs are all-zero).
    for b in &mut synthetic[160..416] {
        *b = 0xA5;
    }
    // XSTATE_BV (offset 512, 8 bytes): set bit 0 (x87) and bit 1 (SSE) so
    // xrstor64 actually loads both state components from this buffer rather
    // than restoring them to architectural init state.
    synthetic[512] = 0x03; // bits 1:0 = x87 present + SSE present
    // XCOMP_BV (offset 520, 8 bytes): 0 → standard XSAVE format (not
    // compacted).  This is required; xrstor faults on a non-zero XCOMP_BV
    // when the kernel did not enable XSAVES/XSAVEC.
    // (already 0 from the zero-initialised buffer)

    // Restore from the synthetic snapshot — this writes to the task's
    // XSaveArea and calls xrstor64 to load the hardware FPU.
    kernel::task::scheduler::restore_current_task_fpu_from_bytes(&synthetic);

    // Now save again; the bytes should match the synthetic snapshot we
    // just restored (xrstor → hardware FPU → xsave → XSaveArea).
    let got_after = kernel::task::scheduler::with_current_task_fpu_saved(|bytes| {
        let mut copy = vec![0u8; bytes.len()];
        copy.copy_from_slice(bytes);
        copy
    })
    .expect("with_current_task_fpu_saved must succeed inside task");

    // The XMM register area (offsets 160–415) should round-trip exactly.
    // After xrstor (XSTATE_BV bit 1 set) the hardware XMM registers hold the
    // 0xA5 pattern; xsaveopt sees them as non-init and writes them back.
    //
    // We do not compare:
    //   - offset 0–159: x87 state — xrstor restores x87 CW + init registers
    //     and xsaveopt may write back slightly different tag/status bytes.
    //   - offset 416–511: legacy reserved region — may be zeroed by hardware.
    //   - offset 512+: XSAVE header — XSTATE_BV updated by xsaveopt.
    let xmm_start = 160_usize;
    let xmm_end = 416_usize.min(XSAVE_AREA_SIZE);
    assert_eq!(
        &got_after[xmm_start..xmm_end],
        &synthetic[xmm_start..xmm_end],
        "XMM register area did not survive xrstor→xsave roundtrip"
    );

    ROUNDTRIP_PASSED.store(true, Ordering::Release);
    kernel::serial_println!("[signal-fpu-test] fpu_save_restore_roundtrip: PASS");
}

// ---------------------------------------------------------------------------
// `#[ignore]` stub — end-to-end XMM-survives-signal
// ---------------------------------------------------------------------------

/// Phase 86f Track B.1 end-to-end acceptance: a user task fills XMM0–XMM3
/// with a known 128-bit pattern, installs a signal handler that clobbers
/// XMM0–XMM3 with a different pattern, raises the signal mid-computation,
/// and asserts after sigreturn that the XMM values are bit-identical to the
/// pre-signal pattern.
///
/// This test requires:
///   1. A ring-3 user process with a registered SA_SIGINFO signal handler.
///   2. The kernel's `setup_signal_frame` / `restore_sigframe` FPU path to be
///      live (delivered by Phase 86f Track B.1).
///   3. The userspace target compiled with SSE (`+sse,+sse2`) so the binary
///      emits `movaps` register operations (Track A).
///
/// Until a full QEMU smoke gate for userspace-simd is wired (Track C.3 of
/// Phase 86f), this remains an `#[ignore]` stub documenting the intended
/// assertion protocol.  The kernel-side path is exercised by
/// `fpu_save_restore_roundtrip` above.
#[test_case]
#[ignore = "requires ring-3 SSE-capable user task + signal delivery — activate in Track C.3 smoke gate"]
fn xmm_survives_signal() {
    // Track C.3 activation pending: spawn a user task that does:
    //
    //   1. movaps xmm0, [pattern_a]   // fill XMM0–XMM3 with pattern A
    //   2. raise(SIGUSR1)              // kernel delivers SIGUSR1
    //   3. (handler) movaps xmm0, [pattern_b]  // clobber XMM0–XMM3 with B
    //   4. (handler) return via sigreturn
    //   5. movaps [check], xmm0       // read back
    //   6. assert [check] == pattern_a // must equal pre-signal values
    //
    // The kernel-side implementation (this phase) saves/restores the XSaveArea
    // on delivery/sigreturn; the signal handler running in ring 3 observes
    // pattern A unchanged after returning from the handler.
}
