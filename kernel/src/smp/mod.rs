//! Symmetric Multiprocessing (SMP) support.
//!
//! Provides per-core data structures, AP bootstrap, IPI infrastructure, and
//! TLB shootdown. Each core gets its own GDT, TSS, kernel stacks, and
//! scheduler state via [`PerCoreData`].
//!
//! # Per-core access
//!
//! Two mechanisms are provided:
//! - [`current_core_id`]: reads the LAPIC ID register and maps it to a core
//!   index. Works from any context but requires an MMIO read.
//! - [`per_core`]: reads `gs_base` (set to point at the core's [`PerCoreData`])
//!   for O(1) access without MMIO. Requires `gs_base` to have been initialized
//!   via [`init_bsp_per_core`] or the AP entry path.

#![allow(dead_code)]

pub mod boot;
pub mod ipi;
pub mod tlb;

extern crate alloc;

use alloc::{boxed::Box, collections::VecDeque};
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use x86_64::{
    VirtAddr,
    instructions::{segmentation::Segment, tables::load_tss},
    registers::segmentation::{CS, DS, SS},
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector},
        tss::TaskStateSegment,
    },
};

use crate::arch::x86_64::gdt::{DOUBLE_FAULT_IST_INDEX, NMI_IST_INDEX};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of cores supported.
pub const MAX_CORES: usize = 16;

/// Phase 57b C.1 — per-core boot/scheduler-context dummy `preempt_count`.
///
/// Every core's [`PerCoreData::current_preempt_count_ptr`] starts pointing at
/// `&SCHED_PREEMPT_COUNT_DUMMY[core_id]` and is retargeted to the dummy again
/// at every dispatch's switch-out epilogue (Phase 57b C.2).  The retarget
/// guarantees that scheduler-context `IrqSafeMutex::lock` / `Drop` pairs
/// (Phase 57b F.1, future wave) charge the same pointee on acquire and
/// release — the dummy.
///
/// The dummy is `pub` because [`crate::task::scheduler`] dereferences it from
/// the dispatch path's retarget block.  Only the owning core's scheduler
/// stack writes to its slot via `preempt_disable` / `preempt_enable`; reads
/// from other cores are never expected.  All accesses use atomic
/// `fetch_add` / `fetch_sub` so concurrent writes from a self-IPI handler are
/// well-defined.
pub static SCHED_PREEMPT_COUNT_DUMMY: [AtomicI32; MAX_CORES] =
    [const { AtomicI32::new(0) }; MAX_CORES];

/// Size of the dedicated double-fault stack per core (same as BSP).
const DOUBLE_FAULT_STACK_SIZE: usize = 4096 * 5; // 20 KiB

/// Size of the dedicated syscall/kernel stack per core (same as BSP).
const SYSCALL_STACK_SIZE: usize = 4096 * 4; // 16 KiB

// ---------------------------------------------------------------------------
// ISR wakeup queue (lock-free, per-core)
// ---------------------------------------------------------------------------

/// Per-core lock-free ISR wakeup queue.
///
/// ISR context pushes task indices (lock-free SPSC producer).
/// Scheduler loop drains entries (single consumer).
///
/// The ring buffer holds up to 31 entries (one slot is always unused to
/// distinguish full from empty). `u64::MAX` is the sentinel for empty slots.
/// On overflow the push is silently dropped -- the fallback
/// `drain_pending_waiters()` in the scheduler loop will catch it.
pub struct IsrWakeQueue {
    buffer: [AtomicU64; 32],
    /// Write position (ISR advances).
    head: AtomicUsize,
    /// Read position (scheduler advances).
    tail: AtomicUsize,
}

/// Sentinel value stored in empty ring-buffer slots.
const ISR_WAKE_EMPTY: u64 = u64::MAX;

impl IsrWakeQueue {
    /// Create a new empty queue with all slots set to the empty sentinel.
    #[allow(clippy::declare_interior_mutable_const)]
    pub const fn new() -> Self {
        // const-init each AtomicU64 to the sentinel value.
        const EMPTY: AtomicU64 = AtomicU64::new(ISR_WAKE_EMPTY);
        Self {
            buffer: [EMPTY; 32],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Push a task index to the queue (lock-free, ISR-safe).
    ///
    /// Returns `false` if the queue is full (no panic from ISR context!).
    pub fn push(&self, task_idx: usize) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let next = (head + 1) % 32;
        // Full when next would collide with the consumer's tail.
        if next == self.tail.load(Ordering::Acquire) {
            return false;
        }
        self.buffer[head].store(task_idx as u64, Ordering::Relaxed);
        self.head.store(next, Ordering::Release);
        true
    }

    /// Drain all pending entries. Yields task indices until the queue is empty.
    ///
    /// Only called from the scheduler loop on the owning core (single consumer).
    pub fn drain(&self) -> IsrWakeDrain<'_> {
        IsrWakeDrain { queue: self }
    }
}

/// Iterator returned by [`IsrWakeQueue::drain`].
pub struct IsrWakeDrain<'a> {
    queue: &'a IsrWakeQueue,
}

impl Iterator for IsrWakeDrain<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        let tail = self.queue.tail.load(Ordering::Relaxed);
        let head = self.queue.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        let val = self.queue.buffer[tail].load(Ordering::Relaxed);
        // Reset the slot to the sentinel (not strictly required but hygienic).
        self.queue.buffer[tail].store(ISR_WAKE_EMPTY, Ordering::Relaxed);
        let next_tail = (tail + 1) % 32;
        self.queue.tail.store(next_tail, Ordering::Release);
        if val == ISR_WAKE_EMPTY {
            // Sentinel should never appear in a valid entry; skip it.
            self.next()
        } else {
            Some(val as usize)
        }
    }
}

// ---------------------------------------------------------------------------
// Per-core data
// ---------------------------------------------------------------------------

/// Per-core state block.
///
/// Each core has one of these, initialized during BSP init or AP bootstrap.
/// The `gs_base` MSR points to this struct so that `per_core()` can retrieve
/// it in O(1) without MMIO.
///
/// `align(4096)` (Phase 110 A.3b part 4): every core's `PerCoreData` is mapped
/// whole into every KPTI user half (the entry asm reads `gs:[…]` before the
/// CR3 switch — no swapgs in m3OS), so the Box allocation must own its pages
/// exclusively; page alignment + page-multiple size (Rust rounds size up to
/// alignment) keep adjacent heap data out of the mapped pages. Field offsets
/// are unchanged (`repr(C)` layout is alignment-independent), and the
/// `smp::offsets` constants are `offset_of!`-computed so they track anyway.
#[repr(C, align(4096))]
pub struct PerCoreData {
    /// Self-pointer at offset 0 — reserved for future `gs:[0]` access.
    /// Currently unused: `per_core()` reads `IA32_GS_BASE` via `rdmsr`.
    self_ptr: *const PerCoreData,
    /// Logical core index (0 = BSP, 1..n = APs in MADT order).
    pub core_id: u8,
    /// LAPIC ID from the MADT.
    pub apic_id: u8,
    /// Set to `true` once this core has completed initialization.
    pub is_online: AtomicBool,
    /// Phase 57 DEBUG: countdown for the per-core "reschedule IPI
    /// received" INFO log. Initialized to 4 in both BSP and AP
    /// per-core data constructors; the IPI handler decrements with
    /// `fetch_sub(1, Relaxed)` and logs while the pre-decrement value
    /// is positive. Caps the log to the first 4 IPIs each core
    /// receives so the transcript stays readable.
    pub ipi_recv_log_budget: core::sync::atomic::AtomicI32,
    /// Pointer to this core's TSS (for runtime RSP0 updates).
    tss_ptr: *mut TaskStateSegment,
    /// Pointer to this core's GDT (pre-allocated on BSP, loaded on AP).
    gdt_ptr: *const GlobalDescriptorTable,
    /// Segment selectors for this core's GDT.
    gdt_code: SegmentSelector,
    gdt_data: SegmentSelector,
    gdt_tss: SegmentSelector,
    /// Top of this core's syscall/kernel stack.
    pub kernel_stack_top: u64,
    /// Scheduler loop RSP for this core (replaces the global `SCHEDULER_RSP`).
    /// Uses `UnsafeCell` for interior mutability — only written via `switch_context`
    /// on the owning core.
    pub scheduler_rsp: core::cell::UnsafeCell<u64>,
    /// Per-core reschedule flag (replaces the global `RESCHEDULE`).
    pub reschedule: AtomicBool,
    /// Index of the task currently running on this core in the global task vec.
    /// -1 means no task (scheduler loop is running).
    pub current_task_idx: core::sync::atomic::AtomicI32,
    /// Pointer to the [`crate::task::Task`] currently running on this core,
    /// or null when the scheduler loop is running. Set/cleared by the
    /// dispatch path next to `current_task_idx` (under the same scheduler
    /// lock acquisition that reads `tasks[idx]`'s stable address). Read by
    /// IRQ-context CPU-time and rusage helpers so they can mutate the
    /// running task's atomic counters without touching the scheduler lock
    /// (Linux's `task_struct` per-CPU `current` pointer pattern).
    pub current_task_ptr: AtomicPtr<crate::task::Task>,
    /// LAPIC virtual base address (phys_offset + LAPIC phys addr).
    /// Stored here so APs can access it without touching kernel statics.
    pub lapic_virt_base: u64,
    /// LAPIC timer ticks per millisecond (BSP-calibrated, shared by all cores).
    pub lapic_ticks_per_ms: u32,

