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
//! ## What this delivers (and what it deliberately does not)
//!
//! This module is the **builder + validation** plane; the *live* consumers
//! landed across A.2–A.4. A.2 added the syscall CR3 trampoline
//! (`syscall_entry_kpti`, LSTAR-selected). A.3a grew the validated entry set
//! to the interrupt-delivery structures (GDT/IDT/TSS) + the reachability
//! round-trip. A.3b factored the reusable per-process builder
//! ([`build_user_half`] / [`free_user_half`]) and the naked entry/exit stubs
//! that consume it. **A.4 activated the pair**: `KPTI_WIRED = true`, so on
//! Meltdown-susceptible silicon (`auto`, incl. every QEMU TCG boot) each
//! process's user half is built at address-space birth, published per-core at
//! dispatch, and loaded as the ring-3 CR3 by the entry/exit stubs. The boot
//! self-test below still builds a throwaway pair (never loaded) so its
//! invariant walk stays independent of the live tables.
//!
//! ## The minimal entry set (the load-bearing subtlety)
//!
//! m3OS never executes `swapgs`: `GS_BASE` points at this core's
//! [`crate::smp::PerCoreData`] in **both** rings (no FSGSBASE, no ring-3
//! `wrmsr`). The KPTI entry asm therefore reads `gs:[…]` *before* the CR3
//! switch — so the `PerCoreData` page(s) MUST be present in the user PML4. Each
//! entry-set page is mapped into the user half at its existing kernel VA through
//! **freshly-allocated private sub-tables** — never by cloning a whole kernel
//! `PML4[i]` slot (cloning e.g. the direct-map slot would silently re-expose all
//! of physical memory; see [`kernel_core::kpti::may_clone_slot_into_user_half`]).
//!
//! The entry set ([`collect_entry_pages`]) is, per **online core** (a process
//! may run on any core): its `PerCoreData`, GDT, TSS, the top page of each
//! of its NMI + `#DF` IST stacks, and the top page of its KPTI trampoline
//! stack (Phase 110 hardening — where TSS.RSP0 points while KPTI is active,
//! so ring-3 interrupt frames land there instead of on the task kstack); plus
//! the shared page-aligned `.text.kpti_entry`
//! section (both SYSCALL stubs + body + tail, A.2), the `.text.kpti_irq_entry`
//! section (A.3b: the naked IRQ/exception entry stubs — the CPU begins executing
//! them on the user CR3 when an interrupt fires while ring 3 runs), and the IDT.
//! The CPU reads GDT/IDT/TSS and switches to the IST top through the *active*
//! paging when delivering a ring-3 → ring-0 interrupt, so all must be user-mapped
//! or delivery itself triple-faults. [`build_user_half`] adds the two per-process
//! bits: the shared user-mapping slots (`kernel_core::kpti::USER_PML4_SLOTS` —
//! `PML4[0]` for image/brk/mmap and `PML4[255]` for the stack, the same
//! sub-table frames the kernel half points at, so user mappings stay in sync)
//! and this process's **kstack top page** — where the CPU pushes the interrupt
//! frame on the user CR3 before the stub switches to the kernel CR3 (only the
//! top page is exposed; the whole kstack reappears once on the kernel half).
//!
//! The self-test builds a real user half via [`build_user_half`] over a
//! synthetic kernel PML4, then (a) walks it and asserts no kernel-secret leaf
//! and (b) round-trip-translates every entry-set page to prove it is reachable.
//!
//! **Isolation status.** The A.3a caveat — GDT/IDT/TSS as ordinary statics
//! exposing adjacent `.data` through the entry-set mappings — is **closed**
//! (A.3b part 4): GDT/IDT/TSS, every `PerCoreData`, and the BSP entry stacks
//! are `PageIsolated` (`arch::x86_64::gdt::PageIsolated`: page-aligned,
//! page-multiple-sized, so each owns its pages exclusively — the m3OS
//! `cpu_entry_area`), and the self-test asserts their page alignment
//! (`reason=entry-struct-alignment`). Remaining accepted surface: the kstack
//! top page exposes ~4 KiB of this process's **own** kernel stack to itself —
//! small, bounded, self-only; hardening it means a dedicated per-CPU
//! trampoline stack (post-A.4 follow-up).

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
/// `USER_VADDR_MIN`, so it lands in `PML4[0]` — the image/brk/mmap user slot).
const SELFTEST_USER_VA: u64 = 0x0020_0000;

/// VA of the self-test's synthetic user **stack** leaf (one page below the ELF
/// loader's `ELF_STACK_TOP`, so it lands in `PML4[255]` — the stack user
/// slot). Proves the user half shares BOTH user-mapping slots
/// (`kernel_core::kpti::USER_PML4_SLOTS`): sharing only `PML4[0]` was the A.4
/// bring-up wedge (the first ring-3 stack access #PF-looped silently, because
/// the stack mapping existed only in the kernel half).
const SELFTEST_USER_STACK_VA: u64 = crate::mm::elf::ELF_STACK_TOP - 0x1000;

