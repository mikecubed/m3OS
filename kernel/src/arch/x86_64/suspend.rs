//! Phase 103 Track F.3 — ACPI S3 suspend-to-RAM entry + resume.
//!
//! Division of labor (the established Phase 101/103 split): ring-3
//! `powerd` orchestrates (`\_PTS(3)` before, `\_WAK(3)` after, both via
//! acpid), ring-3 `acpid` evaluates `\_S3` and registers the SLP_TYP
//! values ([`crate::syscall::acpi::sys_acpi_register_s3`]); this module
//! owns only the privileged mechanism:
//!
//! 1. **Quiesce** — sync filesystems, park every AP (the panic-park
//!    shape: they are obliterated by the wake-side machine reset
//!    anyway), drain their run queues to the BSP.
//! 2. **Arm the wake path** — a 32-bit shim on the SMP SIPI trampoline
//!    page (sharing its GDT + stack/entry data fields), pointed at by
//!    the FACS **X** firmware waking vector: firmware enters it per
//!    spec in 32-bit flat protected mode with paging off. (OVMF's
//!    legacy-vector path jumps in 64-bit flat mode to a page-truncated
//!    address — observed live, unusable.)
//! 3. **Sleep** — [`suspend_save_and_sleep`] captures the callee-saved
//!    frame and writes `SLP_TYPa<<10 | SLP_EN` to PM1a_CNT **from asm
//!    without touching the stack** (the return-address slot at the
//!    saved RSP must survive for the resume `ret`). Execution stops.
//! 4. **Resume** — the shim → long mode on the kernel CR3 →
//!    [`resume_entry`] on a dedicated stack does register-state work
//!    ONLY (GDT with the TSS busy-bit cleared, IDT, GS bases, syscall
//!    MSRs, PAT/XCR0, TSC monotonic rebase — anything heavier here runs
//!    preempt pairs against the suspended task's counter and wedges its
//!    dispatch), then long-jumps back into [`enter_sleep_s3`], which —
//!    now in task context — re-inits PIC/APIC, restores PCI config
//!    (firmware does not restore OS-visible BARs), re-arms SCI/PWRBTN,
//!    re-handshakes the virtio devices against their retained rings,
//!    reboots the APs, and returns 0 — `powerd` continues exactly where
//!    it slept.
//!
//! Every failure before the PM1a write returns a negative errno with
//! the machine fully live (the Track F fail-closed contract).

use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::instructions::port::Port;

/// FACS offsets (ACPI 6.5 §5.2.10).
const FACS_SIG_OFFSET: usize = 0; // "FACS"
const FACS_WAKING_VECTOR_OFFSET: usize = 12; // u32, real-mode entry
const FACS_X_WAKING_VECTOR_OFFSET: usize = 24; // u64 — zeroed (we want real mode)

/// PM1 control bits.
const SLP_EN: u16 = 1 << 13;
/// PM1 status: wake status (write-1-to-clear).
const PM1_WAK_STS: u16 = 1 << 15;

/// `\_S3` SLP_TYP registration (the S5 shape in `syscall::acpi`):
/// bit 31 = registered, bits 15:8 = SLP_TYPa, bits 7:0 = SLP_TYPb.
static S3_SLP_TYP: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Callee-saved CPU context captured by `suspend_save`, restored by
/// `suspend_resume_longjmp`. Layout is ABI for the asm below.
#[repr(C)]
struct SavedContext {
    rsp: u64,
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rip: u64,
}

static mut SAVED_CTX: SavedContext = SavedContext {
    rsp: 0,
    rbx: 0,
    rbp: 0,
    r12: 0,
    r13: 0,
    r14: 0,
    r15: 0,
    rip: 0,
};

/// Dedicated resume stack: [`resume_entry`] runs on this until the
/// long-jump lands back on the suspended task's kernel stack (which is
/// intact in RAM but must not be touched until the CPU state that its
/// frames assume — GDT, IDT, MSRs — has been re-established).
#[repr(C, align(16))]
struct ResumeStack([u8; 16 * 1024]);
static mut RESUME_STACK: ResumeStack = ResumeStack([0; 16 * 1024]);

/// Monotonic-clock continuity across the reset: microseconds of
/// monotonic time accumulated before the suspend, captured right before
/// the PM1a write and folded back into the TSC base on resume.
static SUSPENDED_ELAPSED_TSC: AtomicU64 = AtomicU64::new(0);

