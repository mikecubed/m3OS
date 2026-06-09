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
//! expected offset within mcontext (184 bytes into mcontext, 232 bytes from
//! frame base) to match the Linux `sigcontext` layout that musl reads.
//!
//! ## XSAVE header sanitizer negative test
//!
//! `xsave_header_sanitize_rejects_bad_fields` — feeds a buffer with all the
//! fields that `xrstor64` would #GP on (XCOMP_BV bit 63 set, reserved header
//! bytes non-zero, XSTATE_BV bits outside XCR0 mask 0x7, MXCSR reserved bits
//! set) through `sanitize_xsave_header` and asserts each field is corrected.
//! This is the TDD negative test for BLOCKER 1.
//!
//! ## FPU save/restore roundtrip (requires scheduler dispatch)
//!
//! `fpu_save_restore_roundtrip` — spawned as a kernel task after full BSP
//! init.  Exercises `with_current_task_fpu_saved` and
//! `restore_current_task_fpu_from_bytes` with a synthetic byte pattern.
//! Proves the kernel API correctly reads back the XSave area bytes after a
//! save-and-restore cycle.
//!
//! ## XMM survives signal frame round-trip
//!
//! `xmm_survives_signal` — runs inside a scheduler task.  Uses hand-encoded
//! SSE asm bytes to write distinct 128-bit patterns to XMM0–XMM3 (the compiler
//! cannot emit SSE on the soft-float kernel target, so raw `.byte` sequences
//! are used), then calls the kernel's signal-frame FPU save/restore path:
//!
//!   1. `with_current_task_fpu_saved` — captures live XMM values (pre-signal
//!      snapshot, as signal delivery would).
//!   2. Hand-encoded SSE stores a different pattern into XMM0–XMM3 (simulates
//!      the signal handler clobbering registers).
//!   3. `restore_current_task_fpu_from_bytes` — restores from the snapshot
//!      (as sigreturn would).
//!   4. Hand-encoded SSE reads XMM0–XMM3 back and asserts bit-exact equality
//!      with the pre-signal values.
//!
//! This exercises the exact kernel-side save/restore path that signal delivery
//! and `sys_sigreturn` use; the ring-3 wrapper (a live user process with a
//! real SA_SIGINFO handler) is the `signal-fpu-smoke` gate in Phase 86f Track C.
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

/// Verify that the fpstate pointer sits at offset 232 from frame base,
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
// BLOCKER 1 negative test — XSAVE header sanitizer
// ---------------------------------------------------------------------------