/// Leaf flags for read-execute entry-set pages (entry text): ring-0 only, no
/// `USER_ACCESSIBLE`, no `NO_EXECUTE`.
const RX: PageTableFlags = PageTableFlags::PRESENT;
/// Leaf flags for read-write entry-set pages (PerCoreData, GDT, IDT, TSS,
/// entry stacks): ring-0 only, writable, non-executable.
const RW: PageTableFlags = PageTableFlags::from_bits_truncate(
    PageTableFlags::PRESENT.bits()
        | PageTableFlags::WRITABLE.bits()
        | PageTableFlags::NO_EXECUTE.bits(),
);

/// `PML4[idx]` of a canonical virtual address.
#[inline]
fn pml4_index(va: u64) -> usize {
    ((va >> 39) & 0x1FF) as usize
}

/// Translate each 4 KiB page of the kernel VA range `[start, end)` through the
/// live kernel mapper and append `(va, phys, flags)` to `out`. The range is
/// rounded down/up to page boundaries. Returns `None` if any page is
/// unmapped (which for an entry-set structure is a build-time bug).
fn push_kernel_range(
    kmapper: &x86_64::structures::paging::OffsetPageTable<'static>,
    out: &mut Vec<(u64, u64, PageTableFlags)>,
    start: u64,
    end: u64,
    flags: PageTableFlags,
) -> Option<()> {
    let mut va = start & !0xFFF;
    let last = (end - 1) & !0xFFF;
    while va <= last {
        let phys = kmapper.translate_addr(VirtAddr::new(va))?.as_u64();
        // De-dup: adjacent structures (e.g. GDT + TSS) can share a page.
        if !out.iter().any(|(v, _, _)| *v == va) {
            out.push((va, phys, flags));
        }
        va += 0x1000;
    }
    Some(())
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
        // The user-mapping slots (image/brk/mmap `PML4[0]` + stack `PML4[255]`),
        // shared verbatim with the kernel half.
        i if kernel_core::kpti::is_user_pml4_slot(i) => KernelRange::UserLowerHalf,
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
    /// Frames the test itself owns: the synthetic **kernel** PML4 + its user
    /// `PML4[0]` sub-tables + the synthetic user leaf. Freed with `free_frame`.
    /// (The user-half PML4's own frames are freed via `free_user_half`.)
    owned_frames: Vec<u64>,
    user_pml4_phys: u64,
    /// Every entry-set page as `(kernel_va, expected_phys)` — used both to
    /// classify leaves as `EntrySet` during the walk and to round-trip-verify
    /// each page actually translates in the built user PML4.
    entry_pages: Vec<(u64, u64)>,
    /// The synthetic user leaves as `(user_va, expected_phys)` — one per
    /// user-mapping slot (`USER_PML4_SLOTS`), round-trip-verified in the built
    /// user half exactly like the entry set (a missing one is the stack-slot
    /// #PF-loop wedge, not a triple fault, so it needs its own check).
    user_leaf_pages: Vec<(u64, u64)>,
}

impl SelfTestArena {
    fn free(self) {
        // The user half via the production free path (frees its private
        // entry-set sub-tables + top frame, skips the shared user slots).
        // SAFETY: user_pml4_phys is a valid user half no longer loaded anywhere.
        unsafe { free_user_half(self.user_pml4_phys) };
        // Then the synthetic kernel PML4 + user chain the test owns.
        for f in self.owned_frames {
            frame_allocator::free_frame(f);
        }
    }
}

/// Build a throwaway **kernel** PML4 carrying one synthetic user leaf per
/// user-mapping slot — [`SELFTEST_USER_VA`] in `PML4[0]` and
/// [`SELFTEST_USER_STACK_VA`] in `PML4[255]` — so [`build_user_half`] has a
/// real kernel half to derive from (each shared slot then presents a
/// `UserLowerHalf` leaf, proving the builder shares the FULL
/// `USER_PML4_SLOTS` set, not just `PML4[0]`). Returns
/// `(kernel_pml4_phys, owned_frames)`; every frame in `owned_frames`
/// (PML4 + PDPT/PD/PT + leaves) is the caller's to free.
fn build_synthetic_kernel_pml4() -> Option<(u64, Vec<u64>)> {
    let phys_off = phys_offset();
    let mut owned: Vec<u64> = Vec::new();

    let pml4 = frame_allocator::allocate_frame()?;
    let pml4_phys = pml4.start_address().as_u64();
    owned.push(pml4_phys);
    // SAFETY: fresh frame, no other reference.
    unsafe {
        core::ptr::write_bytes((phys_off + pml4_phys) as *mut u8, 0, 4096);
    }

    // SAFETY: pml4 is a fresh, non-live table owned solely here.
    let mut mapper = unsafe { mapper_for_frame(pml4) };
    let mut ok = true;
    for va in [SELFTEST_USER_VA, SELFTEST_USER_STACK_VA] {
        let Some(leaf) = frame_allocator::allocate_frame() else {
            ok = false;
            break;
        };
        owned.push(leaf.start_address().as_u64());
        let mut alloc = RecordingAlloc {
            recorded: &mut owned,
        };
        // SAFETY: exclusive mapper; valid frame.
        ok = unsafe {
            map_entry_page(
                &mut mapper,
                va,
                leaf.start_address().as_u64(),
                PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE,
                &mut alloc,
            )
        }
        .is_ok();
        if !ok {
            break;
        }
    }
    if !ok {
        for f in &owned {
            frame_allocator::free_frame(*f);
        }
        return None;
    }
    Some((pml4_phys, owned))
}

