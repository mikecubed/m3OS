use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use kernel_core::input::{ScancodeRouter, ScancodeSink};
use spin::{Lazy, Mutex};
use x86_64::VirtAddr;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::panic_diag;
use crate::serial::_panic_print;

use super::gdt;

// ---------------------------------------------------------------------------
// APIC / PIC mode flag
// ---------------------------------------------------------------------------

/// When `true`, interrupt handlers send EOI to the Local APIC instead of the
/// legacy 8259 PIC. Set by `apic::init()` after the APIC subsystem is fully
/// programmed.
pub static USING_APIC: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Two-phase fault kill path (T001)
// ---------------------------------------------------------------------------

/// PID of the process that triggered a ring-3 exception.
///
/// Written by the exception handler (in interrupt context, interrupts
/// disabled) and read by `fault_kill_trampoline` (in task context, outside
/// interrupt). Single-CPU: no concurrent writers.
static FAULT_KILL_PID: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// CoW fault resolution (P17-T031, T032, T033)
// ---------------------------------------------------------------------------

fn bump_current_addr_space_generation() {
    if let Some(addr_space) = crate::process::current_addr_space() {
        addr_space.bump_generation();
    }
}

/// Ring-0 trampoline that runs *outside* interrupt context.
///
/// The exception handler redirects IRET here so that locking and
/// context-switching (which are forbidden inside an ISR) can happen safely.
fn fault_kill_trampoline() -> ! {
    // Disable interrupts immediately — IRET restored user RFLAGS which may
    // have IF set, and we must not take interrupts before acquiring locks.
    x86_64::instructions::interrupts::disable();
    let pid = FAULT_KILL_PID.load(Ordering::Relaxed);
    log::warn!("[fault_kill] trampoline running for pid {}", pid);
    // Close all open FDs so pipe ref-counts reach 0 and EOF propagates.
    crate::process::close_all_fds_for(pid);
    // Deactivate this core's tracked AddressSpace *before* marking Zombie.
    // Once Zombie, another core can reap() and drop the last Arc, turning
    // our raw current_addrspace pointer into a dangling reference.
    if crate::smp::is_per_core_ready() {
        let pc = crate::smp::per_core();
        let old_as_ptr = pc.current_addrspace;
        if !old_as_ptr.is_null() {
            let core_id = pc.core_id;
            // SAFETY: Arc<AddressSpace> is still alive — process is not
            // yet Zombie so reap cannot have been called.
            unsafe { &*old_as_ptr }.deactivate_on_core(core_id);
            let pc_mut = pc as *const crate::smp::PerCoreData as *mut crate::smp::PerCoreData;
            unsafe { (*pc_mut).current_addrspace = core::ptr::null() };
        }
    }
    // Mark the process zombie with SIGSEGV exit code.
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.state = crate::process::ProcessState::Zombie;
            proc.exit_code = Some(-11);
        }
    }
    // Deliver SIGCHLD to parent so waitpid unblocks.
    crate::process::send_sigchld_to_parent(pid);
    // Read the dying process's CR3 before we switch away from it.
    let cr3_phys = {
        let table = crate::process::PROCESS_TABLE.lock();
        table
            .find(pid)
            .and_then(|p| p.addr_space.as_ref().map(|a| a.pml4_phys()))
    };
    // Restore kernel page table before yielding — same reason as sys_exit.
    crate::mm::restore_kernel_cr3();
    // Free the process's user-space page table frames.
    if let Some(phys) = cr3_phys {
        crate::mm::free_process_page_table(phys.as_u64());
    }
    // Permanently remove the kernel task — the process is dead.
    crate::task::mark_current_dead();
}