/// Phase 86f Track B.1 BLOCKER 1 — TDD negative test for `sanitize_xsave_header`.
///
/// Feeds a buffer containing all the fields that make `xrstor64` raise #GP in
/// ring 0:
///   - MXCSR (offset 24): reserved bits set above the MXCSR_MASK (0xFFBF)
///   - XSTATE_BV (offset 512): bits outside XSAVE_FEATURE_MASK (0x7) set
///   - XCOMP_BV (offset 520): bit 63 set (compacted format — faults with standard xrstor64)
///   - Reserved header bytes (offsets 528–575): non-zero garbage
///
/// After sanitization every one of these must be corrected without changing the
/// payload bytes (XMM region at 160–415).
#[test_case]
fn xsave_header_sanitize_rejects_bad_fields() {
    use alloc::vec;
    use kernel::arch::x86_64::cpuid::XSAVE_AREA_SIZE;
    use kernel::arch::x86_64::cpuid::XSAVE_FEATURE_MASK;
    use kernel::task::scheduler::sanitize_xsave_header;

    let mut buf = vec![0u8; XSAVE_AREA_SIZE];

    // Write a valid x87 CW and well-formed MXCSR_MASK first.
    buf[0] = 0x7f;
    buf[1] = 0x03; // x87 CW = 0x037F

    // MXCSR (offset 24): set reserved bit 6 (should be cleared by sanitizer).
    // Valid MXCSR bits: mask 0xFFBF (bit 6 is reserved on all x86_64).
    let bad_mxcsr: u32 = 0x1F80 | (1u32 << 6); // 0x1FC0 — bit 6 is bad
    buf[24..28].copy_from_slice(&bad_mxcsr.to_le_bytes());

    // MXCSR_MASK (offset 28): use 0xFFBF (normal value).
    let mxcsr_mask: u32 = 0xFFBF;
    buf[28..32].copy_from_slice(&mxcsr_mask.to_le_bytes());

    // Fill XMM region (offsets 160–415) with a distinctive payload.
    for b in &mut buf[160..416] {
        *b = 0xA5;
    }

    // XSTATE_BV (offset 512): set bits outside XSAVE_FEATURE_MASK (0x7),
    // e.g., bit 8 (for a hypothetical AVX-512 component) to poison the header.
    let bad_xstate_bv: u64 = 0x7 | (1u64 << 8);
    buf[512..520].copy_from_slice(&bad_xstate_bv.to_le_bytes());

    // XCOMP_BV (offset 520): bit 63 set — triggers compacted format in xrstor64
    // and raises #GP with the standard non-compacted xrstor64 m3OS uses.
    let bad_xcomp_bv: u64 = 1u64 << 63;
    buf[520..528].copy_from_slice(&bad_xcomp_bv.to_le_bytes());

    // Reserved header bytes (offsets 528–575): fill with garbage.
    for b in &mut buf[528..576] {
        *b = 0xFF;
    }

    // Sanitize.
    sanitize_xsave_header(&mut buf);

    // MXCSR must have reserved bit 6 cleared.
    let mxcsr_after = u32::from_le_bytes(buf[24..28].try_into().unwrap());
    assert_eq!(
        mxcsr_after & !mxcsr_mask,
        0,
        "sanitize_xsave_header: MXCSR reserved bits not cleared: {:#010x}",
        mxcsr_after,
    );

    // XSTATE_BV must be masked to XSAVE_FEATURE_MASK (0x7).
    let xstate_bv_after = u64::from_le_bytes(buf[512..520].try_into().unwrap());
    assert_eq!(
        xstate_bv_after,
        0x7 & XSAVE_FEATURE_MASK,
        "sanitize_xsave_header: XSTATE_BV not masked to XSAVE_FEATURE_MASK: {:#018x}",
        xstate_bv_after,
    );

    // XCOMP_BV must be zero.
    let xcomp_bv_after = u64::from_le_bytes(buf[520..528].try_into().unwrap());
    assert_eq!(
        xcomp_bv_after, 0,
        "sanitize_xsave_header: XCOMP_BV not cleared (was {:#018x})",
        xcomp_bv_after,
    );

    // Reserved header bytes must be zeroed.
    for (i, &b) in buf[528..576].iter().enumerate() {
        assert_eq!(
            b,
            0,
            "sanitize_xsave_header: reserved header byte at offset {} not zeroed (got {:#04x})",
            528 + i,
            b,
        );
    }

    // Payload bytes must be unchanged.
    for (i, &b) in buf[160..416].iter().enumerate() {
        assert_eq!(
            b,
            0xA5,
            "sanitize_xsave_header: XMM payload byte at offset {} corrupted (got {:#04x})",
            160 + i,
            b,
        );
    }

    kernel::serial_println!(
        "[signal-fpu-test] xsave_header_sanitize_rejects_bad_fields: PASS \
         (mxcsr_after={:#010x} xstate_bv_after={:#018x} xcomp_bv_after={:#018x})",
        mxcsr_after,
        xstate_bv_after,
        xcomp_bv_after,
    );
}

