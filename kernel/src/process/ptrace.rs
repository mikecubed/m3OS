//! `ptrace`-backed userspace debugging (Phase 111 Track D).
//!
//! The kernel side of debugging a **ring-3** process the OS runs: a tracer
//! (e.g. `m3gdbserver`) stops a traced tracee on a debug trap, inspects and
//! mutates its registers/memory, and resumes it — the Linux `ptrace` model,
//! restricted to the practical breakpoint / single-step / peek / poke subset.
//!
//! ## How a stop works (the trampoline model)
//!
//! A ring-3 `int3` or `RFLAGS.TF` single-step raises `#BP`/`#DB`, whose
//! naked-entry handlers (Track C) hand a live [`DebugTrapFrame`] to the
//! `from_user` branch of [`crate::arch::x86_64::debug`]. For a **traced**
//! process [`on_user_breakpoint`]/[`on_user_debug`] snapshot the tracee's
//! registers into its [`Ptrace`] state and **redirect the iretq** to
//! [`crate::arch::x86_64::syscall::ptrace_stop_trampoline`] — exactly the
//! `fault_kill_trampoline` technique, so the stop-and-wait runs in a normal,
//! blockable ring-0 continuation instead of inside the exception handler. The
//! trampoline notifies the tracer (via the parent-`wait` path), parks the
//! tracee until the tracer issues `CONT`/`SINGLESTEP`, then re-enters userspace
//! with the (possibly `SETREGS`-modified) registers.
//!
//! Everything here is compiled only under the `ptrace` cargo feature — the
//! syscall is arbitrary cross-process register/memory access and is OFF in
//! production, the same posture as `kgdb`/`panic-test`/`trace`.

use x86_64::structures::paging::{PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use crate::arch::x86_64::debug::DebugTrapFrame;
use crate::signal::SavedUserRegs;

use super::{PROCESS_TABLE, Pid, ProcessState, current_pid};

/// `SIGTRAP` — generated on a ring-3 `int3` and on single-step completion,
/// reported to the tracer as the ptrace-stop signal. (Track D adds this; it was
/// defined-but-unused since Phase 19.)
pub const SIGTRAP: u32 = 5;

/// Tracer's resume command, written by `sys_ptrace` and consumed by the stopped
/// tracee in the trampoline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PtraceResume {
    /// No command yet — the tracee stays parked.
    #[default]
    None,
    /// Resume execution (`PTRACE_CONT`).
    Cont,
    /// Execute one instruction then trap again (`PTRACE_SINGLESTEP`).
    Step,
    /// Stop tracing and resume (`PTRACE_DETACH`).
    Detach,
    /// Terminate the tracee (`PTRACE_KILL`).
    Kill,
}

/// Per-process ptrace state. Bundled into one field so the many `Process`
/// constructors add a single `ptrace: Ptrace::default()` line.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ptrace {
    /// This process is being traced by `tracer_pid`.
    pub traced: bool,
    /// PID of the tracer (0 = none).
    pub tracer_pid: Pid,
    /// True while the tracee is parked in a ptrace-stop awaiting the tracer.
    pub stopped: bool,
    /// True once the tracer's `waitpid` has reported the current stop (one-shot).
    pub stop_reported: bool,
    /// The signal that caused the current stop (SIGTRAP for a debug trap).
    pub stop_sig: u32,
    /// Resume command from the tracer, consumed by the parked tracee.
    pub resume: PtraceResume,
    /// Register snapshot captured at the stop. `GETREGS` reads it, `SETREGS`
    /// writes it, and the resume path restores it to userspace.
    pub regs: SavedUserRegs,
}

// ---------------------------------------------------------------------------
// Register marshalling between the DebugTrapFrame and SavedUserRegs
// ---------------------------------------------------------------------------

/// Snapshot a `#BP`/`#DB` [`DebugTrapFrame`] into a [`SavedUserRegs`] (the
/// GETREGS/resume shape). `gprs` order is
/// `[rax,rbx,rcx,rdx,rsi,rdi,rbp,r8,r9,r10,r11,r12,r13,r14,r15]` (no rsp — the
/// CPU frame carries it).
fn regs_from_frame(frame: &DebugTrapFrame) -> SavedUserRegs {
    SavedUserRegs {
        rax: frame.gprs[0],
        rbx: frame.gprs[1],
        rcx: frame.gprs[2],
        rdx: frame.gprs[3],
        rsi: frame.gprs[4],
        rdi: frame.gprs[5],
        rbp: frame.gprs[6],
        rsp: frame.rsp,
        r8: frame.gprs[7],
        r9: frame.gprs[8],
        r10: frame.gprs[9],
        r11: frame.gprs[10],
        r12: frame.gprs[11],
        r13: frame.gprs[12],
        r14: frame.gprs[13],
        r15: frame.gprs[14],
        rip: frame.rip,
        rflags: frame.rflags,
    }
}

