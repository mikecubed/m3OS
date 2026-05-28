//! Phase 77 Track B — debug-only SMEP / SMAP "deliberate fault" self-test.
//!
//! Gated behind the `smep-smap-test` cargo feature so it is **zero-cost and
//! absent** from production builds.  Run it once at boot (after `CR4.SMEP` /
//! `CR4.SMAP` are enabled and `mm` is up) to *prove the mitigations are live*:
//!
//!   * **SMEP** — ring 0 deliberately `jmp`s into a `USER_ACCESSIBLE`
//!     executable page; the instruction fetch must `#PF`.
//!   * **SMAP** — ring 0 deliberately reads a `USER_ACCESSIBLE` page through
//!     the *user virtual address*; the load must `#PF`.  Reading the same
//!     bytes through the physical-memory direct map (the path every real
//!     kernel user-copy uses) must still succeed.
//!
//! Recovery: a per-core-free expected-fault hook in the page-fault handler
//! (also feature-gated) redirects the faulting RIP to a recovery label and
//! records that the fault happened, so the test continues instead of panicking.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::VirtAddr;
use x86_64::structures::paging::PageTableFlags;

/// Recovery RIP the page-fault handler jumps to on the next ring-0 fault, or 0
/// when no fault is expected.  Single-threaded boot context, so a plain
/// `AtomicU64` (no fencing beyond Acquire/Release) is sufficient.
static EXPECTED_FAULT_RIP: AtomicU64 = AtomicU64::new(0);
/// Set by the handler when it consumes an expected fault.
static FAULT_OCCURRED: AtomicBool = AtomicBool::new(false);

/// Page-fault-handler hook (ring-0 path).  Returns `Some(recovery_rip)` when an
/// expected fault is armed — the caller redirects the trap frame's RIP to it.
#[inline]
pub fn take_expected_fault_recovery() -> Option<u64> {
    let rip = EXPECTED_FAULT_RIP.swap(0, Ordering::AcqRel);
    if rip != 0 {
        FAULT_OCCURRED.store(true, Ordering::Release);
        Some(rip)
    } else {
        None
    }
}

/// Scratch user virtual address for the test page.  Must sit in a PML4 slot
/// that is **unmapped at boot** so the mapping creates *fresh* intermediate
/// tables with the `USER_ACCESSIBLE` bit set at every level — otherwise the
/// page would inherit a SUPERVISOR (and NX) intermediate entry from a reused
/// kernel low mapping, making it effective-supervisor and defeating both
/// SMEP and SMAP (a low address like 0x10_0000 falls in the already-present
/// PML4[0]).  PML4 index 100 (≈54 TiB) is deep in the user half and empty at
/// boot (no user process has run yet).
const SCRATCH_UVADDR: u64 = 100u64 << 39;

/// Deliberately read `uvaddr` (a user page) from ring 0.  Returns `true` if the
/// read `#PF`'d (SMAP live), `false` if it completed.
#[inline(never)]
unsafe fn read_user_vaddr_expect_fault(uvaddr: u64) -> bool {
    // Deliberately does NOT `clac` here: the boot path already cleared AC
    // (`clear_ac_for_smap`) before this self-test runs, so a fault here proves
    // SMAP is enforcing in the *production* AC state, not an artificially
    // forced one.
    FAULT_OCCURRED.store(false, Ordering::Release);
    let exp = EXPECTED_FAULT_RIP.as_ptr();
    unsafe {
        core::arch::asm!(
            "lea rax, [rip + 2f]",
            "mov qword ptr [{exp}], rax",
            "movzx rax, byte ptr [{uptr}]",
            "2:",
            exp = in(reg) exp,
            uptr = in(reg) uvaddr,
            out("rax") _,
            options(nostack),
        );
    }
    EXPECTED_FAULT_RIP.store(0, Ordering::Release);
    FAULT_OCCURRED.load(Ordering::Acquire)
}

