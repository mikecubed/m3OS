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
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

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

/// Size of the dedicated double-fault stack per core (same as BSP).
const DOUBLE_FAULT_STACK_SIZE: usize = 4096 * 5; // 20 KiB

/// Size of the dedicated syscall/kernel stack per core (same as BSP).
const SYSCALL_STACK_SIZE: usize = 4096 * 4; // 16 KiB

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

    /// Fork child entry context — per-core so each core can handle `fork()`
    /// independently without corrupting another core's saved context.
    pub fork_entry_ctx: crate::arch::x86_64::ForkEntryCtx,

    // ----- Phase 43b: per-core trace ring -----
    /// Lockless ring buffer of recent kernel trace events (scheduler, fork, IPC).
    /// Written only by the owning core; read by panic/fault dump and `sys_ktrace`.
    #[cfg(feature = "trace")]
    pub trace_ring: core::cell::UnsafeCell<kernel_core::trace_ring::TraceRing<256>>,
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
        fork_entry_ctx: crate::arch::x86_64::ForkEntryCtx::ZERO,
        #[cfg(feature = "trace")]
        trace_ring: core::cell::UnsafeCell::new(kernel_core::trace_ring::TraceRing::new()),
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
        fork_entry_ctx: crate::arch::x86_64::ForkEntryCtx::ZERO,
        #[cfg(feature = "trace")]
        trace_ring: core::cell::UnsafeCell::new(kernel_core::trace_ring::TraceRing::new()),
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
