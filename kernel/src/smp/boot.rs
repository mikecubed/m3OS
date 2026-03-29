//! AP (Application Processor) bootstrap.
//!
//! Implements the INIT-SIPI-SIPI sequence to bring APs from reset into 64-bit
//! long mode and Rust code. The trampoline runs through three stages:
//!
//! 1. **16-bit real mode** — set up a temporary GDT and enter protected mode.
//! 2. **32-bit protected mode** — enable PAE, load PML4 into CR3, enable long
//!    mode via IA32_EFER, enable paging, jump to 64-bit code.
//! 3. **64-bit long mode** — load the AP's stack, call `ap_entry()`.

use core::sync::atomic::Ordering;

// ---------------------------------------------------------------------------
// Trampoline page layout
// ---------------------------------------------------------------------------

/// Physical address where the AP trampoline is placed.
/// Must be below 1 MiB (SIPI vector is a page number: phys = vector << 12).
/// 0x8000 is a conventional choice — above the real-mode IVT and BIOS data area.
const TRAMPOLINE_PHYS: u64 = 0x8000;

/// SIPI vector = trampoline physical page number.
const SIPI_VECTOR: u8 = (TRAMPOLINE_PHYS >> 12) as u8; // 0x08

// Data field offsets from the trampoline page base.
const DATA_GDT: usize = 0xF00;
const DATA_GDTR: usize = 0xF30;
const DATA_PML4: usize = 0xF38;
const DATA_STACK: usize = 0xF40;
const DATA_ENTRY: usize = 0xF48;
const DATA_PERCOREDATA: usize = 0xF50;
const DATA_IDTR: usize = 0xF58; // 10 bytes: 2-byte limit + 8-byte base
const DATA_CR4: usize = 0xF68; // BSP's CR4 value

// ---------------------------------------------------------------------------
// Trampoline machine code
// ---------------------------------------------------------------------------

