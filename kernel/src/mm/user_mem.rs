//! Safe user-memory access primitives (P12-T005).
//!
//! Kernel syscall handlers must never dereference userspace pointers directly:
//! an unmapped-but-in-range address triggers a ring-0 page fault rather than
//! gracefully returning an error.
//!
//! These functions validate the user virtual address range via page-table
//! translation before copying, returning `Err(())` on any unmapped page.
//! They use `paging::get_mapper()` (which operates on the current CR3) to walk
//! the page tables, so callers must ensure the correct CR3 is active — i.e. the
//! target process's page table must be loaded before calling these functions.

use x86_64::{
    VirtAddr,
    structures::paging::{PageTableFlags, Translate, mapper::TranslateResult},
};

/// Maximum length (bytes) accepted for a single copy_from_user / copy_to_user
/// call. Prevents pathological syscall arguments from scanning huge ranges.
const MAX_COPY_LEN: usize = 64 * 1024; // 64 KiB

/// Copy `len` bytes from userspace virtual address `src_vaddr` into `dst`.
///
/// Validates each 4 KiB page of the source range using the page tables
/// (must be `PRESENT` and `USER_ACCESSIBLE`). Returns `Err(())` if any page
/// is unmapped, the address range is not in canonical user space, or `len`
/// exceeds `MAX_COPY_LEN`.
///
/// # Safety
///
/// The physical-memory offset (from `crate::mm::phys_offset()`) must be
/// correct and the kernel's page table walk must be coherent with the
/// currently-active CR3. On single-CPU without kernel preemption this holds
/// as long as `mm::init` has run.
pub fn copy_from_user(dst: &mut [u8], src_vaddr: u64) -> Result<(), ()> {
    let len = dst.len();
    if len == 0 {
        return Ok(());
    }
    if len > MAX_COPY_LEN {
        return Err(());
    }
    // Reject non-canonical or kernel-space pointers.
    let src_end = src_vaddr.checked_add(len as u64).ok_or(())?;
    if src_vaddr < 0x1000 || src_end > 0x0000_8000_0000_0000u64 {
        return Err(());
    }

    let phys_off = crate::mm::phys_offset();
    let mapper = unsafe { crate::mm::paging::get_mapper() };

    let mut copied = 0usize;
    let mut vaddr = src_vaddr;

    while copied < len {
        let page_offset = (vaddr & 0xFFF) as usize;
        let page_base = vaddr & !0xFFF;
        let avail = (0x1000 - page_offset).min(len - copied);

        // Translate the page and validate flags. If the page is not present,
        // try to demand-fault it (Phase 36: lazy mmap pages).
        let phys = match mapper.translate_addr(VirtAddr::new(page_base)) {
            Some(p) => p,
            None => {
                if !try_demand_fault(page_base) {
                    return Err(());
                }
                // Re-acquire mapper after page table modification.
                let mapper = unsafe { crate::mm::paging::get_mapper() };
                mapper.translate_addr(VirtAddr::new(page_base)).ok_or(())?
            }
        };

        // Verify USER_ACCESSIBLE via page-table walk.
        let mapper = unsafe { crate::mm::paging::get_mapper() };
        if !is_user_accessible(&mapper, VirtAddr::new(page_base)) {
            return Err(());
        }

        let frame_virt = phys_off + phys.as_u64() + page_offset as u64;
        // SAFETY: frame_virt is a kernel virtual address for a mapped frame.
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame_virt as *const u8,
                dst[copied..].as_mut_ptr(),
                avail,
            );
        }

        copied += avail;
        vaddr += avail as u64;
    }

    Ok(())
}