    // ----- Phase 35: per-core run queue -----
    /// Per-core run queue of task indices into the global `SCHEDULER.tasks` vec.
    pub run_queue: spin::Mutex<VecDeque<usize>>,

    // ----- Phase 35: per-core syscall state (accessed via gs-relative asm) -----
    /// Top of this core's kernel syscall stack for SYSCALL entry.
    pub syscall_stack_top: u64,
    /// User RSP saved by `syscall_entry` assembly stub.
    ///
    /// Per-core for ABI reasons: the syscall-entry asm writes the slot via
    /// gs-relative addressing (`SYSCALL_USER_RSP` offset), and the sysret
    /// tail reads it the same way.  The canonical source of truth is the
    /// per-task [`crate::task::TaskSyscallSnapshot::user_rsp`]; the per-core
    /// slot is a mirror that is reliably refreshed from the per-task
    /// snapshot on every dispatch with IRQs masked (see Phase 57e Bug #4
    /// fix in `scheduler::run`, immediately before `switch_context`).
    ///
    /// Read sites:
    ///  - syscall-entry / sysret asm (via `SYSCALL_USER_RSP`) — IRQs masked.
    ///  - `snapshot_user_return_state()` inside `syscall_handler` — IRQs
    ///    enabled at this point, but safe because the dispatcher already
    ///    populated the slot from this task's per-task snapshot before
    ///    handing control back, and a kernel-mode preempt that switches
    ///    away will, on resume, re-mirror the slot from the per-task
    ///    snapshot before any user-visible code runs.
    pub syscall_user_rsp: u64,
    /// R10 (syscall arg3) saved by `syscall_entry` assembly stub.
    pub syscall_arg3: u64,
    /// Phase 57e Bug #3 fix — pointer to the **current task's**
    /// [`crate::task::TaskSyscallSnapshot`].  Updated by the dispatcher on
    /// every dispatch, before `switch_context`.  The syscall-entry asm
    /// loads this pointer and writes the user GPR snapshot through it, so
    /// two tasks sharing a core can no longer alias the per-core slots.
    /// `make_fork_ctx` and the syscall handlers that consume `r8`/`r9`/etc.
    /// as extra args read the snapshot through this same pointer.
    /// Raw pointer because the asm uses gs-relative indirection; the
    /// snapshot itself lives in the [`crate::task::Task`] struct (heap,
    /// stable address per Track B's `Vec<Box<Task>>` storage discipline).
    /// Null only during very early boot before the first dispatch.
    pub current_syscall_snapshot_ptr: core::cell::UnsafeCell<*mut crate::task::TaskSyscallSnapshot>,

    /// PID of the userspace process currently running on this core.
    /// 0 = no userspace process (kernel task context).
    pub current_pid: AtomicU32,

    /// Pointer to the AddressSpace currently active on this core.
    /// Raw pointer because PerCoreData does not own the AddressSpace
    /// (the Process does via Arc). Null when no user address space is loaded.
    pub current_addrspace: *const crate::mm::AddressSpace,

    /// Fork child entry context — per-core so each core can handle `fork()`
    /// independently without corrupting another core's saved context.
    pub fork_entry_ctx: crate::arch::x86_64::ForkEntryCtx,

    // ----- Phase 52: per-core ISR wakeup queue -----
    /// Lock-free queue for ISR-to-scheduler wakeup delivery.
    ///
    /// ISRs (e.g. keyboard interrupt via `signal_irq`) push task indices here.
    /// The scheduler loop drains entries on each iteration, waking blocked tasks
    /// without requiring the ISR to acquire any mutex.
    pub isr_wake_queue: IsrWakeQueue,

    // ----- Phase 53a: per-CPU page cache (A.1) -----
    /// Per-CPU cache of physical frames for lock-free fast-path allocation/free.
    /// Only accessed by the owning core (with interrupts masked).
    pub page_cache: core::cell::UnsafeCell<crate::mm::frame_allocator::PerCpuPageCache>,

    /// Atomic shadow of the per-CPU page cache count.  Updated by the owning
    /// core whenever the local page cache is mutated.  Read by remote cores
    /// for statistics (avoids UB from reading the non-atomic `UnsafeCell`).
    pub page_cache_count: AtomicUsize,

    // ----- Phase 53a: per-CPU slab magazines (B.3) -----
    /// Per-CPU magazine pairs for each of the 13 slab size classes.
    /// Only accessed by the owning core (with interrupts masked).
    pub slab_magazines: core::cell::UnsafeCell<crate::mm::slab::PerCpuMagazines>,

    // ----- Phase 53a: per-CPU cross-CPU free lists (E.1) -----
    /// Per-size-class atomic MPSC free lists for cross-CPU slab frees.
    /// Any CPU may CAS-push to these lists; only the owning core collects.
    pub cross_cpu_free: crate::mm::slab::CrossCpuFreeLists,

    // ----- Phase 43b: per-core trace ring -----
    /// Lockless ring buffer of recent kernel trace events (scheduler, fork, IPC).
    /// Written only by the owning core; read by panic/fault dump and `sys_ktrace`.
    ///
    /// Size 128 (not 4096, not 256) — the bootloader's 80 KiB kernel stack
    /// can't hold a `Box::new(PerCoreData { ... })` literal whose `trace_ring`
    /// alone is more than ~10 KiB, because debug builds don't elide the
    /// construct-on-stack-then-memcpy-to-heap intermediate.  At 4096 entries
    /// the inline ring was ~224 KiB and overflowed the stack on every
    /// `init_bsp_per_core` call (the symptom: double-fault inside
    /// `kernel::mm::frame_allocator::tests::allocate_frame_hot_path_tolerates_reentrant_free`,
    /// RSP near the unmapped phys-offset boundary, CR2 = phys 0x438 unmapped).
    /// At 256 entries the ring is ~14 KiB after the YieldNow `caller_file`
    /// fields landed (568e5f6) — still tight enough to overflow when combined
    /// with the rest of `PerCoreData` and active stack frames.  128 entries
    /// (~7 KiB) is the largest size that comfortably fits the test harness's
    /// stack budget in debug builds.  If a future workload needs deeper
    /// history, swap to `Box<TraceRing<N>>` initialised via `Box::new_zeroed`
    /// so the ring lives directly on the heap with no stack-resident copy.
    /// Phase 57e deferral cleanup, 2026-05-07.
    #[cfg(feature = "trace")]
    pub trace_ring: core::cell::UnsafeCell<kernel_core::trace_ring::TraceRing<128>>,

    // ----- Phase 57a B.3: lock-ordering guard -----
    /// Set to `true` while this core holds `SCHEDULER.lock`.
    ///
    /// Read by [`Task::with_block_state`] to enforce the pi_lock-is-outer
    /// invariant: acquiring `pi_lock` while holding `SCHEDULER.lock` is
    /// forbidden (Linux `p->pi_lock` → `rq->lock` ordering).  Only accessed
    /// with `Relaxed` ordering — correctness relies on the CPU's program order,
    /// not cross-core visibility, since both the set/clear and the check occur
    /// on the same core.
    pub holds_scheduler_lock: AtomicBool,

    // ----- Phase 57d E.1: deferred-reschedule pending flag -----
    /// Set by `preempt_enable` zero-crossing when `reschedule` is true;
    /// consumed at the next user-mode return boundary. Phase 57d E.1.
    pub preempt_resched_pending: AtomicBool,

    // ----- Phase 57b C.1: per-CPU preempt_count pointer -----
    /// Pointer to the `AtomicI32` that `preempt_disable` / `preempt_enable`
    /// must mutate on this core right now.
    ///
    /// # Invariants
    ///
    /// - The pointer is **always** valid (non-null and pointing at live
    ///   memory).  At boot it targets `&SCHED_PREEMPT_COUNT_DUMMY[core_id]`,
    ///   which is `'static`.  During task execution it targets the running
    ///   task's `Task::preempt_count`.  Track B's `Vec<Box<Task>>` storage
    ///   keeps the cached `Task::preempt_count` address stable across
    ///   `Vec` reallocations.
    /// - The pointer is updated **only** by Phase 57b C.2 (switch-out
    ///   retarget — back to the dummy) and Phase 57b C.3 (switch-in
    ///   retarget — to the incoming task) on the dispatch path.  Both
    ///   updates run inside an interrupt-masked window so no IRQ-context
    ///   `preempt_disable` can observe a half-updated pointer.
    /// - Future helpers (`preempt_disable` / `preempt_enable`, Phase 57b
    ///   D.2) read this pointer with `Acquire` and never take any lock.
    ///   That lock-freedom is what lets Phase 57b F.1 wire
    ///   `preempt_disable` into `IrqSafeMutex::lock` without recursion.
    ///
    /// Stored as an `AtomicPtr<AtomicI32>` rather than a plain pointer so
    /// that retarget store / counter-helper load can use `Release` / `Acquire`
    /// ordering for cross-core visibility on retarget boundaries.
    pub current_preempt_count_ptr: AtomicPtr<AtomicI32>,

