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
//!
//! # Syscall table (Phase 11)
//!
//! | Number | Name         | Args                  |
//! |---|---|---|
//! | 1–5,7–10 | IPC     | (dispatched to ipc::dispatch) |
//! | 6       | exit (legacy) | code                |
//! | 12      | debug_print   | ptr, len            |
//! | 39      | getpid        | —                   |
//! | 57      | fork          | —                   |
//! | 59      | execve        | path_ptr, path_len  |
//! | 60      | exit          | code                |
//! | 61      | waitpid       | pid, status_ptr     |
//! | 110     | getppid       | —                   |
//! | 231     | exit_group    | code (alias exit)   |

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
#[no_mangle]
static mut SYSCALL_USER_RSP: u64 = 0;

/// Virtual address of the top of the kernel syscall stack.
///
/// Updated by the fork-child trampoline when switching to a per-process
/// kernel stack, so that SYSCALL entry uses the correct stack.
#[no_mangle]
pub(crate) static mut SYSCALL_STACK_TOP: u64 = 0;

// ---------------------------------------------------------------------------
// Assembly entry stub
// ---------------------------------------------------------------------------

global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    // At entry:
    //   RSP  = user RSP       (saved to SYSCALL_USER_RSP)
    //   RCX  = user RIP       (return address for SYSRETQ)
    //   R11  = user RFLAGS
    //   RAX  = syscall number
    //   RDI/RSI/RDX = args 0-2

    // --- Switch to kernel stack ---
    "mov [rip + SYSCALL_USER_RSP], rsp",
    "mov rsp, [rip + SYSCALL_STACK_TOP]",
    "cld",
    // --- Save return address and user flags ---
    "push rcx", // user RIP  (restored before SYSRETQ)  [rsp+56 after all pushes]
    "push r11", // user RFLAGS
    // --- Save callee-saved registers ---
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    // --- Set up SysV arguments for syscall_handler ---
    // Stack layout at this point:
    //   rsp+ 0: r15
    //   rsp+ 8: r14
    //   rsp+16: r13
    //   rsp+24: r12
    //   rsp+32: rbp
    //   rsp+40: rbx
    //   rsp+48: r11 (user RFLAGS)
    //   rsp+56: rcx (user RIP)
    //
    // SysV params:
    //   rdi = syscall number  (from rax)
    //   rsi = arg0            (from original rdi)
    //   rdx = arg1            (from original rsi)
    //   rcx = arg2            (from original rdx) — note: overwrites saved rcx
    //   r8  = user_rip        (loaded from saved rcx on stack)
    //   r9  = user_rsp        (loaded from SYSCALL_USER_RSP)
    //
    // Load r8 BEFORE overwriting rcx.
    "mov r8, [rsp + 56]",               // user_rip (5th param)
    "mov r9, [rip + SYSCALL_USER_RSP]", // user_rsp (6th param)
    "mov rcx, rdx",                     // arg2
    "mov rdx, rsi",                     // arg1
    "mov rsi, rdi",                     // arg0
    "mov rdi, rax",                     // syscall number
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
    // --- Restore user RSP and return ---
    "mov rsp, [rip + SYSCALL_USER_RSP]",
    "sysretq",
);

// ---------------------------------------------------------------------------
// Syscall dispatcher
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn syscall_handler(
    number: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    user_rip: u64,
    user_rsp: u64,
) -> u64 {
    match number {
        // IPC syscalls (Phase 6)
        1..=5 | 7..=10 => crate::ipc::dispatch(number, arg0, arg1, arg2, 0, 0),
        // Legacy exit (HELLO_BIN compat)
        6 => sys_exit_legacy(arg0),
        // Debug print
        12 => sys_debug_print(arg0, arg1),
        // Phase 11 process syscalls
        39 => sys_getpid(),
        57 => sys_fork(user_rip, user_rsp),
        59 => sys_execve(arg0, arg1, arg2),
        60 | 231 => sys_exit(arg0 as i32),
        61 => sys_waitpid(arg0, arg1),
        110 => sys_getppid(),
        _ => u64::MAX, // ENOSYS
    }
}

