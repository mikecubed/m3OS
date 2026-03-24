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

/// Saved value of R10 at SYSCALL entry.
///
/// R10 carries syscall arg3 in the Linux ABI (e.g. mmap flags).  It is not
/// a SysV argument-passing register, so the assembly entry stub saves it
/// here before the register setup for `syscall_handler`.  Single-CPU: safe.
#[no_mangle]
static mut SYSCALL_ARG3: u64 = 0;

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
    // --- Save caller-saved registers that Linux preserves across syscalls ---
    // The Linux ABI guarantees all registers except rax, rcx, r11 are preserved.
    // Our SysV rearrangement clobbers rdi/rsi/rdx/r8/r9, and r10 is used for
    // mmap flags, so we must save and restore them here.
    "push rdi",
    "push rsi",
    "push rdx",
    "push r10",
    "push r8",
    "push r9",
    // --- Set up SysV arguments for syscall_handler ---
    // Stack layout (14 pushes):
    //   rsp+  0: r9     rsp+ 48: r15    rsp+ 96: r11 (user RFLAGS)
    //   rsp+  8: r8     rsp+ 56: r14    rsp+104: rcx (user RIP)
    //   rsp+ 16: r10    rsp+ 64: r13
    //   rsp+ 24: rdx    rsp+ 72: r12
    //   rsp+ 32: rsi    rsp+ 80: rbp
    //   rsp+ 40: rdi    rsp+ 88: rbx
    //
    // Save r10 for kernel-side access (mmap flags, etc.)
    "mov [rip + SYSCALL_ARG3], r10",
    // Load r8 (user_rip) BEFORE overwriting rcx.
    "mov r8, [rsp + 104]",              // user_rip (5th param)
    "mov r9, [rip + SYSCALL_USER_RSP]", // user_rsp (6th param)
    "mov rcx, rdx",                     // arg2
    "mov rdx, rsi",                     // arg1
    "mov rsi, rdi",                     // arg0
    "mov rdi, rax",                     // syscall number
    "call syscall_handler",
    // Return value is in RAX.

    // --- Restore caller-saved registers (Linux-preserved) ---
    "pop r9",
    "pop r8",
    "pop r10",
    "pop rdx",
    "pop rsi",
    "pop rdi",
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

