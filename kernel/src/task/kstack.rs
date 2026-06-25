//! Kernel-stack pool with virtual guard pages.
//!
//! Each slot occupies a fixed virtual region of [`KSTACK_SLOT_VSIZE`] bytes
//! split into:
//!
//! ```text
//!   slot_base                        slot_base + GUARD       slot_base + SLOT_VSIZE
//!   |                                |                       |
//!   v                                v                       v
//!   +----------- 4 KiB --------------+----------- 32 KiB ----+
//!   |       guard page (unmapped)    |   usable stack bytes  |
//!   +--------------------------------+-----------------------+
//! ```
//!
//! Stacks grow toward lower addresses, so a kernel-stack overflow hits the
//! unmapped guard page *first* and triggers an immediate ring-0 page fault
//! at a known virtual address. The page-fault handler recognises the
//! address range and prints a `KERNEL STACK OVERFLOW` diagnostic naming the
//! offending slot.
//!
//! Replaces the prior `.bss`-backed `STACK_POOL` layout, where an
//! overflowing stack silently stomped adjacent kernel statics (see
//! `docs/handoffs/2026-05-13-kernel-pipe-table-corruption.md`).
//!
//! ## Sizing
//!
//! [`MAX_KERNEL_STACKS`] covers every consumer that previously allocated a
//! 16-32 KiB stack from the kernel heap (Task kernel-mode stacks, process
//! syscall stacks, AP per-core syscall + double-fault stacks). See the
//! constant's docstring for the breakdown.
//!
//! ## Lock-free claim
//!
//! Slot ownership is tracked by a parallel array of [`AtomicBool`]s.
//! Allocation does a linear `compare_exchange` scan; on success, the caller
//! has exclusive mutable access to the slot's mapped virtual bytes for the
//! lifetime of the returned [`KernelStack`] guard. Deallocation zeros the
//! mapped bytes and releases the bit; the underlying frames stay mapped.

use core::sync::atomic::{AtomicBool, Ordering};

use x86_64::VirtAddr;
use x86_64::structures::paging::{Mapper, Page, PageTableFlags, Size4KiB};

/// Size of one kernel stack's usable region in bytes (64 KiB).
///
/// Bumped from the prior 32 KiB after the guard-page rework surfaced an
/// AP-boot stack overflow (~33 KiB peak) that previously spilled silently
/// into the adjacent `.bss` slot. With the guard page now catching that
/// overflow as a real fault, the usable region must accommodate worst-case
/// boot-time stack usage with a comfortable margin.
pub const KERNEL_STACK_SIZE: usize = 4096 * 16;

/// Size of the unmapped guard page below each stack (one 4 KiB page).
pub const KSTACK_GUARD_SIZE: usize = 4096;

/// Total virtual footprint per slot: guard page + usable stack.
pub const KSTACK_SLOT_VSIZE: usize = KSTACK_GUARD_SIZE + KERNEL_STACK_SIZE;

/// Base of the kernel-stack virtual region (PML4[257]).
///
/// PML4[256] holds the bootstrap heap (`HEAP_START..HEAP_START+64 MiB`) and
/// the bootloader's `phys_offset` linear mapping; placing kstacks in
/// PML4[257] keeps a clean 512 GiB region for ourselves with no risk of
/// collision. The entry is part of the kernel's upper half and is shared
/// into every per-process page table via `new_process_page_table`'s
/// PML4[256..512] copy, so kernel stacks remain addressable on every CR3.
pub const KSTACK_AREA_START: usize = 0xFFFF_8080_0000_0000;

/// Maximum number of concurrent kernel stacks.
///
/// Sized to cover every consumer that historically allocated a 16-32 KiB
/// stack from the kernel heap:
///
/// - One [`crate::task::Task`] kernel-mode stack per task (up to `MAX_TASKS`).
/// - One process-level syscall stack per userspace task — leaked, see
///   `kernel/src/process/mod.rs::alloc_kernel_stack` (up to `MAX_TASKS`).
/// - Two AP per-core stacks (syscall + double-fault IST) per non-BSP core —
///   leaked by `kernel/src/smp/mod.rs::init_ap_per_core` (up to
///   `2 × (MAX_CORES - 1)`).
///
/// The BSP's syscall and double-fault stacks live in `.bss` directly (see
/// `kernel/src/arch/x86_64/gdt.rs`), so they do not consume pool slots.
pub const MAX_KERNEL_STACKS: usize = 2 * crate::task::MAX_TASKS + 2 * (crate::smp::MAX_CORES - 1);

/// End of the kernel-stack virtual region (one past the last byte).
pub const KSTACK_AREA_END: usize = KSTACK_AREA_START + MAX_KERNEL_STACKS * KSTACK_SLOT_VSIZE;

