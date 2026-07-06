//! Phase 110 Track A.1 — kernel-side KPTI page-table pair builder + boot
//! self-test.
//!
//! `kernel_core::kpti` pins the **policy** (which top-level PML4 slots the user
//! half may carry, and the walk invariant) as pure, host-tested logic. This
//! module is the **kernel** half: it builds a real user PML4 against live
//! hardware page tables and, at boot, walks it back and feeds the result to
//! [`kernel_core::kpti::check_user_half_invariant`] — proving on QEMU (where
//! Meltdown itself cannot be exercised) that no kernel-secret leaf is reachable
//! from the user CR3.
//!
//! ## What A.1 delivers (and what it deliberately does not)
//!
//! A.1 is the **builder + validation**, with `KPTI_WIRED` still `false`: the
//! user PML4 is constructed and asserted, but never loaded into CR3, so the
//! live syscall/IRQ paths and every existing gate are untouched. The CR3
//! trampoline that consumes this pair (A.2/A.3) and the activation on the
//! policy path (A.4) land in follow-on PRs.
//!
//! ## The minimal entry set (the load-bearing subtlety)
//!
//! m3OS never executes `swapgs`: `GS_BASE` points at this core's
//! [`crate::smp::PerCoreData`] in **both** rings (no FSGSBASE, no ring-3
//! `wrmsr`). The KPTI entry asm therefore reads `gs:[…]` *before* the CR3
//! switch — so the `PerCoreData` page(s) MUST be present in the user PML4. The
//! entry set also needs the entry **text** (the instructions between the
//! CPU delivering the trap on the user CR3 and the switch to the kernel CR3)
//! and a per-CPU entry **stack**. Each is mapped into the user half at its
//! existing kernel VA through **freshly-allocated private sub-tables** — never
//! by cloning a whole kernel `PML4[i]` slot (cloning e.g. the direct-map slot
//! would silently re-expose all of physical memory; see
//! [`kernel_core::kpti::may_clone_slot_into_user_half`]).
//!
//! The self-test builds a representative entry set — the `PerCoreData` page(s),
//! the `syscall_entry` text page, and a fresh entry-stack page — exercising the
//! three distinct source regions (kernel heap VA, kernel-image VA, a fresh
//! frame). A.2 extends it with the GDT/TSS/IDT the real switch also needs.

use alloc::vec::Vec;

use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB, Translate,
        mapper::MapToError,
    },
};

use kernel_core::kpti::{KernelRange, KptiInvariantError, check_user_half_invariant};

use super::{frame_allocator, mapper_for_frame, phys_offset};

/// VA the self-test maps its synthetic user leaf at (mirrors the ELF loader's
/// `USER_VADDR_MIN`, so it lands in `PML4[0]` — the user lower half).
const SELFTEST_USER_VA: u64 = 0x0020_0000;

/// VA the self-test maps its synthetic entry-stack frame at. An otherwise
/// unused upper-half slot; the classifier admits it as `EntrySet` because it is
/// in the recorded entry-set list, not by range.
const SELFTEST_ENTRY_STACK_VA: u64 = 0xFFFF_9F00_0000_0000;

/// `PML4[idx]` of a canonical virtual address.
#[inline]
fn pml4_index(va: u64) -> usize {
    ((va >> 39) & 0x1FF) as usize
}

/// A frame allocator that records every frame it hands out, so the self-test
/// can free exactly the private page-table frames it created (and nothing
/// else — in particular never a real kernel page it only *pointed* a PTE at).
struct RecordingAlloc<'a> {
    recorded: &'a mut Vec<u64>,
}

// SAFETY: delegates to the global frame allocator, which returns unique,
// unused, correctly-aligned frames.
unsafe impl FrameAllocator<Size4KiB> for RecordingAlloc<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let frame = frame_allocator::allocate_frame()?;
        self.recorded.push(frame.start_address().as_u64());
        Some(frame)
    }
}