/// Linux syscall number → handler dispatch (Phase 12, T011–T026).
///
/// Numbers that happen to match our Phase 11 ABI are handled identically.
/// The custom debug-print syscall is moved from 12 → 0x1000 to free up
/// Linux brk = 12.
///
/// # Syscall audit (T011) — Linux numbers that musl requires
///
/// | Linux # | Name        | Implementation        |
/// |---------|-------------|----------------------|
/// |       0 | read        | ramdisk / stdin stub  |
/// |       1 | write       | stdout → serial       |
/// |       2 | open        | ramdisk lookup        |
/// |       3 | close       | fd-table release      |
/// |       5 | fstat       | minimal stat struct   |
/// |       8 | lseek       | per-fd offset update  |
/// |       9 | mmap        | anonymous only        |
/// |      11 | munmap      | stub (no-op)          |
/// |      12 | brk         | frame-backed heap     |
/// |      16 | ioctl       | TIOCGWINSZ only       |
/// |      19 | readv       | loop over read        |
/// |      20 | writev      | loop over write       |
/// |      39 | getpid      | ✓ same as Phase 11    |
/// |      57 | fork        | ✓ same as Phase 11    |
/// |      59 | execve      | ✓ same as Phase 11    |
/// |      60 | exit        | ✓ same as Phase 11    |
/// |      61 | wait4       | ✓ waitpid Phase 11    |
/// |      63 | uname       | fixed identity string |
/// |      79 | getcwd      | always returns "/"    |
/// |      80 | chdir       | stub (always ok)      |
/// |     110 | getppid     | ✓ same as Phase 11    |
/// |     158 | arch_prctl  | ARCH_SET_FS only       |
/// |     218 | set_tid_addr| stub, returns PID      |
/// |     231 | exit_group  | ✓ same as Phase 11    |
/// |     257 | openat      | delegates to open     |
/// |     262 | newfstatat  | delegates to fstat    |
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
        // Linux-compatible file I/O (Phase 12, T013–T017)
        0 => sys_linux_read(arg0, arg1, arg2),
        1 => sys_linux_write(arg0, arg1, arg2),
        2 => sys_linux_open(arg0, 0),
        3 => sys_linux_close(arg0),
        5 => sys_linux_fstat(arg0, arg1),
        8 => sys_linux_lseek(arg0, arg1, arg2),
        // Linux-compatible memory (Phase 12, T018–T020)
        9 => sys_linux_mmap(arg0, arg1),
        11 => sys_linux_munmap(arg0, arg1),
        12 => sys_linux_brk(arg0),
        // Linux misc (Phase 12, T023–T026)
        16 => sys_linux_ioctl(arg0, arg1, arg2),
        19 => sys_linux_readv(arg0, arg1, arg2),
        20 => sys_linux_writev(arg0, arg1, arg2),
        // IPC syscalls (Phase 6) — kernel-task only.
        // Numbers 5,8,9 are now Linux fstat/lseek/mmap; only 4,7,10 remain.
        4 | 7 | 10 => crate::ipc::dispatch(number, arg0, arg1, arg2, 0, 0),
        // Legacy exit (HELLO_BIN compat)
        6 => sys_exit_legacy(arg0),
        // Phase 11 + Linux-compatible process syscalls (T021–T022)
        39 => sys_getpid(),
        57 => sys_fork(user_rip, user_rsp),
        59 => sys_execve(arg0, arg1, arg2),
        60 | 231 => sys_exit(arg0 as i32),
        61 => sys_waitpid(arg0, arg1),
        63 => sys_linux_uname(arg0),
        79 => sys_linux_getcwd(arg0, arg1),
        80 => sys_linux_chdir(arg0),
        110 => sys_getppid(),
        // musl TLS init (Phase 12, T030 dependency)
        158 => sys_linux_arch_prctl(arg0, arg1),
        218 => sys_linux_set_tid_address(),
        // openat: ignore dirfd, treat as open(path, flags)
        257 => sys_linux_open(arg1, arg2),
        // newfstatat: fstat via path lookup
        262 => sys_linux_fstatat(arg0, arg1, arg2),
        // Custom kernel debug print (moved from 12, Phase 12 T010)
        0x1000 => sys_debug_print(arg0, arg1),
        _ => (-38_i64) as u64, // -ENOSYS
    }
}

// ---------------------------------------------------------------------------
// sys_debug_print
// ---------------------------------------------------------------------------

