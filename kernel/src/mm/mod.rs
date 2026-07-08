//! # Ownership: Keep
//! Memory management is a core kernel primitive — frame allocation, page tables, and address space isolation must remain ring-0.

pub mod debug;
pub mod dma;
pub mod elf;
pub mod frame_allocator;
pub mod heap;
pub mod kpti;
pub mod memory_map;
pub mod paging;
pub mod pkey;
pub mod shm;
pub mod slab;
pub mod slab_box;
pub mod user_mem;
pub mod user_space;

use bootloader_api::BootInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Once;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{OffsetPageTable, PageTable, PhysFrame, Size4KiB},
};

// ---------------------------------------------------------------------------
// First-class address space object (Phase 52b, Track A)
// ---------------------------------------------------------------------------

/// A process's virtual address space descriptor.
///
/// Wraps the PML4 physical address with metadata for TLB shootdown
/// optimization (generation counter) and multi-core tracking.
///
/// Phase 57b — `page_table_lock` is a plain [`spin::Mutex`] with the
/// **preempt-only** discipline applied via [`PageTableGuard`].  An
/// `IrqSafeMutex` would mask IF inside the lock-held region, but several
/// existing consumers (notably PCI BAR map/unmap in
/// [`crate::pci::bar`]) hold the guard alive across
/// [`crate::smp::tlb::tlb_shootdown_range`] — a contender on the same
/// address-space lock would then spin with IF=0 and fail to service the
/// holder's TLB shootdown IPI, deadlocking both cores.  Keeping IF
/// enabled while the lock is held closes that hazard; `preempt_disable`
/// still pins the holder against 57d/57e voluntary or full preemption.
pub struct AddressSpace {
    pml4_phys: PhysAddr,
    /// Phase 110 A.3b part 5 — the KPTI **user-half** PML4 for this address
    /// space (0 = none built). Built by [`AddressSpace::build_kpti_user_half`]
    /// at process creation / execve when KPTI is active; A.4's dispatch prep
    /// publishes it to `gs:[kpti_user_cr3]` for the entry/exit stubs to load
    /// as the ring-3 CR3. Freed by `Drop` via [`kpti::free_user_half`].
    kpti_user_pml4: AtomicU64,
    generation: AtomicU64,
    active_on_cores: AtomicU64,
    page_table_lock: spin::Mutex<()>,
}

/// RAII guard returned by [`AddressSpace::lock_page_tables`].
///
/// On `Drop` the inner spin guard releases first, then `preempt_enable`
/// runs — same shape as [`crate::task::scheduler::IrqSafeGuard`] but
/// without the IF-masking step (see the doc-comment on
/// [`AddressSpace::page_table_lock`] for why IF must stay enabled).
pub struct PageTableGuard<'a> {
    _guard: spin::MutexGuard<'a, ()>,
    _preempt: PageTablePreemptRestore,
}

/// Drop hook that pairs the `preempt_disable` in
/// [`AddressSpace::lock_page_tables`] with a matching `preempt_enable`.
///
/// Field declaration order in [`PageTableGuard`] is load-bearing: the
/// inner spin guard drops first (releasing the lock), then this drops
/// (decrementing `preempt_count`).  Mirrors `IrqSafeGuard`'s drop chain
/// with the IF-restore step deliberately omitted.
struct PageTablePreemptRestore;

impl Drop for PageTablePreemptRestore {
    fn drop(&mut self) {
        crate::task::scheduler::preempt_enable();
    }
}

#[allow(dead_code)]
impl AddressSpace {
    pub fn new(pml4_phys: PhysAddr) -> Self {
        Self {
            pml4_phys,
            kpti_user_pml4: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            active_on_cores: AtomicU64::new(0),
            page_table_lock: spin::Mutex::new(()),
        }
    }

    pub fn pml4_phys(&self) -> PhysAddr {
        self.pml4_phys
    }

