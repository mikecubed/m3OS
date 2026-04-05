//! Crash diagnostics — enriched panic handler output.
//!
//! Provides [`dump_crash_context`] which prints CPU registers, current task
//! info, and per-core scheduler state to the serial port using the
//! deadlock-safe `_panic_print` path. All lock acquisitions use `try_lock()`
//! to avoid deadlocking when the panic occurs while a lock is held.

use core::sync::atomic::Ordering;

use crate::serial::_panic_print;
use crate::smp::{self, MAX_CORES};

// ---------------------------------------------------------------------------
// Register snapshot
// ---------------------------------------------------------------------------

/// Captured CPU register state at panic time.
struct RegisterSnapshot {
    rax: u64,
    rbx: u64,
    rcx: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
    rbp: u64,
    rsp: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rflags: u64,
    cr2: u64,
    cr3: u64,
}

/// Read all general-purpose registers, RFLAGS, CR2, and CR3.
///
/// Note: RIP cannot be directly captured from inline assembly — the values
/// here reflect the state at the point of capture inside this function, not
/// at the original panic site. The panic location printed by the caller
/// (file:line) is the best proxy for the faulting instruction.
fn capture_registers() -> RegisterSnapshot {
    let rax: u64;
    let mut rbx: u64 = 0;
    let rcx: u64;
    let rdx: u64;
    let rsi: u64;
    let rdi: u64;
    let mut rbp: u64 = 0;
    let rsp: u64;
    let r8: u64;
    let r9: u64;
    let r10: u64;
    let r11: u64;
    let r12: u64;
    let r13: u64;
    let r14: u64;
    let r15: u64;
    let rflags: u64;

    // Capture all GPRs atomically.  rbx and rbp are reserved by LLVM and
    // cannot appear as lateout operands, so we store them to memory inside
    // the asm block.  All other GPRs use lateout constraints in a single
    // block so LLVM cannot clobber uncaptured registers between statements.
    unsafe {
        core::arch::asm!(
            "mov [{0}], rbx",
            "mov [{1}], rbp",
            in(reg) &mut rbx as *mut u64,
            in(reg) &mut rbp as *mut u64,
            lateout("rax") rax,
            lateout("rcx") rcx,
            lateout("rdx") rdx,
            lateout("rsi") rsi,
            lateout("rdi") rdi,
            lateout("r8") r8,
            lateout("r9") r9,
            lateout("r10") r10,
            lateout("r11") r11,
            lateout("r12") r12,
            lateout("r13") r13,
            lateout("r14") r14,
            lateout("r15") r15,
            options(nostack, preserves_flags),
        );
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack));
        core::arch::asm!("pushfq; pop {}", out(reg) rflags);
    }

    let cr2 = x86_64::registers::control::Cr2::read_raw();
    let cr3 = {
        let (frame, _flags) = x86_64::registers::control::Cr3::read_raw();
        frame.start_address().as_u64()
    };

    RegisterSnapshot {
        rax,
        rbx,
        rcx,
        rdx,
        rsi,
        rdi,
        rbp,
        rsp,
        r8,
        r9,
        r10,
        r11,
        r12,
        r13,
        r14,
        r15,
        rflags,
        cr2,
        cr3,
    }
}

fn dump_registers(regs: &RegisterSnapshot) {
    _panic_print(format_args!("--- CPU Registers ---\n"));
    _panic_print(format_args!(
        "RAX=0x{:016x}  RBX=0x{:016x}\n",
        regs.rax, regs.rbx
    ));
    _panic_print(format_args!(
        "RCX=0x{:016x}  RDX=0x{:016x}\n",
        regs.rcx, regs.rdx
    ));
    _panic_print(format_args!(
        "RSI=0x{:016x}  RDI=0x{:016x}\n",
        regs.rsi, regs.rdi
    ));
    _panic_print(format_args!(
        "RBP=0x{:016x}  RSP=0x{:016x}\n",
        regs.rbp, regs.rsp
    ));
    _panic_print(format_args!(
        "R8 =0x{:016x}  R9 =0x{:016x}\n",
        regs.r8, regs.r9
    ));
    _panic_print(format_args!(
        "R10=0x{:016x}  R11=0x{:016x}\n",
        regs.r10, regs.r11
    ));
    _panic_print(format_args!(
        "R12=0x{:016x}  R13=0x{:016x}\n",
        regs.r12, regs.r13
    ));
    _panic_print(format_args!(
        "R14=0x{:016x}  R15=0x{:016x}\n",
        regs.r14, regs.r15
    ));
    _panic_print(format_args!("RFLAGS=0x{:016x}\n", regs.rflags));
    _panic_print(format_args!(
        "CR2=0x{:016x}  CR3=0x{:016x}\n",
        regs.cr2, regs.cr3
    ));
    // Note: RIP is not directly capturable via inline asm — use panic location instead.
}