/// Resolve a copy-on-write page fault at `vaddr`.
///
/// Reads the current PTE, allocates a fresh frame, copies the page contents,
/// maps the new frame as writable, and decrements the old frame's refcount.
///
/// Returns `true` on success, `false` if the faulting mapping is no longer a
/// CoW page or if frame allocation fails (OOM).
pub fn resolve_cow_fault(vaddr: u64) -> bool {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PageTable, PageTableFlags};

    let phys_off = crate::mm::phys_offset();
    let phys_offset = VirtAddr::new(phys_off);
    let addr_space = crate::process::current_addr_space();
    let mut old_phys_to_free = None;
    {
        let _page_table_guard = addr_space.map(|addr_space| addr_space.lock_page_tables());

        let (cr3_frame, _) = Cr3::read();
        let pml4_phys = cr3_frame.start_address().as_u64();

        // Walk the page table to find the PTE for the faulting address.
        let p4_idx = ((vaddr >> 39) & 0x1FF) as usize;
        let p3_idx = ((vaddr >> 30) & 0x1FF) as usize;
        let p2_idx = ((vaddr >> 21) & 0x1FF) as usize;
        let p1_idx = ((vaddr >> 12) & 0x1FF) as usize;

        unsafe {
            let pml4: &PageTable = &*(phys_offset + pml4_phys).as_ptr::<PageTable>();
            let p4e = &pml4[p4_idx];
            if !p4e.flags().contains(PageTableFlags::PRESENT) {
                return false;
            }

            let pdpt: &PageTable = &*(phys_offset + p4e.addr().as_u64()).as_ptr::<PageTable>();
            let p3e = &pdpt[p3_idx];
            if !p3e.flags().contains(PageTableFlags::PRESENT) {
                return false;
            }

            let pd: &PageTable = &*(phys_offset + p3e.addr().as_u64()).as_ptr::<PageTable>();
            let p2e = &pd[p2_idx];
            if !p2e.flags().contains(PageTableFlags::PRESENT) {
                return false;
            }

            let pt: &mut PageTable =
                &mut *(phys_offset + p2e.addr().as_u64()).as_mut_ptr::<PageTable>();
            let pte = &mut pt[p1_idx];
            let pte_flags = pte.flags();
            if !pte_flags.contains(PageTableFlags::PRESENT)
                || !pte_flags.contains(PageTableFlags::BIT_9)
                || pte_flags.contains(PageTableFlags::WRITABLE)
            {
                return false;
            }

            let old_phys = pte.addr().as_u64();
            let old_refcount = crate::mm::frame_allocator::refcount_get(old_phys);

            if old_refcount <= 1 {
                // P17-T033: fast path — sole owner, just remap as writable
                // and clear the CoW marker bit.
                let flags = (pte.flags() | PageTableFlags::WRITABLE) & !PageTableFlags::BIT_9;
                pte.set_addr(pte.addr(), flags);
            } else {
                // Allocate a fresh frame. If out of memory, return false so the
                // page fault handler falls through to the kill path instead of
                // panicking the kernel (user-triggerable OOM must not be a DoS).
                let new_frame = match crate::mm::frame_allocator::allocate_frame() {
                    Some(f) => f,
                    None => return false,
                };
                let new_phys = new_frame.start_address().as_u64();

                let src = (phys_off + old_phys) as *const u8;
                let dst = (phys_off + new_phys) as *mut u8;
                core::ptr::copy_nonoverlapping(src, dst, 4096);

                // Map the new frame writable, clear the CoW marker.
                let flags = (pte.flags() | PageTableFlags::WRITABLE) & !PageTableFlags::BIT_9;
                pte.set_addr(new_frame.start_address(), flags);
                old_phys_to_free = Some(old_phys);
            }
        }
    }

    if crate::smp::is_per_core_ready()
        && let Some(addr_space) = addr_space
    {
        crate::smp::tlb::tlb_shootdown_range(addr_space, vaddr, vaddr + 4096);
    } else {
        x86_64::instructions::tlb::flush(VirtAddr::new(vaddr));
    }
    if let Some(old_phys) = old_phys_to_free {
        crate::mm::frame_allocator::free_frame(old_phys);
    }
    bump_current_addr_space_generation();
    true
}

/// Check whether the PTE for `vaddr` has the guard-page marker bit (BIT_10) set.
fn has_guard_marker(vaddr: u64) -> bool {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PageTable, PageTableFlags};

    let phys_off = crate::mm::phys_offset();
    let phys_offset_va = VirtAddr::new(phys_off);

    let (cr3_frame, _) = Cr3::read();
    let pml4_phys = cr3_frame.start_address().as_u64();

    let p4_idx = ((vaddr >> 39) & 0x1FF) as usize;
    let p3_idx = ((vaddr >> 30) & 0x1FF) as usize;
    let p2_idx = ((vaddr >> 21) & 0x1FF) as usize;
    let p1_idx = ((vaddr >> 12) & 0x1FF) as usize;

    unsafe {
        let pml4: &PageTable = &*(phys_offset_va + pml4_phys).as_ptr::<PageTable>();
        if !pml4[p4_idx].flags().contains(PageTableFlags::PRESENT) {
            return false;
        }
        let pdpt: &PageTable =
            &*(phys_offset_va + pml4[p4_idx].addr().as_u64()).as_ptr::<PageTable>();
        if !pdpt[p3_idx].flags().contains(PageTableFlags::PRESENT) {
            return false;
        }
        let pd: &PageTable =
            &*(phys_offset_va + pdpt[p3_idx].addr().as_u64()).as_ptr::<PageTable>();
        if !pd[p2_idx].flags().contains(PageTableFlags::PRESENT) {
            return false;
        }
        let pt: &PageTable = &*(phys_offset_va + pd[p2_idx].addr().as_u64()).as_ptr::<PageTable>();
        pt[p1_idx].flags().contains(PageTableFlags::BIT_10)
    }
}

