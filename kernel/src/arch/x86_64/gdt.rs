use spin::Lazy;
use x86_64::{
    instructions::{segmentation::Segment, tables::load_tss},
    registers::segmentation::{CS, DS, SS},
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector},
        tss::TaskStateSegment,
    },
    VirtAddr,
};

/// IST index used for the double-fault handler stack.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Size of the dedicated double-fault stack.
const DOUBLE_FAULT_STACK_SIZE: usize = 4096 * 5;

/// Size of the dedicated syscall kernel stack (16 KiB).
const SYSCALL_STACK_SIZE: usize = 4096 * 4;

/// User code segment selector (GDT index 4, RPL=3).
///
/// GDT layout: null(0x00) | kernel_code(0x08) | kernel_data(0x10) |
///             user_data(0x18) | user_code(0x20) | TSS(0x28,0x30)
///
/// STAR SYSRET sets CS = STAR[63:48]+16 and SS = STAR[63:48]+8.
/// With user_data at 0x18 stored in bits 63:48 after -8 adjustment,
/// SYSRET yields CS=0x20 (user_code) and SS=0x18 (user_data). RPL=3.
// Used by enter_userspace in mod.rs and exported for future userspace bringup.
#[allow(dead_code)]
pub const USER_CODE_SELECTOR: u16 = 0x20 | 3; // 0x23
/// User data segment selector (GDT index 3, RPL=3).
// Used by enter_userspace in mod.rs and exported for future userspace bringup.
#[allow(dead_code)]
pub const USER_DATA_SELECTOR: u16 = 0x18 | 3; // 0x1B

/// 16-byte-aligned wrapper for the double-fault stack.
///
/// x86_64 ABI requires the stack pointer to be 16-byte aligned before a CALL.
/// Using a plain `[u8; N]` only guarantees 1-byte alignment; wrapping it here
/// ensures the IST pointer we write into the TSS is always correctly aligned.
#[repr(align(16))]
struct AlignedStack<const N: usize>([u8; N]);

/// Static double-fault stack. Must be static so its address is valid for the
/// entire lifetime of the kernel.
///
/// `static mut` is required: an immutable `static` may be placed in `.rodata`
/// (read-only memory) by the linker. The CPU writes to this memory when using
/// it as a stack during a double fault, so it must be writable.
static mut DOUBLE_FAULT_STACK: AlignedStack<DOUBLE_FAULT_STACK_SIZE> =
    AlignedStack([0; DOUBLE_FAULT_STACK_SIZE]);

/// Static kernel stack used on SYSCALL entry and stored in TSS.RSP0.
///
/// `static mut` because this memory is written by the CPU (as a stack) on
/// every ring-3 → ring-0 transition. It must be writable and 'static.
static mut SYSCALL_STACK: AlignedStack<SYSCALL_STACK_SIZE> = AlignedStack([0; SYSCALL_STACK_SIZE]);

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        // Safety: we only take the address of the static here; the CPU writes
        // to it during a double fault, which is the intended use.
        // `addr_of!` is used instead of `.as_ptr()` to avoid creating a
        // shared reference to a `static mut`, which is UB in Rust 2024.
        let stack_start =
            unsafe { VirtAddr::from_ptr(core::ptr::addr_of!(DOUBLE_FAULT_STACK.0).cast::<u8>()) };
        // Stack grows downward, so the "top" is start + size.
        stack_start + DOUBLE_FAULT_STACK_SIZE as u64
    };
    // RSP0: kernel stack used on ring-3 → ring-0 transition via interrupt.
    // The same stack is used by the syscall entry stub (via SYSCALL_STACK_TOP).
    tss.privilege_stack_table[0] = {
        let stack_start =
            unsafe { VirtAddr::from_ptr(core::ptr::addr_of!(SYSCALL_STACK.0).cast::<u8>()) };
        stack_start + SYSCALL_STACK_SIZE as u64
    };
    tss
});

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
    user_code: SegmentSelector,
    user_data: SegmentSelector,
}

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    // Layout (offsets must match USER_CODE_SELECTOR / USER_DATA_SELECTOR above
    // and the STAR MSR arithmetic in syscall::init):
    //   0x00  null          (implicit)
    //   0x08  kernel_code   DPL=0
    //   0x10  kernel_data   DPL=0
    //   0x18  user_data     DPL=3
    //   0x20  user_code     DPL=3
    //   0x28  TSS low
    //   0x30  TSS high
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    let user_data = gdt.append(Descriptor::user_data_segment());
    let user_code = gdt.append(Descriptor::user_code_segment());
    let tss = gdt.append(Descriptor::tss_segment(&TSS));
    (
        gdt,
        Selectors {
            code,
            data,
            tss,
            user_code,
            user_data,
        },
    )
});

/// Return the virtual address of the top of the kernel syscall stack.
///
/// Called by `syscall::init()` to initialize the `SYSCALL_STACK_TOP` pointer
/// used by the assembly entry stub, and by `gdt::init()` to set TSS.RSP0.
pub fn syscall_stack_top() -> u64 {
    // Safety: we only read the address of the static buffer, never its contents.
    let stack_start =
        unsafe { VirtAddr::from_ptr(core::ptr::addr_of!(SYSCALL_STACK.0).cast::<u8>()) };
    (stack_start + SYSCALL_STACK_SIZE as u64).as_u64()
}

/// Update TSS.RSP0 (privilege stack 0) at runtime.
///
/// Called when a new kernel stack should be used for ring-3 → ring-0
/// transitions via hardware interrupts (not SYSCALL, which uses its own
/// assembly stub). Typically called once from `syscall::init`.
///
/// # Safety
///
/// `rsp0` must be the top (highest address) of a valid, writable, static
/// kernel stack. Passing an invalid address will silently corrupt the stack
/// on the next ring-3 interrupt.
pub unsafe fn set_kernel_stack(rsp0: u64) {
    // Safety: `&*TSS` forces the Lazy to initialize and gives us a &TaskStateSegment
    // at the correct address for the inner value (not the Lazy wrapper).
    // Casting to *mut is sound here because:
    //   1. We only call this on the single-CPU init path, before any ring-3 code runs.
    //   2. No interrupt handler references privilege_stack_table[0] between
    //      gdt::init() and the first iretq into userspace.
    let tss_ptr = &*TSS as *const TaskStateSegment as *mut TaskStateSegment;
    (*tss_ptr).privilege_stack_table[0] = VirtAddr::new(rsp0);
}

/// Load the GDT and TSS, and reload the segment registers.
///
/// # Note
///
/// Must be called exactly once, before any exception or interrupt can fire.
pub fn init() {
    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code);
        DS::set_reg(GDT.1.data);
        SS::set_reg(GDT.1.data);
        load_tss(GDT.1.tss);
    }
}

/// Return the kernel code segment selector (for use in STAR MSR setup).
pub fn kernel_code_selector() -> SegmentSelector {
    GDT.1.code
}

/// Return the kernel data segment selector (for use in STAR MSR setup).
pub fn kernel_data_selector() -> SegmentSelector {
    GDT.1.data
}

/// Return the user code segment selector (for use in STAR MSR setup).
pub fn user_code_selector() -> SegmentSelector {
    GDT.1.user_code
}

/// Return the user data segment selector (for use in STAR MSR setup).
pub fn user_data_selector() -> SegmentSelector {
    GDT.1.user_data
}
