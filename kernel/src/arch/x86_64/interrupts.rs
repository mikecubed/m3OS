use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use spin::{Lazy, Mutex};
use x86_64::VirtAddr;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

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
        table.find(pid).and_then(|p| p.page_table_root)
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
/// Returns `true` on success, `false` if frame allocation fails (OOM).
pub fn resolve_cow_fault(vaddr: u64) -> bool {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PageTable, PageTableFlags};

    let phys_off = crate::mm::phys_offset();
    let phys_offset = VirtAddr::new(phys_off);

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
            panic!("CoW: PML4 entry not present for {:#x}", vaddr);
        }

        let pdpt: &PageTable = &*(phys_offset + p4e.addr().as_u64()).as_ptr::<PageTable>();
        let p3e = &pdpt[p3_idx];
        if !p3e.flags().contains(PageTableFlags::PRESENT) {
            panic!("CoW: PDPT entry not present for {:#x}", vaddr);
        }

        let pd: &PageTable = &*(phys_offset + p3e.addr().as_u64()).as_ptr::<PageTable>();
        let p2e = &pd[p2_idx];
        if !p2e.flags().contains(PageTableFlags::PRESENT) {
            panic!("CoW: PD entry not present for {:#x}", vaddr);
        }

        let pt: &mut PageTable =
            &mut *(phys_offset + p2e.addr().as_u64()).as_mut_ptr::<PageTable>();
        let pte = &mut pt[p1_idx];
        if !pte.flags().contains(PageTableFlags::PRESENT) {
            panic!("CoW: PT entry not present for {:#x}", vaddr);
        }

        let old_phys = pte.addr().as_u64();
        let old_refcount = crate::mm::frame_allocator::refcount_get(old_phys);

        if old_refcount <= 1 {
            // P17-T033: fast path — sole owner, just remap as writable
            // and clear the CoW marker bit.
            let flags = (pte.flags() | PageTableFlags::WRITABLE) & !PageTableFlags::BIT_9;
            pte.set_addr(pte.addr(), flags);
            x86_64::instructions::tlb::flush(VirtAddr::new(vaddr));
            return true;
        }

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

        // Flush TLB for this address.
        x86_64::instructions::tlb::flush(VirtAddr::new(vaddr));

        // Decrement the old frame's refcount (may free it if no other sharers).
        crate::mm::frame_allocator::free_frame(old_phys);
    }
    true
}

/// Check whether the PTE for `vaddr` has the CoW marker bit (BIT_9) set.
///
/// Called from the page fault ISR (interrupts disabled, single CPU).
fn has_cow_marker(vaddr: u64) -> bool {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PageTable, PageTableFlags};

    let phys_off = crate::mm::phys_offset();
    let phys_offset = VirtAddr::new(phys_off);

    let (cr3_frame, _) = Cr3::read();
    let pml4_phys = cr3_frame.start_address().as_u64();

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
        let pt: &PageTable = &*(phys_offset + p2e.addr().as_u64()).as_ptr::<PageTable>();
        let pte = &pt[p1_idx];
        pte.flags().contains(PageTableFlags::BIT_9)
    }
}

