//! Kernel task management: task structure, stacks, and context switching.
//!
//! # Context-switch contract
//!
//! [`switch_context`] saves and restores only the six callee-saved registers
//! (`rbx`, `rbp`, `r12`–`r15`) plus `rip` (via `ret`).  The compiler already
//! saves/restores caller-saved registers at every call site, so saving them
//! again in the switch stub would be redundant.
//!
//! Stack layout written by [`init_stack`] for a freshly-spawned task:
//!
//! ```text
//! high address ──────────────────────────────────
//!   [frame_start + 48]  rip  ← entry fn pointer
//!   [frame_start + 40]  rbx
//!   [frame_start + 32]  rbp
//!   [frame_start + 24]  r12
//!   [frame_start + 16]  r13
//!   [frame_start +  8]  r14
//!   [frame_start +  0]  r15   ← saved_rsp points here
//! low address  ──────────────────────────────────
//! ```
//!
//! `saved_rsp` is 16-byte aligned.  After `ret` pops `rip` the entry function
//! sees `RSP ≡ 8 (mod 16)`, matching the x86-64 SysV ABI.

#![allow(dead_code)]

extern crate alloc;

use alloc::boxed::Box;

pub mod scheduler;

pub use scheduler::{run, signal_reschedule, spawn, yield_now};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(crate) const KERNEL_STACK_SIZE: usize = 4096 * 4; // 16 KiB

// ---------------------------------------------------------------------------
// Task ID
// ---------------------------------------------------------------------------

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
    pub id: TaskId,
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
    // Align the rip slot to a 16-byte boundary.
    // frame_start = rip_addr - 48. Because 48 % 16 == 0, frame_start shares
    // the same alignment as rip_addr (16-byte aligned).
    // After `ret` pops rip, RSP = frame_start + 56; (frame_start + 56) % 16
    // = (0 + 56) % 16 = 8, satisfying the SysV ABI call-entry requirement.
    let rip_addr = raw_top & !0xf;
    let frame_start = rip_addr - 6 * 8; // 6 callee-saved regs below rip
    let frame = frame_start as *mut u64;
    // Safety: frame_start is inside the allocated stack slice (raw_top is its
    // past-the-end pointer and we subtract at least 56 bytes to stay inside).
    // The pointer is 8-byte aligned because frame_start is 16-byte aligned.
    unsafe {
        frame.write(0); // r15
        frame.add(1).write(0); // r14
        frame.add(2).write(0); // r13
        frame.add(3).write(0); // r12
        frame.add(4).write(0); // rbp
        frame.add(5).write(0); // rbx
        frame.add(6).write(entry as usize as u64); // rip
    }
    frame_start as u64
}

// ---------------------------------------------------------------------------
// Context switch (assembly stub)
// ---------------------------------------------------------------------------

unsafe extern "C" {
    /// Switch from the current execution context to another.
    ///
    /// # Safety
    ///
    /// * `save_rsp` must be a valid, writable 8-byte-aligned pointer inside a
    ///   kernel stack or the `SCHEDULER_RSP` static.
    /// * `load_rsp` must be a value previously written by `switch_context` (or
    ///   produced by `init_stack`), pointing to a valid callee-saved register
    ///   frame on a live kernel stack.
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
    "  mov  [rdi], rsp",
    "  mov  rsp, rsi",
    "  pop  r15",
    "  pop  r14",
    "  pop  r13",
    "  pop  r12",
    "  pop  rbp",
    "  pop  rbx",
    "  ret",
);