    /// Phase 110 A.3b part 5 — build + attach the KPTI user-half PML4 for this
    /// address space.
    ///
    /// Since the trampoline-stack hardening (18–21/n) the user half is fully
    /// **process-independent** apart from the shared `USER_PML4_SLOTS`: ring-3
    /// interrupt frames land on the per-CPU trampoline stack (already in the
    /// shared entry set), so no per-task kstack page — and no per-thread
    /// `CLONE_VM` map — is needed.
    ///
    /// No-op (returning `true`) while KPTI is inactive (`mitigations=off` /
    /// `auto` on `RDCL_NO` silicon) — that path adds zero per-process
    /// overhead.
    /// Returns `false` on allocation failure (logged); A.4 fails closed on it:
    /// `execve` returns `ENOMEM` before its destructive steps, and the
    /// fork-child trampoline kills the child rather than entering ring 3
    /// unisolated (the exit stubs skip the CR3 switch on `user_cr3 == 0`).
    pub fn build_kpti_user_half(&self) -> bool {
        if !crate::mitigations::state().is_some_and(|s| s.kpti_active) {
            return true;
        }
        // SAFETY: `pml4_phys` is this process's live kernel PML4, per the
        // constructor contracts of every call site (spawn/fork/execve).
        match unsafe { kpti::build_user_half(self.pml4_phys.as_u64()) } {
            Some(user) => {
                self.kpti_user_pml4.store(user, Ordering::Release);
                true
            }
            None => {
                log::error!(
                    "kpti: build_user_half failed for pml4={:#x} (out of frames?)",
                    self.pml4_phys.as_u64()
                );
                false
            }
        }
    }

    /// The KPTI user-half PML4 physical address (0 = none built). A.4's
    /// dispatch prep publishes this to `gs:[kpti_user_cr3]`.
    pub fn kpti_user_pml4(&self) -> u64 {
        self.kpti_user_pml4.load(Ordering::Acquire)
    }

    /// Phase 110 hardening — the `#PF`-time top-level-slot sync. On a
    /// not-present ring-3 fault, re-copy the top-level `PML4` slot covering
    /// `fault_va` from this space's kernel half into its live user half if
    /// they diverged (a `USER_PML4_SLOTS` entry that was empty when the pair
    /// was built and has since been populated in the kernel half). Returns
    /// `true` if it repaired the slot (the caller should retry the faulting
    /// instruction), `false` otherwise (no user half, not a syncable slot, or
    /// already in sync — fall through to the normal fault handling).
    ///
    /// No-op (`false`) when no user half exists (KPTI inactive). Serialized
    /// against other page-table mutators via the page-table lock; a
    /// full-local-both-PCID flush follows a real repair (cheap — this fires
    /// essentially never, and covers the paranoid case of a slot replaced
    /// rather than filled).
    pub fn kpti_sync_user_slot_on_fault(&self, fault_va: u64) -> bool {
        let user = self.kpti_user_pml4.load(Ordering::Acquire);
        if user == 0 {
            return false;
        }
        let _guard = self.lock_page_tables();
        // SAFETY: `pml4_phys` is this space's live kernel half and `user` its
        // user half built over it; the page-table lock serializes writers.
        let repaired = unsafe { kpti::sync_slot_raw(self.pml4_phys.as_u64(), user, fault_va) };
        if repaired {
            crate::smp::tlb::flush_local_all();
        }
        repaired
    }

    pub fn activate_on_core(&self, core_id: u8) {
        self.active_on_cores
            .fetch_or(1u64 << core_id, Ordering::Release);
    }

    pub fn deactivate_on_core(&self, core_id: u8) {
        self.active_on_cores
            .fetch_and(!(1u64 << core_id), Ordering::Release);
    }

    pub fn bump_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel)
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn active_cores(&self) -> u64 {
        self.active_on_cores.load(Ordering::Acquire)
    }

    /// Acquire the page-table lock with **preempt-only** discipline.
    ///
    /// Raises `preempt_count` *before* acquiring the inner spin lock so
    /// that 57d/57e cannot preempt the holder, but does NOT mask
    /// interrupts: holders may invoke [`crate::smp::tlb::tlb_shootdown_range`]
    /// while the guard is alive, and that path requires IF=1 to receive
    /// the remote ack IPIs.  See the doc-comment on
    /// [`AddressSpace::page_table_lock`].
    pub fn lock_page_tables(&self) -> PageTableGuard<'_> {
        crate::task::scheduler::preempt_disable();
        // The spin lock is acquired with IF in whatever state the caller
        // had on entry; we never disable interrupts here.
        let guard = self.page_table_lock.lock();
        PageTableGuard {
            _guard: guard,
            _preempt: PageTablePreemptRestore,
        }
    }
}