// ---------------------------------------------------------------------------
// Trap-path consumers (called from arch::x86_64::debug on a from_user trap)
// ---------------------------------------------------------------------------

/// True if `pid` is currently traced.
pub fn is_traced(pid: Pid) -> bool {
    PROCESS_TABLE
        .lock()
        .find(pid)
        .map(|p| p.ptrace.traced)
        .unwrap_or(false)
}

/// Ring-3 `#BP` consumer. Returns `true` (event consumed) if the current
/// process is traced — it snapshots the trap and redirects into the stop
/// trampoline. Returns `false` for an untraced process (the caller resumes past
/// the `int3`).
///
/// RIP is reported **after** the `int3` byte (Linux `ptrace` semantics): the
/// kernel cannot distinguish a tracer-planted breakpoint from a compiled-in
/// `int3`, so it leaves RIP past the byte and the tracer (`m3gdbserver`) rewinds
/// RIP itself (via `SETREGS`) for the addresses it planted. Rewinding here would
/// make a compiled-in `int3` re-execute forever on `CONT`.
pub fn on_user_breakpoint(bp_addr: u64, frame: &mut DebugTrapFrame) -> bool {
    let _ = bp_addr;
    let pid = current_pid();
    if !is_traced(pid) {
        return false;
    }
    arm_stop(pid, frame, SIGTRAP);
    true
}

/// Ring-3 `#DB` (single-step / hardware breakpoint) consumer. Returns `true` if
/// the current process is traced (snapshot + redirect into the stop
/// trampoline); `false` otherwise so the default clears any stray `TF`.
pub fn on_user_debug(frame: &mut DebugTrapFrame) -> bool {
    let pid = current_pid();
    if !is_traced(pid) {
        return false;
    }
    arm_stop(pid, frame, SIGTRAP);
    true
}

/// Snapshot the tracee's registers into its [`Ptrace`] state and rewrite the
/// live trap frame so the naked-entry stub's `iretq` lands in
/// [`crate::arch::x86_64::syscall::ptrace_stop_trampoline`] on a clean ring-0
/// stack — the `fault_kill_trampoline` redirect technique. The stop-and-wait
/// then runs in a blockable continuation instead of inside the trap handler.
fn arm_stop(pid: Pid, frame: &mut DebugTrapFrame, sig: u32) {
    let regs = regs_from_frame(frame);
    {
        let mut table = PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.ptrace.regs = regs;
            proc.ptrace.stop_sig = sig;
            proc.ptrace.resume = PtraceResume::None;
        }
    }

    // The trampoline runs on the current kernel stack (there is ample room
    // below the current RSP on the 64 KiB per-task kernel stack, and nothing
    // between here and the `iretq` writes below it). Same rationale as the
    // page-fault → fault_kill_trampoline redirect.
    let kernel_rsp: u64;
    // SAFETY: reading the current stack pointer.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) kernel_rsp) };

    use crate::arch::x86_64::gdt;
    frame.rip = crate::arch::x86_64::syscall::ptrace_stop_trampoline as *const () as u64;
    frame.cs = u64::from(gdt::kernel_code_selector().0);
    frame.rflags &= !(1u64 << 9); // clear IF; the trampoline re-enables
    frame.rsp = kernel_rsp;
    frame.ss = u64::from(gdt::kernel_data_selector().0);
}

// ---------------------------------------------------------------------------
// Trampoline body (called from ptrace_stop_trampoline in blockable context)
// ---------------------------------------------------------------------------

/// Outcome the stopped tracee resumes with.
pub enum ResumeAction {
    /// Resume to userspace with these registers (TF already applied for a step).
    Resume(SavedUserRegs),
    /// Terminate the tracee.
    Kill,
}