    // ----- Phase 84 Track A (KPTI): per-core CR3 trampoline state -----
    //
    // Read by the KPTI entry/exit asm via `gs:[OFFSET]` (valid on EITHER CR3
    // because the PerCoreData page is in the user-PML4 minimal entry set). Set
    // via `publish_kpti_cr3_pair` at every dispatch locus when `kpti_active`
    // (Phase 110 A.4). All zero when KPTI is inactive (`mitigations=off` /
    // `auto` on `RDCL_NO` silicon: the non-KPTI SYSCALL stub is installed and
    // every other reader keys off the zero — the paranoid NMI/`#DF` path in
    // particular loads `kpti_kernel_cr3` whenever it is non-zero, which is why
    // the publish helper must stay a no-op on inactive boots).
    /// Kernel-half PML4 phys for the active task (the CR3 to load on ring-3→0).
    pub kpti_kernel_cr3: u64,
    /// User-half PML4 phys for the active task (the CR3 to load on 0→ring-3).
    pub kpti_user_cr3: u64,
    /// Scratch slot the entry asm spills a register into across the CR3 switch
    /// (the SYSCALL path has no free GPR at entry; this lives in the user
    /// minimal set so it is writable on the user CR3).
    pub kpti_scratch: u64,
}

// Safety: PerCoreData is only accessed by its owning core (via gs_base) or
// through atomic fields (is_online, reschedule). The raw pointers (self_ptr,
// tss_ptr) are only dereferenced on the owning core.
unsafe impl Send for PerCoreData {}
unsafe impl Sync for PerCoreData {}

impl PerCoreData {
    /// Phase 57b G.8 — task-context wrapper around [`PerCoreData::run_queue`].
    ///
    /// `run_queue` is an IRQ-shared `spin::Mutex` (per Track A.1 audit row
    /// `kernel/src/smp/mod.rs:194`): `signal_reschedule` and the dispatch
    /// path on the wake side reach this lock from IRQ context, so converting
    /// to `IrqSafeMutex` would not work — the ISR side already runs with
    /// IF=0 and never raises the per-task `preempt_count`. Task-context
    /// callsites must therefore explicitly `preempt_disable` +
    /// `interrupts::without_interrupts` + `run_queue.lock()` +
    /// `preempt_enable` so the F.1 preempt-discipline stays balanced.
    ///
    /// This helper wraps every task-context acquisition of `run_queue` so
    /// the boilerplate lives in one place and every callsite shares the
    /// same shape (matches G.1.c `with_driver`, G.5.c `with_unit_slots`).
    /// The closure receives `&mut VecDeque<usize>` so callers can mutate
    /// the queue uniformly.
    ///
    /// **Do not call this from an ISR**: interrupt handlers already run
    /// with IF=0 and follow their own discipline (no kernel preemption,
    /// no nested `preempt_disable`).
    ///
    /// Lock-ordering: `preempt_disable` is lock-free (Phase 57b D.2), so
    /// calling it before `without_interrupts` cannot recurse.
    #[inline]
    pub fn with_run_queue<R>(&self, f: impl FnOnce(&mut VecDeque<usize>) -> R) -> R {
        crate::task::scheduler::preempt_disable();
        let result = x86_64::instructions::interrupts::without_interrupts(|| {
            let mut q = self.run_queue.lock();
            f(&mut q)
        });
        crate::task::scheduler::preempt_enable();
        result
    }

    /// Phase 110 Track A.3 (KPTI) — `[(base_va, size)]` of this core's
    /// interrupt-delivery structures the user-half entry set must map: its
    /// `PerCoreData`, its GDT, and its TSS. The CPU reads the GDT/TSS through
    /// the *active* paging when delivering a ring-3 → ring-0 interrupt on this
    /// core, and the KPTI entry asm reads `gs:` (this `PerCoreData`) before the
    /// CR3 switch — so all three of *this core's* structures must be present in
    /// the user PML4 of any process that may run here. BSP (`gdt_ptr`/`tss_ptr`
    /// null) falls back to the global `gdt.rs` GDT/TSS.
    pub fn entry_struct_extents(&self) -> [(u64, u64); 3] {
        let pcd = (
            self as *const PerCoreData as u64,
            core::mem::size_of::<PerCoreData>() as u64,
        );
        let gdt = if self.gdt_ptr.is_null() {
            crate::arch::x86_64::gdt::gdt_extent()
        } else {
            (
                self.gdt_ptr as u64,
                core::mem::size_of::<GlobalDescriptorTable>() as u64,
            )
        };
        let tss = if self.tss_ptr.is_null() {
            crate::arch::x86_64::gdt::tss_extent()
        } else {
            (
                self.tss_ptr as u64,
                core::mem::size_of::<TaskStateSegment>() as u64,
            )
        };
        [pcd, gdt, tss]
    }

    /// Phase 110 Track A.3b (KPTI) — this core's NMI and #DF IST stack tops
    /// (`interrupt_stack_table[NMI_IST_INDEX]`, `[DOUBLE_FAULT_IST_INDEX]`).
    ///
    /// The paranoid NMI/#DF stubs run on these IST stacks; the CPU switches RSP
    /// to the IST top and pushes the trap frame there on the *active* (user) CR3
    /// before the stub can switch, so the user-half entry set must map each
    /// stack's top page. BSP (`tss_ptr` null) falls back to the global TSS.
    pub fn ist_top_pages(&self) -> [u64; 2] {
        if self.tss_ptr.is_null() {
            crate::arch::x86_64::gdt::bsp_ist_tops()
        } else {
            // SAFETY: `tss_ptr` points at this core's live TSS (set at AP init);
            // we only read the IST table entries (VirtAddr values).
            let tss = unsafe { &*self.tss_ptr };
            [
                tss.interrupt_stack_table[NMI_IST_INDEX as usize].as_u64(),
                tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize].as_u64(),
            ]
        }
    }
}

/// Global array of per-core data pointers. Indexed by logical core_id (0 = BSP).
/// Null until the core is initialized.
static mut PER_CORE_DATA: [*mut PerCoreData; MAX_CORES] = [core::ptr::null_mut(); MAX_CORES];

/// Number of cores discovered in the MADT (BSP + APs).
static CORE_COUNT: AtomicU8 = AtomicU8::new(0);

/// APIC ID → core_id lookup table. Index is APIC ID, value is core_id.
/// Supports APIC IDs up to 255. 0xFF means unmapped.
static mut APIC_TO_CORE: [u8; 256] = [0xFF; 256];

/// BSP's LAPIC ID, recorded during init.
static BSP_APIC_ID: AtomicU8 = AtomicU8::new(0);

// ---------------------------------------------------------------------------
// Core ID lookup (T003)
// ---------------------------------------------------------------------------

/// Return the logical core ID of the calling core.
///
/// Reads the LAPIC ID register (MMIO) and maps it to a core index.
/// Returns 0 for the BSP. Panics in debug builds if the APIC ID is unknown.
pub fn current_core_id() -> u8 {
    let apic_id = read_lapic_id();
    let core_id = unsafe { APIC_TO_CORE[apic_id as usize] };
    debug_assert_ne!(core_id, 0xFF, "unknown APIC ID {}", apic_id);
    core_id
}

/// Read the current core's LAPIC ID from the LAPIC ID register.
pub(crate) fn read_lapic_id() -> u8 {
    let lapic_base = {
        let phys = crate::acpi::local_apic_address() as u64;
        (crate::mm::phys_offset() + phys) as usize
    };
    // LAPIC ID register is at offset 0x020; ID is in bits 24-31.
    let raw = unsafe { core::ptr::read_volatile((lapic_base + 0x020) as *const u32) };
    (raw >> 24) as u8
}

/// Return the number of cores (BSP + APs).
pub fn core_count() -> u8 {
    CORE_COUNT.load(Ordering::Acquire)
}

// ---------------------------------------------------------------------------
// Phase 99 (Track C.1) — panic-path AP quiesce
// ---------------------------------------------------------------------------
//
// At 4 GiB + KVM + SMP an intermittent panic's banner is unreadable because
// `handle_panic` prints + dumps the trace rings while sibling cores keep
// scheduling and writing to COM1 → byte-interleaved garbage
// (docs/handoffs/2026-06-05-4gib-smp-panic-corrupted-output.md). The panicking
// core broadcasts a halt NMI to its siblings and waits a BOUNDED grace window
// for them to park BEFORE it prints, so the banner lands on a quiet bus. NMI
// (not a fixed IPI) is used so an IF=0 sibling still stops; the sibling's NMI
// handler (`arch::x86_64::interrupts::nmi_handler`) sees `panic_in_progress`
// and, if it is not the panic owner, acks + parks in `hlt_loop` on its clean
// NMI IST stack (NMI-on-IST landed 2026-06-14).
//
// Re-entrancy: the first panicker wins `PANIC_IN_PROGRESS` via CAS and owns all
// output; any core that panics afterward (or is mid-`handle_panic` when the NMI
// lands) parks without printing, so a second panic during the dump cannot
// re-corrupt the banner. Mirrors Linux `panic_smp_self_stop` / `smp_send_stop`.

