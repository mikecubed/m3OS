//! Kernel task management: task structure, stacks, and context switching.
//!
//! Phase 6 activates the scheduler for multi-task IPC demos.  Each task
//! carries its own [`CapabilityTable`] and an optional pending [`Message`]
//! (written by IPC `deliver_message` before waking the task).
#![allow(dead_code)]
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

use crate::ipc::{CapabilityTable, Message};

pub use kernel_core::types::TaskId;

pub mod scheduler;

#[allow(unused_imports)]
pub use scheduler::{
    block_current_on_notif, block_current_on_recv, block_current_on_reply, block_current_on_send,
    current_task_id, deliver_message, insert_cap, mark_current_dead, maybe_load_balance,
    remove_task_cap, run, server_endpoint, set_server_endpoint, signal_reschedule, spawn,
    spawn_idle, spawn_idle_for_core, sys_nice, sys_sched_getaffinity, sys_sched_setaffinity,
    take_message, task_cap, wake_task, yield_now,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(crate) const KERNEL_STACK_SIZE: usize = 4096 * 4; // 16 KiB

// ---------------------------------------------------------------------------
// Task ID
// ---------------------------------------------------------------------------

// TaskId is re-exported from kernel_core::types above.

// ---------------------------------------------------------------------------
// Task state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Task is runnable and will be dispatched by the scheduler.
    Ready,
    /// Task is currently executing on the CPU.
    Running,
    /// Task is blocked waiting to receive a message on an endpoint.
    BlockedOnRecv,
    /// Task is blocked waiting for its send to be picked up.
    BlockedOnSend,
    /// Task has called an endpoint and is waiting for a reply.
    BlockedOnReply,
    /// Task is blocked waiting for a notification bit to be set.
    BlockedOnNotif,
    /// Task has permanently exited; the scheduler will remove it on next pass.
    Dead,
}

// ---------------------------------------------------------------------------
// Task structure
// ---------------------------------------------------------------------------

pub struct Task {
    /// Unique task identifier.
    pub id: TaskId,
    /// Human-readable name.
    #[allow(dead_code)]
    pub name: &'static str,
    pub state: TaskState,
    /// RSP saved by `switch_context` when this task is not running.
    pub saved_rsp: u64,
    /// Per-task IPC capability table.
    pub caps: CapabilityTable,
    /// Pending message delivered by `deliver_message` before waking this task.
    ///
    /// `None` when the task has not yet been sent a message.  Set by the
    /// sender/IPC core; consumed by `take_message` after the task wakes.
    pub pending_msg: Option<Message>,
    /// Endpoint this task is the "server" of (used by `reply_recv` to find
    /// the endpoint to block on after replying).
    pub server_endpoint: Option<crate::ipc::EndpointId>,
    /// Core this task is assigned to for per-CPU run queue dispatch (Phase 35).
    pub assigned_core: u8,
    /// Task priority (Phase 35): 0-9 = real-time, 10-29 = normal, 30 = idle.
    /// Lower numeric value = higher priority.
    pub priority: u8,
    /// CPU affinity mask (Phase 35): one bit per core (max 64 cores).
    /// Default: all bits set (can run on any core).
    pub affinity_mask: u64,
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
            caps: CapabilityTable::new(),
            pending_msg: None,
            server_endpoint: None,
            assigned_core: 0,
            priority: 20,            // Normal priority (middle of 10-29 range)
            affinity_mask: u64::MAX, // Can run on any core
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