/// Build the AP trampoline as a raw byte array.
///
/// The code runs at physical address TRAMPOLINE_PHYS (0x8000) and transitions
/// from 16-bit real mode → 32-bit protected mode → 64-bit long mode.
///
/// Data fields (GDT, GDTR, PML4, stack, entry point, per-core data pointer)
/// are written separately at fixed offsets in the trampoline page.
fn build_trampoline_code() -> alloc::vec::Vec<u8> {
    let mut c = alloc::vec::Vec::with_capacity(256);

    // ---- 16-bit real mode ----
    // AP starts at CS:IP = 0x0800:0x0000 (physical 0x8000)

    c.extend_from_slice(&[0xFA]); // cli
    c.extend_from_slice(&[0x31, 0xC0]); // xor ax, ax
    c.extend_from_slice(&[0x8E, 0xD8]); // mov ds, ax
    c.extend_from_slice(&[0x8E, 0xC0]); // mov es, ax
    c.extend_from_slice(&[0x8E, 0xD0]); // mov ss, ax

    // lgdt [0x8F30] — 66 prefix for 32-bit base in pseudo-descriptor
    c.extend_from_slice(&[0x66, 0x0F, 0x01, 0x16, 0x30, 0x8F]);

    // Enable protected mode
    c.extend_from_slice(&[0x0F, 0x20, 0xC0]); // mov eax, cr0
    c.extend_from_slice(&[0x0C, 0x01]); // or al, 1
    c.extend_from_slice(&[0x0F, 0x22, 0xC0]); // mov cr0, eax

    // Far jump to 32-bit code at 0x8040 with selector 0x08
    c.extend_from_slice(&[0x66, 0xEA]);
    c.extend_from_slice(&0x0000_8040u32.to_le_bytes());
    c.extend_from_slice(&0x0008u16.to_le_bytes());

    // Pad to offset 0x40
    c.resize(0x40, 0x90);

    // ---- 32-bit protected mode ----

    c.extend_from_slice(&[0x66, 0xB8, 0x10, 0x00]); // mov ax, 0x10
    c.extend_from_slice(&[0x8E, 0xD8]); // mov ds, ax
    c.extend_from_slice(&[0x8E, 0xC0]); // mov es, ax
    c.extend_from_slice(&[0x8E, 0xD0]); // mov ss, ax

    // Enable PAE
    c.extend_from_slice(&[0x0F, 0x20, 0xE0]); // mov eax, cr4
    c.extend_from_slice(&[0x83, 0xC8, 0x20]); // or eax, 0x20
    c.extend_from_slice(&[0x0F, 0x22, 0xE0]); // mov cr4, eax

    // Load PML4 into CR3
    c.extend_from_slice(&[0xA1]); // mov eax, [moffs32]
    c.extend_from_slice(&0x0000_8F38u32.to_le_bytes());
    c.extend_from_slice(&[0x0F, 0x22, 0xD8]); // mov cr3, eax

    // Enable long mode + NXE via EFER.
    // NXE (bit 11) MUST be set before enabling paging, because the bootloader's
    // page tables use the NX bit (bit 63). Without NXE, bit 63 is reserved and
    // accessing any page with NX set causes a page fault.
    c.extend_from_slice(&[0xB9]); // mov ecx, imm32
    c.extend_from_slice(&0xC000_0080u32.to_le_bytes());
    c.extend_from_slice(&[0x0F, 0x32]); // rdmsr
    c.extend_from_slice(&[0x0D]); // or eax, imm32
    c.extend_from_slice(&0x0000_0900u32.to_le_bytes()); // LME (bit 8) + NXE (bit 11)
    c.extend_from_slice(&[0x0F, 0x30]); // wrmsr

    // Enable paging
    c.extend_from_slice(&[0x0F, 0x20, 0xC0]); // mov eax, cr0
    c.extend_from_slice(&[0x0D]); // or eax, imm32
    c.extend_from_slice(&0x8000_0000u32.to_le_bytes());
    c.extend_from_slice(&[0x0F, 0x22, 0xC0]); // mov cr0, eax

    // Far jump to 64-bit code at 0x80A0 with selector 0x18
    c.extend_from_slice(&[0xEA]);
    c.extend_from_slice(&0x0000_80A0u32.to_le_bytes());
    c.extend_from_slice(&0x0018u16.to_le_bytes());

    // Pad to offset 0xA0
    c.resize(0xA0, 0x90);

    // ---- 64-bit long mode ----

    c.extend_from_slice(&[0x66, 0xB8, 0x20, 0x00]); // mov ax, 0x20
    c.extend_from_slice(&[0x8E, 0xD8]); // mov ds, ax
    c.extend_from_slice(&[0x8E, 0xC0]); // mov es, ax
    c.extend_from_slice(&[0x8E, 0xD0]); // mov ss, ax
    c.extend_from_slice(&[0x66, 0x31, 0xC0]); // xor ax, ax
    c.extend_from_slice(&[0x8E, 0xE0]); // mov fs, ax
    c.extend_from_slice(&[0x8E, 0xE8]); // mov gs, ax

    // Load stack: REX.W mov rax, [moffs64]; mov rsp, rax
    c.extend_from_slice(&[0x48, 0xA1]);
    c.extend_from_slice(&0x0000_0000_0000_8F40u64.to_le_bytes());
    c.extend_from_slice(&[0x48, 0x89, 0xC4]); // mov rsp, rax

    // Load per-core data ptr into rdi (first argument to ap_entry)
    c.extend_from_slice(&[0x48, 0xA1]);
    c.extend_from_slice(&0x0000_0000_0000_8F50u64.to_le_bytes());
    c.extend_from_slice(&[0x48, 0x89, 0xC7]); // mov rdi, rax

    // Load entry point and jump
    c.extend_from_slice(&[0x48, 0xA1]);
    c.extend_from_slice(&0x0000_0000_0000_8F48u64.to_le_bytes());
    // Align RSP to 8 mod 16 (System V ABI: RSP is 8-mod-16 at function entry)
    c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x08]); // sub rsp, 8
    c.extend_from_slice(&[0xFF, 0xE0]); // jmp rax

    c
}

