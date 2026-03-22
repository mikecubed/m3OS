use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use spin::{Lazy, Mutex};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::serial::_panic_print;

use super::gdt;

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

    idt
});

/// Load the IDT.
pub fn init() {
    IDT.load();
}

// ---------------------------------------------------------------------------
// Exception handlers
// ---------------------------------------------------------------------------

extern "x86-interrupt" fn breakpoint_handler(_stack_frame: InterruptStackFrame) {
    // Use _panic_print to avoid deadlocking on the serial mutex if the exception
    // fires while normal code holds the lock.
    _panic_print(format_args!("[int] breakpoint\n"));
}

extern "x86-interrupt" fn page_fault_handler(
    _stack_frame: InterruptStackFrame,
    err: PageFaultErrorCode,
) {
    let addr = x86_64::registers::control::Cr2::read();
    _panic_print(format_args!(
        "[int] page fault: addr={:?} err={:?}\n",
        addr, err
    ));
    crate::hlt_loop();
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    _err: u64,
) {
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
    PICS.lock().initialize();
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
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer as u8);
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
    SCANCODE_BUF_HEAD.store((head + 1) % SCANCODE_BUF_SIZE, Ordering::Release);
    Some(byte)
}

extern "x86-interrupt" fn keyboard_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut port: Port<u8> = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };

    let tail = SCANCODE_BUF_TAIL.load(Ordering::Relaxed);
    let next_tail = (tail + 1) % SCANCODE_BUF_SIZE;
    if next_tail != SCANCODE_BUF_HEAD.load(Ordering::Acquire) {
        // Safety: single producer; tail is only advanced here and never overtakes head.
        unsafe { SCANCODE_BUF[tail] = scancode };
        SCANCODE_BUF_TAIL.store(next_tail, Ordering::Release);
    }

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard as u8);
    }
}