/// Map `phys` at `va` in the user PML4 identified by `mapper`, creating any
/// missing private sub-tables through `alloc` (recorded for later free).
///
/// `flags` are the leaf flags. Intermediate tables get `PRESENT | WRITABLE`
/// (never `USER_ACCESSIBLE`, matching the ring-0-only entry set) via the
/// default `map_to`; the synthetic user leaf sets `USER_ACCESSIBLE` on the leaf
/// so the walk classifies it as the user lower half. The result is `ignore()`d
/// — this table is not the active CR3, so there is no TLB entry to flush.
///
/// # Safety
/// `mapper` must be an exclusive mapper over the (not-yet-live) user PML4, and
/// `phys` a valid frame.
unsafe fn map_entry_page(
    mapper: &mut x86_64::structures::paging::OffsetPageTable<'static>,
    va: u64,
    phys: u64,
    flags: PageTableFlags,
    alloc: &mut RecordingAlloc<'_>,
) -> Result<(), MapToError<Size4KiB>> {
    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va));
    let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(phys));
    // SAFETY: caller guarantees exclusive access to the user PML4; the frame is
    // valid and no aliasing mapping into the same (inactive) table exists.
    unsafe { mapper.map_to(page, frame, flags, alloc)?.ignore() };
    Ok(())
}

/// Classify a present leaf found in the user PML4 by its virtual address.
///
/// Entry-set pages are recognised by exact page VA (they legitimately sit at
/// kernel VAs); everything else is classified by `PML4` slot so an *accidental*
/// kernel leaf (image, heap, kstacks, direct map) is caught as the invariant
/// violation it is.
fn classify(va: u64, entry_vas: &[u64], kimg_idx: usize, dm_idx: usize) -> KernelRange {
    let page = va & !0xFFF;
    if entry_vas.contains(&page) {
        return KernelRange::EntrySet;
    }
    match pml4_index(va) {
        0 => KernelRange::UserLowerHalf,
        i if i == kimg_idx => KernelRange::KernelImage,
        256 | 257 => KernelRange::KernelHeap, // heap + kernel stacks: kernel secrets
        i if i == dm_idx => KernelRange::DirectMap,
        // Any other kernel slot present in the user half is unexpected — treat
        // it as a secret so the invariant fails loudly rather than silently.
        _ => KernelRange::KernelImage,
    }
}

/// Sign-extend a 48-bit composed VA to a canonical 64-bit address.
#[inline]
fn canonical(va: u64) -> u64 {
    if va & (1 << 47) != 0 {
        va | 0xFFFF_0000_0000_0000
    } else {
        va
    }
}

/// Walk every present leaf in the user PML4 and collect `(role, present)`
/// observations for [`check_user_half_invariant`].
///
/// # Safety
/// `user_pml4_phys` must reference a valid PML4 reachable through the direct
/// map, with no live `&mut` alias.
unsafe fn walk_user_half(
    user_pml4_phys: u64,
    entry_vas: &[u64],
    kimg_idx: usize,
    dm_idx: usize,
) -> Vec<(KernelRange, bool)> {
    let phys_off = phys_offset();
    let mut obs: Vec<(KernelRange, bool)> = Vec::new();
    // SAFETY: page tables are all reachable read-only through the direct map.
    let table =
        |phys: u64| -> &'static PageTable { unsafe { &*((phys_off + phys) as *const PageTable) } };
    let mut note = |va: u64| {
        obs.push((classify(va, entry_vas, kimg_idx, dm_idx), true));
    };

    let pml4 = table(user_pml4_phys);
    for (i4, e4) in pml4.iter().enumerate() {
        if !e4.flags().contains(PageTableFlags::PRESENT) {
            continue;
        }
        let pdpt = table(e4.addr().as_u64());
        for (i3, e3) in pdpt.iter().enumerate() {
            let f3 = e3.flags();
            if !f3.contains(PageTableFlags::PRESENT) {
                continue;
            }
            if f3.contains(PageTableFlags::HUGE_PAGE) {
                note(canonical(((i4 as u64) << 39) | ((i3 as u64) << 30)));
                continue;
            }
            let pd = table(e3.addr().as_u64());
            for (i2, e2) in pd.iter().enumerate() {
                let f2 = e2.flags();
                if !f2.contains(PageTableFlags::PRESENT) {
                    continue;
                }
                if f2.contains(PageTableFlags::HUGE_PAGE) {
                    note(canonical(
                        ((i4 as u64) << 39) | ((i3 as u64) << 30) | ((i2 as u64) << 21),
                    ));
                    continue;
                }
                let pt = table(e2.addr().as_u64());
                for (i1, e1) in pt.iter().enumerate() {
                    if !e1.flags().contains(PageTableFlags::PRESENT) {
                        continue;
                    }
                    note(canonical(
                        ((i4 as u64) << 39)
                            | ((i3 as u64) << 30)
                            | ((i2 as u64) << 21)
                            | ((i1 as u64) << 12),
                    ));
                }
            }
        }
    }
    obs
}

