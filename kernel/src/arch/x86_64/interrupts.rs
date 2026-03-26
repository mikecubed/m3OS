use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use spin::{Lazy, Mutex};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::VirtAddr;

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
    // Mark the process zombie with SIGSEGV exit code.
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.state = crate::process::ProcessState::Zombie;
            proc.exit_code = Some(-11);
        }
    }
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
fn resolve_cow_fault(vaddr: u64) {
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
            // P17-T033: fast path — sole owner, just remap as writable.
            let mut flags = pte.flags();
            flags |= PageTableFlags::WRITABLE;
            pte.set_addr(pte.addr(), flags);
            // Flush TLB for this address.
            x86_64::instructions::tlb::flush(VirtAddr::new(vaddr));
            return;
        }

        // Allocate a fresh frame and copy the page contents.
        let new_frame =
            crate::mm::frame_allocator::allocate_frame().expect("CoW: out of frames for page copy");
        let new_phys = new_frame.start_address().as_u64();

        let src = (phys_off + old_phys) as *const u8;
        let dst = (phys_off + new_phys) as *mut u8;
        core::ptr::copy_nonoverlapping(src, dst, 4096);

        // Map the new frame at the faulting address with WRITABLE restored.
        let mut flags = pte.flags();
        flags |= PageTableFlags::WRITABLE;
        pte.set_addr(new_frame.start_address(), flags);

        // Flush TLB for this address.
        x86_64::instructions::tlb::flush(VirtAddr::new(vaddr));

        // Decrement the old frame's refcount (may free it if no other sharers).
        crate::mm::frame_allocator::free_frame(old_phys);
    }
}

/// Read the physical address of the page mapped at `vaddr` from the current
/// page table.  Returns `None` if any level is not present.
///
/// Called from the page fault ISR (interrupts disabled, single CPU) so no
/// locking is needed beyond what the atomic reads in the frame allocator provide.
fn get_page_phys(vaddr: u64) -> Option<u64> {
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
            return None;
        }
        let pdpt: &PageTable = &*(phys_offset + p4e.addr().as_u64()).as_ptr::<PageTable>();
        let p3e = &pdpt[p3_idx];
        if !p3e.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let pd: &PageTable = &*(phys_offset + p3e.addr().as_u64()).as_ptr::<PageTable>();
        let p2e = &pd[p2_idx];
        if !p2e.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        let pt: &PageTable = &*(phys_offset + p2e.addr().as_u64()).as_ptr::<PageTable>();
        let pte = &pt[p1_idx];
        if !pte.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        Some(pte.addr().as_u64())
    }
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
        // P17-T031: detect CoW faults — a write to a present, non-writable page
        // with a reference count > 0 is a copy-on-write fault.
        let is_write = err.contains(PageFaultErrorCode::CAUSED_BY_WRITE);
        let is_present = err.contains(PageFaultErrorCode::PROTECTION_VIOLATION);
        if is_write && is_present {
            // Check that the page has a refcount (indicating CoW).
            let fault_addr_u64 = match addr {
                Ok(a) => a.as_u64(),
                Err(_) => 0,
            };
            let page_phys = get_page_phys(fault_addr_u64);
            if let Some(phys) = page_phys {
                let refcount = crate::mm::frame_allocator::refcount_get(phys);
                if refcount > 0 {
                    // CoW fault — resolve directly in the ISR. Safe because
                    // the fault is from ring 3 (no kernel locks held) and
                    // we're on a single CPU.
                    resolve_cow_fault(fault_addr_u64);
                    return;
                }
            }
        }

        let pid = crate::process::CURRENT_PID.load(Ordering::Relaxed);
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
        unsafe {
            stack_frame.as_mut().update(|f| {
                f.instruction_pointer = VirtAddr::new(fault_kill_trampoline as *const () as u64);
                f.code_segment = gdt::kernel_code_selector();
                f.cpu_flags &= !x86_64::registers::rflags::RFlags::INTERRUPT_FLAG;
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
        let pid = crate::process::CURRENT_PID.load(Ordering::Relaxed);
        _panic_print(format_args!(
            "[int] userspace GPF: pid={} — process killed\n{:?}\n",
            pid, stack_frame
        ));
        // Store the PID and redirect to the kill trampoline (same pattern as
        // page_fault_handler — no blocking allowed inside an ISR).
        FAULT_KILL_PID.store(pid, Ordering::Relaxed);
        // SAFETY: same as page_fault_handler above.
        unsafe {
            stack_frame.as_mut().update(|f| {
                f.instruction_pointer = VirtAddr::new(fault_kill_trampoline as *const () as u64);
                f.code_segment = gdt::kernel_code_selector();
                f.cpu_flags &= !x86_64::registers::rflags::RFlags::INTERRUPT_FLAG;
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
    let mut pics = PICS.lock();
    pics.initialize();
    // Mask every IRQ line except IRQ0 (timer) and IRQ1 (keyboard).
    // A set bit disables the line. Any unmasked line without an IDT handler
    // would vector into an uninitialized entry and cause a triple fault.
    // master: bits 2–7 masked (0b1111_1100), slave: all 8 lines masked.
    pics.write_masks(0b1111_1100, 0b1111_1111);
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