// ---------------------------------------------------------------------------
// Current task info
// ---------------------------------------------------------------------------

fn dump_current_task() {
    _panic_print(format_args!("--- Current Task ---\n"));

    if !smp::is_per_core_ready() {
        _panic_print(format_args!("  (per-core data not initialized)\n"));
        return;
    }

    let data = smp::per_core();
    let idx = data.current_task_idx.load(Ordering::Relaxed);

    if idx < 0 {
        _panic_print(format_args!(
            "  no active task (scheduler loop) on core {}\n",
            data.core_id
        ));
        return;
    }

    _panic_print(format_args!(
        "  task_idx={} on core {}\n",
        idx, data.core_id
    ));

    match crate::task::try_lock_scheduler() {
        Some(sched) => {
            if let Some(task) = sched.get_task(idx as usize) {
                _panic_print(format_args!(
                    "  TaskId={} state={:?} saved_rsp=0x{:016x}\n",
                    task.id.0, task.state, task.saved_rsp
                ));
                _panic_print(format_args!(
                    "  pid={} assigned_core={} priority={}\n",
                    task.pid, task.assigned_core, task.priority
                ));
            } else {
                _panic_print(format_args!("  task index {} out of range\n", idx));
            }
        }
        None => {
            _panic_print(format_args!(
                "  scheduler lock held -- skipping task dump\n"
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Per-core state
// ---------------------------------------------------------------------------

fn dump_per_core_state() {
    _panic_print(format_args!("--- Per-Core State ---\n"));

    if !smp::is_per_core_ready() {
        _panic_print(format_args!("  (per-core data not initialized)\n"));
        return;
    }

    let faulting_core = smp::per_core().core_id;

    for i in 0..MAX_CORES as u8 {
        let Some(data) = smp::get_core_data(i) else {
            continue;
        };
        if !data.is_online.load(Ordering::Relaxed) {
            continue;
        }

        let marker = if i == faulting_core { ">>>" } else { "   " };
        let task_idx = data.current_task_idx.load(Ordering::Relaxed);
        let resched = data.reschedule.load(Ordering::Relaxed);

        let queue_info: &str;
        let mut queue_len_buf = [0u8; 20];
        let queue_str;

        match data.run_queue.try_lock() {
            Some(q) => {
                queue_str = fmt_usize(q.len(), &mut queue_len_buf);
                queue_info = queue_str;
            }
            None => {
                queue_info = "locked";
            }
        }

        _panic_print(format_args!(
            "{} core {} | online=true task_idx={} resched={} run_queue={}\n",
            marker, i, task_idx, resched, queue_info
        ));
    }
}

/// Format a usize into a decimal string in the provided buffer.
/// Returns a &str slice of the formatted number.
fn fmt_usize(n: usize, buf: &mut [u8; 20]) -> &str {
    if n == 0 {
        return "0";
    }
    let mut val = n;
    let mut pos = buf.len();
    while val > 0 {
        pos -= 1;
        buf[pos] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    // Safety: buf[pos..] contains only ASCII digits.
    unsafe { core::str::from_utf8_unchecked(&buf[pos..]) }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Dump full crash diagnostics to the serial port.
///
/// Called from the panic handler after printing location and message.
/// Uses only `_panic_print` (deadlock-safe) and `try_lock()` to avoid
/// secondary deadlocks.
pub fn dump_crash_context() {
    _panic_print(format_args!("=== CRASH DIAGNOSTICS ===\n"));

    let regs = capture_registers();
    dump_registers(&regs);
    dump_current_task();
    dump_per_core_state();

    _panic_print(format_args!("=== END CRASH DIAGNOSTICS ===\n"));
}