/// Collect the shared entry set — the pages every user PML4 must map so a
/// ring-3 → ring-0 transition (SYSCALL or interrupt) can reach the kernel CR3
/// without faulting. Resolved (VA → phys) through the live kernel map.
///
/// Per **online** core: its `PerCoreData` (read via `gs:` before the CR3
/// switch), its GDT and its TSS (read by the CPU on interrupt delivery). Plus
/// the shared, core-independent `.text.kpti_entry` section and the IDT. A
/// process may run on any core, so every online core's structures are included.
///
/// Does **not** include the per-process bits (`PML4[0]` user PDPT, the process
/// kstack top page) — those are added by [`build_user_half`] / the self-test.
fn collect_entry_pages() -> Option<Vec<(u64, u64, PageTableFlags)>> {
    let mut out: Vec<(u64, u64, PageTableFlags)> = Vec::new();
    // SAFETY: no other mapper is live here; get_mapper wraps the active
    // (kernel) CR3 for translation only, and is dropped before any user PML4 is
    // built over the direct map.
    let kmapper = unsafe { super::paging::get_mapper() };

    // Shared: the entry text (r-x) — the instructions the trampoline runs on
    // the user CR3 before switching — and the IDT (r/w; the CPU sets accessed
    // bits) the CPU reads to deliver any interrupt.
    let (text_start, text_end) = crate::arch::x86_64::syscall::kpti_entry_text_range();
    push_kernel_range(&kmapper, &mut out, text_start, text_end, RX)?;

    // Phase 110 A.3b — the naked maskable-IRQ / IPI entry stubs
    // (`.text.kpti_irq_entry`): when KPTI is active and an IRQ fires while ring 3
    // runs, the CPU begins executing the stub on the *user* CR3, so its
    // instructions up to the `mov cr3` must be user-mapped (r-x).
    let (irq_start, irq_end) = crate::arch::x86_64::interrupts::kpti_irq_entry_range();
    push_kernel_range(&kmapper, &mut out, irq_start, irq_end, RX)?;

    // Phase 110 A.3b part 3 — the ring0→ring3 exit trampolines
    // (`.text.kpti_exit`): the instructions from each trampoline's `mov cr3`
    // (→ user CR3) through its `iretq` execute on the user half, so they must be
    // user-mapped (r-x).
    let (exit_start, exit_end) = crate::arch::x86_64::interrupts::kpti_exit_range();
    push_kernel_range(&kmapper, &mut out, exit_start, exit_end, RX)?;

    let idtr = x86_64::instructions::tables::sidt();
    let idt_base = idtr.base.as_u64();
    push_kernel_range(
        &kmapper,
        &mut out,
        idt_base,
        idt_base + u64::from(idtr.limit) + 1,
        RW,
    )?;

    // Per ALLOCATED core (not merely online, and not bounded by
    // `core_count()` — suspend shrinks that to 1 while the APs are parked):
    // PerCoreData + GDT + TSS. An allocated-but-offline core is either
    // mid-cold-boot (no processes exist yet) or S3-parked — and on resume the
    // BSP can create/exec a process BEFORE `resume_reboot_aps` brings the APs
    // back online, so keying on `is_online`/`core_count()` would build that
    // process's user half without the AP structures and triple-fault its
    // first ring-3 interrupt on that core. The addresses are stable across S3
    // (suspend keeps the allocations; `init_ap_per_core` re-inits them in
    // place), so mapping a parked core's structures is always valid; a
    // boot-failed AP's slot is nulled by `release_failed_ap` and skipped.
    for core_id in 0..crate::smp::MAX_CORES as u8 {
        let Some(pcd) = crate::smp::get_core_data(core_id) else {
            continue;
        };
        for (base, size) in pcd.entry_struct_extents() {
            push_kernel_range(&kmapper, &mut out, base, base + size, RW)?;
        }
        // A.3b — the NMI + #DF IST stack top pages. The paranoid stubs run on
        // these; the CPU pushes the trap frame onto the IST top on the user CR3
        // before the stub can switch, so each top page must be user-mapped (the
        // rest of the IST stack reappears once the stub is on the kernel CR3).
        for ist_top in pcd.ist_top_pages() {
            if ist_top == 0 {
                continue;
            }
            let top_page = (ist_top - 1) & !0xFFF;
            push_kernel_range(&kmapper, &mut out, top_page, top_page + 0x1000, RW)?;
        }
        // Phase 110 hardening — this core's KPTI trampoline stack **top page**
        // (the m3OS `cpu_entry_area` entry stack). When KPTI is active,
        // TSS.RSP0 points at `kpti_tramp_top`, so the CPU pushes every ring-3
        // interrupt frame here on the *user* CR3 (and the exit path builds its
        // `iretq` frame here before the CR3 flip). Only the top page is
        // exposed; the page below is kernel-only diagnostic spillover. Frames
        // on it carry only ring-3 register state — unlike the task-kstack top
        // page it replaces, it holds no kernel data at all.
        let tramp_top = pcd.kpti_tramp_top;
        if tramp_top != 0 {
            let top_page = (tramp_top - 1) & !0xFFF;
            push_kernel_range(&kmapper, &mut out, top_page, top_page + 0x1000, RW)?;
        }
    }

    Some(out)
}

