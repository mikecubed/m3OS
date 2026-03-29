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

extern crate alloc;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use x86_64::{
    instructions::{segmentation::Segment, tables::load_tss},
    registers::segmentation::{CS, DS, SS},
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector},
        tss::TaskStateSegment,
    },
    VirtAddr,
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
    /// Self-pointer at offset 0 — used by `per_core()` via `gs:[0]`.
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
    pub scheduler_rsp: u64,
    /// Per-core reschedule flag (replaces the global `RESCHEDULE`).
    pub reschedule: AtomicBool,
    /// LAPIC virtual base address (phys_offset + LAPIC phys addr).
    /// Stored here so APs can access it without touching kernel statics.
    pub lapic_virt_base: u64,
    /// LAPIC timer ticks per millisecond (BSP-calibrated, shared by all cores).
    pub lapic_ticks_per_ms: u32,
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

// ---------------------------------------------------------------------------
// BSP initialization (T002, T004)
// ---------------------------------------------------------------------------

/// Initialize per-core data for the BSP (core 0).
///
/// Must be called after ACPI/MADT parsing and LAPIC initialization, but
/// before AP bootstrap.
pub fn init_bsp_per_core() {
    let madt = crate::acpi::madt_info();
    let bsp_apic_id = read_lapic_id();
    BSP_APIC_ID.store(bsp_apic_id, Ordering::Relaxed);

    // BSP is always core 0.
    unsafe {
        APIC_TO_CORE[bsp_apic_id as usize] = 0;
    }

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
            unsafe {
                APIC_TO_CORE[entry.apic_id as usize] = next_core_id;
            }
            next_core_id += 1;
        }
    }

    let total_cores = next_core_id;
    CORE_COUNT.store(total_cores, Ordering::Release);
    log::info!(
        "[smp] {} core(s) discovered (BSP APIC ID={})",
        total_cores,
        bsp_apic_id
    );

    // Allocate and initialize BSP's PerCoreData.
    // The BSP reuses the existing GDT/TSS/stacks from gdt.rs.
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
        kernel_stack_top: crate::arch::x86_64::gdt::syscall_stack_top(),
        scheduler_rsp: 0, // set when scheduler loop starts
        reschedule: AtomicBool::new(false),
        lapic_virt_base: {
            let phys = crate::acpi::local_apic_address() as u64;
            crate::mm::phys_offset() + phys
        },
        lapic_ticks_per_ms: crate::arch::x86_64::apic::lapic_ticks_per_ms(),
    }));

    // Fill self-pointer and store in global array.
    unsafe {
        (*bsp_data).self_ptr = bsp_data;
        PER_CORE_DATA[0] = bsp_data;
    }

    // Set gs_base to point to BSP's PerCoreData.
    write_gs_base(bsp_data as u64);

    log::info!("[smp] BSP per-core data initialized, gs_base set");
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
        scheduler_rsp: 0,
        reschedule: AtomicBool::new(false),
        lapic_virt_base: {
            let phys = crate::acpi::local_apic_address() as u64;
            crate::mm::phys_offset() + phys
        },
        lapic_ticks_per_ms: crate::arch::x86_64::apic::lapic_ticks_per_ms(),
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
    // GDT was pre-allocated and populated on the BSP. Just load it.
    let gdt = &*data.gdt_ptr;
    gdt.load();
    CS::set_reg(data.gdt_code);
    DS::set_reg(data.gdt_data);
    SS::set_reg(data.gdt_data);
    load_tss(data.gdt_tss);
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