/// Public entry point for kernel-context VMA demand paging.
///
/// Revalidates the current VMA metadata while holding the address-space
/// mutation lock so concurrent `munmap` / `mprotect` cannot publish stale
/// permissions across the lock boundary.
pub fn demand_map_vma_page_from_kernel(vaddr: u64, require_write: bool) -> bool {
    demand_map_vma_page(vaddr, require_write)
}

/// Demand-page a single 4 KiB user-accessible frame at the page containing
/// `vaddr`. Used for stack growth, VMA demand faults, and any other lazy
/// mapping.
///
/// `prot` uses POSIX constants: `PROT_READ=1`, `PROT_WRITE=2`, `PROT_EXEC=4`.
/// Pass `0x3` (`PROT_READ|PROT_WRITE`) for stack pages.
///
/// Called from the page fault ISR and from kernel-context demand faulting.
/// Returns `true` on success, `false` on OOM.
fn demand_map_user_page_locked(vaddr: u64, prot: u64) -> bool {
    use x86_64::structures::paging::PageTableFlags;
    use x86_64::structures::paging::Translate as _;

    const PROT_WRITE: u64 = 0x2;
    const PROT_EXEC: u64 = 0x4;

    let phys_off = crate::mm::phys_offset();
    let page_vaddr = VirtAddr::new(vaddr & !0xFFF);

    {
        let mapper = unsafe { crate::mm::paging::get_mapper() };
        if mapper.translate_addr(page_vaddr).is_some() {
            return true;
        }
    }

    // Allocate a fresh zeroed frame for the data page.
    let frame = match crate::mm::frame_allocator::allocate_frame() {
        Some(f) => f,
        None => return false,
    };
    let frame_phys = frame.start_address().as_u64();
    unsafe {
        core::ptr::write_bytes((phys_off + frame_phys) as *mut u8, 0, 4096);
    }

    // Build PTE flags from the POSIX prot bits.
    let mut data_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if prot & PROT_WRITE != 0 {
        data_flags |= PageTableFlags::WRITABLE;
    }
    if prot & PROT_EXEC == 0 {
        data_flags |= PageTableFlags::NO_EXECUTE;
    }

    if unsafe { crate::mm::paging::map_current_user_page_locked(page_vaddr, frame, data_flags) }
        .is_err()
    {
        crate::mm::frame_allocator::free_frame(frame_phys);
        return false;
    }
    true
}

fn demand_map_user_page(vaddr: u64, prot: u64) -> bool {
    let addr_space = crate::process::current_addr_space();
    let page_base = vaddr & !0xFFF;
    let mapped = {
        let _page_table_guard = addr_space.map(|addr_space| addr_space.lock_page_tables());
        demand_map_user_page_locked(vaddr, prot)
    };
    if !mapped {
        return false;
    }
    if crate::smp::is_per_core_ready()
        && let Some(addr_space) = addr_space
    {
        crate::smp::tlb::tlb_shootdown_range(addr_space, page_base, page_base + 4096);
    }
    bump_current_addr_space_generation();
    true
}

fn demand_map_vma_page(vaddr: u64, require_write: bool) -> bool {
    const PROT_READ: u64 = 0x1;
    const PROT_WRITE: u64 = 0x2;
    const PROT_EXEC: u64 = 0x4;

    let pid = crate::process::current_pid();
    if pid == 0 {
        return false;
    }

    let addr_space = crate::process::current_addr_space();
    let page_base = vaddr & !0xFFF;
    let mapped = {
        let _page_table_guard = addr_space.map(|addr_space| addr_space.lock_page_tables());

        let Some(prot) = crate::process::shared_vma_prot(pid, vaddr) else {
            return false;
        };

        let any_access = prot & (PROT_READ | PROT_WRITE | PROT_EXEC) != 0;
        let write_ok = !require_write || prot & PROT_WRITE != 0;
        if !any_access || !write_ok {
            return false;
        }

        demand_map_user_page_locked(vaddr, prot)
    };
    if !mapped {
        return false;
    }
    if crate::smp::is_per_core_ready()
        && let Some(addr_space) = addr_space
    {
        crate::smp::tlb::tlb_shootdown_range(addr_space, page_base, page_base + 4096);
    }
    bump_current_addr_space_generation();
    true
}

