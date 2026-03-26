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

extern crate alloc;

// Linux errno values (negated for syscall return convention).
#[allow(dead_code)]
const NEG_EPERM: u64 = (-1_i64) as u64;
const NEG_ENOENT: u64 = (-2_i64) as u64;
const NEG_EBADF: u64 = (-9_i64) as u64;
#[allow(dead_code)]
const NEG_EAGAIN: u64 = (-11_i64) as u64;
const NEG_EFAULT: u64 = (-14_i64) as u64;
const NEG_EINVAL: u64 = (-22_i64) as u64;
const NEG_EMFILE: u64 = (-24_i64) as u64;
const NEG_EEXIST: u64 = (-17_i64) as u64;
const NEG_ENOSPC: u64 = (-28_i64) as u64;
const NEG_EROFS: u64 = (-30_i64) as u64;
const NEG_ENOSYS: u64 = (-38_i64) as u64;
const NEG_ENOTEMPTY: u64 = (-39_i64) as u64;

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
    // Divergent syscalls (exit) never return — handle them first.
    match number {
        6 => sys_exit_legacy(arg0),
        60 | 231 => sys_exit(arg0 as i32),
        _ => {}
    }

    let result = match number {
        // Linux-compatible file I/O (Phase 12, T013–T017)
        0 => sys_linux_read(arg0, arg1, arg2),
        1 => sys_linux_write(arg0, arg1, arg2),
        2 => sys_linux_open(arg0, arg1),
        3 => sys_linux_close(arg0),
        5 => sys_linux_fstat(arg0, arg1),
        8 => sys_linux_lseek(arg0, arg1, arg2),
        // Linux-compatible memory (Phase 12, T018–T020)
        9 => sys_linux_mmap(arg0, arg1),
        11 => sys_linux_munmap(arg0, arg1),
        12 => sys_linux_brk(arg0),
        // Phase 14: signal syscalls (rt_sigaction, rt_sigprocmask)
        13 => sys_rt_sigaction(arg0, arg1, arg2),
        14 => sys_rt_sigprocmask(),
        // Linux misc (Phase 12, T023–T026)
        16 => sys_linux_ioctl(arg0, arg1, arg2),
        19 => sys_linux_readv(arg0, arg1, arg2),
        20 => sys_linux_writev(arg0, arg1, arg2),
        // Phase 14: pipe and dup2
        22 => sys_pipe(arg0),
        33 => sys_dup2(arg0, arg1),
        // Phase 14: nanosleep
        35 => sys_nanosleep(arg0),
        // IPC syscalls (Phase 6) — kernel-task only.
        4 | 7 | 10 => crate::ipc::dispatch(number, arg0, arg1, arg2, 0, 0),
        // Phase 11 + Linux-compatible process syscalls
        39 => sys_getpid(),
        57 => sys_fork(user_rip, user_rsp),
        59 => sys_execve(arg0, arg1, arg2),
        61 => sys_waitpid(arg0, arg1, arg2),
        // Phase 14: signal syscalls
        62 => sys_kill(arg0, arg1),
        63 => sys_linux_uname(arg0),
        // Phase 13: filesystem mutation syscalls
        74 => sys_linux_fsync(arg0),
        76 => sys_linux_truncate(arg0, arg1),
        77 => sys_linux_ftruncate(arg0, arg1),
        79 => sys_linux_getcwd(arg0, arg1),
        80 => sys_linux_chdir(arg0),
        82 => sys_linux_rename(arg0, arg1),
        83 => sys_linux_mkdir(arg0, arg1),
        84 => sys_linux_rmdir(arg0),
        87 => sys_linux_unlink(arg0),
        // Phase 14: process group syscalls
        109 => sys_setpgid(arg0, arg1),
        110 => sys_getppid(),
        121 => sys_getpgid(arg0),
        // musl TLS init (Phase 12, T030 dependency)
        158 => sys_linux_arch_prctl(arg0, arg1),
        217 => sys_linux_getdents64(arg0, arg1, arg2),
        218 => sys_linux_set_tid_address(),
        // openat: ignore dirfd, treat as open(path, flags)
        257 => sys_linux_open(arg1, arg2),
        // newfstatat: fstat via path lookup
        262 => sys_linux_fstatat(arg0, arg1, arg2),
        // Custom kernel debug print (moved from 12, Phase 12 T010)
        0x1000 => sys_debug_print(arg0, arg1),
        _ => NEG_ENOSYS,
    };

    // Phase 14 (P14-T031): check pending signals before returning to userspace.
    check_pending_signals();

    result
}