/// Mark the current tracee ptrace-stopped, notify its tracer, and park until the
/// tracer issues a resume command — then return the resume action. Runs in the
/// trampoline's normal ring-0 task context (blocking is safe here).
pub fn enter_stop_and_wait() -> ResumeAction {
    let pid = current_pid();

    // Publish the stop and notify the tracer (its `waitpid` wakes; the parent
    // path is correct because the fork+TRACEME tracer is the parent).
    let tracer = {
        let mut table = PROCESS_TABLE.lock();
        match table.find_mut(pid) {
            Some(proc) => {
                proc.state = ProcessState::Stopped;
                proc.ptrace.stopped = true;
                proc.ptrace.stop_reported = false;
                proc.ptrace.resume = PtraceResume::None;
                proc.ptrace.tracer_pid
            }
            None => return ResumeAction::Kill,
        }
    };
    if tracer != 0 {
        super::wake_child_waiters(tracer);
        super::send_signal(tracer, super::SIGCHLD);
    }

    // Park until the tracer sets a resume command. Busy-yield mirrors the
    // existing SignalDisposition::Stop path; the tracee is advisory-Stopped but
    // still schedulable, so a plain `yield_now` loop is correct.
    loop {
        let (resume, regs) = {
            let table = PROCESS_TABLE.lock();
            match table.find(pid) {
                Some(proc) => (proc.ptrace.resume, proc.ptrace.regs),
                None => return ResumeAction::Kill,
            }
        };
        match resume {
            PtraceResume::None => {
                crate::task::yield_now();
                continue;
            }
            PtraceResume::Kill => return ResumeAction::Kill,
            PtraceResume::Step => {
                let mut r = regs;
                r.rflags |= 1u64 << 8; // set TF so exactly one instruction steps
                clear_stop(pid, ProcessState::Running);
                return ResumeAction::Resume(r);
            }
            PtraceResume::Cont | PtraceResume::Detach => {
                let mut r = regs;
                r.rflags &= !(1u64 << 8); // clear TF
                if resume == PtraceResume::Detach {
                    detach_now(pid);
                }
                clear_stop(pid, ProcessState::Running);
                return ResumeAction::Resume(r);
            }
        }
    }
}

fn clear_stop(pid: Pid, new_state: ProcessState) {
    let mut table = PROCESS_TABLE.lock();
    if let Some(proc) = table.find_mut(pid) {
        proc.ptrace.stopped = false;
        proc.state = new_state;
    }
}

fn detach_now(pid: Pid) {
    let mut table = PROCESS_TABLE.lock();
    if let Some(proc) = table.find_mut(pid) {
        proc.ptrace.traced = false;
        proc.ptrace.tracer_pid = 0;
    }
}

// ---------------------------------------------------------------------------
// sys_ptrace request handlers (called from the syscall dispatch)
// ---------------------------------------------------------------------------

/// `PTRACE_TRACEME`: the calling process asks to be traced by its parent.
pub fn traceme() -> i64 {
    let pid = current_pid();
    let mut table = PROCESS_TABLE.lock();
    match table.find_mut(pid) {
        Some(proc) => {
            proc.ptrace.traced = true;
            proc.ptrace.tracer_pid = proc.ppid;
            0
        }
        None => -3, // ESRCH
    }
}

/// True if `tracee` is currently stopped and traced by `tracer`.
fn tracer_owns_stopped(tracer: Pid, tracee: Pid) -> bool {
    PROCESS_TABLE
        .lock()
        .find(tracee)
        .map(|p| p.ptrace.traced && p.ptrace.tracer_pid == tracer && p.ptrace.stopped)
        .unwrap_or(false)
}

/// `PTRACE_CONT`/`SINGLESTEP`/`DETACH`/`KILL`: set the resume command on a
/// stopped tracee. The parked tracee's trampoline loop consumes it.
pub fn resume(tracer: Pid, tracee: Pid, cmd: PtraceResume) -> i64 {
    if !tracer_owns_stopped(tracer, tracee) {
        return -3; // ESRCH — not our stopped tracee
    }
    let mut table = PROCESS_TABLE.lock();
    if let Some(proc) = table.find_mut(tracee) {
        proc.ptrace.resume = cmd;
        proc.ptrace.stop_reported = true;
        0
    } else {
        -3
    }
}

/// `PTRACE_GETREGS`: copy the tracee's register snapshot out.
pub fn getregs(tracer: Pid, tracee: Pid) -> Option<SavedUserRegs> {
    let table = PROCESS_TABLE.lock();
    let proc = table.find(tracee)?;
    if !(proc.ptrace.traced && proc.ptrace.tracer_pid == tracer && proc.ptrace.stopped) {
        return None;
    }
    Some(proc.ptrace.regs)
}