/// Build a live per-process **user-half** PML4 for the kernel PML4 at
/// `kernel_pml4_phys`, mapping the shared entry set, this process's kstack top
/// page (`kstack_top_va`, where the CPU pushes an IRQ frame on the user CR3),
/// and — shared with the kernel half — the user-mapping slots
/// (`kernel_core::kpti::USER_PML4_SLOTS`: `PML4[0]` image/brk/mmap +
/// `PML4[255]` stack).
///
/// Returns the user PML4 physical address, or `None` on allocation failure.
/// A.4 publishes this per-core at dispatch (`smp::publish_kpti_cr3_pair`) and
/// the entry/exit stubs load it as the ring-3 CR3 when KPTI is active.
///
/// # Safety
/// `kernel_pml4_phys` must be a valid process kernel PML4 reachable through the
/// direct map, and `kstack_top_va` a mapped kernel-stack top for this process.
pub unsafe fn build_user_half(kernel_pml4_phys: u64, kstack_top_va: u64) -> Option<u64> {
    let phys_off = phys_offset();

    let mut entry_pages = collect_entry_pages()?;
    // This process's kstack top page: RSP0 points here, so the CPU pushes the
    // ring-3 → ring-0 interrupt frame onto it on the *user* CR3. Only the top
    // page is exposed (the stub switches to the kernel CR3, which maps the whole
    // kstack, before touching anything below it).
    {
        let kmapper = unsafe { super::paging::get_mapper() };
        let top_page = (kstack_top_va - 1) & !0xFFF;
        push_kernel_range(&kmapper, &mut entry_pages, top_page, top_page + 0x1000, RW)?;
    }

    // Allocate + zero the user PML4.
    let user_pml4 = frame_allocator::allocate_frame()?;
    let user_pml4_phys = user_pml4.start_address().as_u64();
    // SAFETY: freshly allocated frame, no other reference.
    unsafe {
        core::ptr::write_bytes((phys_off + user_pml4_phys) as *mut u8, 0, 4096);
    }

    // Share every user-mapping slot (kernel_core::kpti::USER_PML4_SLOTS) with
    // the kernel half so user mappings stay in sync automatically — the same
    // sub-table frames the kernel half points at, not copies. Two slots today:
    // PML4[0] (ELF image + brk + the 128 GiB anonymous-mmap region) and
    // PML4[255] (the user stack at ELF_STACK_TOP minus ASLR jitter). Sharing
    // only PML4[0] was the A.4 bring-up wedge: the first ring-3 stack access
    // #PF-looped silently — the fault resolved fine in the kernel half, so the
    // handler saw nothing to fix and the iretq re-faulted forever.
    //
    // The shared slots' sub-trees must already exist in the kernel half here
    // (an empty slot cloned now stays empty in the user half forever): true at
    // every call site — the ELF loader maps image + stack before the
    // AddressSpace (and hence the pair) is created, and fork copies the full
    // table first.
    // SAFETY: both PML4s are reachable through the direct map; we only copy
    // top-level 8-byte slots.
    unsafe {
        let kern = &*((phys_off + kernel_pml4_phys) as *const PageTable);
        let user = &mut *((phys_off + user_pml4_phys) as *mut PageTable);
        for slot in kernel_core::kpti::USER_PML4_SLOTS {
            user[slot] = kern[slot].clone();
        }
    }

    // Map the entry set through fresh private sub-tables (never cloning a whole
    // kernel PML4 slot). All frames created are tracked so a partial-failure
    // rollback (and free_user_half) frees exactly them.
    let mut sink: Vec<u64> = Vec::new();
    // SAFETY: user_pml4 is a fresh, non-live table owned solely here.
    let mut mapper = unsafe { mapper_for_frame(user_pml4) };
    let ok = {
        let mut alloc = RecordingAlloc {
            recorded: &mut sink,
        };
        entry_pages.iter().all(|(va, phys, flags)| {
            // SAFETY: exclusive mapper; valid frames resolved above.
            unsafe { map_entry_page(&mut mapper, *va, *phys, *flags, &mut alloc) }.is_ok()
        })
    };

    if !ok {
        // Roll back: free the sub-table frames created so far + the PML4.
        for f in &sink {
            frame_allocator::free_frame(*f);
        }
        frame_allocator::free_frame(user_pml4_phys);
        return None;
    }

    Some(user_pml4_phys)
}