// ---------------------------------------------------------------------------
// IDT
// ---------------------------------------------------------------------------

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();

    // CPU exceptions
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.general_protection_fault
        .set_handler_fn(general_protection_fault_handler);
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }

    // Hardware IRQs
    idt[InterruptIndex::Timer as u8].set_handler_fn(timer_handler);
    idt[InterruptIndex::Keyboard as u8].set_handler_fn(keyboard_handler);
    idt[InterruptIndex::VirtioNet as u8].set_handler_fn(virtio_net_handler);
    idt[InterruptIndex::Serial as u8].set_handler_fn(serial_handler);

    // APIC spurious interrupt vector — must NOT send EOI.
    idt[InterruptIndex::Spurious as u8].set_handler_fn(spurious_handler);

    // SMP IPI vectors (Phase 25).
    idt[crate::smp::ipi::IPI_RESCHEDULE].set_handler_fn(reschedule_ipi_handler);
    idt[crate::smp::ipi::IPI_TLB_SHOOTDOWN].set_handler_fn(tlb_shootdown_ipi_handler);

    idt
});

/// Load the IDT.
pub fn init() {
    IDT.load();
}

// ---------------------------------------------------------------------------
// Exception handlers
// ---------------------------------------------------------------------------

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    // Use _panic_print to avoid deadlocking on the serial mutex if the exception
    // fires while normal code holds the lock.
    _panic_print(format_args!("[int] breakpoint: {:?}\n", stack_frame));
}

extern "x86-interrupt" fn page_fault_handler(
    mut stack_frame: InterruptStackFrame,
    err: PageFaultErrorCode,
) {
    let addr = x86_64::registers::control::Cr2::read();

    // Check if the fault came from ring 3 (user mode).
    if stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
        // P17-T031: detect CoW faults — a write to a present, non-writable
        // page marked with BIT_9 (the CoW marker set by cow_clone_user_pages).
        let is_write = err.contains(PageFaultErrorCode::CAUSED_BY_WRITE);
        let is_present = err.contains(PageFaultErrorCode::PROTECTION_VIOLATION);
        if is_write
            && is_present
            && let Ok(fault_vaddr) = addr
        {
            let fault_addr_u64 = fault_vaddr.as_u64();
            // CoW fault — revalidate and resolve directly in the ISR. Safe
            // because the fault is from ring 3 (no kernel locks held), and
            // the CoW path serializes its page-table mutation under the
            // current address-space lock before issuing TLB shootdowns.
            if resolve_cow_fault(fault_addr_u64) {
                return;
            }
            // OOM or no-longer-CoW mapping — fall through to other handlers / kill.
        }

        // Demand-paging for the stack region: musl's __init_tls and malloc
        // write above ELF_STACK_TOP (Linux maps an 8 MB region so this is
        // always valid there). When the fault is a write to an unmapped page
        // within 8 MiB above ELF_STACK_TOP, allocate a fresh frame and map it.
        if is_write
            && !is_present
            && let Ok(fault_vaddr) = addr
        {
            let fault_addr_u64 = fault_vaddr.as_u64();
            let stack_top = crate::mm::elf::ELF_STACK_TOP;
            let stack_bottom = stack_top - crate::mm::elf::STACK_PAGES * 4096;
            // Allow demand-paging 8 MiB above ELF_STACK_TOP and down to guard page.
            const DEMAND_LIMIT: u64 = 8 * 1024 * 1024; // 8 MiB
            if fault_addr_u64 >= stack_bottom
                && fault_addr_u64 < stack_top + DEMAND_LIMIT
                && !has_guard_marker(fault_addr_u64)
                && demand_map_user_page(fault_addr_u64, 0x3)
            // PROT_READ|PROT_WRITE
            {
                return;
            }
        }

        // Phase 36: VMA-based demand paging for mmap regions.
        // If the fault address is inside a valid VMA, allocate a frame on demand.
        if !is_present && let Ok(fault_vaddr) = addr {
            let fault_addr_u64 = fault_vaddr.as_u64();
            if demand_map_vma_page(fault_addr_u64, is_write) {
                return;
            }
        }

        let pid = crate::process::current_pid();
        _panic_print(format_args!(
            "[int] userspace page fault: pid={} addr={:?} err={:?} rip={:#x} — process killed\n",
            pid,
            addr,
            err,
            stack_frame.instruction_pointer.as_u64()
        ));
        _panic_print(format_args!(
            "[int] RSP={:#x}\n",
            stack_frame.stack_pointer.as_u64()
        ));
        if crate::smp::is_per_core_ready() {
            let task_idx = crate::smp::per_core()
                .current_task_idx
                .load(Ordering::Relaxed);
            if let Some(guard) = crate::task::try_lock_scheduler()
                && task_idx >= 0
                && let Some(task) = guard.get_task(task_idx as usize)
            {
                _panic_print(format_args!(
                    "[int] task[{}]: state={:?} saved_rsp=0x{:016x}\n",
                    task_idx, task.state, task.saved_rsp
                ));
            }
        }
        panic_diag::dump_crash_context();
        crate::trace::dump_trace_rings();
        // Store the PID for the trampoline. Safe: interrupts are disabled
        // during exception handling on a single CPU.
        FAULT_KILL_PID.store(pid, Ordering::Relaxed);
        // Redirect the interrupted context to fault_kill_trampoline, which
        // runs in ring 0 outside interrupt context where locking is safe.
        // SAFETY: we modify the interrupt return frame while interrupts are
        // disabled. The trampoline is a valid kernel function pointer.
        // We must also set RSP to the current kernel stack (not the user RSP
        // that was saved in the frame), otherwise IRET would pop the user RSP
        // and the trampoline would run with an unmapped stack → GPF.
        let kernel_rsp: u64;
        unsafe {
            core::arch::asm!("mov {}, rsp", out(reg) kernel_rsp);
        }
        unsafe {
            stack_frame.as_mut().update(|f| {
                f.instruction_pointer = VirtAddr::new(fault_kill_trampoline as *const () as u64);
                f.code_segment = gdt::kernel_code_selector();
                f.cpu_flags &= !x86_64::registers::rflags::RFlags::INTERRUPT_FLAG;
                f.stack_pointer = VirtAddr::new(kernel_rsp);
                f.stack_segment = gdt::kernel_data_selector();
            });
        }
        return;
    }

    // Ring-0 page fault: unrecoverable kernel bug.
    _panic_print(format_args!(
        "[int] kernel page fault: addr={:?} err={:?}\n{:?}\n",
        addr, err, stack_frame
    ));
    let (cr3_frame, _) = x86_64::registers::control::Cr3::read_raw();
    _panic_print(format_args!(
        "[int] KERNEL page fault — CR3=0x{:016x}\n",
        cr3_frame.start_address().as_u64()
    ));
    panic_diag::dump_crash_context();
    crate::trace::dump_trace_rings();
    crate::hlt_loop();
}