/// Deliberately fetch+execute an instruction from `ucode` (a user page) in
/// ring 0.  Returns `true` if the fetch `#PF`'d (SMEP live).  Uses `jmp` (not
/// `call`) so no return address is pushed and the recovered RSP is clean.
#[inline(never)]
unsafe fn exec_user_vaddr_expect_fault(ucode: u64) -> bool {
    FAULT_OCCURRED.store(false, Ordering::Release);
    let exp = EXPECTED_FAULT_RIP.as_ptr();
    unsafe {
        core::arch::asm!(
            "lea rax, [rip + 3f]",
            "mov qword ptr [{exp}], rax",
            "jmp {code}",
            "3:",
            exp = in(reg) exp,
            code = in(reg) ucode,
            out("rax") _,
            options(nostack),
        );
    }
    EXPECTED_FAULT_RIP.store(0, Ordering::Release);
    FAULT_OCCURRED.load(Ordering::Acquire)
}

/// Run the SMEP + SMAP deliberate-fault self-test once.  Logs
/// `SMEP_SMAP_SELFTEST:PASS` on success or `:FAIL <detail>` on mismatch.
pub fn run_boot_self_test() {
    let (smep, smap) = crate::arch::x86_64::cpuid::probe_smep_smap();
    if !smep && !smap {
        log::warn!("[smep-smap-test] CPU advertises neither SMEP nor SMAP — skipping");
        return;
    }

    // Map one scratch user page (PRESENT | USER_ACCESSIBLE, executable) into
    // the current (kernel) CR3.  SMEP/SMAP key off the PTE U/S bit regardless
    // of which CR3 is active, so a user page in the kernel address space is a
    // valid target.
    let frame = match crate::mm::frame_allocator::allocate_frame() {
        Some(f) => f,
        None => {
            log::warn!("[smep-smap-test] out of frames — skipping");
            return;
        }
    };
    let leaf_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    let vaddr = VirtAddr::new(SCRATCH_UVADDR);
    if let Err(e) =
        unsafe { crate::mm::paging::map_current_user_page_locked(vaddr, frame, leaf_flags) }
    {
        log::warn!("[smep-smap-test] map scratch page failed: {e} — skipping");
        return;
    }

    // Write a `ret` (0xC3) at offset 0 and a marker byte at offset 1, through
    // the physmap (supervisor mapping — never faults under SMAP).
    let phys_off = crate::mm::phys_offset();
    let frame_kvirt = (phys_off + frame.start_address().as_u64()) as *mut u8;
    const MARKER: u8 = 0x5A;
    unsafe {
        frame_kvirt.write(0xC3); // ret
        frame_kvirt.add(1).write(MARKER);
    }

    let mut ok = true;

    if smap {
        let faulted = unsafe { read_user_vaddr_expect_fault(SCRATCH_UVADDR + 1) };
        if !faulted {
            log::error!("[smep-smap-test] SMAP: ring-0 read of user vaddr did NOT fault");
            ok = false;
        }
        // The physmap read of the same byte must succeed (the real user-copy path).
        let via_physmap = unsafe { frame_kvirt.add(1).read() };
        if via_physmap != MARKER {
            log::error!("[smep-smap-test] SMAP: physmap read mismatch {via_physmap:#x}");
            ok = false;
        }
        if faulted && via_physmap == MARKER {
            log::info!("[smep-smap-test] SMAP: ring-0 user read faulted, physmap read OK");
        }
    }

    if smep {
        let faulted = unsafe { exec_user_vaddr_expect_fault(SCRATCH_UVADDR) };
        if !faulted {
            log::error!("[smep-smap-test] SMEP: ring-0 fetch from user page did NOT fault");
            ok = false;
        } else {
            log::info!("[smep-smap-test] SMEP: ring-0 fetch from user page faulted");
        }
    }

    // Unmap the scratch page and free the frame.
    unsafe {
        let mut mapper = crate::mm::paging::get_mapper();
        use x86_64::structures::paging::{Mapper, Page, Size4KiB};
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        if let Ok((_f, flush)) = mapper.unmap(page) {
            flush.flush();
        }
    }
    crate::mm::frame_allocator::free_frame(frame.start_address().as_u64());

    if ok {
        log::info!("SMEP_SMAP_SELFTEST:PASS");
    } else {
        log::error!("SMEP_SMAP_SELFTEST:FAIL");
    }
}