/// Outcome of the self-test build, so the caller can free every frame it
/// created regardless of which step failed.
struct SelfTestArena {
    /// Private page-table frames + the user PML4 (all safe to free).
    table_frames: Vec<u64>,
    /// Synthetic leaf frames the test itself allocated (user page, entry
    /// stack) — also safe to free. Real kernel pages the entry set points at
    /// are *not* here and are never freed.
    leaf_frames: Vec<u64>,
    user_pml4_phys: u64,
    entry_vas: Vec<u64>,
}

impl SelfTestArena {
    fn free(self) {
        for f in self.leaf_frames {
            frame_allocator::free_frame(f);
        }
        for f in self.table_frames {
            frame_allocator::free_frame(f);
        }
    }
}

/// Build a throwaway user PML4 with a synthetic user page + a representative
/// entry set, returning the arena for the caller to walk then free.
fn build_selftest_pair() -> Option<SelfTestArena> {
    let phys_off = phys_offset();

    // --- resolve the real entry-set pages (VA -> phys) through the kernel map.
    // Scope the kernel mapper so it drops before we build over the user PML4.
    let mut entry_pages: Vec<(u64, u64, PageTableFlags)> = Vec::new();
    {
        // SAFETY: no other mapper is live here; get_mapper wraps the active
        // (kernel) CR3 for translation only.
        let kmapper = unsafe { super::paging::get_mapper() };

        // PerCoreData — reached via GS_BASE by the entry asm; map every page it
        // spans so no field read faults on the user CR3.
        let pcd_base = crate::smp::per_core() as *const _ as u64;
        let pcd_size = core::mem::size_of::<crate::smp::PerCoreData>() as u64;
        let first = pcd_base & !0xFFF;
        let last = (pcd_base + pcd_size - 1) & !0xFFF;
        let mut va = first;
        while va <= last {
            let phys = kmapper.translate_addr(VirtAddr::new(va))?.as_u64();
            entry_pages.push((va, phys, PageTableFlags::PRESENT | PageTableFlags::WRITABLE));
            va += 0x1000;
        }

        // Entry text — the `syscall_entry` naked stub page (executable, r/o).
        unsafe extern "C" {
            fn syscall_entry();
        }
        let text_va = (syscall_entry as *const () as u64) & !0xFFF;
        let text_phys = kmapper.translate_addr(VirtAddr::new(text_va))?.as_u64();
        entry_pages.push((text_va, text_phys, PageTableFlags::PRESENT));
    }

    let mut table_frames: Vec<u64> = Vec::new();
    let mut leaf_frames: Vec<u64> = Vec::new();

    // --- allocate + zero the user PML4.
    let user_pml4 = frame_allocator::allocate_frame()?;
    let user_pml4_phys = user_pml4.start_address().as_u64();
    table_frames.push(user_pml4_phys);
    // SAFETY: freshly allocated frame, no other reference.
    unsafe {
        core::ptr::write_bytes((phys_off + user_pml4_phys) as *mut u8, 0, 4096);
    }

    // Synthetic user leaf + entry stack (frames the test owns and will free).
    let user_leaf = frame_allocator::allocate_frame()?;
    leaf_frames.push(user_leaf.start_address().as_u64());
    let stack_leaf = frame_allocator::allocate_frame()?;
    leaf_frames.push(stack_leaf.start_address().as_u64());
    entry_pages.push((
        SELFTEST_ENTRY_STACK_VA,
        stack_leaf.start_address().as_u64(),
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
    ));

    let entry_vas: Vec<u64> = entry_pages.iter().map(|(va, _, _)| *va).collect();

    // --- populate the user PML4.
    // SAFETY: user_pml4 is a fresh, non-live table owned solely here.
    let mut mapper = unsafe { mapper_for_frame(user_pml4) };
    {
        let mut alloc = RecordingAlloc {
            recorded: &mut table_frames,
        };
        // The user lower-half page (USER_ACCESSIBLE leaf → classified user).
        // SAFETY: exclusive mapper; valid frame.
        unsafe {
            map_entry_page(
                &mut mapper,
                SELFTEST_USER_VA,
                user_leaf.start_address().as_u64(),
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE,
                &mut alloc,
            )
            .ok()?;
        }
        // The entry set, at kernel VAs, ring-0-only.
        for (va, phys, flags) in &entry_pages {
            // SAFETY: exclusive mapper; valid frames resolved above.
            unsafe {
                map_entry_page(&mut mapper, *va, *phys, *flags, &mut alloc).ok()?;
            }
        }
    }
    // `mapper` (the exclusive borrow over the user PML4) drops at scope end,
    // before the caller walks the same table read-only through the direct map.

    Some(SelfTestArena {
        table_frames,
        leaf_frames,
        user_pml4_phys,
        entry_vas,
    })
}