fn sys_debug_print(ptr: u64, len: u64) -> u64 {
    if len > 4096 {
        return u64::MAX;
    }
    let mut buf = [0u8; 4096];
    let dst = &mut buf[..len as usize];
    if crate::mm::user_mem::copy_from_user(dst, ptr).is_err() {
        log::warn!("[sys_debug_print] invalid user pointer {:#x}+{}", ptr, len);
        return u64::MAX;
    }
    if let Ok(s) = core::str::from_utf8(dst) {
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
    // Restore kernel page table before yielding so the next scheduled task
    // does not inherit this process's CR3.
    crate::mm::restore_kernel_cr3();
    // Mark the kernel task as dead so the scheduler reclaims it.
    crate::task::mark_current_dead();
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
        let user_rsp = match unsafe {
            crate::mm::elf::setup_abi_stack(
                loaded.stack_top,
                &mapper,
                phys_off,
                argv,
                loaded.phdr_vaddr,
                loaded.phnum,
            )
        } {
            Ok(rsp) => rsp,
            Err(e) => {
                log::warn!("[execve] ABI stack setup failed: {:?}", e);
                return u64::MAX;
            }
        };
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
        // Capture old CR3 before switching so we can free its frames after.
        let (old_cr3, _) = Cr3::read();
        let old_cr3_phys = old_cr3.start_address().as_u64();
        Cr3::write(new_cr3, Cr3Flags::empty());
        // Free the old page table's user-space frames now that CR3 no longer
        // points to it. The bump allocator makes this a no-op today; the
        // real reclamation happens in Phase 13 when a free list is added.
        crate::mm::free_process_page_table(old_cr3_phys);
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
            // The child's sys_exit called restore_kernel_cr3() before dying, so
            // CR3 may now be the kernel PML4.  copy_to_user needs the caller's
            // page table, so restore it explicitly before writing status.
            // Also update CURRENT_PID — the child's trampoline may have changed it.
            let (caller_cr3_phys, kstack_top) = {
                let table = crate::process::PROCESS_TABLE.lock();
                let cr3 = table.find(calling_pid).and_then(|p| p.page_table_root);
                let kst = table
                    .find(calling_pid)
                    .map(|p| p.kernel_stack_top)
                    .unwrap_or(0);
                (cr3, kst)
            };
            if let Some(phys) = caller_cr3_phys {
                // SAFETY: phys is the caller's live PML4 frame from the process table.
                unsafe {
                    use x86_64::{
                        registers::control::{Cr3, Cr3Flags},
                        structures::paging::PhysFrame,
                    };
                    let frame = PhysFrame::from_start_address(phys).expect("caller cr3 unaligned");
                    Cr3::write(frame, Cr3Flags::empty());
                }
            }
            crate::process::CURRENT_PID.store(calling_pid, core::sync::atomic::Ordering::Relaxed);

            // Restore kernel stack pointers for this process.
            if kstack_top != 0 {
                unsafe {
                    crate::arch::x86_64::gdt::set_kernel_stack(kstack_top);
                    *(core::ptr::addr_of_mut!(crate::arch::x86_64::syscall::SYSCALL_STACK_TOP)) =
                        kstack_top;
                }
            }

            // Write wstatus in Linux-compatible encoding: exit code in bits 15:8.
            if status_ptr != 0 {
                let wstatus = code << 8;
                let bytes = wstatus.to_ne_bytes();
                if crate::mm::user_mem::copy_to_user(status_ptr, &bytes).is_err() {
                    log::warn!("[waitpid] copy_to_user status_ptr {:#x} failed", status_ptr);
                }
            }
            log::info!("[waitpid] pid {} exited with code {}", target_pid, code);
            return target_pid as u64;
        }

        // Child is still running; yield and try again.
        crate::task::yield_now();
        // Restore CURRENT_PID after yield: child's trampoline may have changed it.
        crate::process::CURRENT_PID.store(calling_pid, core::sync::atomic::Ordering::Relaxed);
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
    if crate::mm::user_mem::copy_from_user(&mut buf[..copy_len], ptr).is_err() {
        return None;
    }
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
    SFMask::write(RFlags::INTERRUPT_FLAG | RFlags::TRAP_FLAG);
    unsafe {
        Efer::update(|flags| *flags |= EferFlags::SYSTEM_CALL_EXTENSIONS);
    }
}

// ===========================================================================
// Phase 12 — Linux-compatible syscall implementations (T013–T026)
// ===========================================================================

// ---------------------------------------------------------------------------
// File descriptor table (P12-T013, T015)
// ---------------------------------------------------------------------------

/// Initial virtual address for the program break (heap).
///
/// Placed at 8 GiB — above typical ELF segments (which load at ~4 MiB) and
/// well below the user stack (at ~128 TiB).
const BRK_BASE: u64 = 0x0000_0002_0000_0000;

/// Initial virtual address for anonymous mmap allocations.
///
/// Placed at 128 GiB — above the brk heap region and below the stack.
const ANON_MMAP_BASE: u64 = 0x0000_0020_0000_0000;

/// Maximum number of open file descriptors (FDs 0–2 are stdin/stdout/stderr).
const MAX_FDS: usize = 32;

/// A single open-file entry in the global FD table.
///
/// `content_addr` / `content_len` point into the static ramdisk.
/// `offset` tracks the current read position.
#[derive(Copy, Clone)]
struct FdEntry {
    content_addr: usize,
    content_len: usize,
    offset: usize,
}

const NONE_FD: Option<FdEntry> = None;

