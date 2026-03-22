//! Syscall entry point via the SYSCALL/SYSRET instruction pair.
//!
//! On SYSCALL the CPU:
//!   - saves RIP → RCX, RFLAGS → R11
//!   - switches CS/SS per the STAR MSR
//!   - does NOT change RSP (still user RSP)
//!
//! The entry stub manually switches to the kernel syscall stack, saves
//! callee-saved registers, calls the Rust dispatcher, restores registers,
//! restores user RSP, and returns with SYSRETQ.

use core::arch::global_asm;

use x86_64::{
    registers::{
        model_specific::{Efer, EferFlags, LStar, SFMask, Star},
        rflags::RFlags,
    },
    VirtAddr,
};

use super::gdt;

// ---------------------------------------------------------------------------
// Statics accessed from assembly
// ---------------------------------------------------------------------------

/// Scratch space to save the user RSP during a syscall.
///
/// Single-CPU teaching OS: no per-CPU data, so a plain static suffices.
/// On a real SMP kernel this would live in per-CPU storage.
#[no_mangle]
static mut SYSCALL_USER_RSP: u64 = 0;

/// Virtual address of the top of the kernel syscall stack.
///
/// Written once in `init()` and thereafter read-only (from both Rust and asm).
#[no_mangle]
static mut SYSCALL_STACK_TOP: u64 = 0;

// ---------------------------------------------------------------------------
// Assembly entry stub
// ---------------------------------------------------------------------------

global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    // At entry:
    //   RSP  = user RSP
    //   RCX  = user RIP (return address for SYSRETQ)
    //   R11  = user RFLAGS
    //   RAX  = syscall number
    //   RDI/RSI/RDX/R10/R8/R9 = syscall arguments 0-5

    // --- Switch to kernel stack ---
    "mov [rip + SYSCALL_USER_RSP], rsp",
    "mov rsp, [rip + SYSCALL_STACK_TOP]",
    // Clear the direction flag — userspace may have set it before the syscall;
    // Rust string/copy routines assume DF=0 and will corrupt data if it is set.
    "cld",
    // --- Save return address and user flags ---
    "push rcx", // user RIP  (restored before SYSRETQ)
    "push r11", // user RFLAGS
    // --- Save callee-saved registers (SysV ABI) ---
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    // --- Set up arguments for syscall_handler (SysV calling convention) ---
    // Syscall ABI on entry (SYSCALL instruction):
    //   rax = syscall number,  rdi = arg0, rsi = arg1, rdx = arg2
    //   (r10/r8/r9 = args 3-5, unused in Phase 5)
    //
    // SysV call target: syscall_handler(number, arg0, arg1, arg2)
    //   rdi = number  (from rax)
    //   rsi = arg0    (from original rdi, syscall arg0)
    //   rdx = arg1    (from original rsi, syscall arg1)
    //   rcx = arg2    (from original rdx, syscall arg2)
    //
    // Note: rcx was already pushed above (user RIP) so it is safe to
    // overwrite it here; the saved value on the stack is what we restore.
    "mov rcx, rdx", // arg2 (SysV 4th param) ← original rdx (syscall arg2)
    "mov rdx, rsi", // arg1 (SysV 3rd param) ← original rsi (syscall arg1)
    "mov rsi, rdi", // arg0 (SysV 2nd param) ← original rdi (syscall arg0)
    "mov rdi, rax", // number (SysV 1st param) ← syscall number
    "call syscall_handler",
    // Return value is in RAX.

    // --- Restore callee-saved registers ---
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    // --- Restore return info ---
    "pop r11", // user RFLAGS
    "pop rcx", // user RIP
    // --- Restore user RSP and return to ring 3 ---
    "mov rsp, [rip + SYSCALL_USER_RSP]",
    "sysretq",
);

// ---------------------------------------------------------------------------
// Syscall dispatcher
// ---------------------------------------------------------------------------

/// Kernel syscall dispatcher, called from the assembly stub.
///
/// Arguments are passed in SysV order: rdi, rsi, rdx, rcx.
/// The assembly stub has already translated the raw syscall registers into
/// this layout:
///   rdi = syscall number (was rax at syscall site)
///   rsi = arg0          (was rdi at syscall site)
///   rdx = arg1          (was rsi at syscall site)
///   rcx = arg2          (was rdx at syscall site)
#[no_mangle]
pub extern "C" fn syscall_handler(number: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    match number {
        // IPC syscalls (Phase 6)
        1..=5 | 7..=8 => crate::ipc::dispatch(number, arg0, arg1, arg2, 0, 0),
        // Legacy / debug syscalls
        6 => sys_exit(arg0),
        12 => sys_debug_print(arg0, arg1),
        _ => u64::MAX, // ENOSYS
    }
}