/// Phase 110 A.3b part 5 — the KPTI user half is freed with the
/// `AddressSpace`, not with the kernel PML4: `free_process_page_table` (called
/// manually at the teardown sites) only knows the kernel half, while the last
/// `Arc<AddressSpace>` drop is the natural end-of-life for the pair.
/// [`kpti::free_user_half`] frees only the private entry-set sub-tables and
/// the user PML4 frame itself — never the shared user-mapping slots
/// (`kernel_core::kpti::USER_PML4_SLOTS`, owned by the kernel half) nor any
/// leaf page — so the ordering relative to
/// `free_process_page_table(kernel_pml4)` is immaterial.
impl Drop for AddressSpace {
    fn drop(&mut self) {
        let user = *self.kpti_user_pml4.get_mut();
        if user != 0 {
            // SAFETY: the last Arc reference is gone, so no core can have this
            // user half loaded as its CR3 (execve/exit switch away before
            // dropping the process's Arc).
            unsafe { kpti::free_user_half(user) };
        }
    }
}

static PHYS_OFFSET: Once<u64> = Once::new();

/// Physical address of the kernel's original PML4 (set once during mm::init).
/// Used by `new_process_page_table` and `restore_kernel_cr3` so they always
/// reference the bootloader-created page table rather than whatever CR3 happens
/// to be active when called (which could be a process's page table after fork).
static KERNEL_PML4_PHYS: Once<u64> = Once::new();

/// Returns the physical memory offset established during `mm::init`.
///
/// Panics if called before `mm::init`.
#[allow(dead_code)]
pub fn phys_offset() -> u64 {
    *PHYS_OFFSET.get().expect("mm not initialized")
}

/// Return the physical address of the kernel's PML4 page table.
///
/// Used by the SMP trampoline to load the correct CR3 on AP cores.
pub fn kernel_pml4_phys() -> u64 {
    *KERNEL_PML4_PHYS.get().expect("mm not initialized")
}

/// Load a **kernel-half** PML4 as this core's CR3 across an address-space
/// boundary (scheduler dispatch, `execve`, `fork`, restore-to-kernel).
///
/// This is the single PCID-aware CR3-write locus (Phase 110 A.5). `pml4_phys`
/// is the (page-aligned) kernel half — the full map the kernel runs on. The
/// caller publishes the KPTI CR3 pair separately via
/// [`crate::smp::publish_kpti_cr3_pair`] right after.
///
/// * **PCID scheme active** — load the frame under [`KERNEL_PCID`] with a
///   *flushing* `mov cr3` (drops the previous occupant's kernel-half entries,
///   which reuse the same global PCID), then `INVPCID` the [`USER_PCID`] to drop
///   the previous occupant's user-half entries too (a CR3 load only affects the
///   PCID it loads, and the exit trampolines about to run will load the user
///   half **no-flush**). This is the "flush both PCIDs of the target ASID" the
///   charter requires, applied at the switch-in.
/// * **PCID scheme inactive** (every QEMU lane / no-KPTI) — a plain
///   `Cr3::write(frame, empty())` with `PCID = 0`, byte-identical to Phase 84.
///
/// # Safety-adjacent contract
/// Must run in ring 0 with the target address space's page tables live. When
/// the scheme is active, `CR4.PCIDE` must already be set on this core (the
/// `enable_pcid_if_kpti_active` calls guarantee it precedes any dispatch).
pub fn write_kernel_cr3(pml4_phys: u64) {
    use x86_64::{
        PhysAddr,
        registers::control::{Cr3, Cr3Flags},
        structures::paging::PhysFrame,
    };
    let aligned = pml4_phys & !kernel_core::kpti_pcid::PCID_MASK;
    let frame =
        PhysFrame::from_start_address(PhysAddr::new(aligned)).expect("kernel PML4 unaligned");
    if crate::mitigations::pcid_active() {
        use kernel_core::kpti_pcid::{KERNEL_PCID, USER_PCID};
        use x86_64::instructions::tlb::{InvPcidCommand, Pcid, flush_pcid};
        // SAFETY: ring 0; CR4.PCIDE is enabled under the active scheme; the
        // frame is a live kernel PML4 and the PCIDs are the fixed <= 4095
        // constants.
        unsafe {
            // Flushing load of the kernel half (drops the old KERNEL_PCID
            // entries), then invalidate the user PCID so the exit trampoline's
            // no-flush user load cannot resurrect the old process's user pages.
            Cr3::write_pcid(frame, Pcid::new(KERNEL_PCID).expect("KERNEL_PCID in range"));
            flush_pcid(InvPcidCommand::Single(
                Pcid::new(USER_PCID).expect("USER_PCID in range"),
            ));
        }
    } else {
        // SAFETY: ring 0; `frame` is a live kernel PML4.
        unsafe {
            Cr3::write(frame, Cr3Flags::empty());
        }
    }
}