fn build_trampoline_gdt() -> [u64; 5] {
    [
        0x0000_0000_0000_0000, // null
        0x00CF_9A00_0000_FFFF, // code32: base=0, limit=4GB, code, readable, 32-bit
        0x00CF_9200_0000_FFFF, // data32: base=0, limit=4GB, data, writable, 32-bit
        0x0020_9A00_0000_0000, // code64: long mode, code
        0x0000_9200_0000_0000, // data64: data, writable
    ]
}

// ---------------------------------------------------------------------------
// Trampoline installation
// ---------------------------------------------------------------------------

fn install_trampoline() {
    let phys_off = crate::mm::phys_offset();
    let page_virt = (phys_off + TRAMPOLINE_PHYS) as *mut u8;

    let code = build_trampoline_code();

    unsafe {
        core::ptr::write_bytes(page_virt, 0, 4096);
        core::ptr::copy_nonoverlapping(code.as_ptr(), page_virt, code.len());
    }

    // Write the GDT.
    let gdt = build_trampoline_gdt();
    let gdt_virt = (phys_off + TRAMPOLINE_PHYS + DATA_GDT as u64) as *mut u64;
    for (j, &entry) in gdt.iter().enumerate() {
        unsafe {
            gdt_virt.add(j).write(entry);
        }
    }

    // Write the GDTR pseudo-descriptor.
    let gdtr_virt = (phys_off + TRAMPOLINE_PHYS + DATA_GDTR as u64) as *mut u8;
    let gdt_limit = (gdt.len() * 8 - 1) as u16;
    let gdt_base = (TRAMPOLINE_PHYS + DATA_GDT as u64) as u32;
    unsafe {
        (gdtr_virt as *mut u16).write(gdt_limit);
        (gdtr_virt.add(2) as *mut u32).write(gdt_base);
    }

    // Write the kernel PML4 physical address.
    // The trampoline loads CR3 via `mov eax, [moffs32]` (32-bit), so the
    // PML4 physical address must fit in 32 bits.
    let pml4_phys = crate::mm::kernel_pml4_phys();
    assert!(
        pml4_phys < 0x1_0000_0000,
        "kernel PML4 at {:#x} exceeds 4 GiB — trampoline CR3 load would truncate",
        pml4_phys
    );
    unsafe {
        ((phys_off + TRAMPOLINE_PHYS + DATA_PML4 as u64) as *mut u64).write(pml4_phys);
    }

    // Write the Rust AP entry point.
    unsafe {
        ((phys_off + TRAMPOLINE_PHYS + DATA_ENTRY as u64) as *mut u64)
            .write(ap_entry as *const () as u64);
    }

    // Save the BSP's CR4 so APs can load it (includes PGE, etc.).
    unsafe {
        let cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        ((phys_off + TRAMPOLINE_PHYS + DATA_CR4 as u64) as *mut u64).write(cr4);
    }

    // Note: DATA_IDTR is no longer used — APs now access the IDT via the
    // Lazy static directly (EFER.NXE fix enables kernel static access).

    // Create temporary identity mapping for the trampoline page.
    identity_map_trampoline();

    log::info!(
        "[smp] trampoline installed at phys={:#x}, entry={:#x}",
        TRAMPOLINE_PHYS,
        ap_entry as *const () as u64
    );
}

fn set_trampoline_ap_data(stack_top: u64, per_core_data_ptr: u64) {
    let phys_off = crate::mm::phys_offset();
    unsafe {
        ((phys_off + TRAMPOLINE_PHYS + DATA_STACK as u64) as *mut u64).write_volatile(stack_top);
        ((phys_off + TRAMPOLINE_PHYS + DATA_PERCOREDATA as u64) as *mut u64)
            .write_volatile(per_core_data_ptr);
    }
}