/// Register the `\_S3` sleep-type values (acpid, boot). Masked to the
/// architectural 3 bits.
pub fn register_s3(slp_typa: u64, slp_typb: u64) {
    let packed = (1u32 << 31) | (((slp_typa & 0x7) as u32) << 8) | ((slp_typb & 0x7) as u32);
    S3_SLP_TYP.store(packed, Ordering::Release);
    log::info!(
        "[suspend] \\_S3 registered: SLP_TYPa={} SLP_TYPb={}",
        slp_typa & 0x7,
        slp_typb & 0x7
    );
}

/// Whether the platform can attempt S3 (both `\_S3` registered and a
/// FACS present).
pub fn s3_available() -> bool {
    if S3_SLP_TYP.load(Ordering::Acquire) & (1 << 31) == 0 {
        return false;
    }
    matches!(crate::acpi::fadt_info(), Some(f) if f.facs_phys() != 0 && f.pm1a_cnt_blk != 0)
}

// ---------------------------------------------------------------------------
// The save / restore asm pair
// ---------------------------------------------------------------------------
//
core::arch::global_asm!(
    // suspend_save_and_sleep(ctx: rdi, pm1a_cnt_port: rsi, slp_value: rdx) -> u64
    //
    // Saves the callee-saved context, then writes SLP_TYP|SLP_EN to the
    // PM1a control port WITHOUT TOUCHING THE STACK — the return-address
    // slot at [rsp] must stay intact for the resume path's final `ret`
    // (found live: doing the write from Rust after a returning "save"
    // clobbered that slot with the next push and the resume ret jumped
    // to garbage). Returns 0 if the platform refused the transition
    // (fall-through after a bounded spin) and 1 when execution comes
    // back through `suspend_resume_longjmp` after the wake.
    ".global suspend_save_and_sleep",
    "suspend_save_and_sleep:",
    "mov [rdi + 0x00], rsp",
    "mov [rdi + 0x08], rbx",
    "mov [rdi + 0x10], rbp",
    "mov [rdi + 0x18], r12",
    "mov [rdi + 0x20], r13",
    "mov [rdi + 0x28], r14",
    "mov [rdi + 0x30], r15",
    "lea rax, [rip + 2f]",
    "mov [rdi + 0x38], rax",
    // The sleep write: dx = port, ax = value. The CPU stops here on
    // success; QEMU may take a moment, so spin without stack use.
    "mov rax, rdx",
    "mov rdx, rsi",
    "out dx, ax",
    "mov rcx, 100000000",
    "3:",
    "pause",
    "dec rcx",
    "jnz 3b",
    "xor eax, eax", // still running: the platform refused — fail closed
    "ret",
    "2:",
    "mov rax, 1", // resumed
    "ret",
    ".global suspend_resume_longjmp",
    "suspend_resume_longjmp:",
    "mov rsp, [rdi + 0x00]",
    "mov rbx, [rdi + 0x08]",
    "mov rbp, [rdi + 0x10]",
    "mov r12, [rdi + 0x18]",
    "mov r13, [rdi + 0x20]",
    "mov r14, [rdi + 0x28]",
    "mov r15, [rdi + 0x30]",
    // Retpoline discipline: no indirect `jmp` in kernel text — transfer
    // via push+ret (RSB-predicted; the ELF gate rejects `jmp *`).
    "push qword ptr [rdi + 0x38]",
    "ret",
);