/// Switch CR3 back to the kernel's original page table.
///
/// Called from process-exit paths (syscall handlers, fault trampolines) that
/// run while the current task's CR3 is still pointing at the dying process's
/// page table.  Restoring the kernel CR3 before yielding ensures that the
/// next scheduler-picked task starts with a consistent address space.
///
/// # Safety
///
/// Must only be called with interrupts disabled or inside a syscall handler
/// where re-entrancy is not a concern.  Only callable from ring 0 (Cr3::write
/// is a privileged operation).
pub fn restore_kernel_cr3() {
    let phys = *KERNEL_PML4_PHYS.get().expect("mm not initialized");
    // Load the boot PML4 as the kernel half (PCID-tagged + both-PCID flush when
    // the A.5 scheme is active; a plain flushing `Cr3::write` otherwise).
    write_kernel_cr3(phys);
    // Phase 110 A.4 — this core now runs pure kernel context on the boot PML4:
    // retarget the per-core KPTI pair so the paranoid NMI/#DF entry (which
    // loads `kpti_kernel_cr3` whenever non-zero) never chases the process PML4
    // this core just switched away from — the exit paths that call us free it
    // next — and so the exit stubs stay inert (`user_cr3 = 0`) until the next
    // user dispatch publishes a real pair. No-op while KPTI is inactive.
    crate::smp::publish_kpti_cr3_pair(phys, 0);
}

/// Phase 84 Track A.4 — KPTI `GLOBAL`-bit guard.
///
/// PTEs marked `GLOBAL` survive a CR3 reload, so under KPTI a global kernel TLB
/// entry would persist into userspace and let a Meltdown PoC read kernel data
/// from a stale translation even after the CR3 switch — the most insidious
/// silent-failure mode of a first KPTI (Redox's `startup/memory.rs` encodes
/// exactly this). m3OS marks **no** kernel PTE `GLOBAL` (verified: no
/// `PageTableFlags::GLOBAL` site in `kernel/src/mm`), so this is a **guard**, not
/// a removal: it walks the kernel upper half and returns the number of `GLOBAL`
/// **leaf** entries (4 KiB PTE or huge PDE/PDPTE — the only levels where the bit
/// is architecturally meaningful), which must be `0`. If a future `CR4.PGE`
/// throughput optimization ever introduces `GLOBAL` kernel PTEs, this fires.
///
/// Bounded: only present entries are visited, and the direct map uses huge
/// pages, so the upper half is a few hundred table entries — cheap at boot.
pub fn count_global_kernel_leaf_ptes() -> usize {
    use x86_64::structures::paging::{PageTable, PageTableFlags};
    let phys_off = phys_offset();
    let kpml4_phys = *KERNEL_PML4_PHYS.get().expect("mm not initialized");
    let mut global = 0usize;
    let table = |phys: u64| -> &'static PageTable {
        // SAFETY: every kernel page table is reachable through the
        // physical-memory direct map; we only read.
        unsafe { &*((phys_off + phys) as *const PageTable) }
    };
    let pml4 = table(kpml4_phys);
    // Kernel upper half (256..512): heap, direct map, stacks.
    for p4i in 256..512 {
        let p4e = &pml4[p4i];
        if !p4e.flags().contains(PageTableFlags::PRESENT) {
            continue;
        }
        let pdpt = table(p4e.addr().as_u64());
        for p3e in pdpt.iter() {
            let f3 = p3e.flags();
            if !f3.contains(PageTableFlags::PRESENT) {
                continue;
            }
            if f3.contains(PageTableFlags::HUGE_PAGE) {
                // 1 GiB leaf.
                if f3.contains(PageTableFlags::GLOBAL) {
                    global += 1;
                }
                continue;
            }
            let pd = table(p3e.addr().as_u64());
            for p2e in pd.iter() {
                let f2 = p2e.flags();
                if !f2.contains(PageTableFlags::PRESENT) {
                    continue;
                }
                if f2.contains(PageTableFlags::HUGE_PAGE) {
                    // 2 MiB leaf.
                    if f2.contains(PageTableFlags::GLOBAL) {
                        global += 1;
                    }
                    continue;
                }
                let pt = table(p2e.addr().as_u64());
                for pte in pt.iter() {
                    let f1 = pte.flags();
                    if f1.contains(PageTableFlags::PRESENT) && f1.contains(PageTableFlags::GLOBAL) {
                        global += 1;
                    }
                }
            }
        }
    }
    global
}