/// Global file descriptor table.
///
/// FD 0 = stdin (not implemented — reads return EAGAIN).
/// FD 1 = stdout / FD 2 = stderr (writes go to serial).
/// FD 3+ = ramdisk files opened via `open()`.
static FD_TABLE: spin::Mutex<[Option<FdEntry>; MAX_FDS]> = spin::Mutex::new([NONE_FD; MAX_FDS]);

// ---------------------------------------------------------------------------
// T013: read(fd, buf, count)
// ---------------------------------------------------------------------------

fn sys_linux_read(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    if fd == 0 {
        // stdin: no keyboard input implemented yet — return EAGAIN (-11)
        return (-11_i64) as u64; // -EAGAIN
    }
    let fd = fd as usize;
    if fd >= MAX_FDS {
        return u64::MAX;
    }

    let entry = {
        let table = FD_TABLE.lock();
        match table[fd] {
            Some(e) => e,
            None => return u64::MAX,
        }
    };

    let remaining = entry.content_len.saturating_sub(entry.offset);
    let to_read = (count as usize).min(remaining).min(64 * 1024);
    if to_read == 0 {
        return 0; // EOF
    }

    // SAFETY: content_addr is a static ramdisk pointer (lives forever).
    let src = unsafe {
        core::slice::from_raw_parts((entry.content_addr + entry.offset) as *const u8, to_read)
    };

    if crate::mm::user_mem::copy_to_user(buf_ptr, src).is_err() {
        return u64::MAX;
    }

    // Advance offset.
    FD_TABLE.lock()[fd] = Some(FdEntry {
        content_addr: entry.content_addr,
        content_len: entry.content_len,
        offset: entry.offset + to_read,
    });

    to_read as u64
}

// ---------------------------------------------------------------------------
// T014: write(fd, buf, count)
// ---------------------------------------------------------------------------

fn sys_linux_write(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    // Only stdout (1) and stderr (2) are supported.
    if fd != 1 && fd != 2 {
        return u64::MAX;
    }
    let len = (count as usize).min(4096);
    let mut buf = [0u8; 4096];
    if crate::mm::user_mem::copy_from_user(&mut buf[..len], buf_ptr).is_err() {
        return u64::MAX;
    }
    if let Ok(s) = core::str::from_utf8(&buf[..len]) {
        log::info!("[userspace] {}", s.trim_end_matches('\n'));
    }
    len as u64
}

// ---------------------------------------------------------------------------
// T015: open(path, flags) / openat delegates here
// ---------------------------------------------------------------------------

/// Read a null-terminated C string from userspace into `buf`.
///
/// Copies one byte at a time to handle page boundaries gracefully.
/// Returns the UTF-8 string on success, or `None` if the pointer is invalid,
/// the string exceeds `buf.len()`, or the bytes are not valid UTF-8.
fn read_user_cstr(ptr: u64, buf: &mut [u8; 512]) -> Option<&str> {
    if ptr == 0 {
        return None;
    }
    let mut len = 0usize;
    while len < 512 {
        let mut b = [0u8; 1];
        let addr = ptr.checked_add(len as u64)?;
        if crate::mm::user_mem::copy_from_user(&mut b, addr).is_err() {
            return None;
        }
        if b[0] == 0 {
            break;
        }
        buf[len] = b[0];
        len += 1;
    }
    if len == 0 {
        return None;
    }
    core::str::from_utf8(&buf[..len]).ok()
}

fn sys_linux_open(path_ptr: u64, _flags: u64) -> u64 {
    let mut buf = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return u64::MAX,
    };

    // Strip leading "/" since ramdisk files are stored without path prefix.
    let file_name = name.trim_start_matches('/');
    let content = match crate::fs::ramdisk::get_file(file_name) {
        Some(c) => c,
        None => {
            log::warn!("[open] file not found: {}", name);
            return u64::MAX;
        }
    };

    // Allocate an fd slot (start at 3 to skip stdin/stdout/stderr).
    let mut table = FD_TABLE.lock();
    for i in 3..MAX_FDS {
        if table[i].is_none() {
            table[i] = Some(FdEntry {
                content_addr: content.as_ptr() as usize,
                content_len: content.len(),
                offset: 0,
            });
            log::info!("[open] {} → fd {}", name, i);
            return i as u64;
        }
    }

    log::warn!("[open] fd table full");
    u64::MAX
}