/// Free a user-half PML4 built by [`build_user_half`]: the private entry-set
/// sub-tables and the top PML4 frame. Never frees the shared user-mapping
/// slots' sub-trees (`kernel_core::kpti::USER_PML4_SLOTS`, owned by the kernel
/// half and freed by `free_process_page_table`) nor any leaf page (real kernel
/// structures the entry set only *points* at).
///
/// # Safety
/// `user_pml4_phys` must be a user-half PML4 no longer loaded in any CR3.
pub unsafe fn free_user_half(user_pml4_phys: u64) {
    let phys_off = phys_offset();
    // SAFETY: reachable through the direct map; no live alias (not the active CR3).
    let table =
        |phys: u64| -> &'static PageTable { unsafe { &*((phys_off + phys) as *const PageTable) } };

    let pml4 = table(user_pml4_phys);
    // Every non-user slot holds private entry-set sub-tables; the user-mapping
    // slots (kernel_core::kpti::USER_PML4_SLOTS — image/mmap + stack) are
    // SHARED with the kernel half and must be skipped: their sub-table frames
    // are owned by the kernel half and freed by `free_process_page_table`.
    // Walk each present private slot and free its PDPT/PD/PT frames, never the
    // leaf pages.
    for i4 in 0usize..512 {
        if kernel_core::kpti::is_user_pml4_slot(i4) {
            continue;
        }
        let e4 = &pml4[i4];
        if !e4.flags().contains(PageTableFlags::PRESENT) {
            continue;
        }
        let pdpt_phys = e4.addr().as_u64();
        let pdpt = table(pdpt_phys);
        for e3 in pdpt.iter() {
            if !e3.flags().contains(PageTableFlags::PRESENT)
                || e3.flags().contains(PageTableFlags::HUGE_PAGE)
            {
                continue;
            }
            let pd_phys = e3.addr().as_u64();
            let pd = table(pd_phys);
            for e2 in pd.iter() {
                if !e2.flags().contains(PageTableFlags::PRESENT)
                    || e2.flags().contains(PageTableFlags::HUGE_PAGE)
                {
                    continue;
                }
                // Free the PT frame (its PTEs point at real kernel leaves — not freed).
                frame_allocator::free_frame(e2.addr().as_u64());
            }
            frame_allocator::free_frame(pd_phys);
        }
        frame_allocator::free_frame(pdpt_phys);
    }
    frame_allocator::free_frame(user_pml4_phys);
}

/// Phase 110 A.3b part 5 — map one additional kernel-stack **top page** into
/// an existing user half (a `CLONE_VM` thread's own kstack): the CPU pushes
/// that thread's ring-3 interrupt frames onto *its* kstack top on the user
/// CR3, so every thread's top page must be present, not just the page
/// [`build_user_half`] mapped for the creating task.
///
/// Sub-tables created here are freed by [`free_user_half`]'s generic
/// `PML4[1..512]` walk. The mapping only makes absent entries present, so it
/// is safe while the half is live as a sibling thread's CR3 on another core
/// (no shootdown needed); the *caller* must serialize concurrent mappers
/// (`AddressSpace::kpti_map_thread_kstack` holds the page-table lock).
///
/// # Safety
/// `user_pml4_phys` must be a user half built by [`build_user_half`] over this
/// process's kernel PML4, and `kstack_top_va` a mapped kernel-stack top.
pub unsafe fn map_kstack_top_into_user_half(user_pml4_phys: u64, kstack_top_va: u64) -> Option<()> {
    let top_page = (kstack_top_va - 1) & !0xFFF;
    let phys = {
        // SAFETY: translation-only mapper over the live kernel CR3, dropped
        // before the user-half mapper below is created (the A.1 aliasing rule).
        let kmapper = unsafe { super::paging::get_mapper() };
        kmapper.translate_addr(VirtAddr::new(top_page))?.as_u64()
    };
    let frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(PhysAddr::new(user_pml4_phys));
    // SAFETY: caller serializes mappers over this user half.
    let mut mapper = unsafe { mapper_for_frame(frame) };
    let mut sink: Vec<u64> = Vec::new();
    let res = {
        let mut alloc = RecordingAlloc {
            recorded: &mut sink,
        };
        // SAFETY: exclusive mapper (caller-serialized); valid frame from the
        // live translation above.
        unsafe { map_entry_page(&mut mapper, top_page, phys, RW, &mut alloc) }
    };
    match res {
        Ok(()) => Some(()),
        Err(_) => {
            for f in &sink {
                frame_allocator::free_frame(*f);
            }
            None
        }
    }
}