/// Set once the first panicking core wins the panic race; read by the NMI
/// handler to decide whether to park.
static PANIC_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Bit `i` set when core `i` has acknowledged the panic-stop NMI and parked.
static PANIC_STOP_ACK: AtomicU64 = AtomicU64::new(0);

/// Sentinel for "no panic owner stamped yet".
const PANIC_OWNER_NONE: u8 = 0xFF;

/// Logical core id of the panic owner (the core printing the banner). The NMI
/// handler must NOT park this core. Sentinel [`PANIC_OWNER_NONE`] = no owner yet.
static PANIC_OWNER_CORE: AtomicU8 = AtomicU8::new(PANIC_OWNER_NONE);

/// True once a core has begun the panic-stop sequence.
#[inline]
pub fn panic_in_progress() -> bool {
    PANIC_IN_PROGRESS.load(Ordering::Acquire)
}

/// Whether the NMI handler should park `my_core` because a panic is in progress.
///
/// Parks ONLY when a panic owner has been **stamped** (`PANIC_OWNER_CORE !=
/// PANIC_OWNER_NONE`) and it is **not** this core. This closes the self-park
/// window: `panic_quiesce_aps` publishes `PANIC_IN_PROGRESS = true` (CAS) a few
/// instructions BEFORE it stamps `PANIC_OWNER_CORE`. TLB shootdowns are
/// NMI-delivered (`smp::tlb`), so the owner can take a stray shootdown NMI in
/// that window; if the handler parked on "not yet the owner" it would wedge the
/// owner forever and the banner would never print — strictly worse than the
/// interleave this feature fixes. Treating "owner unknown" as do-not-park lets
/// the owner finish stamping itself (and the bounded grace window absorbs the
/// rare case where a sibling briefly ran an extra shootdown before its own
/// park NMI, which is always sent AFTER the owner is stamped).
#[inline]
pub fn panic_should_park(my_core: u8) -> bool {
    let owner = PANIC_OWNER_CORE.load(Ordering::Acquire);
    owner != PANIC_OWNER_NONE && owner != my_core
}

/// True if `core_id` is the panic owner (it must not self-park on a stray NMI
/// while it is printing the banner).
#[inline]
pub fn is_panic_owner(core_id: u8) -> bool {
    PANIC_OWNER_CORE.load(Ordering::Acquire) == core_id
}

/// Quiesce sibling cores before the panic banner prints (Track C.1).
///
/// Returns `true` if THIS core won the panic race and should proceed to print;
/// `false` if another core is already the panic owner (the caller must then
/// just `hlt_loop` without printing, to avoid re-corrupting the banner).
///
/// On a win: stamps this core as the owner, broadcasts a halt NMI to every
/// other online core, and spins a BOUNDED grace window for them to ack-and-park
/// — a wedged core that never acks does NOT hang this, the window times out and
/// we print anyway. Single-core / pre-SMP boot returns `true` immediately with
/// no NMIs sent. Never allocates; safe from the panic handler.
pub fn panic_quiesce_aps() -> bool {
    // Pre-SMP boot: no siblings, and `send_nmi_to_core` would no-op.
    let my_core = match try_per_core() {
        Some(pc) => pc.core_id,
        None => return true,
    };

    // First panicker wins; everyone else parks without printing.
    if PANIC_IN_PROGRESS
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    // WINDOW: `PANIC_IN_PROGRESS` is now true but the owner is not yet stamped.
    // A stray NMI to THIS core in this gap must NOT park it — `panic_should_park`
    // returns false while the owner is `PANIC_OWNER_NONE`, so the owner survives
    // the window. The store below is `Release` so any core that later observes a
    // stamped owner also observes a consistent value.
    PANIC_OWNER_CORE.store(my_core, Ordering::Release);

    // Build the target mask (all online cores except self) and NMI each.
    let n = core_count();
    let mut target_mask: u64 = 0;
    for core_id in 0..n {
        if core_id == my_core {
            continue;
        }
        if let Some(data) = get_core_data(core_id)
            && data.is_online.load(Ordering::Acquire)
        {
            target_mask |= 1u64 << core_id;
            ipi::send_nmi_to_core(core_id);
        }
    }
    if target_mask == 0 {
        return true; // nobody to wait for
    }

    // Bounded grace window: spin until every target acks, or a fixed iteration
    // budget elapses. A genuinely-wedged core may never ack — do NOT hang the
    // panic path on it. 200M `pause` iterations is ~hundreds of ms to ~1 s on a
    // multi-GHz KVM guest; a panic is not latency-critical and siblings normally
    // ack in microseconds, so the budget is only a wedged-core backstop.
    const SPIN_BUDGET: u64 = 200_000_000;
    let mut spun: u64 = 0;
    while (PANIC_STOP_ACK.load(Ordering::Acquire) & target_mask) != target_mask {
        core::hint::spin_loop();
        spun += 1;
        if spun >= SPIN_BUDGET {
            break;
        }
    }
    true
}

/// Acknowledge the panic-stop NMI and park forever. Called from the NMI handler
/// on a sibling core when a panic is in progress and this core is not the owner.
pub fn panic_stop_ack_and_park() -> ! {
    if let Some(pc) = try_per_core() {
        PANIC_STOP_ACK.fetch_or(1u64 << pc.core_id, Ordering::Release);
    }
    // `hlt_loop` marks this core offline (so any later shootdown from the owner
    // excludes it) and halts. In NMI context this never IRETQs — exactly what we
    // want: the core stays frozen and quiet on COM1 while the owner prints.
    crate::hlt_loop()
}

// ---------------------------------------------------------------------------
// Phase 111 Track C.4 — kgdb all-stop quiesce (releasable, unlike panic-stop)
// ---------------------------------------------------------------------------
//
// When the in-kernel GDB stub takes a trap it must freeze every OTHER core so
// the developer inspects a still machine, then release them on continue. This
// reuses the panic-quiesce NMI mechanism but is *releasable*: a parked core
// spins inside its NMI handler until the stub owner clears `KGDB_STOP` and then
// `iretq`s back to exactly where it was — whereas panic-stop parks forever.
// Only compiled under the `kgdb` feature.

/// Set while the stub owns the machine; read by the NMI handler to decide
/// whether to park (and by the owner to release).
#[cfg(feature = "kgdb")]
static KGDB_STOP: AtomicBool = AtomicBool::new(false);

/// Logical core id of the stub owner (must not park itself).
#[cfg(feature = "kgdb")]
static KGDB_OWNER_CORE: AtomicU8 = AtomicU8::new(PANIC_OWNER_NONE);

/// Bit `i` set while core `i` is parked in the kgdb NMI wait loop.
#[cfg(feature = "kgdb")]
static KGDB_STOP_ACK: AtomicU64 = AtomicU64::new(0);

/// True while the stub has the machine all-stopped.
#[cfg(feature = "kgdb")]
#[inline]
pub fn kgdb_stop_requested() -> bool {
    KGDB_STOP.load(Ordering::Acquire)
}

/// True if `my_core` is the stub owner (it must not self-park on a stray NMI).
#[cfg(feature = "kgdb")]
#[inline]
pub fn kgdb_is_owner(my_core: u8) -> bool {
    KGDB_OWNER_CORE.load(Ordering::Acquire) == my_core
}

/// Freeze every other online core into the kgdb NMI wait loop and wait (bounded)
/// for them to park. Called by the stub on entry, before it serves any packet.
/// Returns the mask of cores parked (for the release symmetry / sentinel).
/// Pre-SMP boot (no siblings) is a no-op returning 0.
#[cfg(feature = "kgdb")]
pub fn kgdb_stop_all_aps() -> u64 {
    let my_core = match try_per_core() {
        Some(pc) => pc.core_id,
        None => {
            // Pre-SMP: stamp an owner anyway so `kgdb_is_owner` is meaningful.
            KGDB_OWNER_CORE.store(0, Ordering::Release);
            KGDB_STOP.store(true, Ordering::Release);
            return 0;
        }
    };
    KGDB_STOP_ACK.store(0, Ordering::Release);
    KGDB_OWNER_CORE.store(my_core, Ordering::Release);
    KGDB_STOP.store(true, Ordering::Release);

    let n = core_count();
    let mut target_mask: u64 = 0;
    for core_id in 0..n {
        if core_id == my_core {
            continue;
        }
        if let Some(data) = get_core_data(core_id)
            && data.is_online.load(Ordering::Acquire)
        {
            target_mask |= 1u64 << core_id;
            ipi::send_nmi_to_core(core_id);
        }
    }
    if target_mask == 0 {
        return 0;
    }
    // Bounded wait for every target to park — a wedged core must not hang the
    // debugger. ~200M pause iterations (~hundreds of ms) is generous; the stub
    // proceeds anyway on timeout (a non-parked core is a diagnosable anomaly,
    // not a reason to deadlock the operator's session).
    const SPIN_BUDGET: u64 = 200_000_000;
    let mut spun: u64 = 0;
    while (KGDB_STOP_ACK.load(Ordering::Acquire) & target_mask) != target_mask {
        core::hint::spin_loop();
        spun += 1;
        if spun >= SPIN_BUDGET {
            break;
        }
    }
    target_mask
}