#[allow(clippy::declare_interior_mutable_const)]
static STACK_USED: [AtomicBool; MAX_KERNEL_STACKS] = {
    const SLOT_FREE: AtomicBool = AtomicBool::new(false);
    [SLOT_FREE; MAX_KERNEL_STACKS]
};

/// Tripped by [`init`] once the pool's frames are mapped. Allocations that
/// race ahead of [`init`] hit the assertion in [`KernelStack::alloc`].
static POOL_READY: AtomicBool = AtomicBool::new(false);

/// Virtual address of slot `i`'s base (lowest byte, inside the guard page).
const fn slot_base(i: usize) -> usize {
    KSTACK_AREA_START + i * KSTACK_SLOT_VSIZE
}

/// Virtual address of slot `i`'s first usable byte (just past the guard).
const fn slot_usable_base(i: usize) -> usize {
    slot_base(i) + KSTACK_GUARD_SIZE
}

/// Virtual address one past slot `i`'s last usable byte (stack top).
const fn slot_top(i: usize) -> usize {
    slot_usable_base(i) + KERNEL_STACK_SIZE
}

/// Map the usable region of every pool slot, leaving each slot's bottom
/// 4 KiB unmapped as a guard page.
///
/// Allocates `MAX_KERNEL_STACKS × 8` frames from the buddy allocator and
/// maps them through the current PML4. Must be called exactly once, after
/// `mm::init` (so the heap, frame allocator, and `get_mapper` are live)
/// and before any task or AP boot consumes a slot.
pub fn init() {
    use crate::mm::frame_allocator;
    use crate::mm::paging::{GlobalFrameAlloc, get_mapper};

    assert!(
        !POOL_READY.load(Ordering::Acquire),
        "kstack::init called twice"
    );

    let mut mapper = unsafe { get_mapper() };
    let mut frame_alloc = GlobalFrameAlloc;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    let pages_per_slot = KERNEL_STACK_SIZE / 4096;
    let mut mapped_pages: usize = 0;

    for slot in 0..MAX_KERNEL_STACKS {
        let usable_base = slot_usable_base(slot);
        for page_idx in 0..pages_per_slot {
            let va = VirtAddr::new((usable_base + page_idx * 4096) as u64);
            let page: Page<Size4KiB> = Page::containing_address(va);
            let frame = frame_allocator::allocate_frame()
                .expect("kstack::init: out of frames mapping the kernel-stack pool");
            // SAFETY: each slot's virtual range is distinct (slot_idx +
            // page_idx tuple) and the region is owned by this module — no
            // aliased mapping exists.
            let flush = unsafe {
                mapper
                    .map_to(page, frame, flags, &mut frame_alloc)
                    .expect("kstack::init: map_to failed")
            };
            flush.flush();
            // Zero the freshly-mapped page so the debug_assert! in
            // KernelStack::alloc (which expects all-zero on claim) holds.
            unsafe {
                core::ptr::write_bytes(va.as_mut_ptr::<u8>(), 0, 4096);
            }
            mapped_pages += 1;
        }
    }

    POOL_READY.store(true, Ordering::Release);

    log::info!(
        "[kstack] pool ready: {} slots × {} KiB usable + 4 KiB guard at {:#x}..{:#x} ({} pages mapped)",
        MAX_KERNEL_STACKS,
        KERNEL_STACK_SIZE / 1024,
        KSTACK_AREA_START,
        KSTACK_AREA_END,
        mapped_pages,
    );
}

/// If `vaddr` falls inside a kstack guard page, return the slot index.
///
/// Called from the ring-0 page-fault handler to recognise stack overflows
/// at the source. Returns `None` for any address outside the kstack region
/// or that lands in a slot's usable area.
pub fn classify_guard_page_fault(vaddr: u64) -> Option<usize> {
    let v = vaddr as usize;
    if !(KSTACK_AREA_START..KSTACK_AREA_END).contains(&v) {
        return None;
    }
    let offset = v - KSTACK_AREA_START;
    let slot = offset / KSTACK_SLOT_VSIZE;
    let slot_offset = offset % KSTACK_SLOT_VSIZE;
    if slot_offset < KSTACK_GUARD_SIZE {
        Some(slot)
    } else {
        None
    }
}

/// Return the (usable_base, top) virtual addresses of slot `i`'s mapped
/// stack region — i.e. the bytes just above the guard page up to the stack
/// top. Used by the double-fault overflow diagnostic to scan the exhausted
/// stack for return addresses (the guard page itself is unmapped, so a scan
/// must start at the usable base, not at the faulting RSP).
pub fn slot_usable_bounds(i: usize) -> (u64, u64) {
    (slot_usable_base(i) as u64, slot_top(i) as u64)
}