/// Build a real user-half PML4 via the production [`build_user_half`] over a
/// synthetic kernel PML4, returning the arena for the caller to walk then free.
///
/// This exercises the exact builder + free path A.4 activation uses (the shared
/// entry set from `collect_entry_pages`, the shared user-mapping slots, the
/// kstack top page, and `free_user_half`), rather than a bespoke test-only
/// table.
fn build_selftest_pair() -> Option<SelfTestArena> {
    // A synthetic kernel half with one user leaf per user-mapping slot
    // (PML4[0] image + PML4[255] stack).
    let (kernel_pml4_phys, owned_frames) = build_synthetic_kernel_pml4()?;

    // A real, mapped kernel-stack top to stand in for the process kstack (its
    // top page is what RSP0 points at / the CPU pushes an IRQ frame onto).
    let kstack_top = crate::arch::x86_64::gdt::syscall_stack_top();

    // The production builder.
    // SAFETY: kernel_pml4_phys is the synthetic half just built; kstack_top is a
    // live mapped kernel stack top.
    let user_pml4_phys = match unsafe { build_user_half(kernel_pml4_phys, kstack_top) } {
        Some(p) => p,
        None => {
            for f in &owned_frames {
                frame_allocator::free_frame(*f);
            }
            return None;
        }
    };

    // A.3b part 5 — exercise the CLONE_VM thread-kstack add path
    // (`map_kstack_top_into_user_half`) against the just-built half. The page
    // below the BSP syscall-stack top stands in for a second thread's kstack
    // top: live-mapped, and not already in the entry set (the builder mapped
    // only the top page). Its reachability is asserted by the round-trip below
    // like every other entry-set page.
    let thread_kstack_top = kstack_top - 0x1000;
    if unsafe { map_kstack_top_into_user_half(user_pml4_phys, thread_kstack_top) }.is_none() {
        unsafe { free_user_half(user_pml4_phys) };
        for f in &owned_frames {
            frame_allocator::free_frame(*f);
        }
        return None;
    }

    // Recompute the entry-set page list (deterministic) for classification +
    // reachability: the shared set + this half's kstack top page + the
    // thread-add page.
    let mut entry_pages = collect_entry_pages()?;
    let top_page = (kstack_top - 1) & !0xFFF;
    // SAFETY: translate through the live kernel map for the expected phys.
    let (kstack_phys, thread_page_phys) = {
        let kmapper = unsafe { super::paging::get_mapper() };
        (
            kmapper.translate_addr(VirtAddr::new(top_page))?.as_u64(),
            kmapper
                .translate_addr(VirtAddr::new((thread_kstack_top - 1) & !0xFFF))?
                .as_u64(),
        )
    };
    entry_pages.push((top_page, kstack_phys, RW));
    entry_pages.push(((thread_kstack_top - 1) & !0xFFF, thread_page_phys, RW));

    // The synthetic user leaves, resolved through the synthetic KERNEL half —
    // the user half must translate each to the same frame (shared sub-trees).
    let mut user_leaf_pages: Vec<(u64, u64)> = Vec::new();
    for va in [SELFTEST_USER_VA, SELFTEST_USER_STACK_VA] {
        // SAFETY: the synthetic kernel PML4 is valid, non-live, direct-map
        // reachable.
        let phys = unsafe { translate_in(kernel_pml4_phys, va) }?;
        user_leaf_pages.push((va, phys));
    }

    Some(SelfTestArena {
        owned_frames,
        user_pml4_phys,
        entry_pages: entry_pages
            .iter()
            .map(|(va, phys, _)| (*va, *phys))
            .collect(),
        user_leaf_pages,
    })
}

