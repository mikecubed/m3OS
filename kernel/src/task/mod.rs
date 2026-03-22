//! Kernel task management: task structure, stacks, and context switching.
//!
//! # Context-switch contract
//!
//! [`switch_context`] saves and restores the six callee-saved registers
//! (`rbx`, `rbp`, `r12`–`r15`) plus `RFLAGS` (via `pushf`/`cli`/`popf`) and
//! `rip` (via `ret`).  The compiler already saves/restores caller-saved
//! registers at every call site, so saving them again in the switch stub would
//! be redundant.
//!
//! The stub issues `cli` after `pushf` to disable interrupts before switching
//! RSP, and `popf` atomically re-enables them when loading the new task's
//! saved RFLAGS.  This keeps the critical stack-swap window (between
//! `mov rsp, rsi` and `popf`) non-interruptible without requiring callers to
//! wrap the call in `without_interrupts`.
//!
//! A freshly-spawned task starts with `RFLAGS = 0x202` (IF=1), so the first
//! `popf` on dispatch restores interrupts automatically.
//!
//! Stack layout written by [`init_stack`] for a freshly-spawned task:
//!
//! ```text
//! high address ──────────────────────────────────
//!   [frame_start + 56]  rip  ← entry fn pointer
//!   [frame_start + 48]  rbx
//!   [frame_start + 40]  rbp
//!   [frame_start + 32]  r12
//!   [frame_start + 24]  r13
//!   [frame_start + 16]  r14
//!   [frame_start +  8]  r15
//!   [frame_start +  0]  RFLAGS = 0x202  ← saved_rsp points here
//! low address  ──────────────────────────────────
//! ```
//!
//! `saved_rsp` is `≡ 8 (mod 16)`.  After `popf` + six `pop`s + `ret`, RSP
//! advances 64 bytes, giving RSP `≡ 8 (mod 16)` at the entry function — the
//! value required by the x86-64 SysV ABI at a call boundary.

extern crate alloc;

use alloc::boxed::Box;

pub mod scheduler;

pub use scheduler::{run, signal_reschedule, spawn, spawn_idle, yield_now};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(crate) const KERNEL_STACK_SIZE: usize = 4096 * 4; // 16 KiB

// ---------------------------------------------------------------------------
// Task ID
// ---------------------------------------------------------------------------

/// Unique identifier for a kernel task.
///
/// Not yet consumed outside this module; the allow silences the dead-code
/// lint for the inner field so the identifier is available for future use
/// (e.g. IPC, logging, wait-queues).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(pub u64);

// ---------------------------------------------------------------------------
// Task state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
}

// ---------------------------------------------------------------------------
// Task structure
// ---------------------------------------------------------------------------

pub struct Task {
    /// Unique task identifier — not yet read outside this module.
    #[allow(dead_code)]
    pub id: TaskId,
    /// Human-readable name — not yet read outside this module.
    #[allow(dead_code)]
    pub name: &'static str,
    pub state: TaskState,
    /// RSP saved by `switch_context` when this task is not running.
    pub saved_rsp: u64,
    /// Owns the allocated kernel stack — dropped when the `Task` is dropped.
    _stack: Box<[u8]>,
}

impl Task {
    /// Allocate a new task with its own kernel stack, initialized to enter
    /// `entry` when first scheduled.
    pub fn new(entry: fn() -> !, name: &'static str) -> Self {
        static NEXT_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
        let id = TaskId(NEXT_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed));

        let mut stack = alloc::vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
        let saved_rsp = init_stack(&mut stack, entry);

        Task {
            id,
            name,
            state: TaskState::Ready,
            saved_rsp,
            _stack: stack,
        }
    }
}

// ---------------------------------------------------------------------------
// Stack initialization
// ---------------------------------------------------------------------------

/// Write the initial register frame at the top of `stack` so that
/// `switch_context` can resume execution at `entry`.
///
/// Returns the value that should be stored in `Task::saved_rsp`.
pub(crate) fn init_stack(stack: &mut [u8], entry: fn() -> !) -> u64 {
    let raw_top = stack.as_ptr() as usize + stack.len();
    // Align the rip slot to a 16-byte boundary.  Subtract 8 first so that
    // when raw_top is already 16-byte aligned we do not write past the end
    // of the allocation.
    // frame_start = rip_addr - 56. Because rip_addr ≡ 0 (mod 16),
    // frame_start ≡ -56 ≡ 8 (mod 16).  After `popf` + 6 `pop`s + `ret`,
    // RSP = frame_start + 64 ≡ 8 + 64 ≡ 8 (mod 16), satisfying the SysV
    // ABI call-entry requirement.
    let rip_addr = (raw_top - 8) & !0xf;
    let frame_start = rip_addr - 7 * 8; // RFLAGS + 6 callee-saved regs below rip
    let frame = frame_start as *mut u64;
    // Safety: frame_start is inside the allocated stack slice (raw_top is its
    // past-the-end pointer and we subtract at least 64 bytes to stay inside).
    // The pointer is 8-byte aligned because frame_start ≡ 8 (mod 16).
    unsafe {
        frame.write(0x202); // RFLAGS: IF=1 (bit 9) + reserved bit 1 always set
        frame.add(1).write(0); // r15
        frame.add(2).write(0); // r14
        frame.add(3).write(0); // r13
        frame.add(4).write(0); // r12
        frame.add(5).write(0); // rbp
        frame.add(6).write(0); // rbx
        frame.add(7).write(entry as usize as u64); // rip
    }
    frame_start as u64
}

// ---------------------------------------------------------------------------
// Context switch (assembly stub)
// ---------------------------------------------------------------------------

unsafe extern "C" {
    /// Switch from the current execution context to another.
    ///
    /// Saves callee-saved registers and RFLAGS onto the current stack, stores
    /// RSP at `*save_rsp`, loads `load_rsp` as the new stack, restores RFLAGS
    /// and the callee-saved registers, then returns to the new task's `rip`.
    ///
    /// Interrupt masking for the critical stack-swap window is handled
    /// internally: `pushf` captures RFLAGS (including IF), `cli` disables
    /// interrupts before changing RSP, and `popf` atomically restores IF from
    /// the new task's saved RFLAGS.  Callers do not need an external
    /// `without_interrupts` wrapper.
    ///
    /// # Safety
    ///
    /// * `save_rsp` must be a valid, writable 8-byte-aligned pointer inside a
    ///   kernel stack or the `SCHEDULER_RSP` static.
    /// * `load_rsp` must be a value previously written by `switch_context` (or
    ///   produced by `init_stack`), pointing to a valid register frame on a
    ///   live kernel stack.
    /// * Must not be called while holding any spin lock that the resumed task
    ///   may also try to acquire (would deadlock).
    pub(crate) fn switch_context(save_rsp: *mut u64, load_rsp: u64);
}

core::arch::global_asm!(
    ".global switch_context",
    "switch_context:",
    "  push rbx",
    "  push rbp",
    "  push r12",
    "  push r13",
    "  push r14",
    "  push r15",
    "  pushf",           // save RFLAGS (includes IF bit)
    "  cli",             // disable interrupts to protect the stack-swap window
    "  mov  [rdi], rsp", // save current RSP into *save_rsp
    "  mov  rsp, rsi",   // load new task's RSP (IF=0 while RSP is mid-swap)
    "  popf",            // restore RFLAGS → atomically re-enables IF if it was set
    "  pop  r15",
    "  pop  r14",
    "  pop  r13",
    "  pop  r12",
    "  pop  rbp",
    "  pop  rbx",
    "  ret", // pop rip from new stack → jump to resumed task
);