// ---------------------------------------------------------------------------
// T015 (close) / T013 (close)
// ---------------------------------------------------------------------------

fn sys_linux_close(fd: u64) -> u64 {
    let fd = fd as usize;
    if !(3..MAX_FDS).contains(&fd) {
        return 0; // closing stdin/stdout/stderr or invalid: always ok
    }
    FD_TABLE.lock()[fd] = None;
    0
}

// ---------------------------------------------------------------------------
// T016: fstat(fd, stat_ptr)
// ---------------------------------------------------------------------------

/// Write a minimal Linux x86_64 `stat` struct to `stat_ptr`.
///
/// Only `st_size` (offset 48) and `st_mode` (offset 24) are filled in;
/// all other fields are zero.  This satisfies musl's `fstat` use in `fopen`.
fn sys_linux_fstat(fd: u64, stat_ptr: u64) -> u64 {
    let size = match fd {
        1 | 2 => 0u64, // stdout/stderr — no meaningful size
        _ => {
            let fd = fd as usize;
            if fd >= MAX_FDS {
                return u64::MAX;
            }
            match FD_TABLE.lock()[fd] {
                Some(e) => e.content_len as u64,
                None => return u64::MAX,
            }
        }
    };

    // x86_64 stat struct (144 bytes, all little-endian).
    let mut stat = [0u8; 144];
    // st_mode at offset 24: S_IFREG (0x8000) | 0o644
    let mode: u32 = 0x8000 | 0o644;
    stat[24..28].copy_from_slice(&mode.to_ne_bytes());
    // st_size at offset 48: file size
    stat[48..56].copy_from_slice(&size.to_ne_bytes());
    // st_blksize at offset 56: 4096
    let blksize: u64 = 4096;
    stat[56..64].copy_from_slice(&blksize.to_ne_bytes());

    if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
        return u64::MAX;
    }
    0
}

// ---------------------------------------------------------------------------
// T017: lseek(fd, offset, whence)
// ---------------------------------------------------------------------------

fn sys_linux_lseek(fd: u64, offset: u64, whence: u64) -> u64 {
    let fd = fd as usize;
    if !(3..MAX_FDS).contains(&fd) {
        return u64::MAX;
    }

    let mut table = FD_TABLE.lock();
    let entry = match &mut table[fd] {
        Some(e) => e,
        None => return u64::MAX,
    };

    const SEEK_SET: u64 = 0;
    const SEEK_CUR: u64 = 1;
    const SEEK_END: u64 = 2;

    let offset = offset as i64;

    let new_offset: i64 = match whence {
        SEEK_SET => offset,
        SEEK_CUR => match (entry.offset as i64).checked_add(offset) {
            Some(v) => v,
            None => return u64::MAX,
        },
        SEEK_END => match (entry.content_len as i64).checked_add(offset) {
            Some(v) => v,
            None => return u64::MAX,
        },
        _ => return u64::MAX,
    };

    if new_offset < 0 || new_offset as usize > entry.content_len {
        return u64::MAX;
    }

    entry.offset = new_offset as usize;
    entry.offset as u64
}

// ---------------------------------------------------------------------------
// T018: mmap(addr, len, prot, flags[from SYSCALL_ARG3], fd, offset)
//
// Only MAP_PRIVATE|MAP_ANONYMOUS (flags 0x22) with fd=-1 is supported.
// ---------------------------------------------------------------------------