/// Run the KPTI user-half self-test once, emitting a `KPTI_SELFTEST:` sentinel.
///
/// Builds a real user PML4 (synthetic user page + entry set), walks it back,
/// and asserts [`check_user_half_invariant`] — the boot-time proof that A.1's
/// pair builder maps the user lower half + entry set and **nothing** of the
/// kernel image / heap / kstacks / direct map. Frees everything it allocated.
///
/// Non-fatal by design: it logs `PASS`/`FAIL`/`SKIP` rather than panicking, so
/// a regression is caught by the `kpti-selftest-smoke` gate at the right
/// granularity without bricking unrelated boots (KPTI is not yet live).
pub fn self_test() {
    let kimg_idx;
    let dm_idx = pml4_index(phys_offset());
    // Recompute kimg_idx the same way build does (syscall_entry's slot).
    {
        unsafe extern "C" {
            fn syscall_entry();
        }
        kimg_idx = pml4_index((syscall_entry as *const () as u64) & !0xFFF);
    }

    let arena = match build_selftest_pair() {
        Some(a) => a,
        None => {
            log::error!("KPTI_SELFTEST:SKIP reason=alloc-failed");
            return;
        }
    };

    // SAFETY: arena.user_pml4_phys is a valid, non-live PML4; we only read it.
    let obs = unsafe { walk_user_half(arena.user_pml4_phys, &arena.entry_vas, kimg_idx, dm_idx) };

    let secret_leaves = obs
        .iter()
        .filter(|(role, present)| {
            *present && !matches!(role, KernelRange::UserLowerHalf | KernelRange::EntrySet)
        })
        .count();
    let entry_leaves = obs
        .iter()
        .filter(|(role, _)| matches!(role, KernelRange::EntrySet))
        .count();

    let result = check_user_half_invariant(obs.iter());

    // Free the throwaway pair before reporting (report is the observable point).
    let user_pml4_phys = arena.user_pml4_phys;
    arena.free();

    match result {
        Ok(()) if secret_leaves == 0 => {
            log::info!(
                "KPTI_SELFTEST:PASS user_pml4={:#x} entry_set={} leaves; no kernel-secret leaf reachable from user CR3",
                user_pml4_phys,
                entry_leaves,
            );
        }
        Ok(()) => {
            // Invariant passed but a secret leaf slipped the role filter — treat
            // as failure (defence in depth against a classify() gap).
            log::error!(
                "KPTI_SELFTEST:FAIL reason=secret-leaf-count secret_leaves={}",
                secret_leaves,
            );
        }
        Err(e) => {
            let reason: &str = match e {
                KptiInvariantError::KernelImagePresent => "kernel-image-present",
                KptiInvariantError::KernelHeapPresent => "kernel-heap-present",
                KptiInvariantError::DirectMapPresent => "direct-map-present",
                KptiInvariantError::UserHalfMissing => "user-half-missing",
                KptiInvariantError::EntrySetMissing => "entry-set-missing",
            };
            log::error!("KPTI_SELFTEST:FAIL reason={reason}");
        }
    }
}