/// Check and deliver pending signals for the current process.
///
/// Called after every syscall (except exit/execve which diverge).
/// For now, only default actions are supported:
///   - Terminate: kill the process
///   - Stop: mark as Stopped and yield
///   - Continue: already handled in send_signal
///   - Ignore: do nothing
fn check_pending_signals() {
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    if pid == 0 {
        return; // kernel task, no signals
    }

    loop {
        let sig = crate::process::dequeue_signal(pid);
        match sig {
            None => break,
            Some((signum, disposition)) => {
                use crate::process::SignalDisposition;
                match disposition {
                    SignalDisposition::Terminate => {
                        log::info!("[p{}] killed by signal {}", pid, signum);
                        // Use negative exit code to indicate signal death.
                        sys_exit(-(signum as i32));
                    }
                    SignalDisposition::Stop => {
                        log::info!("[p{}] stopped by signal {}", pid, signum);
                        {
                            let mut table = crate::process::PROCESS_TABLE.lock();
                            if let Some(proc) = table.find_mut(pid) {
                                proc.state = crate::process::ProcessState::Stopped;
                                proc.stop_signal = signum;
                                proc.stop_reported = false;
                            }
                        }
                        crate::process::send_sigchld_to_parent(pid);
                        // Yield until SIGCONT resumes us.
                        while {
                            let table = crate::process::PROCESS_TABLE.lock();
                            table
                                .find(pid)
                                .map(|p| p.state == crate::process::ProcessState::Stopped)
                                .unwrap_or(false)
                        } {
                            crate::task::yield_now();
                            crate::process::CURRENT_PID
                                .store(pid, core::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    SignalDisposition::Continue | SignalDisposition::Ignore => {
                        // Nothing to do.
                    }
                }
            }
        }
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
/// Marks the process as a zombie, stores the exit code, restores the kernel
/// CR3 (so the next scheduled task has a consistent address space), then marks
/// the kernel task as dead so it is never rescheduled.
fn sys_exit(code: i32) -> ! {
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    log::info!("[p{}] exit({})", pid, code);
    if pid != 0 {
        // Close all open FDs so pipe ref-counts reach 0 and EOF propagates.
        crate::process::close_all_fds_for(pid);
        {
            let mut table = crate::process::PROCESS_TABLE.lock();
            if let Some(proc) = table.find_mut(pid) {
                proc.state = crate::process::ProcessState::Zombie;
                proc.exit_code = Some(code);
            }
        }
        // Deliver SIGCHLD to parent (Phase 14, P14-T033a).
        crate::process::send_sigchld_to_parent(pid);
    }
    // Read the dying process's CR3 before we switch away from it.
    let cr3_phys = {
        let table = crate::process::PROCESS_TABLE.lock();
        table.find(pid).and_then(|p| p.page_table_root)
    };
    // Restore kernel page table before yielding so the next scheduled task
    // does not inherit this process's CR3.
    crate::mm::restore_kernel_cr3();
    // Free the process's user-space page table frames now that we are back
    // on the kernel CR3 and no longer using the process's address space.
    if let Some(phys) = cr3_phys {
        crate::mm::free_process_page_table(phys.as_u64());
    }
    // Mark the kernel task as dead so the scheduler reclaims it.
    crate::task::mark_current_dead();
}

// ---------------------------------------------------------------------------
// Phase 14: Signal syscalls (P14-T029, T030, T033)
// ---------------------------------------------------------------------------

/// `kill(pid, sig)` — send a signal to a process (syscall 62).
fn sys_kill(pid: u64, sig: u64) -> u64 {
    let sig = sig as u32;
    let target_pid = pid as i64;

    if sig > 63 {
        return NEG_EINVAL;
    }

    // sig=0: permission check only, no signal sent.
    if sig == 0 {
        const NEG_ESRCH: u64 = (-3_i64) as u64;
        let table = crate::process::PROCESS_TABLE.lock();
        return if table.find(pid as crate::process::Pid).is_some() {
            0
        } else {
            NEG_ESRCH
        };
    }

    const NEG_ESRCH_KILL: u64 = (-3_i64) as u64;
    if target_pid > 0 {
        // Send to a specific process.
        if crate::process::send_signal(target_pid as crate::process::Pid, sig) {
            0
        } else {
            NEG_ESRCH_KILL
        }
    } else if target_pid < -1 {
        // Send to process group |pid|.
        let pgid = (-target_pid) as crate::process::Pid;
        crate::process::send_signal_to_group(pgid, sig);
        0
    } else if target_pid == 0 {
        // Send to caller's process group.
        let caller_pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
        let pgid = {
            let table = crate::process::PROCESS_TABLE.lock();
            table.find(caller_pid).map(|p| p.pgid).unwrap_or(0)
        };
        if pgid != 0 {
            crate::process::send_signal_to_group(pgid, sig);
        }
        0
    } else {
        // pid=0 or pid=-1: not fully implemented yet.
        NEG_EINVAL
    }
}

/// `rt_sigaction(sig, act, oldact, sigsetsize)` — install/query signal handler (syscall 13).
///
/// We only support Default and Ignore (no user signal handlers yet).
fn sys_rt_sigaction(sig: u64, act_ptr: u64, oldact_ptr: u64) -> u64 {
    let sig = sig as u32;
    if sig == 0 || sig >= 32 {
        return NEG_EINVAL;
    }
    // SIGKILL and SIGSTOP cannot be caught or ignored.
    if sig == crate::process::SIGKILL || sig == crate::process::SIGSTOP {
        return NEG_EINVAL;
    }

    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find_mut(pid) {
        Some(p) => p,
        None => return NEG_EINVAL,
    };

    // Write old action if requested.
    // struct sigaction: sa_handler (8 bytes) + sa_flags (8 bytes) + sa_restorer (8 bytes) + sa_mask (8 bytes) = 32 bytes min
    if oldact_ptr != 0 {
        let mut old_sa = [0u8; 32];
        let handler: u64 = match proc.signal_actions[sig as usize] {
            crate::process::SignalAction::Default => 0, // SIG_DFL
            crate::process::SignalAction::Ignore => 1,  // SIG_IGN
        };
        old_sa[0..8].copy_from_slice(&handler.to_ne_bytes());
        if crate::mm::user_mem::copy_to_user(oldact_ptr, &old_sa).is_err() {
            return NEG_EFAULT;
        }
    }

    // Read new action if provided.
    if act_ptr != 0 {
        let mut sa = [0u8; 32];
        if crate::mm::user_mem::copy_from_user(&mut sa, act_ptr).is_err() {
            return NEG_EFAULT;
        }
        let handler = u64::from_ne_bytes(sa[0..8].try_into().unwrap());
        proc.signal_actions[sig as usize] = match handler {
            0 => crate::process::SignalAction::Default, // SIG_DFL
            1 => crate::process::SignalAction::Ignore,  // SIG_IGN
            _ => crate::process::SignalAction::Default, // user handlers → treat as default for now
        };
    }

    0
}

/// `rt_sigprocmask(how, set, oldset, sigsetsize)` — stub (syscall 14).
///
/// Signal masking is not implemented; always returns success.
fn sys_rt_sigprocmask() -> u64 {
    0
}

// ---------------------------------------------------------------------------
// Phase 14: pipe (P14-T009) and dup2 (P14-T014)
// ---------------------------------------------------------------------------

/// `pipe(pipefd_ptr)` — create a pipe (syscall 22).
///
/// Writes `[read_fd, write_fd]` to userspace memory at `pipefd_ptr`.
fn sys_pipe(pipefd_ptr: u64) -> u64 {
    let pipe_id = crate::pipe::create_pipe();

    let read_entry = FdEntry {
        backend: FdBackend::PipeRead { pipe_id },
        offset: 0,
        readable: true,
        writable: false,
    };
    let write_entry = FdEntry {
        backend: FdBackend::PipeWrite { pipe_id },
        offset: 0,
        readable: false,
        writable: true,
    };

    let read_fd = match alloc_fd(3, read_entry) {
        Some(fd) => fd,
        None => {
            crate::pipe::pipe_close_reader(pipe_id);
            crate::pipe::pipe_close_writer(pipe_id);
            return NEG_EMFILE;
        }
    };
    let write_fd = match alloc_fd(3, write_entry) {
        Some(fd) => fd,
        None => {
            // Clean up the read fd we just allocated.
            with_current_fd_mut(read_fd, |slot| *slot = None);
            crate::pipe::pipe_close_reader(pipe_id);
            crate::pipe::pipe_close_writer(pipe_id);
            return NEG_EMFILE;
        }
    };

    // Write [read_fd, write_fd] as two i32s to user memory.
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&(read_fd as i32).to_ne_bytes());
    bytes[4..].copy_from_slice(&(write_fd as i32).to_ne_bytes());
    if crate::mm::user_mem::copy_to_user(pipefd_ptr, &bytes).is_err() {
        // Clean up on failure.
        with_current_fd_mut(read_fd, |slot| *slot = None);
        with_current_fd_mut(write_fd, |slot| *slot = None);
        crate::pipe::pipe_close_reader(pipe_id);
        crate::pipe::pipe_close_writer(pipe_id);
        return NEG_EFAULT;
    }

    log::info!(
        "[pipe] created pipe_id={} → fd[{}(r), {}(w)]",
        pipe_id,
        read_fd,
        write_fd
    );
    0
}

/// `dup2(oldfd, newfd)` — duplicate a file descriptor (syscall 33).
fn sys_dup2(oldfd: u64, newfd: u64) -> u64 {
    let oldfd = oldfd as usize;
    let newfd = newfd as usize;

    if oldfd >= MAX_FDS || newfd >= MAX_FDS {
        return NEG_EBADF;
    }

    // dup2(fd, fd) returns fd without closing.
    if oldfd == newfd {
        return if current_fd_entry(oldfd).is_some() {
            newfd as u64
        } else {
            NEG_EBADF
        };
    }

    let entry = match current_fd_entry(oldfd) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    // Close newfd if it's open (including pipe cleanup).
    if current_fd_entry(newfd).is_some() {
        sys_linux_close(newfd as u64);
    }

    // Increment pipe ref-count for the duplicated FD.
    match &entry.backend {
        FdBackend::PipeRead { pipe_id } => crate::pipe::pipe_add_reader(*pipe_id),
        FdBackend::PipeWrite { pipe_id } => crate::pipe::pipe_add_writer(*pipe_id),
        _ => {}
    }

    // Copy the FD entry to the new slot.
    with_current_fd_mut(newfd, |slot| {
        *slot = Some(entry);
    });

    newfd as u64
}

// ---------------------------------------------------------------------------
// Phase 14: process group syscalls (P14-T035)
// ---------------------------------------------------------------------------

/// `setpgid(pid, pgid)` — set process group ID (syscall 109).
fn sys_setpgid(pid: u64, pgid: u64) -> u64 {
    let caller = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    let target = if pid == 0 {
        caller
    } else {
        pid as crate::process::Pid
    };
    let new_pgid = if pgid == 0 {
        target
    } else {
        pgid as crate::process::Pid
    };

    let mut table = crate::process::PROCESS_TABLE.lock();
    match table.find_mut(target) {
        Some(p) => {
            p.pgid = new_pgid;
            0
        }
        None => NEG_EINVAL,
    }
}

/// `getpgid(pid)` — get process group ID (syscall 121).
fn sys_getpgid(pid: u64) -> u64 {
    let target = if pid == 0 {
        crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed)
    } else {
        pid as crate::process::Pid
    };

    let table = crate::process::PROCESS_TABLE.lock();
    match table.find(target) {
        Some(p) => p.pgid as u64,
        None => NEG_EINVAL,
    }
}

/// `nanosleep(req, rem)` — sleep for the specified time (syscall 35).
///
/// Reads a `timespec` struct from user memory and yield-loops for the
/// requested number of timer ticks.
fn sys_nanosleep(req_ptr: u64) -> u64 {
    if req_ptr == 0 {
        return NEG_EFAULT;
    }
    let mut ts = [0u8; 16]; // struct timespec { tv_sec: i64, tv_nsec: i64 }
    if crate::mm::user_mem::copy_from_user(&mut ts, req_ptr).is_err() {
        return NEG_EFAULT;
    }
    let secs = i64::from_ne_bytes(ts[0..8].try_into().unwrap());
    let nsecs = i64::from_ne_bytes(ts[8..16].try_into().unwrap());
    if secs < 0 || !(0..1_000_000_000).contains(&nsecs) {
        return NEG_EINVAL;
    }
    // Each PIT tick is ~10ms (100 Hz). Convert seconds+nsec to ticks.
    let ticks = (secs as u64).saturating_mul(100) + (nsecs as u64) / 10_000_000;
    let start = crate::arch::x86_64::interrupts::tick_count();
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    while crate::arch::x86_64::interrupts::tick_count().wrapping_sub(start) < ticks {
        crate::task::yield_now();
        crate::process::CURRENT_PID.store(pid, core::sync::atomic::Ordering::Relaxed);
        if has_pending_signal() {
            return NEG_EINTR;
        }
    }
    0
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

    // CoW-clone user-accessible pages: share physical frames between parent
    // and child, clearing WRITABLE so writes trigger page faults.
    let phys_off = crate::mm::phys_offset();
    let cow_result = {
        // SAFETY: child_cr3 was just allocated; no other mapper over it exists.
        let mut child_mapper = unsafe { crate::mm::mapper_for_frame(child_cr3) };
        // SAFETY: current CR3 is the parent; we modify its PTEs to clear WRITABLE.
        unsafe { cow_clone_user_pages(phys_off, &mut child_mapper) }
        // child_mapper drops here, ending its borrow of the page table.
    };
    if let Err(e) = cow_result {
        log::warn!("[fork] CoW clone failed: {:?}", e);
        crate::mm::free_process_page_table(child_cr3.start_address().as_u64());
        return u64::MAX;
    }

    // Inherit parent's brk/mmap state and FD table so the child's heap
    // and file descriptors are consistent with the copied address space.
    let (parent_brk, parent_mmap, parent_fds, parent_pgid) = {
        let table = crate::process::PROCESS_TABLE.lock();
        match table.find(parent_pid) {
            Some(p) => (p.brk_current, p.mmap_next, p.fd_table.clone(), p.pgid),
            None => (
                0,
                0,
                {
                    const NONE: Option<crate::process::FdEntry> = None;
                    [NONE; crate::process::MAX_FDS]
                },
                0,
            ),
        }
    };

    // Increment pipe ref-counts for cloned FDs before creating the child.
    crate::process::add_pipe_refs(&parent_fds);

    // Create child process entry with cloned FD table (Phase 14, P14-T003).
    // Inherit parent's pgid so fork children are in the same process group.
    let child_pid = crate::process::spawn_process_with_cr3_and_fds(
        parent_pid,
        user_rip,
        user_rsp,
        x86_64::PhysAddr::new(child_cr3.start_address().as_u64()),
        parent_brk,
        parent_mmap,
        parent_fds,
        parent_pgid,
    );

    // Push the fork context so fork_child_trampoline can find the right RIP/RSP.
    crate::process::push_fork_ctx(child_pid, user_rip, user_rsp);

    // Spawn a kernel task for the child; it will enter ring 3 on first dispatch.
    crate::task::spawn(crate::process::fork_child_trampoline, "fork-child");

    log::info!("[p{}] fork() → child pid {}", parent_pid, child_pid);
    child_pid as u64
}

/// Read a null-terminated array of char* pointers from user memory, copying
/// each pointed-to C string into a kernel `Vec<Vec<u8>>`.
///
/// Returns an empty vec if `array_ptr` is 0 (NULL).
/// Returns at most `max_entries` strings; each string is capped at 4096 bytes.
/// Read a null-terminated array of `char*` pointers from user memory.
///
/// Returns `Ok(vec)` on success, `Err(())` if a user pointer is invalid
/// (caller should return EFAULT).
fn read_user_string_array(
    array_ptr: u64,
    max_entries: usize,
) -> Result<alloc::vec::Vec<alloc::vec::Vec<u8>>, ()> {
    let mut result = alloc::vec::Vec::new();
    if array_ptr == 0 {
        return Ok(result);
    }
    for i in 0..max_entries {
        let ptr_addr = match array_ptr.checked_add((i * 8) as u64) {
            Some(a) => a,
            None => return Err(()),
        };
        let mut ptr_bytes = [0u8; 8];
        if crate::mm::user_mem::copy_from_user(&mut ptr_bytes, ptr_addr).is_err() {
            return Err(());
        }
        let str_ptr = u64::from_ne_bytes(ptr_bytes);
        if str_ptr == 0 {
            break; // NULL terminator
        }
        // Read the C string byte by byte.
        let mut s = alloc::vec::Vec::new();
        let mut found_nul = false;
        for j in 0..4096u64 {
            let addr = match str_ptr.checked_add(j) {
                Some(a) => a,
                None => return Err(()),
            };
            let mut b = [0u8; 1];
            if crate::mm::user_mem::copy_from_user(&mut b, addr).is_err() {
                return Err(());
            }
            if b[0] == 0 {
                found_nul = true;
                break;
            }
            s.push(b[0]);
        }
        if !found_nul {
            return Err(());
        }
        result.push(s);
    }
    Ok(result)
}

/// `execve(filename, argv, envp)` — replace the calling process's image
/// with a new ELF binary read from the ramdisk.
///
/// Phase 14: now parses argv and envp from user memory (Linux ABI).
fn sys_execve(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> u64 {
    // Read the filename as a null-terminated C string.
    let mut name_cstr = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut name_cstr) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    log::info!(
        "[p{}] execve({})",
        crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed),
        name
    );

    // Parse argv and envp from user memory.
    let user_argv = match read_user_string_array(argv_ptr, 256) {
        Ok(v) => v,
        Err(()) => return NEG_EFAULT,
    };
    let user_envp = match read_user_string_array(envp_ptr, 256) {
        Ok(v) => v,
        Err(()) => return NEG_EFAULT,
    };

    // Strip leading "/" or path prefix to get the ramdisk filename.
    let file_name = name.trim_start_matches('/');

    // Read the binary from the ramdisk.
    let data = match crate::fs::ramdisk::get_file(file_name) {
        Some(d) => d,
        None => {
            log::warn!("[execve] file not found: {}", file_name);
            return NEG_ENOENT;
        }
    };

    // Allocate a fresh page table for the new image.
    const NEG_ENOMEM: u64 = (-12_i64) as u64;
    let new_cr3 = match crate::mm::new_process_page_table() {
        Some(f) => f,
        None => return NEG_ENOMEM,
    };

    let phys_off = crate::mm::phys_offset();

    // Build argv slices: use user-provided argv if non-empty, else [filename].
    let argv_refs: alloc::vec::Vec<&[u8]> = if user_argv.is_empty() {
        alloc::vec![name.as_bytes()]
    } else {
        user_argv.iter().map(|v| v.as_slice()).collect()
    };
    let envp_refs: alloc::vec::Vec<&[u8]> = user_envp.iter().map(|v| v.as_slice()).collect();

    let (loaded, user_rsp) = {
        // SAFETY: new_cr3 is freshly allocated; no other mapper exists.
        let mut mapper = unsafe { crate::mm::mapper_for_frame(new_cr3) };
        let loaded = match unsafe { crate::mm::elf::load_elf_into(&mut mapper, phys_off, data) } {
            Ok(l) => l,
            Err(e) => {
                log::warn!("[execve] ELF load failed: {:?}", e);
                return NEG_ENOENT; // treat invalid ELF as "not found"
            }
        };
        // SAFETY: stack pages were just mapped by load_elf_into; mapper is valid.
        let user_rsp = match unsafe {
            crate::mm::elf::setup_abi_stack_with_envp(
                loaded.stack_top,
                &mapper,
                phys_off,
                &argv_refs,
                &envp_refs,
                loaded.phdr_vaddr,
                loaded.phnum,
            )
        } {
            Ok(rsp) => rsp,
            Err(e) => {
                log::warn!("[execve] ABI stack setup failed: {:?}", e);
                return NEG_ENOMEM;
            }
        };
        (loaded, user_rsp)
    };

    // Update the process entry with the new CR3 and entry point.
    // Reset brk/mmap state since the address space is completely replaced.
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.page_table_root = Some(x86_64::PhysAddr::new(new_cr3.start_address().as_u64()));
            proc.entry_point = loaded.entry;
            proc.user_stack_top = user_rsp;
            proc.brk_current = 0;
            proc.mmap_next = 0;
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
/// `waitpid(pid, status_ptr, options)` — wait for a child to exit or stop.
///
/// Supports pid > 0 (specific child), pid == -1 (any child), pid == 0
/// (any child in caller's process group).
/// WUNTRACED (0x2): also report stopped children.
fn sys_waitpid(pid: u64, status_ptr: u64, options: u64) -> u64 {
    let target_pid = pid as i64;
    let calling_pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    const WUNTRACED: u64 = 0x2;
    let report_stopped = options & WUNTRACED != 0;

    // For specific PID: verify it's a child.
    if target_pid > 0 {
        let table = crate::process::PROCESS_TABLE.lock();
        const NEG_ECHILD_PRE: u64 = (-10_i64) as u64;
        match table.find(target_pid as crate::process::Pid) {
            None => return NEG_ECHILD_PRE,
            Some(p) if p.ppid != calling_pid => return NEG_ECHILD_PRE,
            Some(_) => {}
        }
    }

    const NEG_ECHILD: u64 = (-10_i64) as u64;

    loop {
        // Scan for a matching child that is zombie (or stopped if WUNTRACED).
        let result = {
            let mut table = crate::process::PROCESS_TABLE.lock();
            let mut found_pid = None;
            let mut found_code = None;
            let mut found_stopped = false;
            let mut has_eligible_child = false;

            for proc in table.iter() {
                if proc.ppid != calling_pid {
                    continue;
                }
                let matches = match target_pid {
                    p if p > 0 => proc.pid == p as crate::process::Pid,
                    -1 => true, // any child
                    0 => {
                        // Same process group as caller.
                        let caller_pgid = table
                            .find(calling_pid)
                            .map(|p| p.pgid)
                            .unwrap_or(calling_pid);
                        proc.pgid == caller_pgid
                    }
                    neg => proc.pgid == (-neg) as crate::process::Pid,
                };
                if !matches {
                    continue;
                }
                has_eligible_child = true;

                if proc.state == crate::process::ProcessState::Zombie {
                    found_pid = Some(proc.pid);
                    found_code = proc.exit_code;
                    break;
                }
                if report_stopped
                    && proc.state == crate::process::ProcessState::Stopped
                    && !proc.stop_reported
                {
                    found_pid = Some(proc.pid);
                    found_stopped = true;
                    found_code = Some(proc.stop_signal as i32);
                    break;
                }
            }

            if !has_eligible_child {
                return NEG_ECHILD;
            }

            if let Some(pid) = found_pid {
                if found_stopped {
                    // Mark as reported so subsequent waitpid calls don't re-report.
                    if let Some(p) = table.find_mut(pid) {
                        p.stop_reported = true;
                    }
                    Some((pid, found_code, true)) // stopped
                } else {
                    let code = found_code.unwrap_or(0);
                    table.reap(pid);
                    Some((pid, Some(code), false))
                }
            } else {
                None
            }
        };

        if let Some((child_pid, code_opt, stopped)) = result {
            // Restore caller context.
            waitpid_restore_caller(calling_pid);

            // Write wstatus.
            if status_ptr != 0 {
                let wstatus = if stopped {
                    // WIFSTOPPED: (sig << 8) | 0x7f
                    let sig = code_opt.unwrap_or(crate::process::SIGTSTP as i32);
                    (sig & 0xff) << 8 | 0x7f
                } else {
                    let code = code_opt.unwrap_or(0);
                    if code >= 0 {
                        (code & 0xff) << 8 // WIFEXITED
                    } else {
                        (-code) & 0x7f // WIFSIGNALED
                    }
                };
                let bytes = wstatus.to_ne_bytes();
                let _ = crate::mm::user_mem::copy_to_user(status_ptr, &bytes);
            }
            log::info!(
                "[waitpid] pid {} {}",
                child_pid,
                if stopped { "stopped" } else { "exited" }
            );
            return child_pid as u64;
        }

        // No matching child ready; yield and try again.
        crate::task::yield_now();
        crate::process::CURRENT_PID.store(calling_pid, core::sync::atomic::Ordering::Relaxed);
    }
}

/// Restore the caller's CR3 and kernel stack after waitpid.
fn waitpid_restore_caller(calling_pid: crate::process::Pid) {
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
    if kstack_top != 0 {
        unsafe {
            crate::arch::x86_64::gdt::set_kernel_stack(kstack_top);
            *(core::ptr::addr_of_mut!(crate::arch::x86_64::syscall::SYSCALL_STACK_TOP)) =
                kstack_top;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check if the current process has pending signals that would interrupt.
///
/// Only returns true for signals whose disposition is not Ignore (e.g.,
/// SIGCHLD defaults to Ignore and should not cause EINTR).
fn has_pending_signal() -> bool {
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    if pid == 0 {
        return false;
    }
    let table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find(pid) {
        Some(p) => p,
        None => return false,
    };
    if proc.pending_signals == 0 {
        return false;
    }
    // Check if any pending signal has a non-Ignore disposition.
    for sig in 0..64u32 {
        if proc.pending_signals & (1u64 << sig) != 0 {
            let action = if sig < 32 {
                proc.signal_actions[sig as usize]
            } else {
                crate::process::SignalAction::Default
            };
            let disposition = match action {
                crate::process::SignalAction::Ignore => {
                    if sig == crate::process::SIGKILL || sig == crate::process::SIGSTOP {
                        return true; // cannot be ignored
                    }
                    crate::process::SignalDisposition::Ignore
                }
                crate::process::SignalAction::Default => crate::process::default_signal_action(sig),
            };
            if disposition != crate::process::SignalDisposition::Ignore {
                return true;
            }
        }
    }
    false
}

const NEG_EINTR: u64 = (-4_i64) as u64;

/// Copy-on-write clone of user-accessible pages from the parent's page table
/// into the child's page table.
///
/// Instead of copying page contents, both parent and child share the same
/// physical frames.  Writable pages have their WRITABLE bit cleared in both
/// parent and child so that a write triggers a page fault which is resolved
/// by `resolve_cow_fault` in the page fault handler.  Frame reference counts
/// are incremented for each
/// shared frame.
///
/// # Safety
/// The current CR3 must be the parent's page table and `dst_mapper` must
/// reference the child's freshly-allocated PML4.
unsafe fn cow_clone_user_pages(
    phys_off: u64,
    dst_mapper: &mut x86_64::structures::paging::OffsetPageTable<'_>,
) -> Result<(), crate::mm::elf::ElfError> {
    use x86_64::{
        registers::control::Cr3,
        structures::paging::{Mapper, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB},
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

                // Get a mutable reference to the parent's PT so we can clear
                // WRITABLE on CoW pages.
                let pt: &mut PageTable =
                    &mut *(phys_offset + p2e.addr().as_u64()).as_mut_ptr::<PageTable>();
                for p1 in 0usize..512 {
                    let pte = &mut pt[p1];
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
                    let mut flags = pte.flags();

                    // For writable pages, clear WRITABLE and set BIT_9 (CoW
                    // marker) in both parent and child so the page fault
                    // handler can distinguish CoW pages from genuinely
                    // read-only shared pages (e.g. .text/.rodata).
                    if flags.contains(PageTableFlags::WRITABLE) {
                        let new_parent_flags =
                            (flags & !PageTableFlags::WRITABLE) | PageTableFlags::BIT_9;
                        pte.set_addr(src_phys, new_parent_flags);
                        flags = new_parent_flags;
                    }

                    // Map the same physical frame in the child with the same
                    // flags (WRITABLE already cleared for formerly-writable pages).
                    let page = Page::<Size4KiB>::from_start_address(VirtAddr::new(vaddr)).map_err(
                        |_| crate::mm::elf::ElfError::MappingFailed("invalid vaddr in fork"),
                    )?;
                    let frame = PhysFrame::from_start_address(src_phys)
                        .expect("CoW: unaligned frame address");
                    dst_mapper
                        .map_to(page, frame, flags, &mut frame_alloc)
                        .map_err(|_| {
                            crate::mm::elf::ElfError::MappingFailed("map_to failed in cow fork")
                        })?
                        .ignore();

                    // Increment refcount after successful map_to — avoids
                    // leaking a reference if map_to fails.
                    crate::mm::frame_allocator::refcount_inc(src_phys.as_u64());
                }
            }
        }
    }

    // Flush parent's TLB to ensure CPU sees the cleared WRITABLE bits.
    // A full CR3 reload is the simplest approach.
    let (current_cr3, cr3_flags) = Cr3::read();
    Cr3::write(current_cr3, cr3_flags);

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

// Re-export FD types from process module (Phase 14 — per-process FD table).
use crate::process::{FdBackend, FdEntry, MAX_FDS};

/// Clone the FD entry at `fd` from the current process's FD table.
///
/// Returns `None` if no process is running or the FD slot is empty.
fn current_fd_entry(fd: usize) -> Option<FdEntry> {
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    let table = crate::process::PROCESS_TABLE.lock();
    let proc = table.find(pid)?;
    proc.fd_table.get(fd)?.clone()
}

/// Mutate the FD entry at `fd` in the current process's FD table.
fn with_current_fd_mut<F: FnOnce(&mut Option<FdEntry>)>(fd: usize, f: F) {
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    let mut table = crate::process::PROCESS_TABLE.lock();
    if let Some(proc) = table.find_mut(pid) {
        if let Some(slot) = proc.fd_table.get_mut(fd) {
            f(slot);
        }
    }
}

/// Allocate the lowest available FD slot (starting from `min_fd`) in the
/// current process's FD table. Returns the FD number or `None` if full.
fn alloc_fd(min_fd: usize, entry: FdEntry) -> Option<usize> {
    let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = table.find_mut(pid)?;
    for i in min_fd..MAX_FDS {
        if proc.fd_table[i].is_none() {
            proc.fd_table[i] = Some(entry);
            return Some(i);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// T013: read(fd, buf, count)
// ---------------------------------------------------------------------------

fn sys_linux_read(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    let fd = fd as usize;
    if fd >= MAX_FDS {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(fd) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    if !entry.readable {
        return NEG_EBADF;
    }

    match &entry.backend {
        FdBackend::Stdin => {
            // Read from kernel stdin buffer (Phase 14, Track E).
            // Yield-loop until data is available (line-buffered).
            let capped = (count as usize).min(4096);
            let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
            loop {
                if crate::stdin::has_data() {
                    let mut tmp = [0u8; 4096];
                    let n = crate::stdin::read(&mut tmp[..capped]);
                    if n > 0 {
                        if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                            return NEG_EFAULT;
                        }
                        return n as u64;
                    }
                }
                // Check for pending signals so Ctrl-C works while blocked.
                if has_pending_signal() {
                    return NEG_EINTR;
                }
                crate::task::yield_now();
                crate::process::CURRENT_PID.store(pid, core::sync::atomic::Ordering::Relaxed);
            }
        }
        FdBackend::Stdout => NEG_EBADF,
        FdBackend::Ramdisk {
            content_addr,
            content_len,
        } => {
            let remaining = content_len.saturating_sub(entry.offset);
            let to_read = (count as usize).min(remaining).min(64 * 1024);
            if to_read == 0 {
                return 0; // EOF
            }

            // SAFETY: content_addr is a static ramdisk pointer (lives forever).
            let src = unsafe {
                core::slice::from_raw_parts((*content_addr + entry.offset) as *const u8, to_read)
            };

            if crate::mm::user_mem::copy_to_user(buf_ptr, src).is_err() {
                return NEG_EFAULT;
            }

            with_current_fd_mut(fd, |slot| {
                if let Some(e) = slot {
                    e.offset += to_read;
                }
            });
            to_read as u64
        }
        FdBackend::Tmpfs { path } => {
            // Cap count at 64 KiB to match ramdisk path and prevent overflow.
            let capped_count = (count as usize).min(64 * 1024);
            let tmpfs = crate::fs::tmpfs::TMPFS.lock();
            let data = match tmpfs.read_file(path, entry.offset, capped_count) {
                Ok(d) => d,
                Err(crate::fs::tmpfs::TmpfsError::NotFound) => return NEG_ENOENT,
                Err(_) => return NEG_EBADF,
            };
            let to_read = data.len();
            if to_read == 0 {
                return 0; // EOF
            }

            if crate::mm::user_mem::copy_to_user(buf_ptr, data).is_err() {
                return NEG_EFAULT;
            }

            drop(tmpfs);
            with_current_fd_mut(fd, |slot| {
                if let Some(e) = slot {
                    e.offset += to_read;
                }
            });
            to_read as u64
        }
        FdBackend::PipeRead { pipe_id } => {
            let pipe_id = *pipe_id;
            let capped = (count as usize).min(4096);
            let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
            // Yield-loop until data is available or writer closes.
            loop {
                let mut tmp = [0u8; 4096];
                match crate::pipe::pipe_read(pipe_id, &mut tmp[..capped]) {
                    Ok(0) => return 0, // EOF
                    Ok(n) => {
                        if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                            return NEG_EFAULT;
                        }
                        return n as u64;
                    }
                    Err(_would_block) => {
                        if has_pending_signal() {
                            return NEG_EINTR;
                        }
                        crate::task::yield_now();
                        crate::process::CURRENT_PID
                            .store(pid, core::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
        FdBackend::PipeWrite { .. } => NEG_EBADF,
    }
}

// ---------------------------------------------------------------------------
// T014: write(fd, buf, count)
// ---------------------------------------------------------------------------

fn sys_linux_write(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    if !entry.writable {
        return NEG_EBADF;
    }

    match &entry.backend {
        FdBackend::Stdout => {
            // stdout/stderr go to serial.
            let len = (count as usize).min(4096);
            let mut buf = [0u8; 4096];
            if crate::mm::user_mem::copy_from_user(&mut buf[..len], buf_ptr).is_err() {
                return NEG_EFAULT;
            }
            if let Ok(s) = core::str::from_utf8(&buf[..len]) {
                log::info!("[userspace] {}", s.trim_end_matches('\n'));
            }
            len as u64
        }
        FdBackend::Stdin => NEG_EBADF,
        FdBackend::Ramdisk { .. } => NEG_EBADF, // ramdisk is read-only
        FdBackend::Tmpfs { path } => {
            let len = (count as usize).min(64 * 1024);
            let mut buf = [0u8; 4096];
            let mut written = 0usize;
            let mut offset = entry.offset;

            // Write in 4 KiB chunks to avoid huge stack buffers.
            while written < len {
                let chunk = (len - written).min(4096);
                let user_ptr = match buf_ptr.checked_add(written as u64) {
                    Some(p) => p,
                    None => {
                        if written == 0 {
                            return NEG_EFAULT;
                        }
                        break;
                    }
                };
                if crate::mm::user_mem::copy_from_user(&mut buf[..chunk], user_ptr).is_err() {
                    if written == 0 {
                        return NEG_EFAULT;
                    }
                    break;
                }
                let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
                if let Err(e) = tmpfs.write_file(path, offset, &buf[..chunk]) {
                    if written == 0 {
                        return match e {
                            crate::fs::tmpfs::TmpfsError::NoSpace => NEG_ENOSPC,
                            crate::fs::tmpfs::TmpfsError::NotFound => NEG_EBADF,
                            _ => NEG_EINVAL,
                        };
                    }
                    break;
                }
                drop(tmpfs);
                written += chunk;
                offset += chunk;
            }

            with_current_fd_mut(fd_idx, |slot| {
                if let Some(e) = slot {
                    e.offset = offset;
                }
            });
            written as u64
        }
        FdBackend::PipeWrite { pipe_id } => {
            let pipe_id = *pipe_id;
            let len = (count as usize).min(4096);
            let mut buf = [0u8; 4096];
            if crate::mm::user_mem::copy_from_user(&mut buf[..len], buf_ptr).is_err() {
                return NEG_EFAULT;
            }
            let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
            // Yield-loop until space is available or reader closes.
            loop {
                match crate::pipe::pipe_write(pipe_id, &buf[..len]) {
                    Ok(n) => return n as u64,
                    Err(false) => {
                        // Reader closed — EPIPE.
                        const NEG_EPIPE: u64 = (-32_i64) as u64;
                        return NEG_EPIPE;
                    }
                    Err(true) => {
                        // Would block — yield and retry.
                        if has_pending_signal() {
                            return NEG_EINTR;
                        }
                        crate::task::yield_now();
                        crate::process::CURRENT_PID
                            .store(pid, core::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
        FdBackend::PipeRead { .. } => NEG_EBADF,
    }
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
    if len >= buf.len() {
        return None; // no NUL terminator found within buffer
    }
    if len == 0 {
        return Some("");
    }
    core::str::from_utf8(&buf[..len]).ok()
}

/// Linux open flags.
const O_CREAT: u64 = 0o100;
const O_TRUNC: u64 = 0o1000;
const O_APPEND: u64 = 0o2000;

/// Check if a path targets the tmpfs mount at `/tmp`.
///
/// Returns `Some(relative_path)` if so (e.g. "/tmp/foo" → "foo").
/// Rejects paths containing `.`, `..`, or empty segments to prevent
/// traversal outside the `/tmp` mount boundary.
fn tmpfs_relative_path(path: &str) -> Option<&str> {
    let trimmed = path.trim_start_matches('/');
    let rest = if trimmed == "tmp" {
        ""
    } else {
        trimmed.strip_prefix("tmp/")?
    };

    // For non-empty relative paths, reject `.`, `..`, and empty segments.
    if !rest.is_empty() {
        for segment in rest.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return None;
            }
        }
    }

    Some(rest)
}

fn sys_linux_open(path_ptr: u64, flags: u64) -> u64 {
    let mut buf = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    // Decode POSIX access mode (O_ACCMODE = 0o3).
    let (readable, writable) = match flags & 0o3 {
        0 => (true, false),     // O_RDONLY
        1 => (false, true),     // O_WRONLY
        2 => (true, true),      // O_RDWR
        _ => return NEG_EINVAL, // invalid combination
    };
    let create = flags & O_CREAT != 0;
    let truncate = flags & O_TRUNC != 0;
    let append = flags & O_APPEND != 0;

    // Check if this is a tmpfs path.
    if let Some(rel) = tmpfs_relative_path(name) {
        if rel.is_empty() {
            // Opening /tmp itself — it's a directory, not a regular file.
            const NEG_EISDIR: u64 = (-21_i64) as u64;
            return NEG_EISDIR;
        }

        let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();

        // Open or create the file.
        match tmpfs.open_or_create(rel, create) {
            Ok(_created) => {}
            Err(crate::fs::tmpfs::TmpfsError::NotFound) => return NEG_ENOENT,
            Err(crate::fs::tmpfs::TmpfsError::WrongType) => {
                const NEG_EISDIR: u64 = (-21_i64) as u64;
                return NEG_EISDIR;
            }
            Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => {
                const NEG_ENOTDIR: u64 = (-20_i64) as u64;
                return NEG_ENOTDIR;
            }
            Err(_) => return NEG_EINVAL,
        }

        if truncate && writable {
            let _ = tmpfs.truncate(rel, 0);
        }

        let initial_offset = if append {
            tmpfs.file_size(rel).unwrap_or(0)
        } else {
            0
        };

        drop(tmpfs);

        // Allocate an fd slot in the current process's table.
        let entry = FdEntry {
            backend: FdBackend::Tmpfs {
                path: alloc::string::String::from(rel),
            },
            offset: initial_offset,
            readable,
            writable,
        };
        match alloc_fd(3, entry) {
            Some(i) => {
                log::info!("[open] {} → fd {} (tmpfs)", name, i);
                return i as u64;
            }
            None => {
                log::warn!("[open] fd table full");
                return NEG_EMFILE;
            }
        }
    }

    // Fall through to ramdisk lookup — ramdisk is read-only.
    if writable {
        return NEG_EROFS;
    }

    let file_name = name.trim_start_matches('/');
    let content = match crate::fs::ramdisk::get_file(file_name) {
        Some(c) => c,
        None => {
            log::warn!("[open] file not found: {}", name);
            return NEG_ENOENT;
        }
    };

    let entry = FdEntry {
        backend: FdBackend::Ramdisk {
            content_addr: content.as_ptr() as usize,
            content_len: content.len(),
        },
        offset: 0,
        readable: true,
        writable: false,
    };
    match alloc_fd(3, entry) {
        Some(i) => {
            log::info!("[open] {} → fd {}", name, i);
            i as u64
        }
        None => {
            log::warn!("[open] fd table full");
            NEG_EMFILE
        }
    }
}

// ---------------------------------------------------------------------------
// T015 (close) / T013 (close)
// ---------------------------------------------------------------------------

fn sys_linux_close(fd: u64) -> u64 {
    let fd = fd as usize;
    // stdin/stdout/stderr (0–2) are virtual and cannot be closed.
    if fd < 3 {
        return 0;
    }
    if fd >= MAX_FDS {
        return NEG_EBADF;
    }
    // Check if this FD is a pipe end; if so, close it in the pipe table.
    if let Some(entry) = current_fd_entry(fd) {
        match &entry.backend {
            FdBackend::PipeRead { pipe_id } => crate::pipe::pipe_close_reader(*pipe_id),
            FdBackend::PipeWrite { pipe_id } => crate::pipe::pipe_close_writer(*pipe_id),
            _ => {}
        }
    }
    let mut found = false;
    with_current_fd_mut(fd, |slot| {
        if slot.is_some() {
            *slot = None;
            found = true;
        }
    });
    if found {
        0
    } else {
        NEG_EBADF
    }
}

// ---------------------------------------------------------------------------
// T016: fstat(fd, stat_ptr)
// ---------------------------------------------------------------------------

/// Write a minimal Linux x86_64 `stat` struct to `stat_ptr`.
///
/// Only `st_size` (offset 48) and `st_mode` (offset 24) are filled in;
/// all other fields are zero.  This satisfies musl's `fstat` use in `fopen`.
fn sys_linux_fstat(fd: u64, stat_ptr: u64) -> u64 {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }
    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };
    let size = match &entry.backend {
        FdBackend::Stdout
        | FdBackend::Stdin
        | FdBackend::PipeRead { .. }
        | FdBackend::PipeWrite { .. } => 0u64,
        FdBackend::Ramdisk { content_len, .. } => *content_len as u64,
        FdBackend::Tmpfs { path } => {
            let tmpfs = crate::fs::tmpfs::TMPFS.lock();
            match tmpfs.file_size(path) {
                Ok(size) => size as u64,
                Err(_) => return NEG_ENOENT,
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
        return NEG_EFAULT;
    }
    0
}

// ---------------------------------------------------------------------------
// T017: lseek(fd, offset, whence)
// ---------------------------------------------------------------------------

fn sys_linux_lseek(fd: u64, offset: u64, whence: u64) -> u64 {
    let fd = fd as usize;
    if !(3..MAX_FDS).contains(&fd) {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(fd) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    const SEEK_SET: u64 = 0;
    const SEEK_CUR: u64 = 1;
    const SEEK_END: u64 = 2;

    let file_len = match &entry.backend {
        FdBackend::Stdout
        | FdBackend::Stdin
        | FdBackend::PipeRead { .. }
        | FdBackend::PipeWrite { .. } => return NEG_EINVAL, // not seekable
        FdBackend::Ramdisk { content_len, .. } => *content_len,
        FdBackend::Tmpfs { path } => {
            let tmpfs = crate::fs::tmpfs::TMPFS.lock();
            match tmpfs.file_size(path) {
                Ok(len) => len,
                Err(_) => return NEG_ENOENT,
            }
        }
    };

    let offset = offset as i64;

    let new_offset: i64 = match whence {
        SEEK_SET => offset,
        SEEK_CUR => match (entry.offset as i64).checked_add(offset) {
            Some(v) => v,
            None => return NEG_EINVAL,
        },
        SEEK_END => match (file_len as i64).checked_add(offset) {
            Some(v) => v,
            None => return NEG_EINVAL,
        },
        _ => return NEG_EINVAL,
    };

    if new_offset < 0 || new_offset as usize > file_len {
        return NEG_EINVAL;
    }

    // Update offset in per-process FD table.
    with_current_fd_mut(fd, |slot| {
        if let Some(e) = slot {
            e.offset = new_offset as usize;
        }
    });
    new_offset as u64
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
        return NEG_EINVAL;
    }

    let len = if len == 0 {
        return NEG_EINVAL;
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
            None => return NEG_EINVAL,
        };
        if proc.mmap_next == 0 {
            proc.mmap_next = ANON_MMAP_BASE;
        }
        // Hint address is ignored: always allocate linearly.
        let _ = addr_hint;
        let base = proc.mmap_next;
        let total_size = match pages.checked_mul(4096) {
            Some(s) => s,
            None => return NEG_EINVAL,
        };
        proc.mmap_next = match base.checked_add(total_size) {
            Some(v) => v,
            None => return NEG_EINVAL,
        };
        base
    };

    // Validate that the entire range fits in canonical user space (< 0x0000_8000_0000_0000).
    let total_size = match pages.checked_mul(4096) {
        Some(s) => s,
        None => return NEG_EINVAL,
    };
    let range_end = match base.checked_add(total_size) {
        Some(e) => e,
        None => return NEG_EINVAL,
    };
    if range_end > 0x0000_8000_0000_0000 {
        return NEG_EINVAL;
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
                return NEG_EINVAL;
            }
        };
        // Zero the frame via physical offset.
        let phys_off = crate::mm::phys_offset();
        unsafe {
            let ptr = (phys_off + frame.start_address().as_u64()) as *mut u8;
            core::ptr::write_bytes(ptr, 0, 4096);
        }
        // SAFETY: mapper covers the current CR3; frame was just allocated; page is unmapped.
        match unsafe { mapper.map_to(page, frame, flags_pt, &mut frame_alloc) } {
            Ok(flush) => flush.flush(),
            Err(_) => {
                log::warn!("[mmap] map_to failed at page {}", i);
                return NEG_EINVAL;
            }
        }
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

    // Always initialise brk_current to BRK_BASE if it is still 0, regardless
    // of the requested addr.  This ensures that even a first call with a
    // nonzero addr has a valid base to grow from, and if page mapping fails
    // later we still have a consistent brk_current.
    let current = {
        let mut table = crate::process::PROCESS_TABLE.lock();
        match table.find_mut(pid) {
            Some(p) => {
                if p.brk_current == 0 {
                    p.brk_current = BRK_BASE;
                }
                p.brk_current
            }
            None => return 0,
        }
    };

    // brk(0) or no-advance: just return current break.
    if addr == 0 || addr <= current {
        return current;
    }

    // Align new break up to page boundary.
    let new_brk = match addr.checked_add(0xFFF) {
        Some(v) => v & !0xFFF,
        None => return current,
    };
    // Reject non-canonical / kernel-range addresses.
    if new_brk > 0x0000_7FFF_FFFF_FFFF {
        return current;
    }
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
        match unsafe { mapper.map_to(page, frame, flags, &mut frame_alloc) } {
            Ok(flush) => flush.flush(),
            Err(_) => {
                log::warn!("[brk] map_to failed at page {}", i);
                return current;
            }
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
    if iovcnt > 1024 {
        return NEG_EINVAL;
    }
    let iovcnt = iovcnt as usize;
    let mut total = 0u64;
    for i in 0..iovcnt {
        // struct iovec { void *base (8B), size_t len (8B) }
        let offset = match (i as u64).checked_mul(16) {
            Some(v) => v,
            None => return NEG_EFAULT,
        };
        let iov_addr = match iov_ptr.checked_add(offset) {
            Some(a) => a,
            None => return NEG_EFAULT,
        };
        let mut iov_bytes = [0u8; 16];
        if crate::mm::user_mem::copy_from_user(&mut iov_bytes, iov_addr).is_err() {
            if total == 0 {
                return NEG_EFAULT;
            }
            break;
        }
        let base = u64::from_ne_bytes(iov_bytes[0..8].try_into().unwrap());
        let len = u64::from_ne_bytes(iov_bytes[8..16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let written = sys_linux_write(fd, base, len);
        if (written as i64) < 0 {
            // If no bytes transferred yet, propagate the error.
            if total == 0 {
                return written;
            }
            break;
        }
        if written == 0 {
            break;
        }
        total += written;
        // Short write: fewer bytes than requested means we should stop.
        if written < len {
            break;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// T023: readv(fd, iov, iovcnt)
// ---------------------------------------------------------------------------

fn sys_linux_readv(fd: u64, iov_ptr: u64, iovcnt: u64) -> u64 {
    if iovcnt > 1024 {
        return NEG_EINVAL;
    }
    let iovcnt = iovcnt as usize;
    let mut total = 0u64;
    for i in 0..iovcnt {
        let offset = match (i as u64).checked_mul(16) {
            Some(v) => v,
            None => return NEG_EFAULT,
        };
        let iov_addr = match iov_ptr.checked_add(offset) {
            Some(a) => a,
            None => return NEG_EFAULT,
        };
        let mut iov_bytes = [0u8; 16];
        if crate::mm::user_mem::copy_from_user(&mut iov_bytes, iov_addr).is_err() {
            if total == 0 {
                return NEG_EFAULT;
            }
            break;
        }
        let base = u64::from_ne_bytes(iov_bytes[0..8].try_into().unwrap());
        let len = u64::from_ne_bytes(iov_bytes[8..16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let n = sys_linux_read(fd, base, len);
        if (n as i64) < 0 {
            // If no bytes transferred yet, propagate the error.
            if total == 0 {
                return n;
            }
            break;
        }
        if n == 0 {
            break; // EOF
        }
        total += n;
        // Short read: fewer bytes than requested means EOF / no more data.
        if n < len {
            break;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// T024: getcwd(buf, size) — always returns "/"
// ---------------------------------------------------------------------------

fn sys_linux_getcwd(buf_ptr: u64, size: u64) -> u64 {
    if size < 2 {
        return NEG_EINVAL;
    }
    let cwd = b"/\0";
    if crate::mm::user_mem::copy_to_user(buf_ptr, cwd).is_err() {
        return NEG_EFAULT;
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
            return NEG_EFAULT;
        }
        return 0;
    }
    // All other ioctl requests return EINVAL.
    let _ = fd;
    NEG_EINVAL
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
    fill(&mut utsname[0..65], b"m3os"); // sysname
    fill(&mut utsname[65..130], b"m3os"); // nodename
    fill(&mut utsname[130..195], b"0.14.0"); // release
    fill(&mut utsname[195..260], b"phase-14"); // version
    fill(&mut utsname[260..325], b"x86_64"); // machine
                                             // domainname left as zero
    if crate::mm::user_mem::copy_to_user(buf_ptr, &utsname).is_err() {
        return NEG_EFAULT;
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
        None => return NEG_EFAULT,
    };

    // Check tmpfs first.
    if let Some(rel) = tmpfs_relative_path(name) {
        let tmpfs = crate::fs::tmpfs::TMPFS.lock();
        let st = match tmpfs.stat(rel) {
            Ok(s) => s,
            Err(crate::fs::tmpfs::TmpfsError::NotFound) => return NEG_ENOENT,
            Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => {
                const NEG_ENOTDIR: u64 = (-20_i64) as u64;
                return NEG_ENOTDIR;
            }
            Err(_) => return NEG_EINVAL,
        };
        let mode: u32 = if st.is_dir {
            0x4000 | 0o755 // S_IFDIR
        } else {
            0x8000 | 0o644 // S_IFREG
        };
        let mut stat = [0u8; 144];
        stat[24..28].copy_from_slice(&mode.to_ne_bytes());
        let size = st.size as u64;
        stat[48..56].copy_from_slice(&size.to_ne_bytes());
        let blksize: u64 = 4096;
        stat[56..64].copy_from_slice(&blksize.to_ne_bytes());
        drop(tmpfs);
        if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
            return NEG_EFAULT;
        }
        return 0;
    }

    let file_name = name.trim_start_matches('/');
    let size = match crate::fs::ramdisk::get_file(file_name) {
        Some(c) => c.len() as u64,
        None => return NEG_ENOENT,
    };

    let mut stat = [0u8; 144];
    let mode: u32 = 0x8000 | 0o644;
    stat[24..28].copy_from_slice(&mode.to_ne_bytes());
    stat[48..56].copy_from_slice(&size.to_ne_bytes());
    let blksize: u64 = 4096;
    stat[56..64].copy_from_slice(&blksize.to_ne_bytes());

    if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
        return NEG_EFAULT;
    }
    0
}

// ---------------------------------------------------------------------------
// Phase 13: mkdir(pathname) — syscall 83
// ---------------------------------------------------------------------------

fn sys_linux_mkdir(path_ptr: u64, _mode: u64) -> u64 {
    let mut buf = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let rel = match tmpfs_relative_path(name) {
        Some(r) => r,
        None => return NEG_EROFS, // can only mkdir in tmpfs
    };
    if rel.is_empty() {
        return NEG_EINVAL; // can't mkdir /tmp itself
    }

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    match tmpfs.mkdir(rel) {
        Ok(()) => {
            log::info!("[mkdir] {}", name);
            0
        }
        Err(crate::fs::tmpfs::TmpfsError::AlreadyExists) => NEG_EEXIST,
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => {
            const NEG_ENOTDIR: u64 = (-20_i64) as u64;
            NEG_ENOTDIR
        }
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: rmdir(pathname) — syscall 84
// ---------------------------------------------------------------------------

fn sys_linux_rmdir(path_ptr: u64) -> u64 {
    let mut buf = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let rel = match tmpfs_relative_path(name) {
        Some(r) => r,
        None => return NEG_EROFS,
    };
    if rel.is_empty() {
        return NEG_EINVAL; // can't rmdir /tmp itself
    }

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    match tmpfs.rmdir(rel) {
        Ok(()) => {
            log::info!("[rmdir] {}", name);
            0
        }
        Err(crate::fs::tmpfs::TmpfsError::NotEmpty) => NEG_ENOTEMPTY,
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(
            crate::fs::tmpfs::TmpfsError::WrongType | crate::fs::tmpfs::TmpfsError::NotADirectory,
        ) => {
            const NEG_ENOTDIR: u64 = (-20_i64) as u64;
            NEG_ENOTDIR
        }
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: unlink(pathname) — syscall 87
// ---------------------------------------------------------------------------

fn sys_linux_unlink(path_ptr: u64) -> u64 {
    let mut buf = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let rel = match tmpfs_relative_path(name) {
        Some(r) => r,
        None => return NEG_EROFS,
    };
    if rel.is_empty() {
        return NEG_EINVAL;
    }

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    match tmpfs.unlink(rel) {
        Ok(()) => {
            log::info!("[unlink] {}", name);
            0
        }
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(crate::fs::tmpfs::TmpfsError::WrongType) => {
            const NEG_EISDIR: u64 = (-21_i64) as u64;
            NEG_EISDIR
        }
        Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => {
            const NEG_ENOTDIR: u64 = (-20_i64) as u64;
            NEG_ENOTDIR
        }
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: rename(oldpath, newpath) — syscall 82
// ---------------------------------------------------------------------------

fn sys_linux_rename(old_ptr: u64, new_ptr: u64) -> u64 {
    let mut buf1 = [0u8; 512];
    let old_name = match read_user_cstr(old_ptr, &mut buf1) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };
    // Copy old_name to owned string since we need buf for new_name too.
    let mut old_owned = [0u8; 512];
    let old_len = old_name.len();
    old_owned[..old_len].copy_from_slice(old_name.as_bytes());

    let mut buf2 = [0u8; 512];
    let new_name = match read_user_cstr(new_ptr, &mut buf2) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let old_str = core::str::from_utf8(&old_owned[..old_len]).unwrap();
    let old_rel = match tmpfs_relative_path(old_str) {
        Some(r) => r,
        None => return NEG_EROFS,
    };
    let new_rel = match tmpfs_relative_path(new_name) {
        Some(r) => r,
        None => return NEG_EROFS,
    };

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    match tmpfs.rename(old_rel, new_rel) {
        Ok(()) => {
            log::info!("[rename] {} → {}", old_str, new_name);
            0
        }
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: truncate(path, length) — syscall 76
// ---------------------------------------------------------------------------

fn sys_linux_truncate(path_ptr: u64, length: u64) -> u64 {
    // Linux truncate() takes a signed off_t.
    let length_i64 = length as i64;
    if length_i64 < 0 {
        return NEG_EINVAL;
    }

    let mut buf = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let rel = match tmpfs_relative_path(name) {
        Some(r) => r,
        None => return NEG_EROFS,
    };
    if rel.is_empty() {
        const NEG_EISDIR: u64 = (-21_i64) as u64;
        return NEG_EISDIR;
    }

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    match tmpfs.truncate(rel, length_i64 as usize) {
        Ok(()) => 0,
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(crate::fs::tmpfs::TmpfsError::NoSpace) => NEG_ENOSPC,
        Err(crate::fs::tmpfs::TmpfsError::WrongType) => {
            const NEG_EISDIR: u64 = (-21_i64) as u64;
            NEG_EISDIR
        }
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: ftruncate(fd, length) — syscall 77
// ---------------------------------------------------------------------------

fn sys_linux_ftruncate(fd: u64, length: u64) -> u64 {
    // Linux ftruncate() takes a signed off_t.
    let length_i64 = length as i64;
    if length_i64 < 0 {
        return NEG_EINVAL;
    }

    let fd_idx = fd as usize;
    if !(3..MAX_FDS).contains(&fd_idx) {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    if !entry.writable {
        return NEG_EBADF;
    }

    match &entry.backend {
        FdBackend::Stdout
        | FdBackend::Stdin
        | FdBackend::PipeRead { .. }
        | FdBackend::PipeWrite { .. } => NEG_EINVAL,
        FdBackend::Ramdisk { .. } => NEG_EROFS,
        FdBackend::Tmpfs { path } => {
            let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
            match tmpfs.truncate(path, length_i64 as usize) {
                Ok(()) => 0,
                Err(crate::fs::tmpfs::TmpfsError::NoSpace) => NEG_ENOSPC,
                Err(_) => NEG_EINVAL,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 13: fsync(fd) — syscall 74 (no-op for tmpfs)
// ---------------------------------------------------------------------------

fn sys_linux_fsync(fd: u64) -> u64 {
    let fd_idx = fd as usize;
    if !(3..MAX_FDS).contains(&fd_idx) {
        return NEG_EBADF;
    }
    if current_fd_entry(fd_idx).is_none() {
        return NEG_EBADF;
    }
    0 // no-op: tmpfs has no persistence
}

// ---------------------------------------------------------------------------
// Phase 13: getdents64(fd, buf, count) — syscall 217
// ---------------------------------------------------------------------------

fn sys_linux_getdents64(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    // Not implemented — returns ENOSYS so callers know to fall back.
    // Directory listing via getdents64 is deferred to a future phase.
    let _ = (fd, buf_ptr, count);
    NEG_ENOSYS
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
            let vaddr = match x86_64::VirtAddr::try_new(addr) {
                Ok(v) => v,
                Err(_) => return NEG_EINVAL,
            };
            x86_64::registers::model_specific::FsBase::write(vaddr);
            0
        }
        _ => NEG_EINVAL,
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