fn sys_linux_mmap(addr_hint: u64, len: u64) -> u64 {
    // Read flags from SYSCALL_ARG3 (r10 at syscall entry).
    // SAFETY: single-CPU, read after every SYSCALL entry stores to SYSCALL_ARG3.
    let flags = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SYSCALL_ARG3)) };

    const MAP_ANONYMOUS: u64 = 0x20;
    if flags & MAP_ANONYMOUS == 0 {
        log::warn!(
            "[mmap] non-anonymous mmap not supported (flags={:#x})",
            flags
        );
        return u64::MAX;
    }

    let len = if len == 0 {
        return u64::MAX;
    } else {
        len
    };
    let pages = len.div_ceil(4096);

    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);

    // Determine base address: use process mmap_next or default ANON_MMAP_BASE.
    let base = {
        let mut table = crate::process::PROCESS_TABLE.lock();
        let proc = match table.find_mut(pid) {
            Some(p) => p,
            None => return u64::MAX,
        };
        if proc.mmap_next == 0 {
            proc.mmap_next = ANON_MMAP_BASE;
        }
        // Hint address is ignored: always allocate linearly.
        let _ = addr_hint;
        let base = proc.mmap_next;
        let total_size = match pages.checked_mul(4096) {
            Some(s) => s,
            None => return u64::MAX,
        };
        proc.mmap_next = match base.checked_add(total_size) {
            Some(v) => v,
            None => return u64::MAX,
        };
        base
    };

    // Validate that the entire range fits in canonical user space (< 0x0000_8000_0000_0000).
    let total_size = match pages.checked_mul(4096) {
        Some(s) => s,
        None => return u64::MAX,
    };
    let range_end = match base.checked_add(total_size) {
        Some(e) => e,
        None => return u64::MAX,
    };
    if range_end > 0x0000_8000_0000_0000 {
        return u64::MAX;
    }

    // Map pages in the current address space (current CR3 = this process).
    use x86_64::{
        structures::paging::{Mapper, Page, PageTableFlags, Size4KiB},
        VirtAddr,
    };
    let flags_pt = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;
    // SAFETY: current CR3 is the process's page table; no other mapper is live.
    let mut mapper = unsafe { crate::mm::paging::get_mapper() };
    let mut frame_alloc = crate::mm::paging::GlobalFrameAlloc;

    for i in 0..pages {
        // SAFETY: canonical range validated above.
        let vaddr = VirtAddr::new(base + i * 4096);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        let frame = match crate::mm::frame_allocator::allocate_frame() {
            Some(f) => f,
            None => {
                log::warn!("[mmap] out of frames at page {}/{}", i, pages);
                return u64::MAX;
            }
        };
        // Zero the frame via physical offset.
        let phys_off = crate::mm::phys_offset();
        unsafe {
            let ptr = (phys_off + frame.start_address().as_u64()) as *mut u8;
            core::ptr::write_bytes(ptr, 0, 4096);
        }
        // SAFETY: mapper covers the current CR3; frame was just allocated; page is unmapped.
        if unsafe { mapper.map_to(page, frame, flags_pt, &mut frame_alloc) }.is_err() {
            log::warn!("[mmap] map_to failed at page {}", i);
            return u64::MAX;
        }
        // No TLB flush needed for new mappings (no stale entry to evict).
    }

    log::info!("[mmap] anon {}×4K @ {:#x}", pages, base);
    base
}

// ---------------------------------------------------------------------------
// T019: munmap(addr, len) — stub (bump allocator; no reclamation in Phase 12)
// ---------------------------------------------------------------------------

fn sys_linux_munmap(_addr: u64, _len: u64) -> u64 {
    // Frames are not reclaimed (bump allocator).  Return success so programs
    // that call munmap (e.g. musl free for large chunks) do not fail.
    0
}

// ---------------------------------------------------------------------------
// T020: brk(addr)
// ---------------------------------------------------------------------------