// ---------------------------------------------------------------------------
// sys_debug_print
// ---------------------------------------------------------------------------

fn sys_debug_print(ptr: u64, len: u64) -> u64 {
    use crate::mm::elf::ELF_STACK_TOP;
    use crate::mm::user_space::{
        USER_CODE_BASE, USER_CODE_PAGES, USER_STACK_PAGES, USER_STACK_TOP,
    };

    // Reference the loader's constant so stack sizing stays in sync.
    let elf_stack_pages = crate::mm::elf::STACK_PAGES;

    if len > 4096 {
        return u64::MAX;
    }
    let code_end = USER_CODE_BASE + USER_CODE_PAGES * 4096;
    let stack_start = USER_STACK_TOP - USER_STACK_PAGES * 4096;
    let elf_stack_start = ELF_STACK_TOP - elf_stack_pages * 4096;
    let ptr_end = ptr.saturating_add(len);

    let in_code = ptr >= USER_CODE_BASE && ptr_end <= code_end;
    let in_stack = ptr >= stack_start && ptr_end <= USER_STACK_TOP;
    let in_elf_stack = ptr >= elf_stack_start && ptr_end <= ELF_STACK_TOP;
    // Also allow any user-accessible address in the valid user range.
    let in_user_range = ptr >= 0x400000 && ptr_end <= 0x0000_8000_0000_0000;

    if !in_code && !in_stack && !in_elf_stack && !in_user_range {
        return u64::MAX;
    }
    // Safety: we checked the bounds; kernel+user share the address space.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    if let Ok(s) = core::str::from_utf8(bytes) {
        log::info!("[userspace] {}", s.trim_end_matches('\n'));
    }
    0
}

// ---------------------------------------------------------------------------
// sys_exit (legacy, for HELLO_BIN)
// ---------------------------------------------------------------------------

fn sys_exit_legacy(code: u64) -> ! {
    log::info!("[userspace] legacy exit with code {}", code);
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}

// ---------------------------------------------------------------------------
// Phase 11 syscalls
// ---------------------------------------------------------------------------

/// `getpid()` — return the calling process's PID.
fn sys_getpid() -> u64 {
    crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed) as u64
}

/// `getppid()` — return the calling process's parent PID.
fn sys_getppid() -> u64 {
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    crate::process::PROCESS_TABLE
        .lock()
        .find(pid)
        .map(|p| p.ppid as u64)
        .unwrap_or(0)
}

/// `exit(code)` / `exit_group(code)` — terminate the calling process.
///
/// Marks the process as a zombie, stores the exit code, then permanently
/// blocks the kernel task so it is never rescheduled.
fn sys_exit(code: i32) -> ! {
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    log::info!("[p{}] exit({})", pid, code);
    if pid != 0 {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.state = crate::process::ProcessState::Zombie;
            proc.exit_code = Some(code);
        }
    }
    // Block the kernel task permanently; no sender will ever wake it.
    crate::task::block_current_on_recv();
    // Unreachable — satisfy the `!` return type.
    loop {
        x86_64::instructions::hlt();
    }
}