/// Release the cores frozen by [`kgdb_stop_all_aps`]. Called by the stub on
/// `c`/`s`/`D`/`k`. Each parked core observes `KGDB_STOP == false`, exits its
/// NMI wait loop, and `iretq`s back to its interrupted context.
#[cfg(feature = "kgdb")]
pub fn kgdb_release_aps() {
    KGDB_OWNER_CORE.store(PANIC_OWNER_NONE, Ordering::Release);
    KGDB_STOP.store(false, Ordering::Release);
}

/// Acknowledge the kgdb-stop NMI and spin until the stub owner releases us, then
/// return so the NMI handler `iretq`s back to the interrupted context. Called
/// from the NMI handler on a non-owner core while a kgdb stop is in progress.
/// Unlike [`panic_stop_ack_and_park`] this RETURNS — the core resumes on release.
#[cfg(feature = "kgdb")]
pub fn kgdb_ack_and_wait() {
    let my_core = try_per_core().map(|pc| pc.core_id).unwrap_or(0xFF);
    if my_core != 0xFF {
        KGDB_STOP_ACK.fetch_or(1u64 << my_core, Ordering::Release);
    }
    while KGDB_STOP.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    if my_core != 0xFF {
        KGDB_STOP_ACK.fetch_and(!(1u64 << my_core), Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Per-core access via gs_base (T004)
// ---------------------------------------------------------------------------

/// Dedicated flag set after `init_bsp_per_core()` completes.
///
/// Using a dedicated `AtomicBool` instead of checking `gs_base != 0` avoids
/// false positives when firmware leaves a non-zero `gs_base` value before
/// SMP init runs.
static SMP_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Check if per-core data is initialized on the calling core.
///
/// Returns `false` during early boot before `init_bsp_per_core()` has been
/// called. Used by `signal_reschedule()` to avoid accessing gs_base before
/// it's set.
pub fn is_per_core_ready() -> bool {
    SMP_INITIALIZED.load(Ordering::Acquire)
}

/// Return a reference to the calling core's [`PerCoreData`], or `None` if
/// per-core data has not been initialized yet.
///
/// Safe to call from ISR context — never panics.
pub fn try_per_core() -> Option<&'static PerCoreData> {
    if !SMP_INITIALIZED.load(Ordering::Acquire) {
        return None;
    }
    let ptr = read_gs_base();
    if ptr == 0 {
        return None;
    }
    Some(unsafe { &*(ptr as *const PerCoreData) })
}

/// Return a reference to the calling core's [`PerCoreData`].
///
/// Reads the `IA32_GS_BASE` MSR, which was set to point at this core's
/// `PerCoreData` during initialization. This is O(1) with no MMIO.
///
/// # Panics
///
/// Panics if `gs_base` has not been initialized.
pub fn per_core() -> &'static PerCoreData {
    let ptr = read_gs_base();
    assert_ne!(ptr, 0, "gs_base not initialized");
    unsafe { &*(ptr as *const PerCoreData) }
}

/// Read the IA32_GS_BASE MSR (0xC000_0101).
fn read_gs_base() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") 0xC000_0101u32,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags),
        );
    }
    (hi as u64) << 32 | lo as u64
}

/// Write the IA32_GS_BASE MSR (0xC000_0101).
/// Phase 103 F.3 — restore this core's per-core pointer after the S3
/// machine reset wiped the GS base MSRs (the `ap_entry` pair).
pub fn restore_bsp_gs_base() {
    if let Some(pc) = get_core_data(0) {
        let ptr = pc as *const PerCoreData as u64;
        write_gs_base(ptr);
        write_kernel_gs_base(ptr);
    }
}

fn write_gs_base(value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xC000_0101u32,
            in("eax") lo,
            in("edx") hi,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Write the IA32_KERNEL_GS_BASE MSR (0xC000_0102).
///
/// This MSR is swapped with GS_BASE on `swapgs`. Set to PerCoreData so that
/// `swapgs` on syscall entry loads the correct per-core pointer.
pub fn write_kernel_gs_base(value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xC000_0102u32,
            in("eax") lo,
            in("edx") hi,
            options(nomem, nostack, preserves_flags),
        );
    }
}

// ---------------------------------------------------------------------------
// Per-core field offsets for assembly access (Phase 35)
// ---------------------------------------------------------------------------

/// Offset constants for `PerCoreData` fields accessed from assembly via `gs:[OFFSET]`.
/// These are computed at compile time using `offset_of!` and passed to `global_asm!`
/// as `const` operands.
pub mod offsets {
    use super::PerCoreData;

    pub const SYSCALL_STACK_TOP: usize = core::mem::offset_of!(PerCoreData, syscall_stack_top);
    pub const SYSCALL_USER_RSP: usize = core::mem::offset_of!(PerCoreData, syscall_user_rsp);
    pub const SYSCALL_ARG3: usize = core::mem::offset_of!(PerCoreData, syscall_arg3);
    /// Phase 57e Bug #3 fix — see [`PerCoreData::current_syscall_snapshot_ptr`].
    pub const CURRENT_SYSCALL_SNAPSHOT_PTR: usize =
        core::mem::offset_of!(PerCoreData, current_syscall_snapshot_ptr);
    pub const CURRENT_PID: usize = core::mem::offset_of!(PerCoreData, current_pid);
    pub const FORK_ENTRY_CTX: usize = core::mem::offset_of!(PerCoreData, fork_entry_ctx);

    // Phase 84 Track A (KPTI) — CR3 trampoline state read from `gs:` by the
    // entry/exit asm.
    pub const KPTI_KERNEL_CR3: usize = core::mem::offset_of!(PerCoreData, kpti_kernel_cr3);
    pub const KPTI_USER_CR3: usize = core::mem::offset_of!(PerCoreData, kpti_user_cr3);
    pub const KPTI_SCRATCH: usize = core::mem::offset_of!(PerCoreData, kpti_scratch);
}

/// Phase 110 Track A.4 — publish this core's KPTI CR3 pair for the task it is
/// about to run (or has just switched to).
///
/// `kernel_cr3` must be the PML4 this core's CR3 was just loaded with (the
/// full kernel map); `user_cr3` the task's KPTI user-half PML4, or 0 for
/// kernel threads / the boot PML4 (every exit stub skips its CR3 switch on 0).
/// Call sites: the scheduler dispatch prep, the fork-child trampoline, execve's
/// mid-syscall retarget, and `restore_kernel_cr3` (which republishes the boot
/// PML4 so the slot never dangles at a dying process's soon-freed PML4 — the
/// paranoid NMI/`#DF` entry loads `kpti_kernel_cr3` whenever it is non-zero).
///
/// No-op unless KPTI is **active** this boot: while inactive the slots must
/// stay 0, precisely so that paranoid load never happens.
///
/// Tearing: an NMI can land between the two stores, but each value is
/// individually valid at every call site (both PML4s are live), the paranoid
/// path consumes only `kpti_kernel_cr3`, and `kpti_user_cr3` is only consumed
/// at a ring-3 transition — which cannot occur mid-publish (the publishing
/// code path itself stands between the stores and any user return).
pub fn publish_kpti_cr3_pair(kernel_cr3: u64, user_cr3: u64) {
    let Some(state) = crate::mitigations::state() else {
        return;
    };
    if !state.kpti_active || !is_per_core_ready() {
        return;
    }
    // Phase 110 A.5 — when the PCID scheme is active, bake the fixed
    // kernel/user PCIDs + the no-flush bit into the published slot values. The
    // entry/exit trampolines load these verbatim (`mov cr3, gs:[…]`), so a
    // syscall/IRQ kernel↔user round trip within one process is no-flush: the
    // two halves' entries coexist under distinct PCIDs and neither is dropped.
    // The `user_cr3 == 0` kernel-thread sentinel stays 0 (never no-flush: the
    // exit stubs skip the switch on 0). While the scheme is inactive the slots
    // carry the raw page-aligned frames exactly as in A.4 (PCID = 0, and bit 63
    // clear — mandatory, since a `mov cr3` with bit 63 set `#GP`s when
    // CR4.PCIDE = 0).
    let (kernel_val, user_val) = if state.pcid_active {
        use kernel_core::kpti_pcid::{kernel_cr3 as tag_kernel, user_cr3 as tag_user};
        let user_val = if user_cr3 == 0 {
            0
        } else {
            tag_user(user_cr3, true)
        };
        // A 0 kernel value (published by `restore_kernel_cr3`'s successor is
        // always the real boot PML4, so `kernel_cr3` here is nonzero) is passed
        // through untagged for safety; every real caller passes a live PML4.
        let kernel_val = if kernel_cr3 == 0 {
            0
        } else {
            tag_kernel(kernel_cr3, true)
        };
        (kernel_val, user_val)
    } else {
        (kernel_cr3, user_cr3)
    };
    let pc = per_core() as *const PerCoreData as *mut PerCoreData;
    // SAFETY: PerCoreData is only written by its owning core; volatile so the
    // `gs:`-relative asm readers always see the stores.
    unsafe {
        core::ptr::write_volatile(&raw mut (*pc).kpti_kernel_cr3, kernel_val);
        core::ptr::write_volatile(&raw mut (*pc).kpti_user_cr3, user_val);
    }
}

// ---------------------------------------------------------------------------
// BSP initialization (T002, T004)
// ---------------------------------------------------------------------------

