//! Page Attribute Table (PAT) programming — enables a true Write-Combining
//! memory type, primarily for the framebuffer.
//!
//! The x86 default PAT has **no WC entry**: the eight slots decode as
//! `[WB, WT, UC-, UC, WB, WT, UC-, UC]`. The only kernel mappings that select
//! PAT index 2 (PCD set / PWT clear = [`PageTableFlags::NO_CACHE`] alone) are
//! the prefetchable-BAR "WC approximation" in [`crate::pci::bar`] (whose comment
//! flags it as awaiting "dedicated MMIO PAT slots") and — once
//! [`set_range_write_combining`] runs — the framebuffer. We therefore reprogram
//! **index 2 from UC- to WC**, which (a) costs nothing for any existing strong-UC
//! mapping (those use index 3, `NO_CACHE | WRITE_THROUGH`) and (b) upgrades the
//! framebuffer + prefetchable BARs to real write-combining. Index 2 is selected
//! by PCD (bit 4) set, PWT (bit 3) clear, **and the PAT bit clear** — otherwise
//! the type decodes as index 6 (UC-). The PAT bit's position is leaf-dependent
//! (bit 7 in a 4 KiB PTE, bit 12 in a 2 MiB PDE): [`set_range_write_combining`]
//! clears it explicitly on 4 KiB leaves (the framebuffer case) and relies on it
//! already being 0 on 2 MiB leaves, which `update_flags` cannot reach (there bit
//! 12 is part of the frame address).
//!
//! PAT is **per-core**: the Intel SDM requires every logical CPU mapping a shared
//! page to agree on its memory type, so [`init`] runs on the BSP and on every AP
//! (`smp::boot::ap_entry`) before any WC mapping is touched.
//!
//! This is a bare-metal performance fix: QEMU's framebuffer is RAM-backed and
//! fast regardless, but on real hardware the bootloader's uncacheable FB mapping
//! makes every pixel write a bus transaction (~0.2 s per scrolled line). WC lets
//! the CPU batch them into burst writes.

use x86_64::registers::model_specific::Msr;

/// `IA32_PAT` MSR number.
const IA32_PAT: u32 = 0x277;

// Memory-type encodings (Intel SDM Vol 3A §11.12.3):
//   UC=0x00  WC=0x01  WT=0x04  WP=0x05  WB=0x06  UC-=0x07
//
// Default PAT (byte index 0..=7, little-endian):
//   [WB(06), WT(04), UC-(07), UC(00), WB(06), WT(04), UC-(07), UC(00)]
//     = 0x0007_0406_0007_0406
// Reprogrammed — index 2 (the slot PCD-alone selects) set to WC(01):
//   [WB(06), WT(04),  WC(01), UC(00), WB(06), WT(04), UC-(07), UC(00)]
//     = 0x0007_0406_0001_0406
const PAT_WITH_WC: u64 = 0x0007_0406_0001_0406;

/// Program `IA32_PAT` so PAT index 2 ([`PageTableFlags::NO_CACHE`] alone) is
/// Write-Combining. Per-core; idempotent.
pub fn init() {
    // SAFETY: IA32_PAT is architectural and `PAT_WITH_WC` is a valid memory-type
    // table. Called at BSP early-init and at AP bring-up *before* any WC mapping
    // exists on that core, so no stale TLB entry can carry a mismatched type.
    unsafe {
        Msr::new(IA32_PAT).write(PAT_WITH_WC);
    }
}