unsafe extern "C" {
    fn suspend_save_and_sleep(ctx: *mut SavedContext, pm1a_cnt_port: u64, slp_value: u64) -> u64;
    fn suspend_resume_longjmp(ctx: *const SavedContext) -> !;
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

/// Raw COM1 byte — usable at any point in the resume path (before or
/// after `serial::init`, no locks).
fn raw_mark(b: u8) {
    unsafe { Port::<u8>::new(0x3F8).write(b) };
}

const NEG_ENOSYS: i64 = -38;
const NEG_EIO: i64 = -5;
const NEG_EBUSY: i64 = -16;

/// Enter S3 and return after resume. 0 = resumed; negative errno with
/// the machine fully live otherwise. Interrupt state: called from the
/// syscall path with interrupts enabled; returns the same way.
pub fn enter_sleep_s3() -> i64 {
    let packed = S3_SLP_TYP.load(Ordering::Acquire);
    if packed & (1 << 31) == 0 {
        return NEG_ENOSYS;
    }
    let Some(fadt) = crate::acpi::fadt_info() else {
        return NEG_ENOSYS;
    };
    let facs_phys = fadt.facs_phys();
    if facs_phys == 0 || fadt.pm1a_cnt_blk == 0 {
        return NEG_ENOSYS;
    }
    // Validate the FACS signature before writing into firmware memory.
    let phys_off = crate::mm::phys_offset();
    let facs = (phys_off + facs_phys) as *mut u8;
    let sig = unsafe { core::ptr::read_unaligned(facs.add(FACS_SIG_OFFSET) as *const [u8; 4]) };
    if &sig != b"FACS" {
        log::warn!("[suspend] FACS signature mismatch at {facs_phys:#x}");
        return NEG_EIO;
    }

    log::info!("[suspend] entering S3 (suspend-to-RAM)");

    // 0. Run the entry on the BSP: the wake-side reset restarts the boot
    //    CPU, so the context we save must belong to core 0 (found live by
    //    suspend-smoke — the syscall can arrive on any core, and a core
    //    cannot park itself).
    if !crate::task::scheduler::migrate_current_to_bsp() {
        log::warn!("[suspend] could not migrate to the BSP; failing closed");
        return NEG_EBUSY;
    }

    // 1. Filesystem sync while the disk stack is fully alive.
    crate::arch::x86_64::syscall::kernel_shutdown_sync();

    // 2. Park + release the APs (they are reset on wake; rebooted below).
    if !crate::smp::suspend_park_and_release_aps() {
        return NEG_EBUSY; // fail closed: the machine is fully live
    }

    // 3. From here to the PM1a write: interrupts off, single core.
    x86_64::instructions::interrupts::disable();

    // 3b. Drain in-flight block I/O (a write submitted between the sync
    //     and the cli would otherwise die with the reset and orphan its
    //     waiter — found live via post-resume completion timeouts).
    if !crate::blk::virtio_blk::quiesce_for_suspend() {
        x86_64::instructions::interrupts::enable();
        crate::smp::resume_reboot_aps();
        return NEG_EBUSY;
    }

    // 3c. Snapshot PCI config — firmware does not restore OS-visible
    //     BARs across the wake reset (post-resume reads were all-ones).
    crate::pci::save_config_for_suspend();

    // 4. Arm the wake path: trampoline at 0x8000 with resume_entry as
    //    the 64-bit target, on the dedicated resume stack.
    let resume_stack_top = {
        let base = &raw mut RESUME_STACK as u64;
        (base + 16 * 1024) & !0xF
    };
    crate::smp::boot::install_trampoline_for_resume(
        resume_entry as *const () as u64,
        resume_stack_top,
    );
    unsafe {
        // X vector → the 32-bit resume shim, entered per spec in 32-bit
        // flat protected mode with paging off (OSPM flags stay clear so
        // firmware never picks the 64-bit variant). The legacy vector
        // stays zero — OVMF's legacy path proved unusable (see
        // `install_trampoline_for_resume`).
        (facs.add(FACS_WAKING_VECTOR_OFFSET) as *mut u32).write_unaligned(0);
        (facs.add(FACS_X_WAKING_VECTOR_OFFSET) as *mut u64).write_unaligned(
            crate::smp::boot::TRAMPOLINE_PHYS + crate::smp::boot::RESUME_STUB_OFFSET,
        );
    }

    // 5. Monotonic-clock continuity: capture elapsed TSC now; resume
    //    rebases the boot-TSC so CLOCK_MONOTONIC never jumps.
    let elapsed =
        unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(crate::arch::x86_64::apic::boot_tsc());
    SUSPENDED_ELAPSED_TSC.store(elapsed, Ordering::Release);

    // 6. Clear a stale WAK_STS, save context, write SLP_TYP|SLP_EN.
    let pm1a_sts = fadt.pm1a_evt_blk as u16;
    let pm1a_cnt = fadt.pm1a_cnt_blk as u16;
    let slp_typa = ((packed >> 8) & 0x7) as u16;
    let value = (slp_typa << 10) | SLP_EN;
    log::info!("[suspend] PM1a_CNT {pm1a_cnt:#x} <- {value:#06x} (S3)");
    // The resume path's re-init runs preempt pairs against THIS task's
    // counter (the per-core current-task pointer still targets us);
    // snapshot the depth and force it back after the long-jump.
    let preempt_depth = crate::task::scheduler::current_preempt_count();
    let resumed = unsafe {
        if pm1a_sts != 0 {
            Port::<u16>::new(pm1a_sts).write(PM1_WAK_STS);
        }
        suspend_save_and_sleep(&raw mut SAVED_CTX, pm1a_cnt as u64, value as u64) == 1
    };
    if resumed {
        crate::task::scheduler::force_preempt_count(preempt_depth);
    }

    if !resumed {
        // Fail closed: undo and hand back a live machine.
        log::warn!("[suspend] S3 write did not suspend; failing closed");
        unsafe {
            (facs.add(FACS_WAKING_VECTOR_OFFSET) as *mut u32).write_unaligned(0);
            (facs.add(FACS_X_WAKING_VECTOR_OFFSET) as *mut u64).write_unaligned(0);
        }
        x86_64::instructions::interrupts::enable();
        crate::smp::resume_reboot_aps();
        return NEG_EIO;
    }

    // ---- We are back from S3 (via resume_entry's longjmp) -------------
    // Single core, interrupts off, descriptor tables + APIC + serial
    // re-established by resume_entry. Clear WAK_STS and re-arm ACPI.
    unsafe {
        if pm1a_sts != 0 {
            Port::<u16>::new(pm1a_sts).write(PM1_WAK_STS);
        }
        // SCI_EN survives in some firmwares and not others; the acpid
        // enable handshake set it at boot — re-set it if the reset
        // cleared it (SCI_EN is bit 0 of PM1a_CNT).
        let cnt: u16 = Port::<u16>::new(pm1a_cnt).read();
        if cnt & 1 == 0 && fadt.smi_cmd != 0 && fadt.acpi_enable != 0 {
            Port::<u8>::new(fadt.smi_cmd as u16).write(fadt.acpi_enable);
        }
    }

    raw_mark(b'W');
    // Heavyweight re-init, now in proper task context (interrupts still
    // off): Spectre/mitigation MSRs, cpufreq probe, interrupt
    // controllers (PICs masked + LAPIC/IOAPIC/timer — calibrations are
    // Once-cached from boot and stable across S3).
    crate::mitigations::init_bsp();
    crate::arch::x86_64::cpufreq::init_bsp();
    unsafe {
        crate::arch::x86_64::interrupts::init_pics();
    }
    crate::arch::x86_64::apic::init();

    // Bring PCI config space back before any driver touches its BARs.
    crate::pci::restore_config_after_resume();

    // Restore the SCI route + power-button enable (the IOAPIC redirection
    // and PM1_EN were wiped; acpid's subscription is RAM state and lives).
    crate::acpi::sci::reroute_after_resume();

    // Re-init the virtio devices: the machine reset wiped their queue
    // registers and status; the rings + indices survive in RAM.
    crate::blk::virtio_blk::resume_after_s3();
    crate::net::virtio_net::resume_after_s3();

    x86_64::instructions::interrupts::enable();

    // Reboot the APs through the normal SIPI path (they are in
    // wait-for-SIPI after the reset).
    crate::smp::resume_reboot_aps();

    log::info!("[suspend] resumed from S3");
    0
}

/// The 64-bit resume target the trampoline jumps to (real mode →
/// protected → long mode with the kernel CR3 already loaded). Runs on
/// the dedicated resume stack; re-establishes enough CPU state to make
/// the suspended kernel stack usable again, then long-jumps back.
extern "C" fn resume_entry(_unused: *mut core::ffi::c_void) -> ! {
    // MINIMAL register-state re-establishment only — this stretch runs
    // on the resume stack while the per-core current-task pointer still
    // targets the suspended task, so it must not touch locks, the
    // allocator, or anything that runs `preempt_disable`/`enable` pairs
    // (found live: heavyweight re-init here left the suspended task's
    // preempt accounting unbalanced and wedged the BSP dispatcher).
    // Everything heavier happens after the long-jump, in task context.
    crate::serial::init();
    crate::arch::x86_64::gdt::reinit_after_resume();
    crate::arch::x86_64::interrupts::init();
    // GS bases (the per-core pointer) were wiped with the MSRs — restore
    // before anything touches `per_core()`.
    crate::smp::restore_bsp_gs_base();
    crate::arch::x86_64::syscall::init();
    crate::arch::x86_64::pat::init();
    // SAFETY: same call the BSP boot and every AP entry make — sets this
    // core's XCR0/CR4.OSXSAVE from the boot-probed feature set.
    unsafe {
        crate::arch::x86_64::cpuid::enable_xsave_state();
    }

    // Monotonic clock continuity: TSC restarted near zero — rebase the
    // boot TSC so `now - boot` continues from the pre-suspend elapsed.
    let elapsed = SUSPENDED_ELAPSED_TSC.load(Ordering::Acquire);
    let now = unsafe { core::arch::x86_64::_rdtsc() };
    crate::arch::x86_64::apic::rebase_boot_tsc(now.wrapping_sub(elapsed));

    // Back to the suspended context (enter_sleep_s3, resumed = true).
    raw_mark(b'U');
    unsafe { suspend_resume_longjmp(&raw const SAVED_CTX) }
}