/// Initialize per-core data for the BSP (core 0).
///
/// Must be called after ACPI/MADT parsing and LAPIC initialization, but
/// before AP bootstrap.
pub fn init_bsp_per_core() {
    // If MADT is available, enumerate cores. Otherwise, BSP-only single-core mode.
    let (bsp_apic_id, total_cores, lapic_virt_base, lapic_tpm) =
        if crate::acpi::io_apic_address().is_some() {
            let madt = crate::acpi::madt_info();
            let bsp_apic_id = read_lapic_id();

            // Enumerate APs from MADT and assign core IDs.
            let mut next_core_id: u8 = 1;
            for i in 0..madt.local_apic_count {
                if let Some(entry) = &madt.local_apics[i] {
                    if entry.apic_id == bsp_apic_id {
                        continue;
                    }
                    if entry.flags & 1 == 0 {
                        continue;
                    }
                    if next_core_id >= MAX_CORES as u8 {
                        log::warn!(
                            "[smp] skipping AP APIC ID={}: exceeds MAX_CORES ({})",
                            entry.apic_id,
                            MAX_CORES
                        );
                        break;
                    }
                    unsafe {
                        APIC_TO_CORE[entry.apic_id as usize] = next_core_id;
                    }
                    next_core_id += 1;
                }
            }

            let lapic_virt = {
                let phys = crate::acpi::local_apic_address() as u64;
                crate::mm::phys_offset() + phys
            };
            let lapic_tpm = crate::arch::x86_64::apic::lapic_ticks_per_ms();

            (bsp_apic_id, next_core_id, lapic_virt, lapic_tpm)
        } else {
            // No MADT — single-core BSP-only mode.
            log::info!("[smp] no MADT/I/O APIC — single-core BSP-only mode");
            (0u8, 1u8, 0u64, 0u32)
        };

    BSP_APIC_ID.store(bsp_apic_id, Ordering::Relaxed);

    // BSP is always core 0.
    unsafe {
        APIC_TO_CORE[bsp_apic_id as usize] = 0;
    }

    CORE_COUNT.store(total_cores, Ordering::Release);
    log::info!(
        "[smp] {} core(s) discovered (BSP APIC ID={})",
        total_cores,
        bsp_apic_id
    );

    // Allocate and initialize BSP's PerCoreData.
    // The BSP reuses the existing GDT/TSS/stacks from gdt.rs.
    let bsp_stack_top = crate::arch::x86_64::gdt::syscall_stack_top();
    let bsp_data = Box::into_raw(Box::new(PerCoreData {
        self_ptr: core::ptr::null(), // filled below
        core_id: 0,
        apic_id: bsp_apic_id,
        is_online: AtomicBool::new(true),
        ipi_recv_log_budget: core::sync::atomic::AtomicI32::new(1024),
        tss_ptr: core::ptr::null_mut(), // BSP uses existing gdt.rs TSS
        gdt_ptr: core::ptr::null(),     // BSP uses existing gdt.rs GDT
        gdt_code: SegmentSelector(0),
        gdt_data: SegmentSelector(0),
        gdt_tss: SegmentSelector(0),
        kernel_stack_top: bsp_stack_top,
        scheduler_rsp: core::cell::UnsafeCell::new(0), // set when scheduler loop starts
        reschedule: AtomicBool::new(false),
        current_task_idx: core::sync::atomic::AtomicI32::new(-1),
        current_task_ptr: AtomicPtr::new(core::ptr::null_mut()),
        lapic_virt_base,
        lapic_ticks_per_ms: lapic_tpm,
        run_queue: spin::Mutex::new(VecDeque::new()),
        // Phase 35: per-core syscall state
        syscall_stack_top: bsp_stack_top,
        syscall_user_rsp: 0,
        syscall_arg3: 0,
        // Phase 57e Bug #3 fix — null until first dispatch sets it.
        // syscall_entry asm dereferences this; if it fires before the
        // dispatcher has run (impossible — userspace cannot syscall before
        // init is dispatched) the kernel would fault on the null deref.
        current_syscall_snapshot_ptr: core::cell::UnsafeCell::new(core::ptr::null_mut()),
        current_pid: AtomicU32::new(0),
        current_addrspace: core::ptr::null(),
        fork_entry_ctx: crate::arch::x86_64::ForkEntryCtx::ZERO,
        isr_wake_queue: IsrWakeQueue::new(),
        page_cache: core::cell::UnsafeCell::new(crate::mm::frame_allocator::PerCpuPageCache::new()),
        page_cache_count: AtomicUsize::new(0),
        slab_magazines: core::cell::UnsafeCell::new(crate::mm::slab::PerCpuMagazines::new()),
        cross_cpu_free: crate::mm::slab::CrossCpuFreeLists::new(),
        #[cfg(feature = "trace")]
        trace_ring: core::cell::UnsafeCell::new(kernel_core::trace_ring::TraceRing::new()),
        holds_scheduler_lock: AtomicBool::new(false),
        preempt_resched_pending: AtomicBool::new(false),
        // Phase 57b C.1: pointer starts at this core's dummy slot.  The
        // dispatch path (C.2 / C.3) retargets it to the running task's
        // `preempt_count` while the task executes and back to the dummy
        // on switch-out.
        current_preempt_count_ptr: AtomicPtr::new(
            &SCHED_PREEMPT_COUNT_DUMMY[0] as *const AtomicI32 as *mut AtomicI32,
        ),
        kpti_kernel_cr3: 0,
        kpti_user_cr3: 0,
        kpti_scratch: 0,
    }));

    // Fill self-pointer and store in global array.
    unsafe {
        (*bsp_data).self_ptr = bsp_data;
        PER_CORE_DATA[0] = bsp_data;
    }

    // Set gs_base to point to BSP's PerCoreData for gs-relative access.
    // Also set kernel_gs_base for consistency (unused — swapgs is not used
    // because user code cannot change gs_base: no FSGSBASE, no wrmsr in ring 3).
    write_gs_base(bsp_data as u64);
    write_kernel_gs_base(bsp_data as u64);

    log::info!("[smp] BSP per-core data initialized, gs_base set");

    SMP_INITIALIZED.store(true, Ordering::Release);
}

// ---------------------------------------------------------------------------
// AP per-core data population (T005)
// ---------------------------------------------------------------------------