/// `fork()` — create a child process that resumes after the syscall with rax=0.
///
/// Allocates a fresh page table for the child (eager copy of user pages),
/// registers the child in the process table, and spawns a kernel task whose
/// entry function enters ring 3 at `user_rip` with `user_rsp` and rax=0.
///
/// Returns the child PID to the parent.
fn sys_fork(user_rip: u64, user_rsp: u64) -> u64 {
    let parent_pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    log::info!("[p{}] fork()", parent_pid);

    // Allocate a new page table for the child, copying kernel entries.
    let child_cr3 = match crate::mm::new_process_page_table() {
        Some(f) => f,
        None => {
            log::warn!("[fork] out of frames for child page table");
            return u64::MAX;
        }
    };

    // Copy all user-accessible pages from parent into child's page table.
    let phys_off = crate::mm::phys_offset();
    {
        // SAFETY: child_cr3 was just allocated; no other mapper over it exists.
        let mut child_mapper = unsafe { crate::mm::mapper_for_frame(child_cr3) };
        // SAFETY: current CR3 is the parent; we iterate its lower half.
        if let Err(e) = unsafe { copy_user_pages(phys_off, &mut child_mapper) } {
            log::warn!("[fork] page copy failed: {:?}", e);
            return u64::MAX;
        }
    }

    // Create child process entry.
    let child_pid = crate::process::spawn_process_with_cr3(
        parent_pid,
        user_rip,
        user_rsp,
        x86_64::PhysAddr::new(child_cr3.start_address().as_u64()),
    );

    // Push the fork context so fork_child_trampoline can find the right RIP/RSP.
    crate::process::push_fork_ctx(child_pid, user_rip, user_rsp);

    // Spawn a kernel task for the child; it will enter ring 3 on first dispatch.
    crate::task::spawn(crate::process::fork_child_trampoline, "fork-child");

    log::info!("[p{}] fork() → child pid {}", parent_pid, child_pid);
    child_pid as u64
}

/// `execve(path_ptr, path_len, _envp)` — replace the calling process's image
/// with a new ELF binary read from the ramdisk.
fn sys_execve(path_ptr: u64, path_len: u64, _arg2: u64) -> u64 {
    if path_len > 255 {
        return u64::MAX;
    }
    let mut name_buf = [0u8; 255];
    let name = match path_name_buf(path_ptr, path_len, &mut name_buf) {
        Some(n) => n,
        None => return u64::MAX,
    };

    log::info!(
        "[p{}] execve({})",
        crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed),
        name
    );

    // Read the binary from the ramdisk.
    let data = match crate::fs::ramdisk::get_file(name) {
        Some(d) => d,
        None => {
            log::warn!("[execve] file not found: {}", name);
            return u64::MAX;
        }
    };

    // Allocate a fresh page table for the new image.
    let new_cr3 = match crate::mm::new_process_page_table() {
        Some(f) => f,
        None => return u64::MAX,
    };

    let phys_off = crate::mm::phys_offset();

    let (loaded, user_rsp) = {
        // SAFETY: new_cr3 is freshly allocated; no other mapper exists.
        let mut mapper = unsafe { crate::mm::mapper_for_frame(new_cr3) };
        let loaded = match unsafe { crate::mm::elf::load_elf_into(&mut mapper, phys_off, data) } {
            Ok(l) => l,
            Err(e) => {
                log::warn!("[execve] ELF load failed: {:?}", e);
                return u64::MAX;
            }
        };
        // Build the SysV AMD64 ABI initial stack with argv[0] = binary name.
        let argv: &[&[u8]] = &[name.as_bytes()];
        // SAFETY: stack pages were just mapped by load_elf_into; mapper is valid.
        let user_rsp =
            unsafe { crate::mm::elf::setup_abi_stack(loaded.stack_top, &mapper, phys_off, argv) };
        (loaded, user_rsp)
    };

    // Update the process entry with the new CR3 and entry point.
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.page_table_root = Some(x86_64::PhysAddr::new(new_cr3.start_address().as_u64()));
            proc.entry_point = loaded.entry;
            proc.user_stack_top = user_rsp;
        }
    }

    // Switch to the new page table and enter ring 3.
    // SAFETY: new_cr3 is valid, entry and user_rsp are within it.
    unsafe {
        use x86_64::registers::control::{Cr3, Cr3Flags};
        Cr3::write(new_cr3, Cr3Flags::empty());
        // Update TSS.RSP0 so interrupts from ring 3 use the correct kernel stack.
        let kstack_top = crate::process::PROCESS_TABLE
            .lock()
            .find(pid)
            .map(|p| p.kernel_stack_top)
            .unwrap_or(0);
        if kstack_top != 0 {
            gdt::set_kernel_stack(kstack_top);
            SYSCALL_STACK_TOP = kstack_top;
        }
        crate::arch::x86_64::enter_userspace(loaded.entry, user_rsp)
    }
}