/// Print a UTF-8 string from userspace to the kernel serial log.
///
/// # Arguments
/// * `ptr` — virtual address of the string buffer (userspace pointer)
/// * `len` — byte length of the string
///
/// # Safety (internal)
/// In Phase 5 the kernel and user share the same address space, so the
/// virtual address is directly accessible from ring 0.  We cap `len` at
/// 4096 and validate against the **actual mapped user subranges** (code pages
/// and stack pages) to avoid ring-0 page faults from pointers into the large
/// unmapped gap between those regions.
fn sys_debug_print(ptr: u64, len: u64) -> u64 {
    use crate::mm::user_space::{
        USER_CODE_BASE, USER_CODE_PAGES, USER_STACK_PAGES, USER_STACK_TOP,
    };

    if len > 4096 {
        return u64::MAX;
    }
    // Reject pointers outside the actual mapped user regions.
    // Phase 5 maps two subranges; anything in the gap between them is unmapped
    // and would trigger a kernel-mode page fault (which halts the machine).
    let code_end = USER_CODE_BASE + USER_CODE_PAGES * 4096;
    let stack_start = USER_STACK_TOP - USER_STACK_PAGES * 4096;
    let ptr_end = ptr.saturating_add(len);
    let in_code = ptr >= USER_CODE_BASE && ptr_end <= code_end;
    let in_stack = ptr >= stack_start && ptr_end <= USER_STACK_TOP;
    if !in_code && !in_stack {
        return u64::MAX;
    }
    // Safety: pointer is within a mapped user region, len is capped at 4096,
    // and kernel + user share one address space in Phase 5.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    if let Ok(s) = core::str::from_utf8(bytes) {
        log::info!("[userspace] {}", s.trim_end_matches('\n'));
    }
    0
}

/// Terminate the current userspace task.
///
/// Logs the exit code and halts the CPU.  In a fully-featured kernel this
/// would tear down the task and schedule the next one; for Phase 5 we just
/// halt.
fn sys_exit(code: u64) -> ! {
    log::info!("[userspace] exited with code {}", code);
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Configure the SYSCALL/SYSRET mechanism.
///
/// Sets the STAR, LSTAR, and SFMASK MSRs so that `syscall` from ring 3
/// enters `syscall_entry` with the kernel code/data segments and the kernel
/// syscall stack.
///
/// # Safety
///
/// Must be called after `gdt::init()` (so segment selectors are valid) and
/// before any userspace code executes.  Must be called only once.
pub fn init() {
    // Store the syscall stack top so the assembly stub can load it.
    let stack_top = gdt::syscall_stack_top();
    // Safety: single-CPU init, no concurrent access.
    unsafe {
        SYSCALL_STACK_TOP = stack_top;
    }

    // Also keep TSS.RSP0 in sync so hardware interrupts arriving while in
    // ring 3 also use the kernel stack.
    unsafe {
        gdt::set_kernel_stack(stack_top);
    }

    // STAR: kernel CS/SS base (bits 47:32) and user CS/SS SYSRET base (63:48).
    //
    // Star::write(cs_sysret, ss_sysret, cs_syscall, ss_syscall):
    //   cs_sysret  = user_code  (0x23, RPL=3) ─┐  x86_64 stores STAR[63:48] = ss_sysret.0 − 8
    //   ss_sysret  = user_data  (0x1B, RPL=3) ─┘    = 0x1B − 8 = 0x13
    //                                               SYSRET: CS = 0x13+16 = 0x23 (user code)
    //                                                        SS = 0x13+ 8 = 0x1B (user data)
    //   cs_syscall = kernel_code (0x08, RPL=0)   SYSCALL: CS = 0x08 directly
    //   ss_syscall = kernel_data (0x10, RPL=0)            SS = 0x08 + 8 = 0x10
    //
    // Star::write validates that RPL=3 for the sysret pair and RPL=0 for the
    // syscall pair, returning Err if the GDT layout is inconsistent.
    Star::write(
        gdt::user_code_selector(),
        gdt::user_data_selector(),
        gdt::kernel_code_selector(),
        gdt::kernel_data_selector(),
    )
    .expect("STAR MSR write failed: segment selector layout mismatch");

    // LSTAR: virtual address of the syscall entry stub.
    // Safety: syscall_entry is a valid kernel code address.
    extern "C" {
        fn syscall_entry();
    }
    LStar::write(VirtAddr::new(syscall_entry as *const () as u64));

    // SFMASK: bits set here are cleared in RFLAGS on SYSCALL entry.
    // Clear IF (interrupts) so the entry stub runs with interrupts disabled;
    // the handler may re-enable them if needed.
    SFMask::write(RFlags::INTERRUPT_FLAG);

    // Enable the SYSCALL/SYSRET instructions by setting EFER.SCE (bit 0).
    // Without this the CPU treats SYSCALL as an invalid opcode (#UD).
    // Safety: single-CPU init path; no concurrent EFER readers.
    unsafe {
        Efer::update(|flags| *flags |= EferFlags::SYSTEM_CALL_EXTENSIONS);
    }
}