/// Populate `PerCoreData` for an AP.
///
/// Allocates a fresh TSS plus a per-core syscall stack, double-fault stack,
/// and NMI stack. The stacks are claimed from the static `.bss` pool in
/// [`crate::task::kstack`] rather than the kernel heap, so they cannot
/// alias with any other heap allocation — see
/// `docs/handoffs/ap-core-gpf-saved-rsp-stack-corruption.md` for the
/// failure mode that motivated the change.
///
/// Returns a raw pointer to the initialized data (stored in `PER_CORE_DATA`).
/// Called from the BSP before sending SIPI to the AP.
pub fn init_ap_per_core(core_id: u8, apic_id: u8) -> *mut PerCoreData {
    assert!(
        (core_id as usize) < MAX_CORES,
        "core_id {} exceeds MAX_CORES",
        core_id
    );

    // Phase 110 A.4 — S3-resume re-boot: REUSE the existing allocation.
    // Every KPTI user half maps this core's PerCoreData / GDT / TSS / IST-top
    // pages at their pre-suspend addresses, so re-allocating here would leave
    // every pre-suspend process's user half pointing at the OLD frames — the
    // first ring-3 interrupt delivered on this core would then read an
    // unmapped GDT/TSS through the user CR3 and escalate #PF → #DF (new IST
    // top also unmapped) → triple fault. (Observed live: `suspend-smoke` died
    // waiting for `POWERD:resume` the first boot after the A.4 flip.) RAM is
    // preserved across S3, so the GDT/TSS contents, the IST/kstack pool
    // slots, and the LAPIC fields are all still valid; only the
    // dispatch-transient fields are reset to their fresh-boot values. The
    // owned caches (run_queue, page cache, slab magazines, ISR wake queue)
    // are deliberately kept — the previous fresh-Box path leaked their
    // contents on every resume. Field writes go through the raw pointer (the
    // AP is parked and not executing; the BSP is the only accessor here —
    // same exclusivity the cold-boot pre-SIPI writes rely on).
    let existing = unsafe { PER_CORE_DATA[core_id as usize] };
    if !existing.is_null() {
        unsafe {
            debug_assert_eq!((*existing).core_id, core_id);
            (*existing).apic_id = apic_id;
            (*existing).is_online.store(false, Ordering::Release);
            (*existing)
                .ipi_recv_log_budget
                .store(1024, Ordering::Release);
            (*existing).reschedule.store(false, Ordering::Release);
            (*existing).current_task_idx.store(-1, Ordering::Release);
            (*existing)
                .current_task_ptr
                .store(core::ptr::null_mut(), Ordering::Release);
            (*existing).syscall_user_rsp = 0;
            (*existing).syscall_arg3 = 0;
            *(*existing).current_syscall_snapshot_ptr.get() = core::ptr::null_mut();
            (*existing).current_pid.store(0, Ordering::Release);
            (*existing).current_addrspace = core::ptr::null();
            (*existing).fork_entry_ctx = crate::arch::x86_64::ForkEntryCtx::ZERO;
            (*existing)
                .holds_scheduler_lock
                .store(false, Ordering::Release);
            (*existing)
                .preempt_resched_pending
                .store(false, Ordering::Release);
            (*existing).current_preempt_count_ptr.store(
                &SCHED_PREEMPT_COUNT_DUMMY[core_id as usize] as *const AtomicI32 as *mut AtomicI32,
                Ordering::Release,
            );
            (*existing).kpti_kernel_cr3 = 0;
            (*existing).kpti_user_cr3 = 0;
            (*existing).kpti_scratch = 0;
            // Dispatch retargets RSP0 before any ring-3 return; reset it to
            // the boot value for parity with the fresh path anyway.
            (*(*existing).tss_ptr).privilege_stack_table[0] =
                VirtAddr::new((*existing).kernel_stack_top);
            // The pre-suspend `ltr` marked this GDT's TSS descriptor BUSY
            // (type 0xB), and `ltr` on a busy descriptor #GPs — the AP
            // re-boot runs `per_core_gdt_init` (which `ltr`s) BEFORE loading
            // its IDT, so that #GP is a guaranteed triple fault. Clear the
            // busy bit (descriptor bit 41), exactly like the BSP's
            // `gdt::reinit_after_resume` does for its own TSS.
            let gdt_words = (*existing).gdt_ptr as *mut u64;
            let tss_entry = gdt_words.add((*existing).gdt_tss.index() as usize);
            tss_entry.write_volatile(tss_entry.read_volatile() & !(1u64 << 41));
        }
        log::info!(
            "[smp] AP core_id={} apic_id={} per-core data reused for resume (stack_top={:#x})",
            core_id,
            apic_id,
            unsafe { (*existing).kernel_stack_top },
        );
        return existing;
    }
    // Pool slots are 32 KiB each — larger than `SYSCALL_STACK_SIZE` (16 KiB)
    // and `DOUBLE_FAULT_STACK_SIZE` (20 KiB), so the stacks fit comfortably
    // and the unused portion below `top` is harmless. The NMI IST stack
    // (Phase 90b follow-up) is claimed from the same pool so each AP services
    // TLB-shootdown NMIs on a per-core stack — see the BSP equivalent in
    // `gdt.rs` (NMI_IST_INDEX) and
    // `docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md`.
    let kernel_stack_top = crate::task::kstack::alloc_leaked_top();
    let double_fault_stack_top = crate::task::kstack::alloc_leaked_top();
    let nmi_stack_top = crate::task::kstack::alloc_leaked_top();

    // Allocate and configure TSS. Page-isolated (`PageIsolated`, Phase 110
    // A.3b part 4): this TSS (and the GDT below) is mapped into every KPTI
    // user half at its live address, so a plain heap Box — which shares its
    // page with arbitrary neighbouring allocations — would leak adjacent heap
    // data to ring 3. The wrapper gives the allocation exclusive, page-aligned
    // pages; the raw pointer to the inner value is what `tss_ptr`/`gdt_ptr`
    // store (leaked deliberately — per-core structures live forever).
    let tss_iso = Box::into_raw(Box::new(crate::arch::x86_64::gdt::PageIsolated({
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
            VirtAddr::new(double_fault_stack_top);
        tss.interrupt_stack_table[NMI_IST_INDEX as usize] = VirtAddr::new(nmi_stack_top);
        tss.privilege_stack_table[0] = VirtAddr::new(kernel_stack_top);
        tss
    })));
    let tss: *mut TaskStateSegment = unsafe { core::ptr::addr_of_mut!((*tss_iso).0) };

    // Pre-allocate GDT on the BSP so the AP doesn't need heap access.
    let tss_ref: &'static TaskStateSegment = unsafe { &*tss };
    let gdt_iso = Box::into_raw(Box::new(crate::arch::x86_64::gdt::PageIsolated(
        GlobalDescriptorTable::new(),
    )));
    let gdt: *mut GlobalDescriptorTable = unsafe { core::ptr::addr_of_mut!((*gdt_iso).0) };
    let (gdt_code, gdt_data, gdt_tss) = unsafe {
        let gdt_ref = &mut *gdt;
        let code = gdt_ref.append(Descriptor::kernel_code_segment());
        let data_sel = gdt_ref.append(Descriptor::kernel_data_segment());
        let _user_data = gdt_ref.append(Descriptor::user_data_segment());
        let _user_code = gdt_ref.append(Descriptor::user_code_segment());
        let tss_sel = gdt_ref.append(Descriptor::tss_segment(tss_ref));
        (code, data_sel, tss_sel)
    };

    // Allocate PerCoreData.
    let data = Box::into_raw(Box::new(PerCoreData {
        self_ptr: core::ptr::null(), // filled below
        core_id,
        apic_id,
        is_online: AtomicBool::new(false),
        ipi_recv_log_budget: core::sync::atomic::AtomicI32::new(1024),
        tss_ptr: tss,
        gdt_ptr: gdt,
        gdt_code,
        gdt_data,
        gdt_tss,
        kernel_stack_top,
        scheduler_rsp: core::cell::UnsafeCell::new(0),
        reschedule: AtomicBool::new(false),
        current_task_idx: core::sync::atomic::AtomicI32::new(-1),
        current_task_ptr: AtomicPtr::new(core::ptr::null_mut()),
        lapic_virt_base: {
            let phys = crate::acpi::local_apic_address() as u64;
            crate::mm::phys_offset() + phys
        },
        lapic_ticks_per_ms: crate::arch::x86_64::apic::lapic_ticks_per_ms(),
        run_queue: spin::Mutex::new(VecDeque::new()),
        // Phase 35: per-core syscall state
        syscall_stack_top: kernel_stack_top,
        syscall_user_rsp: 0,
        syscall_arg3: 0,
        // Phase 57e Bug #3 fix — null until this AP's first dispatch.
        current_syscall_snapshot_ptr: core::cell::UnsafeCell::new(core::ptr::null_mut()),
        current_pid: AtomicU32::new(0),
        current_addrspace: core::ptr::null(),
        fork_entry_ctx: crate::arch::x86_64::ForkEntryCtx::ZERO,
        isr_wake_queue: IsrWakeQueue::new(),
        page_cache: core::cell::UnsafeCell::new(crate::mm::frame_allocator::PerCpuPageCache::new()),
        page_cache_count: AtomicUsize::new(0),
        slab_magazines: core::cell::UnsafeCell::new(crate::mm::slab::PerCpuMagazines::new()),
        cross_cpu_free: crate::mm::slab::CrossCpuFreeLists::new(),
        #[cfg(feature = "trace")]
        trace_ring: core::cell::UnsafeCell::new(kernel_core::trace_ring::TraceRing::new()),
        holds_scheduler_lock: AtomicBool::new(false),
        preempt_resched_pending: AtomicBool::new(false),
        // Phase 57b C.1: pointer starts at this AP's dummy slot.  The
        // dispatch path (C.2 / C.3) retargets it to the running task's
        // `preempt_count` while the task executes and back to the dummy
        // on switch-out.
        current_preempt_count_ptr: AtomicPtr::new(
            &SCHED_PREEMPT_COUNT_DUMMY[core_id as usize] as *const AtomicI32 as *mut AtomicI32,
        ),
        kpti_kernel_cr3: 0,
        kpti_user_cr3: 0,
        kpti_scratch: 0,
    }));

    unsafe {
        (*data).self_ptr = data;
        PER_CORE_DATA[core_id as usize] = data;
    }

    log::info!(
        "[smp] AP core_id={} apic_id={} per-core data allocated (stack_top={:#x})",
        core_id,
        apic_id,
        kernel_stack_top
    );

    data
}

// ---------------------------------------------------------------------------
// Per-core GDT initialization (T006)
// ---------------------------------------------------------------------------

/// Configure and load a fresh GDT with this core's TSS.
///
/// Called on each AP during its entry sequence. The GDT is heap-allocated
/// and leaked so it remains valid for the core's lifetime.
///
/// # Safety
///
/// Must be called on the AP core itself (not remotely from the BSP).
/// The core must have a valid stack before calling this.
pub unsafe fn per_core_gdt_init(data: &PerCoreData) {
    unsafe {
        // GDT was pre-allocated and populated on the BSP. Just load it.
        let gdt = &*data.gdt_ptr;
        gdt.load();
        CS::set_reg(data.gdt_code);
        DS::set_reg(data.gdt_data);
        SS::set_reg(data.gdt_data);
        load_tss(data.gdt_tss);
    }
}

// ---------------------------------------------------------------------------
// Helpers for per-core TSS updates
// ---------------------------------------------------------------------------

/// Update TSS.RSP0 for the current core.
///
/// Called when switching to a userspace process to set the kernel stack
/// used on ring-3 → ring-0 transitions.
pub fn set_current_core_kernel_stack(rsp0: u64) {
    let data = per_core();
    if data.tss_ptr.is_null() {
        // BSP uses the existing gdt.rs TSS — delegate to the old path.
        unsafe { crate::arch::x86_64::gdt::set_kernel_stack(rsp0) };
    } else {
        unsafe {
            (*data.tss_ptr).privilege_stack_table[0] = VirtAddr::new(rsp0);
        }
    }
}