pub fn init(boot_info: &'static mut BootInfo) {
    // Capture the kernel's PML4 frame before any CR3 switches occur.
    {
        use x86_64::registers::control::Cr3;
        let (kpml4, _) = Cr3::read();
        KERNEL_PML4_PHYS.call_once(|| kpml4.start_address().as_u64());
    }

    // End mutable access; coerce &'static mut → &'static so the borrow checker
    // tracks that we no longer hold exclusive access to BootInfo.
    let boot_info: &'static BootInfo = boot_info;

    // The bootloader guarantees this slice is valid for the kernel's lifetime.
    let static_regions: &'static [bootloader_api::info::MemoryRegion] = &boot_info.memory_regions;

    let phys_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("[mm] bootloader did not provide physical memory offset");

    // Store physical memory offset globally so other modules can rebuild the mapper.
    PHYS_OFFSET.call_once(|| phys_offset);

    // Phase 57e diag (slab UAF Hyp #2): log address-space layout so we can
    // verify the bootloader's Mapping::Dynamic phys-offset does not collide
    // with HEAP_START..HEAP_MAX_SIZE at PML4[256].
    {
        let kernel_pml4 = *KERNEL_PML4_PHYS.get().expect("KERNEL_PML4_PHYS set above");
        let heap_start = heap::HEAP_START as u64;
        let heap_end = heap_start + heap::HEAP_MAX_SIZE as u64;
        let pml4_idx = |va: u64| ((va >> 39) & 0x1FF) as usize;
        let phys_off_pml4 = pml4_idx(phys_offset);
        let heap_pml4 = pml4_idx(heap_start);
        let collide = phys_off_pml4 == heap_pml4;
        log::info!(
            "[mm] addr-space layout: KERNEL_PML4_PHYS={:#x} phys_offset={:#x} (PML4[{}]) heap={:#x}..{:#x} (PML4[{}]) collide={}",
            kernel_pml4,
            phys_offset,
            phys_off_pml4,
            heap_start,
            heap_end,
            heap_pml4,
            collide,
        );
    }

    // BRING-UP DIAGNOSTIC: row-1 POST squares pinpoint which mm sub-step hangs
    // on bare metal (see crate::post_marker). 16=after memory_map, 17=after
    // frame_allocator (the per-frame free-list build — O(RAM); ~16M frames at
    // 64 GiB), 18=after paging+heap, 19=after buddy upgrade.
    memory_map::init(static_regions);
    crate::post_marker(16);
    frame_allocator::init(static_regions, phys_offset);
    crate::post_marker(17);

    // Log reserved regions below 1 MiB to confirm allocator skips them (P2-T008)
    debug::log_reserved_below_1mib();

    // Scope the mapper so it is dropped before any heap allocations that
    // might trigger grow_heap (which calls get_mapper). Holding both would
    // alias &mut PageTable = UB.
    {
        let mut mapper = unsafe { paging::init(x86_64::VirtAddr::new(phys_offset)) };
        heap::init_heap(&mut mapper, &mut paging::GlobalFrameAlloc);
    }
    crate::post_marker(18); // paging + kernel heap up

    // Upgrade from bootstrap free-list to buddy allocator (requires heap).
    frame_allocator::init_buddy();
    crate::post_marker(19); // buddy allocator built (drains the free list)

    // P17-T010: initialize per-frame refcount table (requires heap).
    frame_allocator::init_refcounts();

    // P33: initialize slab caches for fixed-size kernel objects.
    slab::init();

    // Phase 53a C.2: activate the size-class allocator now that slab caches
    // and the buddy allocator are ready.  All subsequent eligible small
    // allocations route through magazine_alloc; large allocations use
    // page-backed buddy frames.  Bootstrap-era allocations continue to be
    // recognized by address range and handled by the bootstrap allocator.
    // The compile-time `legacy-bootstrap-allocator` feature leaves this cutover
    // disabled as a bring-up kill switch.
    heap::activate_size_class_allocator();

    log::info!("[mm] Memory subsystem initialized");
}