fn sys_linux_brk(addr: u64) -> u64 {
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);

    let (current, base) = {
        let table = crate::process::PROCESS_TABLE.lock();
        match table.find(pid) {
            Some(p) => (p.brk_current, p.brk_current),
            None => return u64::MAX,
        }
    };

    // Initialise on first call.
    let current = if current == 0 { BRK_BASE } else { current };

    // brk(0) or no-advance: just return current break.
    if addr == 0 || addr <= current {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(p) = table.find_mut(pid) {
            if p.brk_current == 0 {
                p.brk_current = BRK_BASE;
            }
        }
        return current;
    }
    let _ = base;

    // Align new break up to page boundary.
    let new_brk = (addr + 0xFFF) & !0xFFF;
    let pages_needed = (new_brk - current) / 4096;

    use x86_64::{
        structures::paging::{Mapper, Page, PageTableFlags, Size4KiB},
        VirtAddr,
    };
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;
    // SAFETY: current CR3 is the process's page table.
    let mut mapper = unsafe { crate::mm::paging::get_mapper() };
    let mut frame_alloc = crate::mm::paging::GlobalFrameAlloc;
    let phys_off = crate::mm::phys_offset();

    for i in 0..pages_needed {
        let vaddr = VirtAddr::new(current + i * 4096);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        let frame = match crate::mm::frame_allocator::allocate_frame() {
            Some(f) => f,
            None => {
                log::warn!("[brk] out of frames at page {}/{}", i, pages_needed);
                return current; // return old brk to signal failure
            }
        };
        unsafe {
            let ptr = (phys_off + frame.start_address().as_u64()) as *mut u8;
            core::ptr::write_bytes(ptr, 0, 4096);
        }
        // SAFETY: mapper covers current CR3; frame was just allocated; page is unmapped.
        if unsafe { mapper.map_to(page, frame, flags, &mut frame_alloc) }.is_err() {
            log::warn!("[brk] map_to failed at page {}", i);
            return current;
        }
    }

    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(p) = table.find_mut(pid) {
            p.brk_current = new_brk;
        }
    }

    log::info!("[brk] extended to {:#x} ({} pages)", new_brk, pages_needed);
    new_brk
}

// ---------------------------------------------------------------------------
// T023: writev(fd, iov, iovcnt)
// ---------------------------------------------------------------------------