/// Copy `src` bytes into userspace virtual address `dst_vaddr`.
///
/// Same validation rules as `copy_from_user`; additionally requires pages to
/// be `WRITABLE`. If a page is a CoW (copy-on-write) page (present, user-
/// accessible, BIT_9 set, but not writable), the CoW fault is resolved
/// in-place before copying.
pub fn copy_to_user(dst_vaddr: u64, src: &[u8]) -> Result<(), ()> {
    let len = src.len();
    if len == 0 {
        return Ok(());
    }
    if len > MAX_COPY_LEN {
        return Err(());
    }
    let dst_end = dst_vaddr.checked_add(len as u64).ok_or(())?;
    if dst_vaddr < 0x1000 || dst_end > 0x0000_8000_0000_0000u64 {
        return Err(());
    }

    let phys_off = crate::mm::phys_offset();

    let mut copied = 0usize;
    let mut vaddr = dst_vaddr;

    while copied < len {
        let page_offset = (vaddr & 0xFFF) as usize;
        let page_base = vaddr & !0xFFF;
        let avail = (0x1000 - page_offset).min(len - copied);

        // Re-acquire mapper each iteration because CoW resolution
        // modifies the page tables (invalidating the mapper's view).
        let mapper = unsafe { crate::mm::paging::get_mapper() };

        if !is_user_writable(&mapper, VirtAddr::new(page_base)) {
            // Phase 36: try demand-faulting if the page is not present at all.
            // Only demand-fault for writable VMAs — read-only VMAs should fail
            // with EFAULT to avoid allocating frames that can never be written.
            if mapper.translate_addr(VirtAddr::new(page_base)).is_none() {
                if try_demand_fault_writable(page_base) {
                    // Page now exists — re-check writability on next iteration.
                    continue;
                }
                return Err(());
            }
            // Check for CoW page: present + user-accessible + BIT_9 marker.
            if is_cow_page(&mapper, VirtAddr::new(page_base)) {
                if !crate::arch::x86_64::interrupts::resolve_cow_fault(page_base) {
                    log::warn!("[copy_to_user] OOM resolving CoW at {:#x}", page_base);
                    return Err(()); // OOM — callers return EFAULT (no ENOMEM path yet)
                }
                // Page is now writable — re-translate after CoW resolution.
                let mapper = unsafe { crate::mm::paging::get_mapper() };
                let phys = mapper.translate_addr(VirtAddr::new(page_base)).ok_or(())?;
                let frame_virt = phys_off + phys.as_u64() + page_offset as u64;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src[copied..].as_ptr(),
                        frame_virt as *mut u8,
                        avail,
                    );
                }
                copied += avail;
                vaddr += avail as u64;
                continue;
            }
            return Err(());
        }

        let phys = mapper.translate_addr(VirtAddr::new(page_base)).ok_or(())?;
        let frame_virt = phys_off + phys.as_u64() + page_offset as u64;
        // SAFETY: frame_virt is a kernel virtual address for a mapped writable frame.
        unsafe {
            core::ptr::copy_nonoverlapping(src[copied..].as_ptr(), frame_virt as *mut u8, avail);
        }

        copied += avail;
        vaddr += avail as u64;
    }

    Ok(())
}

/// Like [`try_demand_fault`] but also requires the VMA to be writable.
/// Used by `copy_to_user` to avoid allocating frames for read-only VMAs
/// that would immediately fail the writability check.
fn try_demand_fault_writable(page_base: u64) -> bool {
    let pid = crate::process::current_pid();
    let vma_prot = {
        let table = crate::process::PROCESS_TABLE.lock();
        table
            .find(pid)
            .and_then(|p| p.find_vma(page_base))
            .map(|m| m.prot)
    };
    const PROT_WRITE: u64 = 0x2;
    if let Some(prot) = vma_prot {
        if prot & PROT_WRITE == 0 {
            return false; // VMA is not writable — fail with EFAULT.
        }
        return crate::arch::x86_64::interrupts::demand_map_user_page_from_kernel(page_base, prot);
    }
    false
}

/// Phase 36: demand-fault a user page if it is in a valid VMA but not yet
/// present in the page table. Called from `copy_from_user` when the page
/// table walk finds no mapping.
///
/// Returns `true` if the page was successfully demand-mapped; `false` if the
/// address is not in any VMA or allocation failed.
fn try_demand_fault(page_base: u64) -> bool {
    let pid = crate::process::current_pid();
    let vma_prot = {
        let table = crate::process::PROCESS_TABLE.lock();
        table
            .find(pid)
            .and_then(|p| p.find_vma(page_base))
            .map(|m| m.prot)
    };
    if let Some(prot) = vma_prot {
        // Never demand-map PROT_NONE pages — they are guard pages that must
        // remain inaccessible.
        const PROT_READ: u64 = 0x1;
        const PROT_WRITE: u64 = 0x2;
        const PROT_EXEC: u64 = 0x4;
        if prot & (PROT_READ | PROT_WRITE | PROT_EXEC) == 0 {
            return false;
        }
        return crate::arch::x86_64::interrupts::demand_map_user_page_from_kernel(page_base, prot);
    }
    false
}

/// Check whether the page at `vaddr` is a CoW page (present, user-accessible,
/// BIT_9 marker set, but not writable).
fn is_cow_page(mapper: &x86_64::structures::paging::OffsetPageTable<'_>, vaddr: VirtAddr) -> bool {
    match mapper.translate(vaddr) {
        TranslateResult::Mapped { flags, .. } => {
            flags.contains(
                PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::BIT_9,
            ) && !flags.contains(PageTableFlags::WRITABLE)
        }
        _ => false,
    }
}

/// Check whether the page at `vaddr` is mapped USER_ACCESSIBLE.
fn is_user_accessible(
    mapper: &x86_64::structures::paging::OffsetPageTable<'_>,
    vaddr: VirtAddr,
) -> bool {
    match mapper.translate(vaddr) {
        TranslateResult::Mapped { flags, .. } => {
            flags.contains(PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE)
        }
        _ => false,
    }
}

/// Check whether the page at `vaddr` is USER_ACCESSIBLE and WRITABLE.
fn is_user_writable(
    mapper: &x86_64::structures::paging::OffsetPageTable<'_>,
    vaddr: VirtAddr,
) -> bool {
    match mapper.translate(vaddr) {
        TranslateResult::Mapped { flags, .. } => flags.contains(
            PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE,
        ),
        _ => false,
    }
}