extern "x86-interrupt" fn general_protection_fault_handler(
    mut stack_frame: InterruptStackFrame,
    _err: u64,
) {
    // Check if the fault came from ring 3.
    if stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
        let pid = crate::process::current_pid();
        _panic_print(format_args!(
            "[int] userspace GPF: pid={} — process killed\n{:?}\n",
            pid, stack_frame
        ));
        if crate::smp::is_per_core_ready() {
            let task_idx = crate::smp::per_core()
                .current_task_idx
                .load(Ordering::Relaxed);
            if let Some(guard) = crate::task::try_lock_scheduler()
                && task_idx >= 0
                && let Some(task) = guard.get_task(task_idx as usize)
            {
                _panic_print(format_args!(
                    "[int] pid={} task[{}]: state={:?}\n",
                    pid, task_idx, task.state
                ));
            }
        }
        let selector_idx = _err >> 3;
        let table = (_err >> 1) & 3;
        let external = _err & 1;
        _panic_print(format_args!(
            "[int] GPF error_code={:#x} (selector_idx={}, table={}, external={})\n",
            _err, selector_idx, table, external
        ));
        panic_diag::dump_crash_context();
        crate::trace::dump_trace_rings();
        // Store the PID and redirect to the kill trampoline (same pattern as
        // page_fault_handler — no blocking allowed inside an ISR).
        FAULT_KILL_PID.store(pid, Ordering::Relaxed);
        // SAFETY: same as page_fault_handler above.
        let kernel_rsp: u64;
        unsafe {
            core::arch::asm!("mov {}, rsp", out(reg) kernel_rsp);
        }
        unsafe {
            stack_frame.as_mut().update(|f| {
                f.instruction_pointer = VirtAddr::new(fault_kill_trampoline as *const () as u64);
                f.code_segment = gdt::kernel_code_selector();
                f.cpu_flags &= !x86_64::registers::rflags::RFlags::INTERRUPT_FLAG;
                f.stack_pointer = VirtAddr::new(kernel_rsp);
                f.stack_segment = gdt::kernel_data_selector();
            });
        }
        return;
    }
    _panic_print(format_args!("[int] GPF: {:?}\n", stack_frame));
    let selector_idx = _err >> 3;
    let table = (_err >> 1) & 3;
    let external = _err & 1;
    _panic_print(format_args!(
        "[int] GPF error_code={:#x} (selector_idx={}, table={}, external={})\n",
        _err, selector_idx, table, external
    ));
    panic_diag::dump_crash_context();
    crate::trace::dump_trace_rings();
    crate::hlt_loop();
}