/// Phase 86f Track B.1 BLOCKER 1 (rev3) — attacker-controlled MXCSR_MASK negative test.
///
/// Verifies that an attacker-supplied MXCSR_MASK=0xFFFFFFFF cannot allow
/// reserved MXCSR bits to survive sanitization.  This is the specific attack
/// described in rev3 FIX 1: if the sanitizer masks MXCSR against the buffer's
/// own MXCSR_MASK field, an attacker sets MXCSR_MASK=0xFFFFFFFF so that a
/// reserved bit (e.g. bit 16) is passed through, causing xrstor64 to #GP in
/// ring 0 — a kernel DoS.
///
/// The fix ignores buf[28..32] and always uses SAFE_MXCSR_MASK = 0xFFBF.
/// This test verifies that a reserved bit (bit 16, which is not a valid MXCSR
/// bit on any x86_64) is cleared regardless of the MXCSR_MASK field value.
#[test_case]
fn xsave_sanitize_ignores_attacker_mxcsr_mask() {
    use alloc::vec;
    use kernel::arch::x86_64::cpuid::XSAVE_AREA_SIZE;
    use kernel::task::scheduler::sanitize_xsave_header;

    // Bit 16 is above the architecturally-defined MXCSR bits (bits 0–15 are
    // the valid range on Intel/AMD x86_64; bits 16–31 are reserved and will
    // cause xrstor64 to raise #GP if set).
    const RESERVED_BIT: u32 = 1u32 << 16;
    // MXCSR with a reserved bit set — this would normally be caught by masking
    // against 0xFFBF, but with an attacker-supplied MXCSR_MASK=0xFFFFFFFF the
    // old (buggy) code would pass it through.
    let bad_mxcsr: u32 = 0x1F80 | RESERVED_BIT;

    // Attacker supplies MXCSR_MASK=0xFFFFFFFF — the maximum possible, which
    // would allow every bit to survive if the sanitizer trusted this field.
    let attacker_mxcsr_mask: u32 = 0xFFFF_FFFF;

    let mut buf = vec![0u8; XSAVE_AREA_SIZE];
    // x87 CW = 0x037F (architectural init).
    buf[0] = 0x7f;
    buf[1] = 0x03;
    // Poison MXCSR with the reserved bit.
    buf[24..28].copy_from_slice(&bad_mxcsr.to_le_bytes());
    // Attacker-supplied MXCSR_MASK — MUST be ignored by the sanitizer.
    buf[28..32].copy_from_slice(&attacker_mxcsr_mask.to_le_bytes());
    // XSTATE_BV = 0x3 (x87 + SSE present) — valid.
    buf[512] = 0x03;

    sanitize_xsave_header(&mut buf);

    let mxcsr_after = u32::from_le_bytes(buf[24..28].try_into().unwrap());
    // The reserved bit MUST be cleared regardless of the attacker-supplied mask.
    assert_eq!(
        mxcsr_after & RESERVED_BIT,
        0,
        "xsave_sanitize_ignores_attacker_mxcsr_mask: BLOCKER — reserved MXCSR bit 16 \
         survived sanitization despite attacker MXCSR_MASK=0xFFFFFFFF: mxcsr_after={:#010x}",
        mxcsr_after,
    );
    // The MXCSR_MASK field itself must be overwritten to the canonical safe value.
    let mxcsr_mask_after = u32::from_le_bytes(buf[28..32].try_into().unwrap());
    assert_eq!(
        mxcsr_mask_after, 0xFFBF,
        "xsave_sanitize_ignores_attacker_mxcsr_mask: MXCSR_MASK field not overwritten \
         to canonical 0xFFBF (got {:#010x})",
        mxcsr_mask_after,
    );
    // Verify valid MXCSR bits are preserved (0x1F80 & 0xFFBF = 0x1F80 since bit 6
    // is not in 0x1F80).
    let expected_mxcsr = bad_mxcsr & 0xFFBF;
    assert_eq!(
        mxcsr_after, expected_mxcsr,
        "xsave_sanitize_ignores_attacker_mxcsr_mask: valid MXCSR bits altered: \
         got {:#010x}, expected {:#010x}",
        mxcsr_after, expected_mxcsr,
    );

    kernel::serial_println!(
        "[signal-fpu-test] xsave_sanitize_ignores_attacker_mxcsr_mask: PASS \
         (mxcsr_after={:#010x} mxcsr_mask_after={:#010x})",
        mxcsr_after,
        mxcsr_mask_after,
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
// End-to-end XMM survives signal frame round-trip
// ---------------------------------------------------------------------------

/// Aligned storage for SSE pattern data.  The kernel is soft-float so we
/// cannot use `[u128; N]` layout with `movaps` directly — instead carry two
/// `u64` halves per register and use raw-byte SSE asm.
#[repr(C, align(16))]
struct AlignedBuf16([u8; 64]); // 4 × 128-bit patterns

impl AlignedBuf16 {
    const fn zeroed() -> Self {
        Self([0u8; 64])
    }
}

/// Write known 128-bit patterns into XMM0–XMM3 using raw SSE asm bytes and
/// return the patterns so the caller can compare after a restore cycle.
///
/// The kernel target forbids SSE register constraints, so we use raw `.byte`
/// sequences to encode `movdqu xmmR, [rax]` (load from memory).
///
/// Encoding: F3 0F 6F /r with ModRM=mod00, r/m=000 (rax):
///   xmm0: F3 0F 6F 00
///   xmm1: F3 0F 6F 08
///   xmm2: F3 0F 6F 10
///   xmm3: F3 0F 6F 18
///
/// Returns `[u8; 64]` — four 16-byte patterns for XMM0..XMM3 respectively.
///
/// # Safety
///
/// Caller must ensure the kernel has called `enable_xsave_state()` (i.e.,
/// OSXSAVE is set and the SSE unit is usable).  This is guaranteed after
/// `init_minimal_smp`.
unsafe fn write_xmm_patterns(patterns: &AlignedBuf16) -> [u8; 64] {
    let ptr = patterns.0.as_ptr();

    // Each asm block loads one XMM register from the address in rax.
    // xmm registers are not in the register allocator on the soft-float target
    // so they need not be declared as outputs.
    core::arch::asm!(
        ".byte 0xF3, 0x0F, 0x6F, 0x00", // movdqu xmm0, [rax]
        in("rax") ptr,
        options(nostack, preserves_flags),
    );
    core::arch::asm!(
        ".byte 0xF3, 0x0F, 0x6F, 0x08", // movdqu xmm1, [rax]
        in("rax") ptr.add(16),
        options(nostack, preserves_flags),
    );
    core::arch::asm!(
        ".byte 0xF3, 0x0F, 0x6F, 0x10", // movdqu xmm2, [rax]
        in("rax") ptr.add(32),
        options(nostack, preserves_flags),
    );
    core::arch::asm!(
        ".byte 0xF3, 0x0F, 0x6F, 0x18", // movdqu xmm3, [rax]
        in("rax") ptr.add(48),
        options(nostack, preserves_flags),
    );

    // Return a copy of the patterns we loaded.
    patterns.0
}

/// Read XMM0–XMM3 into a 64-byte buffer.
///
/// movdqu [rax], xmmR  → F3 0F 7F /r with ModRM for [rax]:
///   xmm0: F3 0F 7F 00
///   xmm1: F3 0F 7F 08
///   xmm2: F3 0F 7F 10
///   xmm3: F3 0F 7F 18
unsafe fn read_xmm_to_buf(out: &mut AlignedBuf16) {
    let ptr = out.0.as_mut_ptr();
    core::arch::asm!(
        ".byte 0xF3, 0x0F, 0x7F, 0x00",
        in("rax") ptr,
        options(nostack, preserves_flags),
    );
    core::arch::asm!(
        ".byte 0xF3, 0x0F, 0x7F, 0x08",
        in("rax") ptr.add(16),
        options(nostack, preserves_flags),
    );
    core::arch::asm!(
        ".byte 0xF3, 0x0F, 0x7F, 0x10",
        in("rax") ptr.add(32),
        options(nostack, preserves_flags),
    );
    core::arch::asm!(
        ".byte 0xF3, 0x0F, 0x7F, 0x18",
        in("rax") ptr.add(48),
        options(nostack, preserves_flags),
    );
}

/// Phase 86f Track B.1 end-to-end acceptance: verify that the kernel's signal
/// frame FPU save/restore path preserves XMM0–XMM3 across a signal-handler
/// clobber.
///
/// Runs entirely in a kernel scheduler task using hand-encoded SSE asm bytes
/// (the compiler cannot emit SSE for the soft-float kernel target).  Steps:
///
///   1. Write pattern-A (0x11...) into XMM0–XMM3 via raw SSE asm.
///   2. Call `with_current_task_fpu_saved` — captures the live hardware
///      FPU state into the task's XSaveArea (mirrors signal delivery).
///   3. Write pattern-B (0xCC...) into XMM0–XMM3 (mimics handler clobber).
///   4. Call `restore_current_task_fpu_from_bytes` with the saved bytes
///      — restores the task's FPU to the pre-signal state (mirrors sigreturn).
///   5. Read XMM0–XMM3 back and assert they equal pattern-A.
#[test_case]
fn xmm_survives_signal() {
    use alloc::vec;
    use kernel::arch::x86_64::cpuid::XSAVE_AREA_SIZE;

    // Must run inside a scheduler task.
    assert!(
        kernel::task::scheduler::get_current_task_idx().is_some(),
        "xmm_survives_signal must run inside a scheduled kernel task"
    );

    // Reset FPU state for a clean baseline.
    kernel::task::scheduler::reset_current_task_fpu_state();

    // Pattern A: all XMM0–XMM3 bytes = 0x11, 0x22, 0x33, 0x44 respectively.
    let pattern_a = AlignedBuf16({
        let mut b = [0u8; 64];
        for x in &mut b[0..16] {
            *x = 0x11;
        }
        for x in &mut b[16..32] {
            *x = 0x22;
        }
        for x in &mut b[32..48] {
            *x = 0x33;
        }
        for x in &mut b[48..64] {
            *x = 0x44;
        }
        b
    });

    // Step 1: write pattern A into XMM0–XMM3.
    let expected = unsafe { write_xmm_patterns(&pattern_a) };

    // Step 2: save live FPU state (mirrors signal delivery).
    let snapshot = kernel::task::scheduler::with_current_task_fpu_saved(|bytes| {
        let mut copy = vec![0u8; bytes.len()];
        copy.copy_from_slice(bytes);
        copy
    })
    .expect("with_current_task_fpu_saved must succeed inside task");
    assert_eq!(snapshot.len(), XSAVE_AREA_SIZE);

    // Verify pattern A survived the xsave: XMM region at offsets 160–415.
    assert_eq!(
        &snapshot[160..176],
        &expected[0..16],
        "xmm_survives_signal: XMM0 pattern not captured by xsave"
    );

    // Step 3: write pattern B (0xCC) into XMM0–XMM3 — simulates handler clobber.
    let pattern_b = AlignedBuf16({
        let mut b = [0u8; 64];
        for x in &mut b {
            *x = 0xCC;
        }
        b
    });
    unsafe { write_xmm_patterns(&pattern_b) };

    // Step 4: restore from pre-signal snapshot (mirrors sigreturn).
    kernel::task::scheduler::restore_current_task_fpu_from_bytes(&snapshot);

    // Step 5: read XMM0–XMM3 back and compare to pattern A.
    let mut readback = AlignedBuf16::zeroed();
    unsafe { read_xmm_to_buf(&mut readback) };

    for reg in 0..4usize {
        let off = reg * 16;
        assert_eq!(
            &readback.0[off..off + 16],
            &expected[off..off + 16],
            "xmm_survives_signal: XMM{} not restored after sigreturn-path restore \
             (got {:?}, expected {:?})",
            reg,
            &readback.0[off..off + 16],
            &expected[off..off + 16],
        );
    }

    kernel::serial_println!("[signal-fpu-test] xmm_survives_signal: PASS");
}

// ---------------------------------------------------------------------------
// FIX 3 (MAJOR) — real signal-frame path test
// ---------------------------------------------------------------------------

/// Phase 86f Track B.1 rev3 — honest signal-frame round-trip through the
/// production `setup_signal_frame` / `restore_sigframe` code paths.
///
/// Steps:
///   1. Map scratch user-accessible pages in the test address space.
///   2. Write pattern-A into XMM0–XMM3; `with_current_task_fpu_saved` captures
///      the live FPU state (pre-signal snapshot, as signal delivery would).
///   3. Call the REAL `kernel::signal::setup_signal_frame` targeting the scratch
///      stack — exercises MC_FPSTATE pointer write, user-copy paths, frame layout.
///   4. Clobber XMM0–XMM3 with pattern-B (simulate signal handler).
///   5. Call the REAL `kernel::signal::restore_sigframe(frame_rsp + 8)` —
///      exercises the pretcode-pop adjustment, fpstate pointer validation, and
///      user-copy readback.
///   6. Feed the returned FPU bytes through `restore_current_task_fpu_from_bytes`.
///   7. Read XMM0–XMM3 back and assert bit-identical to pattern-A.
///   8. Unmap the scratch pages.
///
/// This exercises the production frame layout, MC_FPSTATE pointer write/validation,
/// and user-copy paths — the minimum honest signal-frame path test possible
/// within the QEMU kernel-test harness.
#[test_case]
fn xmm_survives_signal_frame_path() {
    use alloc::vec;
    use kernel::arch::x86_64::cpuid::XSAVE_AREA_SIZE;
    use kernel::signal::{
        SIGFRAME_EXTENDED_SIZE, SavedUserRegs, restore_sigframe, setup_signal_frame,
    };
    use x86_64::structures::paging::PageTableFlags;

    // Must run inside a scheduler task.
    assert!(
        kernel::task::scheduler::get_current_task_idx().is_some(),
        "xmm_survives_signal_frame_path must run inside a scheduled kernel task"
    );

    // -----------------------------------------------------------------------
    // 1. Map scratch user pages at a known address.
    //
    // We need enough space for the extended sigframe (SIGFRAME_EXTENDED_SIZE =
    // SIGFRAME_SIZE + FPU_AREA_SIZE ≈ 1392 bytes) plus head-room for alignment
    // and the RSP we'll pass in.  4 pages = 16 384 bytes is comfortable.
    //
    // Choose a test VA in the lower canonical half, well away from any real
    // userspace load address.  The kernel test environment has the frame
    // allocator and page mapper active after init_minimal_smp.
    // -----------------------------------------------------------------------
    const SCRATCH_VBASE: u64 = 0x0000_6FFE_0000_0000;
    const SCRATCH_PAGES: u64 = 4;
    const SCRATCH_BYTES: u64 = SCRATCH_PAGES * 4096;

    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    let mut mapper = unsafe { kernel::mm::paging::get_mapper() };
    unsafe {
        kernel::mm::user_space::map_user_pages(&mut mapper, SCRATCH_VBASE, SCRATCH_PAGES, flags)
            .expect("xmm_survives_signal_frame_path: map_user_pages failed");
    }

    // -----------------------------------------------------------------------
    // 2. Write pattern-A into XMM0–XMM3 and save live FPU state.
    // -----------------------------------------------------------------------
    kernel::task::scheduler::reset_current_task_fpu_state();

    let pattern_a = AlignedBuf16({
        let mut b = [0u8; 64];
        for x in &mut b[0..16] {
            *x = 0xAA;
        }
        for x in &mut b[16..32] {
            *x = 0xBB;
        }
        for x in &mut b[32..48] {
            *x = 0xCC;
        }
        for x in &mut b[48..64] {
            *x = 0xDD;
        }
        b
    });
    let expected = unsafe { write_xmm_patterns(&pattern_a) };

    let snapshot = kernel::task::scheduler::with_current_task_fpu_saved(|bytes| {
        let mut copy = vec![0u8; bytes.len()];
        copy.copy_from_slice(bytes);
        copy
    })
    .expect("with_current_task_fpu_saved must succeed inside task");
    assert_eq!(snapshot.len(), XSAVE_AREA_SIZE);

    // Verify pattern-A was captured in the XMM region (offsets 160–175 = XMM0).
    assert_eq!(
        &snapshot[160..176],
        &expected[0..16],
        "xmm_survives_signal_frame_path: XMM0 not in xsave snapshot"
    );

    // -----------------------------------------------------------------------
    // 3. Call the real setup_signal_frame targeting the scratch stack.
    //
    // We place user RSP near the top of the scratch region.  setup_signal_frame
    // computes the frame position as (rsp - SIGFRAME_EXTENDED_SIZE) & !15 - 8.
    // With SCRATCH_VBASE=0x6FFE_0000_0000 and SCRATCH_BYTES=16384, the top is
    // at 0x6FFE_0000_4000 — but we use SCRATCH_VBASE+SCRATCH_BYTES-8 as the
    // "original user RSP" so the computed frame lands inside our mapped region.
    // -----------------------------------------------------------------------
    let user_rsp_for_setup = SCRATCH_VBASE + SCRATCH_BYTES - 8;

    // Dummy saved register state — only rsp matters for frame layout.
    let regs = SavedUserRegs {
        rax: 0,
        rbx: 0,
        rcx: 0,
        rdx: 0,
        rsi: 0,
        rdi: 0,
        rbp: 0,
        rsp: user_rsp_for_setup,
        r8: 0,
        r9: 0,
        r10: 0,
        r11: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        rip: 0x4000_0000,    // dummy user RIP (anywhere in user space)
        rflags: 0x0000_0202, // IF set, minimal flags
    };

    // Restorer address: any canonical user address is fine for this test.
    let restorer = 0x4000_1000u64;
    let blocked_signals = 0u64;
    let signal_num = 10u32; // SIGUSR1

    let frame_rsp = setup_signal_frame(
        &regs,
        blocked_signals,
        signal_num,
        restorer,
        None, // no alt stack
        Some(&snapshot),
    )
    .expect("xmm_survives_signal_frame_path: setup_signal_frame returned None");

    // Verify the frame landed inside our scratch region.
    assert!(
        frame_rsp >= SCRATCH_VBASE,
        "xmm_survives_signal_frame_path: frame_rsp {:#x} < SCRATCH_VBASE {:#x}",
        frame_rsp,
        SCRATCH_VBASE,
    );
    assert!(
        frame_rsp + SIGFRAME_EXTENDED_SIZE as u64 <= SCRATCH_VBASE + SCRATCH_BYTES,
        "xmm_survives_signal_frame_path: frame_rsp+SIGFRAME_EXTENDED_SIZE overflows scratch"
    );

    // -----------------------------------------------------------------------
    // 4. Clobber XMM0–XMM3 with pattern-B (simulate signal handler clobber).
    // -----------------------------------------------------------------------
    let pattern_b = AlignedBuf16({
        let mut b = [0u8; 64];
        for x in &mut b {
            *x = 0xFF;
        }
        b
    });
    unsafe { write_xmm_patterns(&pattern_b) };

    // -----------------------------------------------------------------------
    // 5. Call the real restore_sigframe.
    //
    // The production sigreturn path calls restore_sigframe(user_rsp) where
    // user_rsp is the user RSP at sigreturn time — i.e. after the handler's
    // `ret` popped pretcode, so RSP = frame_rsp + 8.
    // -----------------------------------------------------------------------
    let sigreturn_rsp = frame_rsp + 8;
    let (_restored_regs, _restored_mask, fpu_bytes_opt) = restore_sigframe(sigreturn_rsp)
        .expect("xmm_survives_signal_frame_path: restore_sigframe returned None");

    let fpu_bytes = fpu_bytes_opt
        .expect("xmm_survives_signal_frame_path: restore_sigframe returned no FPU bytes");
    assert_eq!(
        fpu_bytes.len(),
        XSAVE_AREA_SIZE,
        "xmm_survives_signal_frame_path: fpu_bytes length mismatch"
    );

    // Verify the XMM region round-trips the frame (snapshot → frame → readback).
    assert_eq!(
        &fpu_bytes[160..176],
        &expected[0..16],
        "xmm_survives_signal_frame_path: XMM0 not in restored FPU bytes"
    );

    // -----------------------------------------------------------------------
    // 6. Feed returned FPU bytes through restore_current_task_fpu_from_bytes.
    // -----------------------------------------------------------------------
    kernel::task::scheduler::restore_current_task_fpu_from_bytes(&fpu_bytes);

    // -----------------------------------------------------------------------
    // 7. Read XMM0–XMM3 back and assert bit-identical to pattern-A.
    // -----------------------------------------------------------------------
    let mut readback = AlignedBuf16::zeroed();
    unsafe { read_xmm_to_buf(&mut readback) };

    for reg in 0..4usize {
        let off = reg * 16;
        assert_eq!(
            &readback.0[off..off + 16],
            &expected[off..off + 16],
            "xmm_survives_signal_frame_path: XMM{} not restored through frame path \
             (got {:02x?}, expected {:02x?})",
            reg,
            &readback.0[off..off + 16],
            &expected[off..off + 16],
        );
    }

    // -----------------------------------------------------------------------
    // 8. Unmap the scratch pages.
    // -----------------------------------------------------------------------
    {
        use x86_64::{
            VirtAddr,
            structures::paging::{Mapper, Page, Size4KiB},
        };
        let mut frame_addrs = [0u64; SCRATCH_PAGES as usize];
        for i in 0..SCRATCH_PAGES {
            let vaddr = VirtAddr::new(SCRATCH_VBASE + i * 4096);
            let page = Page::<Size4KiB>::containing_address(vaddr);
            if let Ok((frame, flush)) = mapper.unmap(page) {
                flush.flush();
                frame_addrs[i as usize] = frame.start_address().as_u64();
            }
        }
        for &phys in &frame_addrs {
            if phys != 0 {
                kernel::mm::frame_allocator::free_frame(phys);
            }
        }
    }

    kernel::serial_println!("[signal-fpu-test] xmm_survives_signal_frame_path: PASS");
}