/// Manually translate `va` in the (inactive) PML4 at `pml4_phys` by walking the
/// four levels through the direct map — no CR3 switch, no live mapper alias.
/// Returns the mapped 4 KiB physical frame base, or `None` if unmapped.
///
/// # Safety
/// `pml4_phys` must reference a valid PML4 reachable through the direct map.
unsafe fn translate_in(pml4_phys: u64, va: u64) -> Option<u64> {
    let phys_off = phys_offset();
    // SAFETY: page tables are reachable read-only through the direct map.
    let table =
        |phys: u64| -> &'static PageTable { unsafe { &*((phys_off + phys) as *const PageTable) } };
    let idx = |lvl: u32| ((va >> (12 + 9 * lvl)) & 0x1FF) as usize;

    let e4 = &table(pml4_phys)[idx(3)];
    if !e4.flags().contains(PageTableFlags::PRESENT) {
        return None;
    }
    let e3 = &table(e4.addr().as_u64())[idx(2)];
    if !e3.flags().contains(PageTableFlags::PRESENT) {
        return None;
    }
    if e3.flags().contains(PageTableFlags::HUGE_PAGE) {
        return Some(e3.addr().as_u64() + (va & 0x3FFF_FFFF));
    }
    let e2 = &table(e3.addr().as_u64())[idx(1)];
    if !e2.flags().contains(PageTableFlags::PRESENT) {
        return None;
    }
    if e2.flags().contains(PageTableFlags::HUGE_PAGE) {
        return Some(e2.addr().as_u64() + (va & 0x1F_FFFF));
    }
    let e1 = &table(e2.addr().as_u64())[idx(0)];
    if !e1.flags().contains(PageTableFlags::PRESENT) {
        return None;
    }
    Some(e1.addr().as_u64())
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
    let dm_idx = pml4_index(phys_offset());

    // A.2 layout invariant: both SYSCALL stubs must live inside the
    // page-aligned `.text.kpti_entry` section — it is the ONLY kernel text the
    // user PML4 maps, so a stub drifting outside it (linker/section
    // regression) would #PF-loop on the first KPTI syscall's user-CR3
    // instruction fetch once A.4 activates. Catch it here, at boot, instead.
    let (text_start, text_end) = crate::arch::x86_64::syscall::kpti_entry_text_range();
    let (entry_plain, entry_kpti) = crate::arch::x86_64::syscall::syscall_entry_stub_addrs();
    if text_start % 0x1000 != 0
        || text_end % 0x1000 != 0
        || text_start >= text_end
        || !(text_start..text_end).contains(&entry_plain)
        || !(text_start..text_end).contains(&entry_kpti)
    {
        log::error!(
            "KPTI_SELFTEST:FAIL reason=entry-text-layout start={text_start:#x} end={text_end:#x} \
             syscall_entry={entry_plain:#x} syscall_entry_kpti={entry_kpti:#x}"
        );
        return;
    }

    // A.3b: the naked maskable-IRQ / IPI entry section must likewise be
    // page-aligned and non-empty — it is the only other kernel text the user
    // PML4 maps, so a linker regression that shrank or misaligned it would
    // #PF-loop on the first ring-3 IRQ once A.4 activates. Catch it at boot.
    let (irq_start, irq_end) = crate::arch::x86_64::interrupts::kpti_irq_entry_range();
    if irq_start % 0x1000 != 0 || irq_end % 0x1000 != 0 || irq_start >= irq_end {
        log::error!(
            "KPTI_SELFTEST:FAIL reason=irq-entry-layout start={irq_start:#x} end={irq_end:#x}"
        );
        return;
    }

    // A.3b part 3: the ring0→ring3 exit-trampoline section must likewise be
    // page-aligned and non-empty (a linker regression would #PF-loop the first
    // preempt-resume once A.4 activates).
    let (exit_start, exit_end) = crate::arch::x86_64::interrupts::kpti_exit_range();
    if exit_start % 0x1000 != 0 || exit_end % 0x1000 != 0 || exit_start >= exit_end {
        log::error!(
            "KPTI_SELFTEST:FAIL reason=exit-layout start={exit_start:#x} end={exit_end:#x}"
        );
        return;
    }

    // A.3b part 4: the interrupt-delivery structures (GDT/TSS/IDT +
    // PerCoreData, per online core) must be page-aligned — they are
    // `PageIsolated` (own their pages exclusively, so the user-half mappings
    // leak no adjacent `.data`/heap), and page alignment is the observable
    // proxy for that isolation (an un-isolated static or plain heap Box is
    // essentially never page-aligned by accident). Catch a regression at boot.
    {
        let (gdt_base, _) = crate::arch::x86_64::gdt::gdt_extent();
        let (tss_base, _) = crate::arch::x86_64::gdt::tss_extent();
        let idt_base = x86_64::instructions::tables::sidt().base.as_u64();
        let mut misaligned = [
            ("bsp-gdt", gdt_base),
            ("bsp-tss", tss_base),
            ("idt", idt_base),
        ]
        .iter()
        .find(|(_, base)| base % 0x1000 != 0)
        .map(|(what, base)| (*what, *base));
        if misaligned.is_none() {
            'cores: for core_id in 0..crate::smp::core_count() {
                let Some(pcd) = crate::smp::get_core_data(core_id) else {
                    continue;
                };
                if !pcd.is_online.load(core::sync::atomic::Ordering::Acquire) {
                    continue;
                }
                for (base, _) in pcd.entry_struct_extents() {
                    if base % 0x1000 != 0 {
                        misaligned = Some(("core-entry-struct", base));
                        break 'cores;
                    }
                }
                // Phase 110 hardening — the per-CPU KPTI trampoline stack:
                // must exist and be page-aligned (its top page is entry-set
                // mapped and TSS.RSP0 points at the top when KPTI is active;
                // page alignment is also what keeps the CPU's frame pushes
                // 16-aligned and the mapping free of neighbouring heap data).
                if pcd.kpti_tramp_top == 0 || pcd.kpti_tramp_top % 0x1000 != 0 {
                    misaligned = Some(("kpti-tramp-top", pcd.kpti_tramp_top));
                    break 'cores;
                }
            }
        }
        if let Some((what, base)) = misaligned {
            log::error!("KPTI_SELFTEST:FAIL reason=entry-struct-alignment {what}={base:#x}");
            return;
        }
    }

    // kimg_idx the same way build does (the entry-text section's slot).
    let kimg_idx = pml4_index(text_start);

    // Defence in depth for classify(): its user-slot arm precedes the
    // kernel-image / direct-map arms, so if either secret slot ever collided
    // with a USER_PML4_SLOTS entry the walk would silently classify kernel
    // leaves as user. The layout makes collision impossible today (image at
    // PML4[2] via the fixed 1 TiB PIE base, direct map in the upper half);
    // assert it stays that way.
    if kernel_core::kpti::is_user_pml4_slot(kimg_idx)
        || kernel_core::kpti::is_user_pml4_slot(dm_idx)
    {
        log::error!(
            "KPTI_SELFTEST:FAIL reason=user-slot-overlap kimg_idx={kimg_idx} dm_idx={dm_idx}"
        );
        return;
    }

    let arena = match build_selftest_pair() {
        Some(a) => a,
        None => {
            log::error!("KPTI_SELFTEST:SKIP reason=alloc-failed");
            return;
        }
    };

    let entry_vas: Vec<u64> = arena.entry_pages.iter().map(|(va, _)| *va).collect();

    // SAFETY: arena.user_pml4_phys is a valid, non-live PML4; we only read it.
    let obs = unsafe { walk_user_half(arena.user_pml4_phys, &entry_vas, kimg_idx, dm_idx) };

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

    // Reachability round-trip (A.3): every entry-set page must actually
    // translate in the built user PML4 to the SAME physical frame the kernel
    // map resolves it to. The invariant walk above proves nothing *extra* is
    // reachable; this proves everything the live trampoline will touch on the
    // user CR3 (GDT/IDT/TSS/PerCoreData/entry text/entry stack) IS reachable —
    // a missing entry would be interrupt-delivery #DF-then-triple-fault at A.4.
    let mut unreachable = 0usize;
    for (va, expected_phys) in &arena.entry_pages {
        // SAFETY: user_pml4_phys is a valid, non-live PML4 read via direct map.
        match unsafe { translate_in(arena.user_pml4_phys, *va) } {
            Some(got) if got == (*expected_phys & !0xFFF) => {}
            _ => unreachable += 1,
        }
    }

    // User-leaf round-trip (A.4): one synthetic leaf per user-mapping slot
    // (PML4[0] image + PML4[255] stack) must translate in the user half to the
    // frame the kernel half maps — proving BOTH USER_PML4_SLOTS are shared. A
    // missing one is not a triple fault but the silent stack-slot #PF-loop
    // wedge (the fault resolves in the kernel half, so the handler finds
    // nothing to fix and ring 3 re-faults forever).
    let mut user_unreachable = 0usize;
    for (va, expected_phys) in &arena.user_leaf_pages {
        // SAFETY: user_pml4_phys is a valid, non-live PML4 read via direct map.
        match unsafe { translate_in(arena.user_pml4_phys, *va) } {
            Some(got) if got == (*expected_phys & !0xFFF) => {}
            _ => user_unreachable += 1,
        }
    }

    // Free the throwaway pair before reporting (report is the observable point).
    let user_pml4_phys = arena.user_pml4_phys;
    let entry_page_count = arena.entry_pages.len();
    let user_leaf_count = arena.user_leaf_pages.len();
    arena.free();

    if unreachable != 0 {
        log::error!(
            "KPTI_SELFTEST:FAIL reason=entry-set-unreachable unreachable={unreachable} of {entry_page_count}"
        );
        return;
    }
    if user_unreachable != 0 {
        log::error!(
            "KPTI_SELFTEST:FAIL reason=user-slot-unreachable unreachable={user_unreachable} of {user_leaf_count}"
        );
        return;
    }

    match result {
        Ok(()) if secret_leaves == 0 => {
            log::info!(
                "KPTI_SELFTEST:PASS user_pml4={:#x} entry_set={} leaves ({} pages, all reachable); no kernel-secret leaf reachable from user CR3",
                user_pml4_phys,
                entry_leaves,
                entry_page_count,
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