extern "x86-interrupt" fn double_fault_handler(stack_frame: InterruptStackFrame, _err: u64) -> ! {
    _panic_print(format_args!("[int] DOUBLE FAULT: {:?}\n", stack_frame));
    _panic_print(format_args!(
        "[int] IST RSP={:#x}\n",
        stack_frame.stack_pointer.as_u64()
    ));
    panic_diag::dump_crash_context();
    crate::trace::dump_trace_rings();
    crate::hlt_loop();
}

// ---------------------------------------------------------------------------
// Hardware IRQ vector offsets
// ---------------------------------------------------------------------------

/// IRQ vectors remapped to start above the CPU exception range.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = 32,
    Keyboard = 33,
    VirtioNet = 34,
    Serial = 36,
    Spurious = 0xFF,
}

// ---------------------------------------------------------------------------
// PIC
// ---------------------------------------------------------------------------

static PICS: Mutex<pic8259::ChainedPics> = Mutex::new(unsafe { pic8259::ChainedPics::new(32, 40) });

/// Initialize and unmask the 8259 PIC.
///
/// # Safety
///
/// Must be called after the IDT is loaded and before interrupts are enabled.
/// Calling it out of order can cause IRQs to fire without a registered handler,
/// resulting in a triple fault.
pub unsafe fn init_pics() {
    unsafe {
        let mut pics = PICS.lock();
        pics.initialize();
        // Mask every IRQ line except IRQ0 (timer) and IRQ1 (keyboard).
        // A set bit disables the line. Any unmasked line without an IDT handler
        // would vector into an uninitialized entry and cause a triple fault.
        // master: bits 2–7 masked (0b1111_1100), slave: all 8 lines masked.
        pics.write_masks(0b1111_1100, 0b1111_1111);
    }
}

// ---------------------------------------------------------------------------
// Timer
// ---------------------------------------------------------------------------

static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Return the current timer tick count (monotonically increasing).
pub fn tick_count() -> u64 {
    TICK_COUNT.load(Ordering::Relaxed)
}

extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    // Only the BSP increments the global tick counter. APs have their own
    // per-1ms LAPIC timers that drive the scheduler but must not skew the
    // global wall-clock tick count (which nanosleep and uptime rely on).
    //
    // In PIC mode (no MADT / single-core), we are always the BSP and
    // `is_bsp()` must not be called — it reads LAPIC MMIO which is not
    // mapped when the APIC was never initialised.
    if !USING_APIC.load(Ordering::Relaxed) || crate::smp::is_bsp() {
        TICK_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    crate::task::signal_reschedule();
    if USING_APIC.load(Ordering::Relaxed) {
        super::apic::lapic_eoi();
    } else {
        unsafe {
            PICS.lock()
                .notify_end_of_interrupt(InterruptIndex::Timer as u8);
        }
    }
}

// ---------------------------------------------------------------------------
// Keyboard scancode ring buffer
// ---------------------------------------------------------------------------
//
// There are TWO separate ring buffers:
//
//   SCANCODE_BUF  — normal TTY / kbd_server path; consumed via
//                   `read_scancode()`.  Only populated when no process
//                   owns the framebuffer (FB_OWNER_PID == 0).
//
//   RAW_SCANCODE_BUF — game input path; consumed via `read_raw_scancode()`
//                   (sys_read_scancode syscall 0x1007).  Only populated when
//                   a process owns the framebuffer (FB_OWNER_PID != 0).
//
// Routing is exclusive: each scancode goes to exactly one buffer based on
// framebuffer ownership.  This prevents stale scancodes from accumulating
// in SCANCODE_BUF during gameplay and replaying when the game exits.

const SCANCODE_BUF_SIZE: usize = 256;
// Bitmask wraparound requires a power-of-two buffer size.
const _: () = assert!(
    SCANCODE_BUF_SIZE.is_power_of_two(),
    "SCANCODE_BUF_SIZE must be a power of two for bitmask wraparound"
);

