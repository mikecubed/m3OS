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

use crate::arch::x86_64::gdt::DOUBLE_FAULT_IST_INDEX;

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
#[repr(C)]
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
    pub syscall_user_rsp: u64,
    /// R10 (syscall arg3) saved by `syscall_entry` assembly stub.
    pub syscall_arg3: u64,
    /// Saved user callee-saved registers at syscall entry (for fork child restore).
    pub syscall_user_rbx: u64,
    pub syscall_user_rbp: u64,
    pub syscall_user_r12: u64,
    pub syscall_user_r13: u64,
    pub syscall_user_r14: u64,
    pub syscall_user_r15: u64,
    /// Saved user caller-saved registers at syscall entry (Linux ABI preserves these).
    pub syscall_user_rdi: u64,
    pub syscall_user_rsi: u64,
    pub syscall_user_rdx: u64,
    pub syscall_user_r8: u64,
    pub syscall_user_r9: u64,
    pub syscall_user_r10: u64,
    pub syscall_user_rflags: u64,

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
    #[cfg(feature = "trace")]
    pub trace_ring: core::cell::UnsafeCell<kernel_core::trace_ring::TraceRing<256>>,

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
}

// Safety: PerCoreData is only accessed by its owning core (via gs_base) or
// through atomic fields (is_online, reschedule). The raw pointers (self_ptr,
// tss_ptr) are only dereferenced on the owning core.
unsafe impl Send for PerCoreData {}
unsafe impl Sync for PerCoreData {}

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
    pub const SYSCALL_USER_RBX: usize = core::mem::offset_of!(PerCoreData, syscall_user_rbx);
    pub const SYSCALL_USER_RBP: usize = core::mem::offset_of!(PerCoreData, syscall_user_rbp);
    pub const SYSCALL_USER_R12: usize = core::mem::offset_of!(PerCoreData, syscall_user_r12);
    pub const SYSCALL_USER_R13: usize = core::mem::offset_of!(PerCoreData, syscall_user_r13);
    pub const SYSCALL_USER_R14: usize = core::mem::offset_of!(PerCoreData, syscall_user_r14);
    pub const SYSCALL_USER_R15: usize = core::mem::offset_of!(PerCoreData, syscall_user_r15);
    pub const SYSCALL_USER_RDI: usize = core::mem::offset_of!(PerCoreData, syscall_user_rdi);
    pub const SYSCALL_USER_RSI: usize = core::mem::offset_of!(PerCoreData, syscall_user_rsi);
    pub const SYSCALL_USER_RDX: usize = core::mem::offset_of!(PerCoreData, syscall_user_rdx);
    pub const SYSCALL_USER_R8: usize = core::mem::offset_of!(PerCoreData, syscall_user_r8);
    pub const SYSCALL_USER_R9: usize = core::mem::offset_of!(PerCoreData, syscall_user_r9);
    pub const SYSCALL_USER_R10: usize = core::mem::offset_of!(PerCoreData, syscall_user_r10);
    pub const SYSCALL_USER_RFLAGS: usize = core::mem::offset_of!(PerCoreData, syscall_user_rflags);
    pub const CURRENT_PID: usize = core::mem::offset_of!(PerCoreData, current_pid);
    pub const FORK_ENTRY_CTX: usize = core::mem::offset_of!(PerCoreData, fork_entry_ctx);
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
        lapic_virt_base,
        lapic_ticks_per_ms: lapic_tpm,
        run_queue: spin::Mutex::new(VecDeque::new()),
        // Phase 35: per-core syscall state
        syscall_stack_top: bsp_stack_top,
        syscall_user_rsp: 0,
        syscall_arg3: 0,
        syscall_user_rbx: 0,
        syscall_user_rbp: 0,
        syscall_user_r12: 0,
        syscall_user_r13: 0,
        syscall_user_r14: 0,
        syscall_user_r15: 0,
        syscall_user_rdi: 0,
        syscall_user_rsi: 0,
        syscall_user_rdx: 0,
        syscall_user_r8: 0,
        syscall_user_r9: 0,
        syscall_user_r10: 0,
        syscall_user_rflags: 0,
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
        // Phase 57b C.1: pointer starts at this core's dummy slot.  The
        // dispatch path (C.2 / C.3) retargets it to the running task's
        // `preempt_count` while the task executes and back to the dummy
        // on switch-out.
        current_preempt_count_ptr: AtomicPtr::new(
            &SCHED_PREEMPT_COUNT_DUMMY[0] as *const AtomicI32 as *mut AtomicI32,
        ),
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
/// Allocates a fresh TSS, kernel stack, and double-fault stack on the heap.
/// Returns a raw pointer to the initialized data (stored in `PER_CORE_DATA`).
///
/// Called from the BSP before sending SIPI to the AP.
pub fn init_ap_per_core(core_id: u8, apic_id: u8) -> *mut PerCoreData {
    assert!(
        (core_id as usize) < MAX_CORES,
        "core_id {} exceeds MAX_CORES",
        core_id
    );
    // Allocate stacks.
    let kernel_stack = Box::leak(alloc::vec![0u8; SYSCALL_STACK_SIZE].into_boxed_slice());
    // Align stack top to 16 bytes (x86-64 ABI requirement).
    let kernel_stack_top = ((kernel_stack.as_ptr() as u64) + SYSCALL_STACK_SIZE as u64) & !0xF;

    let double_fault_stack =
        Box::leak(alloc::vec![0u8; DOUBLE_FAULT_STACK_SIZE].into_boxed_slice());
    let double_fault_stack_top =
        (double_fault_stack.as_ptr() as u64) + DOUBLE_FAULT_STACK_SIZE as u64;

    // Allocate and configure TSS.
    let tss = Box::into_raw(Box::new({
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
            VirtAddr::new(double_fault_stack_top);
        tss.privilege_stack_table[0] = VirtAddr::new(kernel_stack_top);
        tss
    }));

    // Pre-allocate GDT on the BSP so the AP doesn't need heap access.
    let tss_ref: &'static TaskStateSegment = unsafe { &*tss };
    let gdt = Box::into_raw(Box::new(GlobalDescriptorTable::new()));
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
        syscall_user_rbx: 0,
        syscall_user_rbp: 0,
        syscall_user_r12: 0,
        syscall_user_r13: 0,
        syscall_user_r14: 0,
        syscall_user_r15: 0,
        syscall_user_rdi: 0,
        syscall_user_rsi: 0,
        syscall_user_rdx: 0,
        syscall_user_r8: 0,
        syscall_user_r9: 0,
        syscall_user_r10: 0,
        syscall_user_rflags: 0,
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
        // Phase 57b C.1: pointer starts at this AP's dummy slot.  The
        // dispatch path (C.2 / C.3) retargets it to the running task's
        // `preempt_count` while the task executes and back to the dummy
        // on switch-out.
        current_preempt_count_ptr: AtomicPtr::new(
            &SCHED_PREEMPT_COUNT_DUMMY[core_id as usize] as *const AtomicI32 as *mut AtomicI32,
        ),
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