// ---------------------------------------------------------------------------
// Per-process page table helpers (P11-T002 / P11-T013)
// ---------------------------------------------------------------------------

/// Create a fresh user-space page table that inherits all kernel mappings.
///
/// Allocates a new PML4 frame, zeroes it, then:
/// - Copies upper-half entries (256–511) from the current PML4 (kernel heap,
///   physical-memory offset mapping, etc.).
/// - Deep-copies PML4[0]'s PDPT and every PD table within it so the process
///   can reach kernel code at low virtual addresses (e.g. the trampoline at
///   0x1d9d0) after CR3 switch, while ELF-loader writes land in the process's
///   private PD instead of contaminating the shared kernel page structures.
///
/// Returns the physical frame of the new PML4, or `None` if frame allocation
/// fails.
#[allow(dead_code)]
pub fn new_process_page_table() -> Option<PhysFrame<Size4KiB>> {
    use x86_64::structures::paging::PageTableFlags;

    let phys_off = VirtAddr::new(phys_offset());

    // Allocate and zero the new PML4.
    let pml4_frame = frame_allocator::allocate_frame()?;
    let new_pml4_virt = phys_off + pml4_frame.start_address().as_u64();
    // SAFETY: frame is freshly allocated; no other reference exists.
    unsafe {
        core::ptr::write_bytes(new_pml4_virt.as_mut_ptr::<u8>(), 0, 4096);
    }

    // Always derive from the kernel's original PML4, not the current CR3.
    // If called from a syscall handler running with a process's CR3, Cr3::read()
    // would return the dying process's PML4 and the new process would inherit
    // its user-space mappings — causing map_to to fail with "already mapped".
    let kernel_pml4_phys = *KERNEL_PML4_PHYS.get().expect("mm not initialized");
    let cur_pml4_virt = phys_off + kernel_pml4_phys;

    // SAFETY: cur_pml4 is the kernel's PML4 (set during mm::init); new_pml4 is ours alone. All virtual
    // addresses are derived from the physical-memory offset established by mm::init.
    unsafe {
        let cur_pml4 = &*(cur_pml4_virt.as_ptr::<PageTable>());
        let new_pml4 = &mut *(new_pml4_virt.as_mut_ptr::<PageTable>());

        // Upper half (256–511): kernel heap, stacks, physmem offset mapping, etc.
        // Lower half (1–255): kernel binary + physical-memory mapping.
        // The kernel is linked at low addresses and the bootloader maps it via a
        // virtual-address offset (e.g. 0x10000000000 → PML4[2]).  Without copying
        // these entries the CPU triple-faults immediately after CR3 switch because
        // the kernel's next instruction is unreachable in the new address space.
        // ELF-loader user mappings always land in PML4[0] (USER_VADDR_MIN = 0x200000),
        // so shallow-copying PML4[1..256] never causes page-table contamination.
        for i in 1usize..512 {
            new_pml4[i] = cur_pml4[i].clone();
        }

        // PML4[0]: deep-copy the PDPT and each PD so the ELF loader can add user
        // entries (at USER_VADDR_MIN = 0x200000) to a process-private PD rather
        // than the shared kernel page structures.  If the kernel's PML4[0] is not
        // present (common case: kernel binary is in PML4[2]), this block is skipped
        // and the ELF loader creates a fresh PDPT/PD chain for the user mapping.
        let p4e = &cur_pml4[0];
        if p4e.flags().contains(PageTableFlags::PRESENT) {
            let pdpt_frame = frame_allocator::allocate_frame()?;
            let new_pdpt_virt = phys_off + pdpt_frame.start_address().as_u64();
            core::ptr::write_bytes(new_pdpt_virt.as_mut_ptr::<u8>(), 0, 4096);

            let cur_pdpt = &*(phys_off + p4e.addr().as_u64()).as_ptr::<PageTable>();
            let new_pdpt = &mut *new_pdpt_virt.as_mut_ptr::<PageTable>();

            for j in 0usize..512 {
                let p3e = &cur_pdpt[j];
                if !p3e.flags().contains(PageTableFlags::PRESENT) {
                    continue;
                }
                if p3e.flags().contains(PageTableFlags::HUGE_PAGE) {
                    // 1 GiB huge page: no sub-table to contaminate; copy as-is.
                    new_pdpt[j] = p3e.clone();
                    continue;
                }
                // Non-huge PDPT entry: deep-copy its PD so the ELF loader can
                // add user-space entries without touching the kernel's PD.
                let pd_frame = frame_allocator::allocate_frame()?;
                let new_pd_virt = phys_off + pd_frame.start_address().as_u64();
                core::ptr::write_bytes(new_pd_virt.as_mut_ptr::<u8>(), 0, 4096);

                let cur_pd = &*(phys_off + p3e.addr().as_u64()).as_ptr::<PageTable>();
                let new_pd = &mut *new_pd_virt.as_mut_ptr::<PageTable>();

                // Copy all PD entries: kernel huge-page/4 KiB entries carry over;
                // user entries (USER_VADDR_MIN+) will be populated by the ELF loader.
                for k in 0usize..512 {
                    new_pd[k] = cur_pd[k].clone();
                }

                // Ensure USER_ACCESSIBLE on the intermediate entry so the CPU can
                // follow the walk to user-mapped pages within this PDPT slot.
                new_pdpt[j].set_addr(
                    pd_frame.start_address(),
                    p3e.flags()
                        | PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::USER_ACCESSIBLE,
                );
            }

            // Point PML4[0] at the private PDPT with USER_ACCESSIBLE so the CPU
            // can walk to user-mapped pages in the lower half.
            new_pml4[0].set_addr(
                pdpt_frame.start_address(),
                p4e.flags()
                    | PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE,
            );
        }
    }

    Some(pml4_frame)
}