// TTY path buffer
static mut SCANCODE_BUF: [u8; SCANCODE_BUF_SIZE] = [0u8; SCANCODE_BUF_SIZE];
static SCANCODE_BUF_HEAD: AtomicUsize = AtomicUsize::new(0);
static SCANCODE_BUF_TAIL: AtomicUsize = AtomicUsize::new(0);

// Raw / game-input path buffer
static mut RAW_SCANCODE_BUF: [u8; SCANCODE_BUF_SIZE] = [0u8; SCANCODE_BUF_SIZE];
static RAW_SCANCODE_BUF_HEAD: AtomicUsize = AtomicUsize::new(0);
static RAW_SCANCODE_BUF_TAIL: AtomicUsize = AtomicUsize::new(0);
static RAW_INPUT_ROUTER: Mutex<ScancodeRouter> = Mutex::new(ScancodeRouter::new());

/// Pop one scancode from the **TTY** ring buffer, or `None` if it is empty.
#[allow(dead_code)]
pub fn read_scancode() -> Option<u8> {
    let head = SCANCODE_BUF_HEAD.load(Ordering::Acquire);
    let tail = SCANCODE_BUF_TAIL.load(Ordering::Acquire);
    if head == tail {
        return None;
    }
    // Safety: single consumer; head is only advanced here and never overtakes tail.
    let byte = unsafe { SCANCODE_BUF[head] };
    SCANCODE_BUF_HEAD.store((head + 1) & (SCANCODE_BUF_SIZE - 1), Ordering::Release);
    Some(byte)
}

/// Pop one scancode from the **raw / game-input** ring buffer, or `None`.
pub fn read_raw_scancode() -> Option<u8> {
    let _guard = RAW_INPUT_ROUTER.lock();
    let head = RAW_SCANCODE_BUF_HEAD.load(Ordering::Acquire);
    let tail = RAW_SCANCODE_BUF_TAIL.load(Ordering::Acquire);
    if head == tail {
        return None;
    }
    let byte = unsafe { RAW_SCANCODE_BUF[head] };
    RAW_SCANCODE_BUF_HEAD.store((head + 1) & (SCANCODE_BUF_SIZE - 1), Ordering::Release);
    Some(byte)
}

pub fn reset_raw_input_state() {
    let mut router = RAW_INPUT_ROUTER.lock();
    router.reset();
    RAW_SCANCODE_BUF_HEAD.store(0, Ordering::Release);
    RAW_SCANCODE_BUF_TAIL.store(0, Ordering::Release);
}

#[inline(always)]
unsafe fn push_to_buf(buf: *mut u8, head: &AtomicUsize, tail: &AtomicUsize, byte: u8) {
    let t = tail.load(Ordering::Relaxed);
    let next = (t + 1) & (SCANCODE_BUF_SIZE - 1);
    if next != head.load(Ordering::Acquire) {
        // Safety: caller guarantees `buf` points to a [u8; SCANCODE_BUF_SIZE]
        // and that this is the sole writer (single-producer ISR context).
        unsafe { buf.add(t).write(byte) };
        tail.store(next, Ordering::Release);
    }
    // else: buffer full — silently drop (prefer losing a typematic repeat
    // over blocking an interrupt handler).
}