/// `PTRACE_SETREGS`: overwrite the tracee's register snapshot (applied on
/// resume).
pub fn setregs(tracer: Pid, tracee: Pid, regs: SavedUserRegs) -> i64 {
    let mut table = PROCESS_TABLE.lock();
    match table.find_mut(tracee) {
        Some(proc)
            if proc.ptrace.traced && proc.ptrace.tracer_pid == tracer && proc.ptrace.stopped =>
        {
            proc.ptrace.regs = regs;
            0
        }
        _ => -3,
    }
}

// ---------------------------------------------------------------------------
// Cross-address-space memory access (PEEKTEXT / POKETEXT)
// ---------------------------------------------------------------------------

/// Resolve `pid`'s page-table root (CR3 / PML4 physical base).
fn pml4_phys_for_pid(pid: Pid) -> Option<u64> {
    PROCESS_TABLE
        .lock()
        .find(pid)
        .and_then(|p| p.addr_space.as_ref().map(|a| a.pml4_phys().as_u64()))
}

/// Translate a tracee virtual address to a kernel physmap pointer via the
/// tracee's own page tables (without switching CR3). Returns `None` if unmapped.
///
/// # Safety
/// The returned pointer aliases the physical frame through the physmap; the
/// caller must only touch `len` bytes within the same page.
unsafe fn tracee_va_to_kptr(pid: Pid, vaddr: u64) -> Option<*mut u8> {
    use x86_64::structures::paging::mapper::{Translate, TranslateResult};
    let cr3 = pml4_phys_for_pid(pid)?;
    let frame = PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(cr3)).ok()?;
    // SAFETY: `frame` is the tracee's live PML4; `mapper_for_frame` does not
    // switch CR3, it only reads the table through the physmap.
    let mapper = unsafe { crate::mm::mapper_for_frame(frame) };
    match mapper.translate(VirtAddr::new(vaddr)) {
        TranslateResult::Mapped { frame, offset, .. } => {
            let phys = frame.start_address().as_u64() + offset;
            Some((crate::mm::phys_offset() + phys) as *mut u8)
        }
        _ => None,
    }
}

/// `PTRACE_PEEKTEXT`/`PEEKDATA`: read one word (8 bytes) from the tracee's
/// address space. Returns the word, or `None` if unmapped / not our tracee.
pub fn peek(tracer: Pid, tracee: Pid, addr: u64) -> Option<u64> {
    if !is_our_tracee(tracer, tracee) {
        return None;
    }
    let mut bytes = [0u8; 8];
    for (i, b) in bytes.iter_mut().enumerate() {
        // SAFETY: single-byte read within the translated page; ptrace stops the
        // machine's tracee, so no concurrent unmap races the read.
        let p = unsafe { tracee_va_to_kptr(tracee, addr + i as u64)? };
        *b = unsafe { core::ptr::read_volatile(p) };
    }
    Some(u64::from_le_bytes(bytes))
}

/// `PTRACE_POKETEXT`/`POKEDATA`: write one word (8 bytes) into the tracee's
/// address space via the physmap alias — this bypasses the tracee's page-table
/// write protection, so a debugger can plant an `int3` in read-only code text.
pub fn poke(tracer: Pid, tracee: Pid, addr: u64, data: u64) -> i64 {
    if !is_our_tracee(tracer, tracee) {
        return -3; // ESRCH
    }
    let bytes = data.to_le_bytes();
    for (i, b) in bytes.iter().enumerate() {
        // SAFETY: single-byte write within the translated page; the tracee is
        // stopped. Writing through the physmap ignores the (possibly RO) user
        // PTE, matching ptrace POKETEXT semantics.
        match unsafe { tracee_va_to_kptr(tracee, addr + i as u64) } {
            Some(p) => unsafe { core::ptr::write_volatile(p, *b) },
            None => return -14, // EFAULT
        }
    }
    // Serialize so the tracee's CPU refetches modified instruction bytes.
    // SAFETY: `mfence` is always valid.
    unsafe { core::arch::asm!("mfence", options(nomem, nostack, preserves_flags)) };
    0
}

fn is_our_tracee(tracer: Pid, tracee: Pid) -> bool {
    PROCESS_TABLE
        .lock()
        .find(tracee)
        .map(|p| p.ptrace.traced && p.ptrace.tracer_pid == tracer)
        .unwrap_or(false)
}