/// `waitpid(pid, status_ptr, _flags)` — wait for a child to exit.
///
/// Spins with `yield_now()` until the target child is a zombie, then
/// collects its exit code and reaps it.
fn sys_waitpid(pid: u64, status_ptr: u64) -> u64 {
    let target_pid = pid as crate::process::Pid;
    let calling_pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);

    // Verify that target_pid is a child of the calling process before blocking.
    // This prevents one process from reaping another process's children.
    {
        let table = crate::process::PROCESS_TABLE.lock();
        match table.find(target_pid) {
            None => return u64::MAX,
            Some(p) if p.ppid != calling_pid => {
                log::warn!(
                    "[waitpid] pid {} is not a child of calling pid {}",
                    target_pid,
                    calling_pid
                );
                return u64::MAX;
            }
            Some(_) => {}
        }
    }

    loop {
        let result = {
            let mut table = crate::process::PROCESS_TABLE.lock();
            match table.find(target_pid) {
                None => return u64::MAX, // no such child (reaped between check and loop)
                Some(p) if p.state == crate::process::ProcessState::Zombie => {
                    let code = p.exit_code.unwrap_or(0);
                    table.reap(target_pid);
                    Some(code)
                }
                Some(_) => None, // not yet done
            }
        };

        if let Some(code) = result {
            // Write wstatus in Linux-compatible encoding: exit code in bits 15:8.
            if status_ptr != 0 {
                // Validate: must be 4-byte aligned and within the user address
                // range (below the 128 TiB canonical boundary).
                let ptr_end = status_ptr.saturating_add(4);
                let in_user = status_ptr.is_multiple_of(4) && ptr_end <= 0x0000_8000_0000_0000u64;
                if in_user {
                    // SAFETY: validated above; kernel+user share one AS (Phase 11).
                    unsafe {
                        (status_ptr as *mut i32).write(code << 8);
                    }
                } else {
                    log::warn!(
                        "[waitpid] invalid status_ptr {:#x} — skipping write",
                        status_ptr
                    );
                }
            }
            log::info!("[waitpid] pid {} exited with code {}", target_pid, code);
            return target_pid as u64;
        }

        // Child is still running; yield and try again.
        crate::task::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a UTF-8 path string from a userspace pointer + length.
/// Copy at most 255 bytes from a userspace pointer into a stack buffer and
/// return a reference to the valid UTF-8 prefix.
///
/// Using a local buffer avoids the `&'static str` lifetime lie: the resulting
/// reference is scoped to the caller's stack frame so Rust enforces that it
/// cannot outlive the buffer.
fn path_name_buf(ptr: u64, len: u64, buf: &mut [u8; 255]) -> Option<&str> {
    let copy_len = (len as usize).min(255);
    if copy_len == 0 || ptr == 0 {
        return None;
    }
    // SAFETY: kernel + user share one address space (Phase 11); ptr is
    // a userspace stack address pointing to a valid string for this syscall.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, copy_len) };
    buf[..copy_len].copy_from_slice(bytes);
    core::str::from_utf8(&buf[..copy_len]).ok()
}

/// Copy all user-accessible pages from the currently-active page table
/// into `dst_mapper`'s page table.
///
/// Walks PML4 indices 0–255 (the user-space half).
///
/// # Safety
/// The current CR3 must be valid and `dst_mapper` must reference a different,
/// freshly-allocated PML4.
unsafe fn copy_user_pages(
    phys_off: u64,
    dst_mapper: &mut x86_64::structures::paging::OffsetPageTable<'_>,
) -> Result<(), crate::mm::elf::ElfError> {
    use x86_64::{
        registers::control::Cr3,
        structures::paging::{
            FrameAllocator, Mapper, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
        },
        VirtAddr,
    };

    let phys_offset = VirtAddr::new(phys_off);

    let (src_frame, _) = Cr3::read();
    let src_pml4: &PageTable =
        &*(phys_offset + src_frame.start_address().as_u64()).as_ptr::<PageTable>();

    let mut frame_alloc = crate::mm::paging::GlobalFrameAlloc;

    // Walk indices 0–255 (user half).
    for p4 in 0usize..256 {
        let p4e = &src_pml4[p4];
        if !p4e.flags().contains(PageTableFlags::PRESENT) {
            continue;
        }

        let pdpt: &PageTable = &*(phys_offset + p4e.addr().as_u64()).as_ptr::<PageTable>();
        for p3 in 0usize..512 {
            let p3e = &pdpt[p3];
            if !p3e.flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            if p3e.flags().contains(PageTableFlags::HUGE_PAGE) {
                continue;
            }

            let pd: &PageTable = &*(phys_offset + p3e.addr().as_u64()).as_ptr::<PageTable>();
            for p2 in 0usize..512 {
                let p2e = &pd[p2];
                if !p2e.flags().contains(PageTableFlags::PRESENT) {
                    continue;
                }
                if p2e.flags().contains(PageTableFlags::HUGE_PAGE) {
                    continue;
                }

                let pt: &PageTable = &*(phys_offset + p2e.addr().as_u64()).as_ptr::<PageTable>();
                for p1 in 0usize..512 {
                    let pte = &pt[p1];
                    if !pte.flags().contains(PageTableFlags::PRESENT) {
                        continue;
                    }
                    if !pte.flags().contains(PageTableFlags::USER_ACCESSIBLE) {
                        continue;
                    }

                    let vaddr: u64 = ((p4 as u64) << 39)
                        | ((p3 as u64) << 30)
                        | ((p2 as u64) << 21)
                        | ((p1 as u64) << 12);

                    let src_phys = pte.addr();
                    let flags = pte.flags();

                    // Allocate a new frame and copy the page content.
                    let new_frame: PhysFrame<Size4KiB> = frame_alloc
                        .allocate_frame()
                        .ok_or(crate::mm::elf::ElfError::OutOfFrames)?;

                    let src_virt = phys_offset + src_phys.as_u64();
                    let dst_virt = phys_offset + new_frame.start_address().as_u64();
                    core::ptr::copy_nonoverlapping(
                        src_virt.as_ptr::<u8>(),
                        dst_virt.as_mut_ptr::<u8>(),
                        4096,
                    );

                    let page = Page::<Size4KiB>::from_start_address(VirtAddr::new(vaddr)).map_err(
                        |_| crate::mm::elf::ElfError::MappingFailed("invalid vaddr in fork"),
                    )?;
                    dst_mapper
                        .map_to(page, new_frame, flags, &mut frame_alloc)
                        .map_err(|_| {
                            crate::mm::elf::ElfError::MappingFailed("map_to failed in fork")
                        })?
                        .ignore();
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

pub fn init() {
    let stack_top = gdt::syscall_stack_top();
    unsafe {
        SYSCALL_STACK_TOP = stack_top;
    }
    unsafe {
        gdt::set_kernel_stack(stack_top);
    }

    Star::write(
        gdt::user_code_selector(),
        gdt::user_data_selector(),
        gdt::kernel_code_selector(),
        gdt::kernel_data_selector(),
    )
    .expect("STAR MSR write failed: segment selector layout mismatch");

    extern "C" {
        fn syscall_entry();
    }
    LStar::write(VirtAddr::new(syscall_entry as *const () as u64));
    SFMask::write(RFlags::INTERRUPT_FLAG);
    unsafe {
        Efer::update(|flags| *flags |= EferFlags::SYSTEM_CALL_EXTENSIONS);
    }
}