extern "x86-interrupt" fn keyboard_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut data_port: Port<u8> = Port::new(0x60);
    let mut status_port: Port<u8> = Port::new(0x64);

    // Drain all pending bytes from the i8042 output buffer.
    //
    // Extended scancodes (e.g. 0xE0 prefixed arrow keys) arrive as multi-byte
    // sequences.  With edge-triggered IRQ delivery, reading only one byte per
    // interrupt can strand the second byte of an extended break code until the
    // next key event, making keys appear stuck.  We loop while the i8042
    // status register's Output Buffer Full bit (bit 0) is set.
    //
    // A small iteration cap prevents pathological infinite loops if the
    // controller misbehaves.
    const MAX_DRAIN: usize = 16;

    // Route each byte to the appropriate sink.
    //
    // Ownership can change while we are draining the i8042, so we re-check the
    // framebuffer owner for every new sequence instead of snapshotting once per
    // batch. Multi-byte prefixes (`0xE0`, `0xE1`) stay latched to the sink that
    // received their first byte so an ownership handoff cannot split one
    // extended scancode sequence across RAW_SCANCODE_BUF and SCANCODE_BUF.
    let mut got_tty_byte = false;
    let mut raw_input_router = RAW_INPUT_ROUTER.lock();

    for _ in 0..MAX_DRAIN {
        let status = unsafe { status_port.read() };
        if status & 0x01 == 0 {
            break; // output buffer empty — nothing left to read
        }
        let scancode: u8 = unsafe { data_port.read() };

        match raw_input_router.route_byte(scancode, crate::fb::fb_owner_pid() != 0) {
            ScancodeSink::Raw => unsafe {
                // Raw path: scancodes go to the game buffer only.
                // kbd_server does NOT receive scancodes while a process
                // (DOOM) owns the framebuffer — this prevents stale
                // scancodes from accumulating and replaying after the
                // game exits.
                push_to_buf(
                    (&raw mut RAW_SCANCODE_BUF).cast::<u8>(),
                    &RAW_SCANCODE_BUF_HEAD,
                    &RAW_SCANCODE_BUF_TAIL,
                    scancode,
                );
            },
            ScancodeSink::Tty => {
                unsafe {
                    push_to_buf(
                        (&raw mut SCANCODE_BUF).cast::<u8>(),
                        &SCANCODE_BUF_HEAD,
                        &SCANCODE_BUF_TAIL,
                        scancode,
                    );
                }
                got_tty_byte = true;
            }
        }
    }

    // Signal kbd_server once after draining the whole batch, not per byte.
    if got_tty_byte {
        crate::ipc::notification::signal_irq(1);
    }

    if USING_APIC.load(Ordering::Relaxed) {
        super::apic::lapic_eoi();
    } else {
        unsafe {
            PICS.lock()
                .notify_end_of_interrupt(InterruptIndex::Keyboard as u8);
        }
    }
}

// ---------------------------------------------------------------------------
// APIC spurious interrupt handler
// ---------------------------------------------------------------------------

extern "x86-interrupt" fn spurious_handler(_stack_frame: InterruptStackFrame) {
    // Spurious interrupt (vector 0xFF) — no EOI must be sent.
}

// ---------------------------------------------------------------------------
// SMP IPI handlers (Phase 25)
// ---------------------------------------------------------------------------

/// Reschedule IPI handler (vector 0xFE).
///
/// Sets the reschedule flag on the receiving core, causing the scheduler to
/// pick the next ready task on the next opportunity.
extern "x86-interrupt" fn reschedule_ipi_handler(_stack_frame: InterruptStackFrame) {
    crate::task::signal_reschedule();
    super::apic::lapic_eoi();
}

/// TLB shootdown IPI handler (vector 0xFD).
///
/// Invalidates a specific page on this core's TLB. The target address and
/// synchronization are managed by the TLB shootdown request in `smp::tlb`.
extern "x86-interrupt" fn tlb_shootdown_ipi_handler(_stack_frame: InterruptStackFrame) {
    crate::smp::tlb::handle_tlb_shootdown_ipi();
    super::apic::lapic_eoi();
}

// ---------------------------------------------------------------------------
// virtio-net IRQ handler (P16-T011, P16-T012)
// ---------------------------------------------------------------------------

/// Tracks whether a virtio-net interrupt has fired (for polling by the net task).
pub static VIRTIO_NET_IRQ_PENDING: AtomicBool = AtomicBool::new(false);

extern "x86-interrupt" fn virtio_net_handler(_stack_frame: InterruptStackFrame) {
    // Read ISR status to acknowledge the interrupt on the device side.
    // This is lock-free (reads io_base from an atomic) to avoid deadlock
    // if the interrupt fires while send_frame/recv_frames holds the DRIVER lock.
    let _isr = crate::net::virtio_net::isr_status();

    // Signal to the network processing task that frames may be available.
    VIRTIO_NET_IRQ_PENDING.store(true, Ordering::Release);

    if USING_APIC.load(Ordering::Relaxed) {
        super::apic::lapic_eoi();
    } else {
        unsafe {
            PICS.lock()
                .notify_end_of_interrupt(InterruptIndex::VirtioNet as u8);
        }
    }
}

// ---------------------------------------------------------------------------
// Serial (COM1) IRQ handler — vector 36
// ---------------------------------------------------------------------------

extern "x86-interrupt" fn serial_handler(_stack_frame: InterruptStackFrame) {
    crate::serial::handle_serial_irq();

    if USING_APIC.load(Ordering::Relaxed) {
        super::apic::lapic_eoi();
    } else {
        unsafe {
            PICS.lock()
                .notify_end_of_interrupt(InterruptIndex::Serial as u8);
        }
    }
}