// ---------------------------------------------------------------------------
// Access to per-core data by core_id (for IPI targeting, etc.)
// ---------------------------------------------------------------------------

/// Return a reference to the per-core data for the given logical core ID.
pub fn get_core_data(core_id: u8) -> Option<&'static PerCoreData> {
    if (core_id as usize) < MAX_CORES {
        let ptr = unsafe { PER_CORE_DATA[core_id as usize] };
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { &*ptr })
        }
    } else {
        None
    }
}

/// Phase 57 fix: drop the per-core data for an AP whose
/// `INIT-SIPI-SIPI` boot timed out. Called by `boot_aps` after the
/// online-flag wait expires. Without this, the dead AP's
/// `PerCoreData` slot stays populated (it was allocated by
/// `init_ap_per_core` *before* the boot wait) and `get_core_data`
/// returns `Some(_)` — which silently misleads the scheduler's load
/// balancer into queuing tasks onto a runqueue nothing drains.
///
/// # Safety
///
/// Caller must guarantee that the AP at `core_id` never came online,
/// so no other core holds a live reference to its `PerCoreData`.
/// `boot_aps` enforces this by polling `is_online` to false before
/// calling.
pub(super) unsafe fn release_failed_ap(core_id: u8) {
    if (core_id as usize) >= MAX_CORES {
        return;
    }
    let dead_ptr = unsafe { PER_CORE_DATA[core_id as usize] };
    if dead_ptr.is_null() {
        return;
    }
    // Reclaim the Box that `init_ap_per_core` allocated.
    drop(unsafe { Box::from_raw(dead_ptr) });
    unsafe {
        PER_CORE_DATA[core_id as usize] = core::ptr::null_mut();
    }
    // Clear the APIC → core mapping so a stray IPI cannot aim at
    // the freed slot. Use raw pointer indexing to avoid taking a
    // mutable reference to the `static mut` array (Rust 2024
    // compat: `static_mut_refs` is now a hard deny).
    let map_ptr = &raw mut APIC_TO_CORE;
    for i in 0..MAX_CORES {
        unsafe {
            if (*map_ptr)[i] == core_id {
                (*map_ptr)[i] = 0xFF;
            }
        }
    }
}

/// Phase 57 fix: shrink `CORE_COUNT` to reflect APs that actually
/// booted. Called by `boot_aps` once all AP boot attempts have run.
/// `count` is `1 + number_of_online_APs` (BSP plus successful APs);
/// `least_loaded_core` only iterates `0..CORE_COUNT`, so a smaller
/// value keeps the load balancer from even considering dead slots
/// — defense in depth alongside `release_failed_ap`.
pub(super) fn set_core_count(count: u8) {
    CORE_COUNT.store(count, Ordering::Release);
}

/// Return the BSP's LAPIC ID.
pub fn bsp_apic_id() -> u8 {
    BSP_APIC_ID.load(Ordering::Relaxed)
}

/// Returns `true` if the calling core is the Bootstrap Processor (core 0).
#[inline]
pub fn is_bsp() -> bool {
    // Fast path: compare current LAPIC ID against the recorded BSP LAPIC ID.
    // This is safe to call from interrupt context.
    let apic_id = crate::arch::x86_64::apic::current_lapic_id();
    apic_id == BSP_APIC_ID.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Phase 103 F.3 — S3 suspend AP park / resume
// ---------------------------------------------------------------------------

/// True while [`suspend_park_and_release_aps`] is asking sibling cores to
/// park (checked by the NMI handler alongside the panic-park flag).
static SUSPEND_PARK: AtomicBool = AtomicBool::new(false);

/// Ack bitmask mirroring `PANIC_STOP_ACK` for the suspend park round.
static SUSPEND_PARK_ACK: AtomicU64 = AtomicU64::new(0);

/// Whether the NMI handler should park this core for an S3 suspend.
#[inline]
pub fn suspend_should_park(my_core: u8) -> bool {
    my_core != 0 && SUSPEND_PARK.load(Ordering::Acquire)
}

/// Acknowledge the suspend-park NMI and halt. The wake-side machine
/// reset obliterates this core; it is rebooted through the normal SIPI
/// path by [`resume_reboot_aps`].
pub fn suspend_ack_and_park() -> ! {
    if let Some(pc) = try_per_core() {
        SUSPEND_PARK_ACK.fetch_or(1u64 << pc.core_id, Ordering::Release);
    }
    crate::hlt_loop()
}

/// Quiesce every AP before an S3 entry. **Cooperative, not NMI**: an
/// NMI can catch a core mid-critical-section (scheduler/run-queue lock
/// held) and parking it there deadlocks the BSP's drain — found live by
/// the first suspend-smoke run. Instead the flag + a reschedule IPI
/// steer each AP to the top of its scheduler `run()` loop — a point
/// that by construction holds no locks — where it acks and halts. The
/// parked APs' queued tasks migrate to the BSP and their per-core state
/// is released (the failed-AP shape) so no IPI/TLB-shootdown can target
/// a core that will not exist after the wake-side reset.
///
/// Returns `false` (with every already-parked AP rebooted and the
/// machine fully live) if any AP failed to park in time — the caller
/// fails the suspend closed.
pub fn suspend_park_and_release_aps() -> bool {
    let count = core_count();
    if count <= 1 {
        return true;
    }
    SUSPEND_PARK_ACK.store(0, Ordering::Release);
    SUSPEND_PARK.store(true, Ordering::Release);

    let mut target_mask = 0u64;
    for core in 1..count {
        if let Some(data) = get_core_data(core)
            && data.is_online.load(Ordering::Acquire)
        {
            target_mask |= 1u64 << core;
            data.reschedule.store(true, Ordering::Release);
            ipi::send_ipi_to_core(core, ipi::IPI_RESCHEDULE);
        }
    }

    // Wait for acks. Each AP reaches the run()-loop check within one
    // timer tick (10 ms) even from a CPU-bound userspace task; the
    // bound is generous wall-clock (~seconds under TCG).
    let mut spins = 0u64;
    let parked = loop {
        if (SUSPEND_PARK_ACK.load(Ordering::Acquire) & target_mask) == target_mask {
            break true;
        }
        core::hint::spin_loop();
        spins += 1;
        if spins > 2_000_000_000 {
            break false;
        }
    };
    SUSPEND_PARK.store(false, Ordering::Release);

    if !parked {
        log::warn!(
            "[suspend] AP park ack timeout (mask {:#x} vs {:#x}) — aborting suspend",
            SUSPEND_PARK_ACK.load(Ordering::Acquire),
            target_mask
        );
        // Reboot whichever APs did park (they are offline in hlt); the
        // stragglers never stopped. resume_reboot_aps re-derives the
        // APIC map and re-runs boot_aps; the acked cores' per-core state
        // is KEPT (see the A.4 note below) and re-inited in place by
        // `init_ap_per_core`.
        let acked = SUSPEND_PARK_ACK.load(Ordering::Acquire);
        for core in 1..count {
            if acked & (1u64 << core) != 0 && get_core_data(core).is_some() {
                crate::task::scheduler::detach_core_for_suspend(core);
            }
        }
        resume_reboot_aps();
        return false;
    }

    // Migrate stranded work + retire idle tasks. Phase 110 A.4: the per-core
    // state is deliberately NOT freed any more (this used to
    // `release_failed_ap` each core) — every KPTI user half maps each core's
    // PerCoreData / GDT / TSS / IST-top pages at their live addresses, so the
    // old free-and-reallocate across the S3 cycle left every pre-suspend
    // process's user half pointing at the OLD frames, and the first ring-3
    // interrupt on a resumed AP triple-faulted (unmapped GDT through the user
    // CR3 → #PF → #DF → unmapped IST top). The parked cores keep their
    // allocations (RAM is preserved across S3, so the addresses and contents
    // stay valid); `init_ap_per_core` re-inits them in place on the resume
    // re-boot. `set_core_count(1)` still hides the parked slots from the load
    // balancer, and `is_online=false` gates every IPI targeter.
    for core in 1..count {
        if get_core_data(core).is_some() {
            crate::task::scheduler::detach_core_for_suspend(core);
        }
    }
    set_core_count(1);
    log::info!("[suspend] APs parked (per-core state kept for the resume re-boot)");
    true
}

/// After the S3 wake: re-derive the AP APIC→core mappings (released
/// during park — must match the boot-time walk order so core ids stay
/// stable) and reboot the APs through the normal INIT-SIPI-SIPI path.
pub fn resume_reboot_aps() {
    let madt = crate::acpi::madt_info();
    let bsp = bsp_apic_id();
    let mut next_core_id = 1u8;
    let map_ptr = &raw mut APIC_TO_CORE;
    for i in 0..madt.local_apic_count {
        let Some(entry) = &madt.local_apics[i] else {
            continue;
        };
        if entry.apic_id == bsp || entry.flags & 1 == 0 {
            continue;
        }
        if (next_core_id as usize) >= MAX_CORES {
            break;
        }
        unsafe {
            (*map_ptr)[entry.apic_id as usize] = next_core_id;
        }
        next_core_id += 1;
    }
    if next_core_id > 1 {
        crate::smp::boot::boot_aps();
    }
}
