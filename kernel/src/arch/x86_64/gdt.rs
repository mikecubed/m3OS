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

/// 16-byte-aligned wrapper for the double-fault stack.
///
/// x86_64 ABI requires the stack pointer to be 16-byte aligned before a CALL.
/// Using a plain `[u8; N]` only guarantees 1-byte alignment; wrapping it here
/// ensures the IST pointer we write into the TSS is always correctly aligned.
#[repr(align(16))]
struct AlignedStack([u8; DOUBLE_FAULT_STACK_SIZE]);

/// Static double-fault stack. Must be static so its address is valid for the
/// entire lifetime of the kernel.
///
/// `static mut` is required: an immutable `static` may be placed in `.rodata`
/// (read-only memory) by the linker. The CPU writes to this memory when using
/// it as a stack during a double fault, so it must be writable.
static mut DOUBLE_FAULT_STACK: AlignedStack = AlignedStack([0; DOUBLE_FAULT_STACK_SIZE]);

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
    tss
});

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
}

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    let tss = gdt.append(Descriptor::tss_segment(&TSS));
    (gdt, Selectors { code, data, tss })
});

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