// ---------------------------------------------------------------------------
// IPI sending (delegates to super::ipi LAPIC helpers)
// ---------------------------------------------------------------------------

use super::ipi::{lapic_read, lapic_write, wait_icr_idle, LAPIC_ICR_HIGH, LAPIC_ICR_LOW};

fn send_init_ipi(apic_id: u8) {
    unsafe {
        wait_icr_idle();
        lapic_write(LAPIC_ICR_HIGH, (apic_id as u32) << 24);
        lapic_write(LAPIC_ICR_LOW, 0x0000_C500); // INIT assert
        wait_icr_idle();
        lapic_write(LAPIC_ICR_HIGH, (apic_id as u32) << 24);
        lapic_write(LAPIC_ICR_LOW, 0x0000_8500); // INIT de-assert
        wait_icr_idle();
    }
}

fn send_sipi(apic_id: u8, vector: u8) {
    unsafe {
        wait_icr_idle();
        lapic_write(LAPIC_ICR_HIGH, (apic_id as u32) << 24);
        lapic_write(LAPIC_ICR_LOW, 0x0000_0600 | vector as u32);
        wait_icr_idle();
    }
}

/// LAPIC timer current-count register offset.
const LAPIC_TIMER_CURRENT: usize = 0x390;

fn delay_us(us: u64) {
    let tpm = crate::arch::x86_64::apic::lapic_ticks_per_ms();
    let target_ticks = (tpm as u64 * us) / 1000;
    let start = unsafe { lapic_read(LAPIC_TIMER_CURRENT) };
    loop {
        let current = unsafe { lapic_read(LAPIC_TIMER_CURRENT) };
        let elapsed = start.wrapping_sub(current) as u64;
        if elapsed >= target_ticks {
            break;
        }
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// AP boot orchestration
// ---------------------------------------------------------------------------

/// Boot all Application Processors discovered in the MADT.
///
/// Must be called from the BSP after APIC, scheduler, and per-core data
/// initialization.
pub fn boot_aps() {
    let madt = crate::acpi::madt_info();
    let bsp_apic_id = super::bsp_apic_id();

    install_trampoline();

    let mut booted = 0u8;

    for i in 0..madt.local_apic_count {
        let entry = match &madt.local_apics[i] {
            Some(e) => e,
            None => continue,
        };

        if entry.apic_id == bsp_apic_id {
            continue;
        }
        if entry.flags & 1 == 0 {
            continue;
        }

        let core_id = unsafe { super::APIC_TO_CORE[entry.apic_id as usize] };
        if core_id == 0xFF {
            continue;
        }

        let per_core_ptr = super::init_ap_per_core(core_id, entry.apic_id);
        let stack_top = unsafe { (*per_core_ptr).kernel_stack_top };
        set_trampoline_ap_data(stack_top, per_core_ptr as u64);

        log::info!(
            "[smp] booting AP: core_id={}, APIC ID={}",
            core_id,
            entry.apic_id
        );

        // INIT-SIPI-SIPI sequence per Intel spec.
        send_init_ipi(entry.apic_id);
        delay_us(10_000); // 10 ms after INIT
        send_sipi(entry.apic_id, SIPI_VECTOR);
        delay_us(200);
        send_sipi(entry.apic_id, SIPI_VECTOR);

        // Wait for AP to signal via is_online.
        let mut started = false;
        let online_flag = unsafe { &(*per_core_ptr).is_online };
        for _ in 0..10_000_000u64 {
            if online_flag.load(Ordering::Acquire) {
                started = true;
                break;
            }
            core::hint::spin_loop();
        }

        if started {
            booted += 1;
            log::info!(
                "[smp] AP core_id={} (APIC ID={}) is online",
                core_id,
                entry.apic_id
            );
        } else {
            log::warn!("[smp] AP APIC ID={} did not start (timeout)", entry.apic_id);
        }
    }

    log::info!("[smp] {} AP(s) booted successfully", booted);
    remove_trampoline_identity_map();
}

// ---------------------------------------------------------------------------
// AP entry point
// ---------------------------------------------------------------------------

/// Rust entry point for APs, called from the trampoline.
extern "C" fn ap_entry(per_core_data_ptr: *mut super::PerCoreData) -> ! {
    // Load BSP's CR4 value to match feature flags (PGE, etc.).
    let bsp_cr4 =
        unsafe { core::ptr::read_volatile((TRAMPOLINE_PHYS + DATA_CR4 as u64) as *const u64) };
    unsafe {
        core::arch::asm!("mov cr4, {}", in(reg) bsp_cr4, options(nostack));
    }

    let data = unsafe { &*per_core_data_ptr };

    // Load this AP's GDT and TSS (pre-allocated on BSP).
    unsafe {
        super::per_core_gdt_init(data);
    }

    // Load the shared IDT. With EFER.NXE set, the AP can now access
    // kernel statics including the IDT Lazy.
    crate::arch::x86_64::interrupts::init();

    // Set gs_base to this AP's PerCoreData.
    super::write_gs_base(per_core_data_ptr as u64);

    // Initialize this AP's LAPIC and timer using the phys_offset-based address.
    // Now that EFER.NXE is set, APs can access the phys_offset range.
    ap_lapic_init_from(data.lapic_virt_base as usize, data.lapic_ticks_per_ms);

    // Signal that this AP is online.
    data.is_online.store(true, Ordering::Release);

    log::info!(
        "[smp] AP core_id={} fully initialized, entering scheduler",
        data.core_id
    );

    // Spawn a per-core idle task for this AP.
    crate::task::spawn_idle_for_core(ap_idle_task, data.core_id);

    // Enter the per-core scheduler loop (never returns).
    crate::task::run()
}

/// Idle task for AP cores — halts until an interrupt wakes the core.
fn ap_idle_task() -> ! {
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

// ---------------------------------------------------------------------------
// Identity mapping for the trampoline page
// ---------------------------------------------------------------------------

/// Identity-map a physical page at virtual address = physical address.
///
/// Allocates intermediate page table levels as needed.
fn identity_map_page(phys_addr: u64) {
    use x86_64::structures::paging::{PageTable, PageTableFlags};

    let phys_off = crate::mm::phys_offset();
    let pml4_phys = crate::mm::kernel_pml4_phys();
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    let page_aligned = phys_addr & !0xFFF;

    let p4_idx = ((page_aligned >> 39) & 0x1FF) as usize;
    let p3_idx = ((page_aligned >> 30) & 0x1FF) as usize;
    let p2_idx = ((page_aligned >> 21) & 0x1FF) as usize;
    let p1_idx = ((page_aligned >> 12) & 0x1FF) as usize;

    unsafe {
        let pml4: &mut PageTable = &mut *((phys_off + pml4_phys) as *mut PageTable);

        if !pml4[p4_idx].flags().contains(PageTableFlags::PRESENT) {
            let frame =
                crate::mm::frame_allocator::allocate_frame().expect("OOM: identity map PDPT");
            core::ptr::write_bytes(
                (phys_off + frame.start_address().as_u64()) as *mut u8,
                0,
                4096,
            );
            pml4[p4_idx].set_addr(frame.start_address(), flags);
        }

        let pdpt: &mut PageTable =
            &mut *((phys_off + pml4[p4_idx].addr().as_u64()) as *mut PageTable);
        if !pdpt[p3_idx].flags().contains(PageTableFlags::PRESENT) {
            let frame = crate::mm::frame_allocator::allocate_frame().expect("OOM: identity map PD");
            core::ptr::write_bytes(
                (phys_off + frame.start_address().as_u64()) as *mut u8,
                0,
                4096,
            );
            pdpt[p3_idx].set_addr(frame.start_address(), flags);
        }

        let pd: &mut PageTable =
            &mut *((phys_off + pdpt[p3_idx].addr().as_u64()) as *mut PageTable);
        if !pd[p2_idx].flags().contains(PageTableFlags::PRESENT) {
            let frame = crate::mm::frame_allocator::allocate_frame().expect("OOM: identity map PT");
            core::ptr::write_bytes(
                (phys_off + frame.start_address().as_u64()) as *mut u8,
                0,
                4096,
            );
            pd[p2_idx].set_addr(frame.start_address(), flags);
        }

        let pt: &mut PageTable = &mut *((phys_off + pd[p2_idx].addr().as_u64()) as *mut PageTable);
        pt[p1_idx].set_addr(x86_64::PhysAddr::new(page_aligned), flags);
        x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(page_aligned));
    }
}

fn identity_map_trampoline() {
    identity_map_page(TRAMPOLINE_PHYS);
    log::info!(
        "[smp] identity-mapped trampoline page at {:#x}",
        TRAMPOLINE_PHYS
    );
}

/// Remove the identity mapping for a page.
fn remove_identity_map(phys_addr: u64) {
    use x86_64::structures::paging::{PageTable, PageTableFlags};

    let phys_off = crate::mm::phys_offset();
    let pml4_phys = crate::mm::kernel_pml4_phys();
    let page_aligned = phys_addr & !0xFFF;

    let p4_idx = ((page_aligned >> 39) & 0x1FF) as usize;
    let p3_idx = ((page_aligned >> 30) & 0x1FF) as usize;
    let p2_idx = ((page_aligned >> 21) & 0x1FF) as usize;
    let p1_idx = ((page_aligned >> 12) & 0x1FF) as usize;

    unsafe {
        let pml4: &mut PageTable = &mut *((phys_off + pml4_phys) as *mut PageTable);
        if !pml4[p4_idx].flags().contains(PageTableFlags::PRESENT) {
            return;
        }
        let pdpt: &mut PageTable =
            &mut *((phys_off + pml4[p4_idx].addr().as_u64()) as *mut PageTable);
        if !pdpt[p3_idx].flags().contains(PageTableFlags::PRESENT) {
            return;
        }
        let pd: &mut PageTable =
            &mut *((phys_off + pdpt[p3_idx].addr().as_u64()) as *mut PageTable);
        if !pd[p2_idx].flags().contains(PageTableFlags::PRESENT) {
            return;
        }
        let pt: &mut PageTable = &mut *((phys_off + pd[p2_idx].addr().as_u64()) as *mut PageTable);
        pt[p1_idx].set_unused();
        x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(page_aligned));
    }
}

/// Initialize the LAPIC on an AP using a direct base address and BSP-calibrated ticks/ms.
fn ap_lapic_init_from(lapic_base: usize, tpm: u32) {
    unsafe {
        let spur = core::ptr::read_volatile((lapic_base + 0x0F0) as *const u32);
        core::ptr::write_volatile((lapic_base + 0x0F0) as *mut u32, spur | 0x1FF);
        core::ptr::write_volatile((lapic_base + 0x080) as *mut u32, 0); // TPR = 0

        // LAPIC timer: periodic, vector 32, 10 ms period.
        core::ptr::write_volatile((lapic_base + 0x320) as *mut u32, 32 | (1 << 17));
        core::ptr::write_volatile((lapic_base + 0x3E0) as *mut u32, 0x03); // divide-by-16
        let init_count = (tpm as u64 * 10).min(u32::MAX as u64) as u32;
        core::ptr::write_volatile((lapic_base + 0x380) as *mut u32, init_count);
    }
}

fn remove_trampoline_identity_map() {
    remove_identity_map(TRAMPOLINE_PHYS);
    log::info!("[smp] removed trampoline identity mapping");
}