fn sys_linux_writev(fd: u64, iov_ptr: u64, iovcnt: u64) -> u64 {
    let iovcnt = (iovcnt as usize).min(16);
    let mut total = 0u64;
    for i in 0..iovcnt {
        // struct iovec { void *base (8B), size_t len (8B) }
        let iov_addr = iov_ptr + (i * 16) as u64;
        let mut iov_bytes = [0u8; 16];
        if crate::mm::user_mem::copy_from_user(&mut iov_bytes, iov_addr).is_err() {
            break;
        }
        let base = u64::from_ne_bytes(iov_bytes[0..8].try_into().unwrap());
        let len = u64::from_ne_bytes(iov_bytes[8..16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let written = sys_linux_write(fd, base, len);
        if written == u64::MAX {
            break;
        }
        total += written;
    }
    total
}

// ---------------------------------------------------------------------------
// T023: readv(fd, iov, iovcnt)
// ---------------------------------------------------------------------------

fn sys_linux_readv(fd: u64, iov_ptr: u64, iovcnt: u64) -> u64 {
    let iovcnt = (iovcnt as usize).min(16);
    let mut total = 0u64;
    for i in 0..iovcnt {
        let iov_addr = iov_ptr + (i * 16) as u64;
        let mut iov_bytes = [0u8; 16];
        if crate::mm::user_mem::copy_from_user(&mut iov_bytes, iov_addr).is_err() {
            break;
        }
        let base = u64::from_ne_bytes(iov_bytes[0..8].try_into().unwrap());
        let len = u64::from_ne_bytes(iov_bytes[8..16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let n = sys_linux_read(fd, base, len);
        if n == u64::MAX || n == 0 {
            break;
        }
        total += n;
    }
    total
}

// ---------------------------------------------------------------------------
// T024: getcwd(buf, size) — always returns "/"
// ---------------------------------------------------------------------------

fn sys_linux_getcwd(buf_ptr: u64, size: u64) -> u64 {
    if size < 2 {
        return u64::MAX;
    }
    let cwd = b"/\0";
    if crate::mm::user_mem::copy_to_user(buf_ptr, cwd).is_err() {
        return u64::MAX;
    }
    buf_ptr // Linux getcwd returns the buffer pointer on success
}

// ---------------------------------------------------------------------------
// T024: chdir — stub (always succeeds)
// ---------------------------------------------------------------------------

fn sys_linux_chdir(_path_ptr: u64) -> u64 {
    0
}

// ---------------------------------------------------------------------------
// T025: ioctl — TIOCGWINSZ only
// ---------------------------------------------------------------------------

fn sys_linux_ioctl(fd: u64, req: u64, arg: u64) -> u64 {
    const TIOCGWINSZ: u64 = 0x5413;
    if req == TIOCGWINSZ {
        // struct winsize { ws_row, ws_col, ws_xpixel, ws_ypixel } — each u16
        let winsize: [u8; 8] = {
            let mut w = [0u8; 8];
            w[0..2].copy_from_slice(&24u16.to_ne_bytes()); // rows
            w[2..4].copy_from_slice(&80u16.to_ne_bytes()); // cols
            w
        };
        if crate::mm::user_mem::copy_to_user(arg, &winsize).is_err() {
            return u64::MAX;
        }
        return 0;
    }
    // All other ioctl requests return ENOSYS.
    let _ = fd;
    u64::MAX
}

// ---------------------------------------------------------------------------
// T026: uname(buf) — writes a fixed struct utsname
// ---------------------------------------------------------------------------

fn sys_linux_uname(buf_ptr: u64) -> u64 {
    // struct utsname: 6 fields of 65 bytes each = 390 bytes
    let mut utsname = [0u8; 390];
    let fill = |dst: &mut [u8], s: &[u8]| {
        let n = s.len().min(dst.len() - 1);
        dst[..n].copy_from_slice(&s[..n]);
    };
    fill(&mut utsname[0..65], b"ostest"); // sysname
    fill(&mut utsname[65..130], b"ostest"); // nodename
    fill(&mut utsname[130..195], b"0.12.0"); // release
    fill(&mut utsname[195..260], b"phase-12"); // version
    fill(&mut utsname[260..325], b"x86_64"); // machine
                                             // domainname left as zero
    if crate::mm::user_mem::copy_to_user(buf_ptr, &utsname).is_err() {
        return u64::MAX;
    }
    0
}

// ---------------------------------------------------------------------------
// T026 (via path): newfstatat(dirfd, path, stat_ptr, flags)
// ---------------------------------------------------------------------------

fn sys_linux_fstatat(_dirfd: u64, path_ptr: u64, stat_ptr: u64) -> u64 {
    let mut buf = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return u64::MAX,
    };
    let file_name = name.trim_start_matches('/');
    let size = match crate::fs::ramdisk::get_file(file_name) {
        Some(c) => c.len() as u64,
        None => return u64::MAX,
    };

    let mut stat = [0u8; 144];
    let mode: u32 = 0x8000 | 0o644;
    stat[24..28].copy_from_slice(&mode.to_ne_bytes());
    stat[48..56].copy_from_slice(&size.to_ne_bytes());
    let blksize: u64 = 4096;
    stat[56..64].copy_from_slice(&blksize.to_ne_bytes());

    if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
        return u64::MAX;
    }
    0
}

// ---------------------------------------------------------------------------
// arch_prctl(code, addr) — syscall 158 (musl TLS initialization)
// ---------------------------------------------------------------------------

/// Handles `ARCH_SET_FS` (0x1002) which musl uses to set the FS.base MSR for
/// thread-local storage.  Other sub-commands return -EINVAL.
fn sys_linux_arch_prctl(code: u64, addr: u64) -> u64 {
    const ARCH_SET_FS: u64 = 0x1002;
    match code {
        ARCH_SET_FS => {
            x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(addr));
            0
        }
        _ => u64::MAX, // -EINVAL
    }
}

// ---------------------------------------------------------------------------
// set_tid_address(tidptr) — syscall 218 (musl TLS initialization)
// ---------------------------------------------------------------------------

/// Stub: stores nothing, returns the caller's PID (which is also the TID in
/// our single-threaded model).  musl calls this during `__init_tls` to record
/// the `clear_child_tid` pointer; we can safely ignore the pointer.
fn sys_linux_set_tid_address() -> u64 {
    crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed) as u64
}