/// Free all user-space page table frames for the given PML4 physical address.
///
/// Walks the process's PML4, freeing user-accessible leaf pages and any
/// page-table structure frames that are process-private (not shared with the
/// kernel).  Shared kernel entries (PML4[1..256]) are detected by comparing
/// against the kernel's PML4 and skipped entirely.
///
/// # Safety
///
/// `cr3_phys` must be the physical address of a valid, now-unreachable PML4
/// that is no longer loaded in CR3. No other code may access the page table
/// after this call.
#[track_caller]
pub fn free_process_page_table(cr3_phys: u64) {
    use alloc::vec::Vec;
    use x86_64::structures::paging::{PageTable, PageTableFlags};
    // Phase 57e Session 8 — defensive sanity check.
    //
    // Freeing the currently-active CR3 leaves CR3 dangling on this core.
    // The only legitimate caller (sys_exit, fault_kill_trampoline) calls
    // `restore_kernel_cr3` first; execve switches via Cr3::write before
    // the free.  If this fires we know an upstream bug — most likely a
    // recurrence of Bug #7's stale `old_cr3_phys == new_cr3_phys` race.
    //
    // Kept after the fix landed because the cost is one Cr3::read per
    // process exit and the WARN is silent under correct operation.
    {
        use x86_64::registers::control::Cr3;
        let (active_cr3, _) = Cr3::read();
        if active_cr3.start_address().as_u64() == cr3_phys {
            let caller = core::panic::Location::caller();
            log::warn!(
                "[free_pt] !!! cr3_phys={:#x} EQUALS active CR3 — caller={}:{}",
                cr3_phys,
                caller.file(),
                caller.line()
            );
        }
    }
    let phys_off = VirtAddr::new(phys_offset());
    let kernel_pml4_phys = *KERNEL_PML4_PHYS.get().expect("mm not initialized");

    // Helper: read present, non-huge child table addresses from a page table,
    // scoping the &PageTable reference so it drops before any free_frame calls.
    unsafe fn collect_children(
        phys_off: VirtAddr,
        table_phys: u64,
        count: usize,
        filter: fn(PageTableFlags) -> bool,
    ) -> Vec<u64> {
        unsafe {
            // Validate the table physical address before dereferencing.
            if table_phys == 0 || table_phys & 0xFFF != 0 {
                return Vec::new();
            }
            let mut addrs = Vec::with_capacity(count);
            let pt: &PageTable = &*(phys_off + table_phys).as_ptr::<PageTable>();
            for i in 0..count {
                let entry = &pt[i];
                let flags = entry.flags();
                if !flags.contains(PageTableFlags::PRESENT) {
                    continue;
                }
                if !filter(flags) {
                    continue;
                }
                let addr = entry.addr().as_u64();
                // Skip entries with invalid physical addresses.
                if addr == 0 || addr & 0xFFF != 0 {
                    continue;
                }
                addrs.push(addr);
            }
            addrs
        }
    }

    fn not_huge(flags: PageTableFlags) -> bool {
        !flags.contains(PageTableFlags::HUGE_PAGE)
    }
    fn user_leaf(flags: PageTableFlags) -> bool {
        // BIT_11 marks "device/hardware frame" (e.g. UEFI framebuffer) that
        // must NOT be returned to the frame allocator on process teardown.
        flags.contains(PageTableFlags::USER_ACCESSIBLE) && !flags.contains(PageTableFlags::BIT_11)
    }
    fn any_user(flags: PageTableFlags) -> bool {
        flags.contains(PageTableFlags::USER_ACCESSIBLE)
    }

    // SAFETY: cr3_phys is a valid PML4 frame being freed. The caller guarantees
    // it is no longer active (not in CR3) and has exclusive ownership.
    // All &PageTable references are scoped within collect_children so they
    // drop before free_frame writes allocator metadata into the frame.
    unsafe {
        let pml4: &PageTable = &*(phys_off + cr3_phys).as_ptr::<PageTable>();
        let kernel_pml4: &PageTable = &*(phys_off + kernel_pml4_phys).as_ptr::<PageTable>();

        // Collect PDPT addresses for non-kernel PML4 entries.
        let mut pdpt_addrs = Vec::new();
        for p4 in 0usize..256 {
            let p4e = &pml4[p4];
            if !p4e.flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            if kernel_pml4[p4].flags().contains(PageTableFlags::PRESENT)
                && p4e.addr() == kernel_pml4[p4].addr()
            {
                continue;
            }
            pdpt_addrs.push(p4e.addr().as_u64());
        }
        // PML4/kernel_pml4 references are fine — those frames aren't freed yet.

        for pdpt_phys in &pdpt_addrs {
            let pd_addrs = collect_children(phys_off, *pdpt_phys, 512, not_huge);

            for pd_phys in &pd_addrs {
                let pt_addrs = collect_children(phys_off, *pd_phys, 512, not_huge);

                for pt_phys in &pt_addrs {
                    let leaf_addrs = collect_children(phys_off, *pt_phys, 512, user_leaf);
                    // A PT that holds only BIT_11 (device-frame) entries still needs its
                    // own frame freed — separate the "free leaves" predicate from the
                    // "free this PT" predicate.
                    let pt_has_user =
                        !collect_children(phys_off, *pt_phys, 512, any_user).is_empty();
                    for leaf in &leaf_addrs {
                        frame_allocator::free_frame(*leaf);
                    }
                    if pt_has_user {
                        frame_allocator::free_frame(*pt_phys);
                    }
                }
                frame_allocator::free_frame(*pd_phys);
            }
            frame_allocator::free_frame(*pdpt_phys);
        }
        frame_allocator::free_frame(cr3_phys);
    }
}

/// Build an `OffsetPageTable` mapper over an arbitrary PML4 frame.
///
/// Does **not** switch CR3, so the current address space remains active.
/// All page-table walks go through the physical-memory offset, allowing the
/// kernel to manipulate any process's page table without changing CR3.
///
/// # Safety
///
/// - `cr3_frame` must point to a valid, 4 KiB-aligned PML4.
/// - No other `OffsetPageTable` over the same frame may be alive at the same
///   time (aliasing `&mut PageTable` is UB).
/// - The physical memory offset must be valid (i.e. `mm::init` must have run).
#[allow(dead_code)]
pub unsafe fn mapper_for_frame(cr3_frame: PhysFrame<Size4KiB>) -> OffsetPageTable<'static> {
    unsafe {
        let phys_off = VirtAddr::new(phys_offset());
        let pml4_virt = phys_off + cr3_frame.start_address().as_u64();
        let pml4: &'static mut PageTable = &mut *pml4_virt.as_mut_ptr();
        OffsetPageTable::new(pml4, phys_off)
    }
}