/// RAII handle for one kernel-stack slot.
///
/// Owns exclusive access to the slot's mapped virtual range until dropped.
/// On drop, zeros the stack bytes (so a future claimant sees a clean
/// buffer) and releases the bit in [`STACK_USED`]. The mapping itself is
/// permanent — guard pages stay unmapped for the kernel's lifetime.
pub struct KernelStack {
    slot_idx: usize,
}

impl KernelStack {
    /// Claim a free slot from the pool. Returns `None` if the pool is
    /// exhausted (see [`MAX_KERNEL_STACKS`] for the cap).
    pub fn alloc() -> Option<Self> {
        debug_assert!(
            POOL_READY.load(Ordering::Acquire),
            "KernelStack::alloc before kstack::init"
        );
        for (i, used) in STACK_USED.iter().enumerate() {
            if used
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                // SAFETY: we just won the CAS for slot `i`, so we hold
                // exclusive access to its mapped bytes. The Drop impl is
                // the only path that releases the bit, and it runs after
                // the last `as_mut_slice` borrow is gone (RAII).
                let bytes = unsafe { slot_bytes_mut(i) };
                // The slot is zero-initialized at boot (init writes zero
                // through the fresh mapping) and re-zeroed on free, so this
                // is a no-op except as a defence-in-depth tripwire if a
                // future code path forgets to zero on free.
                debug_assert!(
                    bytes.iter().all(|&b| b == 0),
                    "kstack slot {} not zeroed before alloc",
                    i
                );
                return Some(KernelStack { slot_idx: i });
            }
        }
        None
    }

    /// Mutable view of the stack bytes.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: we own this slot exclusively (see `alloc`).
        unsafe { slot_bytes_mut(self.slot_idx) }
    }

    /// Immutable view of the stack bytes.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: we own this slot exclusively (see `alloc`).
        unsafe { slot_bytes(self.slot_idx) }
    }

    /// `(base, top)` virtual addresses of this stack — `base` is the lowest
    /// usable byte (just past the guard page), `top` is one past the last
    /// byte. Matches the prior `Box<[u8]>::as_ptr() / +len()` semantics.
    pub fn bounds(&self) -> (u64, u64) {
        (
            slot_usable_base(self.slot_idx) as u64,
            slot_top(self.slot_idx) as u64,
        )
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        // Zero before release so a subsequent allocator sees a clean buffer
        // (and so any stale saved-frame bytes can't be loaded by a misrouted
        // dispatch — defence in depth against the original GPF signature).
        // SAFETY: we still own the slot until the store below.
        unsafe { slot_bytes_mut(self.slot_idx).fill(0) };
        STACK_USED[self.slot_idx].store(false, Ordering::Release);
    }
}

/// SAFETY: caller must own slot `i` exclusively (via [`STACK_USED`] CAS).
unsafe fn slot_bytes_mut(i: usize) -> &'static mut [u8] {
    debug_assert!(i < MAX_KERNEL_STACKS);
    unsafe { core::slice::from_raw_parts_mut(slot_usable_base(i) as *mut u8, KERNEL_STACK_SIZE) }
}

/// SAFETY: caller must own slot `i` exclusively (via [`STACK_USED`] CAS).
unsafe fn slot_bytes(i: usize) -> &'static [u8] {
    debug_assert!(i < MAX_KERNEL_STACKS);
    unsafe { core::slice::from_raw_parts(slot_usable_base(i) as *const u8, KERNEL_STACK_SIZE) }
}

/// Claim a slot for a permanently-leaked kernel stack and return the
/// 16-byte-aligned top address (one past the highest byte, masked down to
/// 16-byte alignment).
///
/// Used by [`crate::process`] for per-process syscall stacks and by AP
/// per-core setup. The legacy design (`Box::into_raw`) intentionally leaked
/// the heap allocation; process-cleanup-time deallocation is deferred to a
/// later phase. Routing through the pool keeps the same leak semantics but
/// guarantees the memory has a guard page below it — overflowing the leaked
/// stack faults immediately at a known virtual address.
///
/// Panics if the pool is exhausted (the current pool size is documented at
/// [`MAX_KERNEL_STACKS`]).
pub fn alloc_leaked_top() -> u64 {
    debug_assert!(
        POOL_READY.load(Ordering::Acquire),
        "alloc_leaked_top before kstack::init"
    );
    for (i, used) in STACK_USED.iter().enumerate() {
        if used
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            // SAFETY: we just claimed slot `i`; nobody else can touch it. The
            // slot bit is left set forever (leak by design — see doc above).
            let top = slot_top(i) as u64;
            return top & !15;
        }
    }
    panic!(
        "kstack: leaked-stack pool exhausted (cap = {})",
        MAX_KERNEL_STACKS
    );
}