/// Demand-page a single 4 KiB user-accessible frame at the page containing
/// `vaddr`. Used to grow the stack region on first write (musl's TLS/TCB
/// allocation writes above the initial RSP — Linux maps 8 MiB so this is
/// always valid there; we grow on demand).
///
/// Called from the page fault ISR (interrupts disabled, single CPU).
/// Returns `true` on success, `false` on OOM.
fn demand_map_user_page(vaddr: u64) -> bool {
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

    let user_flags =
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    unsafe {
        let pml4: &mut PageTable = &mut *(phys_offset_va + pml4_phys).as_mut_ptr::<PageTable>();
        if !pml4[p4_idx].flags().contains(PageTableFlags::PRESENT) {
            // Allocate a PDPT page.
            let frame = match crate::mm::frame_allocator::allocate_frame() {
                Some(f) => f,
                None => return false,
            };
            let frame_phys = frame.start_address().as_u64();
            core::ptr::write_bytes((phys_off + frame_phys) as *mut u8, 0, 4096);
            pml4[p4_idx].set_addr(frame.start_address(), user_flags);
        }

        let pdpt: &mut PageTable =
            &mut *(phys_offset_va + pml4[p4_idx].addr().as_u64()).as_mut_ptr::<PageTable>();
        if !pdpt[p3_idx].flags().contains(PageTableFlags::PRESENT) {
            let frame = match crate::mm::frame_allocator::allocate_frame() {
                Some(f) => f,
                None => return false,
            };
            let frame_phys = frame.start_address().as_u64();
            core::ptr::write_bytes((phys_off + frame_phys) as *mut u8, 0, 4096);
            pdpt[p3_idx].set_addr(frame.start_address(), user_flags);
        }

        let pd: &mut PageTable =
            &mut *(phys_offset_va + pdpt[p3_idx].addr().as_u64()).as_mut_ptr::<PageTable>();
        if !pd[p2_idx].flags().contains(PageTableFlags::PRESENT) {
            let frame = match crate::mm::frame_allocator::allocate_frame() {
                Some(f) => f,
                None => return false,
            };
            let frame_phys = frame.start_address().as_u64();
            core::ptr::write_bytes((phys_off + frame_phys) as *mut u8, 0, 4096);
            pd[p2_idx].set_addr(frame.start_address(), user_flags);
        }

        let pt: &mut PageTable =
            &mut *(phys_offset_va + pd[p2_idx].addr().as_u64()).as_mut_ptr::<PageTable>();
        if pt[p1_idx].flags().contains(PageTableFlags::PRESENT) {
            // Already mapped — this shouldn't happen for demand paging.
            return false;
        }

        // Allocate a fresh zeroed frame for the data page.
        let frame = match crate::mm::frame_allocator::allocate_frame() {
            Some(f) => f,
            None => return false,
        };
        let frame_phys = frame.start_address().as_u64();
        core::ptr::write_bytes((phys_off + frame_phys) as *mut u8, 0, 4096);

        let data_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;
        pt[p1_idx].set_addr(frame.start_address(), data_flags);

        x86_64::instructions::tlb::flush(VirtAddr::new(vaddr & !0xFFF));
    }
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
            if has_cow_marker(fault_addr_u64) {
                // CoW fault — resolve directly in the ISR. Safe because
                // the fault is from ring 3 (no kernel locks held) and
                // we're on a single CPU. On OOM, fall through to kill.
                if resolve_cow_fault(fault_addr_u64) {
                    return;
                }
                // OOM during CoW — fall through to kill the process.
            }
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
                && demand_map_user_page(fault_addr_u64)
            {
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
    crate::hlt_loop();
}

extern "x86-interrupt" fn double_fault_handler(stack_frame: InterruptStackFrame, _err: u64) -> ! {
    _panic_print(format_args!("[int] DOUBLE FAULT: {:?}\n", stack_frame));
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
    TICK_COUNT.fetch_add(1, Ordering::Relaxed);
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

// Lock-free SPSC ring buffer: the keyboard IRQ is the sole producer (writes at
// `tail`), and `read_scancode` is the sole consumer (reads at `head`).  Using a
// plain `static mut` avoids any mutex in the IRQ path, eliminating the risk of
// spinning forever if the consumer holds a lock when the IRQ fires.
const SCANCODE_BUF_SIZE: usize = 64;
// Bitmask wraparound requires a power-of-two buffer size.
const _: () = assert!(
    SCANCODE_BUF_SIZE.is_power_of_two(),
    "SCANCODE_BUF_SIZE must be a power of two for bitmask wraparound"
);
static mut SCANCODE_BUF: [u8; SCANCODE_BUF_SIZE] = [0u8; SCANCODE_BUF_SIZE];
static SCANCODE_BUF_HEAD: AtomicUsize = AtomicUsize::new(0);
static SCANCODE_BUF_TAIL: AtomicUsize = AtomicUsize::new(0);

/// Pop one scancode from the ring buffer, or `None` if it is empty.
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

extern "x86-interrupt" fn keyboard_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut port: Port<u8> = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };

    // Push scancode to the ring buffer for polling consumers.
    let tail = SCANCODE_BUF_TAIL.load(Ordering::Relaxed);
    let next_tail = (tail + 1) & (SCANCODE_BUF_SIZE - 1);
    if next_tail != SCANCODE_BUF_HEAD.load(Ordering::Acquire) {
        // Safety: single producer; tail is only advanced here and never overtakes head.
        unsafe { SCANCODE_BUF[tail] = scancode };
        SCANCODE_BUF_TAIL.store(next_tail, Ordering::Release);
    }

    // Signal any notification object registered for IRQ1 (keyboard).
    // This wakes a kernel task blocked in notification::wait() — the IPC-based
    // IRQ delivery path used by kbd_server in Phase 7+.
    crate::ipc::notification::signal_irq(1);

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