/// Remap an already-present kernel virtual range to Write-Combining by setting
/// `NO_CACHE` (PCD → PAT index 2, programmed WC by [`init`]) and clearing
/// `WRITE_THROUGH` on each leaf, preserving every other flag. Handles 4 KiB and
/// 2 MiB leaves. Returns the number of leaves updated.
///
/// # Safety
/// [`init`] must have run on this core first. The range must be a kernel mapping
/// for which write-combining is sound (the framebuffer / a prefetchable BAR).
/// WC is weakly ordered — callers needing write visibility must `sfence`.
pub unsafe fn set_range_write_combining(virt_base: usize, size: usize) -> usize {
    use x86_64::VirtAddr;
    use x86_64::structures::paging::mapper::{MappedFrame, TranslateResult};
    use x86_64::structures::paging::{Mapper, Page, PageTableFlags, Size2MiB, Size4KiB, Translate};

    if size == 0 {
        return 0;
    }
    // SAFETY: the caller runs during single-threaded BSP init; no other mapper
    // over the active tables is alive in this scope.
    let mut mapper = unsafe { crate::mm::paging::get_mapper() };

    let end = virt_base.saturating_add(size);
    let mut addr = virt_base & !0xFFF;
    let mut updated = 0usize;
    // Bound the walk to the range's own 4 KiB-page count (+slack) so a bad size
    // can't spin, while never truncating a large (e.g. 4K/8K) framebuffer the way
    // a fixed cap would — `kernel_main` passes the real framebuffer `byte_len`.
    let max_iters = (size >> 12) + 16;
    let mut iters = 0;
    while addr < end && iters < max_iters {
        iters += 1;
        let vaddr = VirtAddr::new(addr as u64);
        match mapper.translate(vaddr) {
            TranslateResult::Mapped { frame, flags, .. } => {
                // Select PAT index 2 (WC) = PAT 0 / PCD 1 / PWT 0. Set PCD, clear
                // PWT — and the PAT bit must be 0 too, or the index becomes 6
                // (UC-) and the WC upgrade silently no-ops. The PAT bit's
                // position is leaf-size dependent, so it is handled per arm.
                // Every other flag (PRESENT, WRITABLE, NO_EXECUTE, GLOBAL) is
                // preserved so page semantics are unchanged.
                let base = (flags | PageTableFlags::NO_CACHE) & !PageTableFlags::WRITE_THROUGH;
                match frame {
                    MappedFrame::Size4KiB(_) => {
                        // In a 4 KiB PTE the PAT bit is bit 7, which the x86_64
                        // crate models as HUGE_PAGE. Clear it so the type is
                        // unambiguously index 2 even if the prior mapping had
                        // PAT=1 — a 4 KiB leaf never legitimately has PS set, so
                        // this is always safe and reaches the bit via set_flags
                        // (bit 7 is outside the bits-12..51 address mask).
                        let wc = base & !PageTableFlags::HUGE_PAGE;
                        let page = Page::<Size4KiB>::containing_address(vaddr);
                        if let Ok(f) = unsafe { mapper.update_flags(page, wc) } {
                            f.flush();
                            updated += 1;
                        }
                        addr += 0x1000;
                    }
                    MappedFrame::Size2MiB(_) => {
                        // In a 2 MiB PDE the PAT bit is bit 12, which sits inside
                        // the crate's bits-12..51 frame-address mask, so
                        // update_flags() preserves it and cannot clear it (and
                        // HUGE_PAGE/PS must stay set). The only callers — the
                        // bootloader framebuffer (4 KiB in practice) and
                        // prefetchable BARs — map with PAT=0, so index 2 holds;
                        // a 2 MiB leaf arriving with PAT=1 would need a raw PDE
                        // rewrite outside the Mapper API to correct.
                        let wc = base;
                        let page = Page::<Size2MiB>::containing_address(vaddr);
                        if let Ok(f) = unsafe { mapper.update_flags(page, wc) } {
                            f.flush();
                            updated += 1;
                        }
                        addr = (addr & !0x1F_FFFF) + 0x20_0000;
                    }
                    MappedFrame::Size1GiB(_) => {
                        // Framebuffers are never 1 GiB-mapped; step past the page.
                        addr = (addr & !0x3FFF_FFFF) + 0x4000_0000;
                    }
                }
            }
            _ => {
                addr += 0x1000;
            }
        }
    }
    updated
}
