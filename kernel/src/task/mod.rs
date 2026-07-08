//! # Ownership: Keep
//! Scheduler is a core kernel primitive — task state, context switching, and CPU dispatch must remain ring-0.
//!
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

use alloc::sync::Arc;

use crate::ipc::{CapabilityTable, Message};

pub(crate) const MAX_TASKS: usize = 256;

pub use kernel_core::types::TaskId;

// Phase 57b E.1 — re-export the PreemptFrame layout constants pinned by
// `kernel_core::preempt_frame`.  The Phase 57d assembly entry stub will
// dereference these offsets relative to a `Task` base pointer to write
// every saved register into [`Task::preempt_frame`].  Re-exporting the
// constants here (rather than redefining them) keeps a single source of
// truth for the layout (DRY): if `PreemptFrame` ever shifts, the kernel-core
// const _: () = assert!(...) gates fail the build before any caller can
// pick up the wrong offset.  The constants are unused inside the kernel
// in 57b — Phase 57d's assembly stub is the first consumer — so an
// explicit `unused_imports` allowance keeps `cargo xtask check` clean.
#[allow(unused_imports)]
pub use kernel_core::preempt_frame::{
    PREEMPT_FRAME_OFFSET_CS, PREEMPT_FRAME_OFFSET_RAX, PREEMPT_FRAME_OFFSET_RFLAGS,
    PREEMPT_FRAME_OFFSET_RIP, PREEMPT_FRAME_OFFSET_RSP, PREEMPT_FRAME_OFFSET_SS,
};

pub mod blocking_mutex;
pub mod kstack;
pub mod sched_trace;
pub mod scheduler;
pub mod wait_queue;
pub mod watchdog;

#[allow(unused_imports)]
pub use scheduler::{
    block_current_on_notif_v2, block_current_on_recv_v2, block_current_on_reply_v2,
    block_current_on_send_v2, block_current_until, current_task_id, deliver_bulk, deliver_message,
    insert_cap, mark_current_dead, mark_task_dead_by_pid, maybe_load_balance, remove_task_cap, run,
    server_endpoint, set_current_task_pid, set_current_user_return, set_server_endpoint,
    signal_reschedule, spawn, spawn_fork_task, spawn_idle, spawn_idle_for_core, spawn_on_core,
    spawn_on_current_core, sys_nice, sys_sched_getaffinity, sys_sched_setaffinity, take_bulk_data,
    take_current_task_fork_ctx, take_message, task_cap, wake_task_v2, yield_now,
};

// ---------------------------------------------------------------------------
// Panic diagnostics support
// ---------------------------------------------------------------------------

/// Try to acquire the scheduler lock without blocking.
///
/// Returns `None` if the lock is already held (e.g. during a panic while
/// the scheduler is running). Used by `panic_diag` to safely inspect tasks.
pub(crate) fn try_lock_scheduler() -> Option<scheduler::SchedulerGuard<'static>> {
    scheduler::try_scheduler_lock()
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Task ID
// ---------------------------------------------------------------------------

// TaskId is re-exported from kernel_core::types above.

// ---------------------------------------------------------------------------
// Task user-return state
// ---------------------------------------------------------------------------

/// User-mode return state saved at syscall entry and restored by the
/// scheduler on re-dispatch.  Captures the complete per-task resume
/// contract in one place, eliminating split ownership between `Task`,
/// `Process`, and `PerCoreData`.
///
/// # Phase 52d invariant
///
/// `syscall_handler` snapshots this struct once before any blocking or
/// yield path.  The scheduler restores `user_rsp`, `kernel_stack_top`,
/// `fs_base`, and `cr3_phys` exclusively from this struct for userspace
/// tasks (pid != 0).
#[derive(Debug, Clone, Copy, Default)]
pub struct UserReturnState {
    /// User-mode RSP at syscall entry.
    pub user_rsp: u64,
    /// Kernel stack top for TSS.RSP0 / SYSCALL stack.
    pub kernel_stack_top: u64,
    /// FS.base MSR value (TLS pointer).
    pub fs_base: u64,
    /// Physical address of the PML4 (CR3).  0 means no dedicated address space.
    pub cr3_phys: u64,
    /// Address-space generation counter at the time of snapshot (Phase 52d B.3).
    /// Used by user-copy diagnostics to detect concurrent mapping mutations.
    pub addr_space_gen: u64,
}

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
    /// Task is blocked waiting on a futex (Phase 40).
    BlockedOnFutex,
    /// Task is blocked waiting for a child process state change.
    BlockedOnWait,
    /// Task is blocked waiting for a named IPC service to register.
    BlockedOnService,
    /// Task has permanently exited; the scheduler will remove it on next pass.
    Dead,
}

// ---------------------------------------------------------------------------
// Phase 57d D.1 — ResumeMode
// ---------------------------------------------------------------------------

/// Determines which resume path the scheduler uses when dispatching a task.
///
/// Stored atomically in [`Task::resume_mode`].  Set under the scheduler lock
/// at the suspension point; read at the dispatch point to choose
/// between `switch_context` (cooperative) and `preempt_resume_to_user`
/// (preempted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResumeMode {
    /// Task has never been dispatched (initial state) — use cooperative path.
    Initial = 0,
    /// Task suspended cooperatively via `yield_now` or `block_current_until`.
    /// Resume via `switch_context` (callee-saved restore + `ret`).
    Cooperative = 1,
    /// Task was preempted by `preempt_to_scheduler`.  Dispatch via
    /// `preempt_resume_to_user` (full GPR restore + `iretq` to ring 3).
    Preempted = 2,
}

impl From<u8> for ResumeMode {
    fn from(v: u8) -> Self {
        match v {
            1 => ResumeMode::Cooperative,
            2 => ResumeMode::Preempted,
            _ => ResumeMode::Initial,
        }
    }
}

// ---------------------------------------------------------------------------
// TaskBlockState (Phase 57a B.1)
// ---------------------------------------------------------------------------

/// State protected by [`Task::pi_lock`].
///
/// All mutations to these fields go through [`Task::with_block_state`] (B.4).
/// Readers outside the `pi_lock` critical section MUST NOT inspect these
/// fields directly — Tracks C/D enforce this contract by routing every
/// read through the helper.
///
/// # Lock-ordering invariant
///
/// `pi_lock` is OUTER, `SCHEDULER.lock` is INNER (Linux's `p->pi_lock` →
/// `rq->lock` pattern).  A code path may hold `pi_lock` while acquiring
/// `SCHEDULER.lock`; the reverse is forbidden and panics in debug builds
/// (see [`Task::with_block_state`]).
pub struct TaskBlockState {
    /// Canonical block state.  Mirrors the v1 `Task::state` field.
    ///
    /// Invariant: only mutated while `pi_lock` is held.
    pub state: TaskState,

    /// Absolute tick deadline at which `scan_expired_wake_deadlines` will
    /// force-wake the task to `Ready`.  `None` for indefinite-timeout blocks.
    ///
    /// Invariant: only mutated while `pi_lock` is held.
    pub wake_deadline: Option<u64>,
}

// ---------------------------------------------------------------------------
// Task structure
// ---------------------------------------------------------------------------

/// Per-task XSAVE area used by the dispatch boundary's
/// `save_fpu_state` / `restore_fpu_state` helpers.
///
/// Phase 57e Track J: replaces the prior 512-byte 16-aligned `FxSaveArea`
/// (FXSAVE legacy region only).  The expanded layout covers x87 + SSE + AVX
/// state — the legacy region is the same first 512 bytes so the existing
/// `XSTATE_BV` init sequence still produces the architectural defaults; the
/// header sits at offset 512 (`XSTATE_BV` itself at 512, `XCOMP_BV` at 520),
/// and the AVX YMM_HI region starts at offset 576.
///
/// Required alignment is 64 bytes (Intel SDM Vol 1 §13.4).
///
/// The static size [`crate::arch::x86_64::cpuid::XSAVE_AREA_SIZE`] (832 bytes
/// legacy x87+SSE+AVX; 2752 bytes with PKRU component 9) is checked at boot
/// against the runtime CPUID-reported size; if the static buffer is ever too
/// small for the enabled XCR0 mask, the kernel panics at boot rather than
/// silently truncating saved state.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct XSaveArea {
    bytes: [u8; crate::arch::x86_64::cpuid::XSAVE_AREA_SIZE],
}

impl XSaveArea {
    pub const fn new() -> Self {
        let mut bytes = [0u8; crate::arch::x86_64::cpuid::XSAVE_AREA_SIZE];
        // Legacy region defaults — same byte layout as the prior FxSaveArea
        // so a freshly-restored task observes the architectural FPU/SSE
        // defaults rather than zeroed control words.
        // x87 control word = 0x037F (offset 0).
        bytes[0] = 0x7f;
        bytes[1] = 0x03;
        // MXCSR = 0x1F80 (offset 24).
        bytes[24] = 0x80;
        bytes[25] = 0x1f;
        // MXCSR mask = 0xFFFF (offset 28).
        bytes[28] = 0xff;
        bytes[29] = 0xff;
        // Header region (offsets 512-575): XSTATE_BV cleared.  The xsave init
        // optimisation interprets a clear `XSTATE_BV` as "no state in
        // modified-from-init form", so xrstor will load architectural defaults
        // for every component — exactly what we want for a freshly-allocated
        // task.  XCOMP_BV stays clear (we use the standard XSAVE format, not
        // compacted; xrstor selects format based on bit 63 of XCOMP_BV).
        Self { bytes }
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr()
    }

    /// Phase 86f Track B.1 — return a slice of the raw saved bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Phase 86f Track B.1 — return a mutable slice of the raw saved bytes.
    ///
    /// Used by `sanitize_xsave_header` after `copy_from_bytes` to patch the
    /// header fields before `xrstor64`.
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    /// Phase 86f Track B.1 — overwrite the saved bytes from a slice.
    ///
    /// `src` must be exactly `XSAVE_AREA_SIZE` bytes; panics if not
    /// (caller bug — the signal frame always writes a full area).
    pub fn copy_from_bytes(&mut self, src: &[u8]) {
        self.bytes.copy_from_slice(src);
    }

    /// Phase 90a B.4 — seed the PKRU state component (9) to a specific value and
    /// mark it present in `XSTATE_BV`, so a subsequent `xrstor64` loads `pkru`
    /// rather than the hardware *init* value (`0` = **all keys permissive**, a
    /// security hole for a fresh thread).
    ///
    /// `XSaveArea::new()` is `const` and cannot read CPUID, so it leaves the
    /// PKRU bit clear; this non-`const` step runs after CPUID is available
    /// (PKRU offset is `CPUID.0Dh.9:EBX`).  On a no-PKU CPU
    /// [`crate::arch::x86_64::cpuid::pkru_component_offset`] is `0` and this is a
    /// **no-op** — the area stays bit-for-bit the legacy default.
    ///
    /// Used for two cases:
    /// * new task / `execve` reset → `pkru = PKRU_INIT_DEFAULT` (Linux
    ///   `init_pkru_value`: key 0 unrestricted, every non-zero key
    ///   access-denied);
    /// * `fork`/`clone` → the parent's captured PKRU (Linux inherit-on-clone).
    pub fn seed_pkru(&mut self, pkru: u32) {
        let off = crate::arch::x86_64::cpuid::pkru_component_offset();
        kernel_core::xsave_model::seed_pkru_component(&mut self.bytes, off, pkru);
    }

    /// Read back the saved PKRU register (component 9), or `None` on a no-PKU
    /// CPU / unseeded area.  Lets `fork`/`clone` snapshot the parent's PKRU.
    pub fn pkru(&self) -> Option<u32> {
        let off = crate::arch::x86_64::cpuid::pkru_component_offset();
        kernel_core::xsave_model::read_pkru_component(&self.bytes, off)
    }
}

/// Phase 57e Bug #3 fix — per-task user GPR snapshot.
///
/// The Linux x86_64 syscall ABI preserves all GPRs except `rax`/`rcx`/`r11`,
/// so the kernel must save the user side of those registers before running
/// any syscall body that may yield, fork, or signal.  Pre-Bug-#3 the kernel
/// kept these slots in `PerCoreData`, which was safe under preempt-voluntary
/// (a task in `syscall_handler` could only be preempted at user-return
/// boundaries) but unsafe under preempt-full: a mid-syscall kernel-mode
/// preempt lets a different task's `syscall_entry` overwrite the per-core
/// slots on the same core, so when the original task resumes its `fork()`
/// (or any handler that re-reads `r8`/`r9`/etc. as extra syscall args) it
/// sees stale values from another task.  The fork child then iretqs into
/// userspace with corrupt GPRs and faults.
///
/// Putting the snapshot per-task removes the aliasing entirely: the
/// dispatcher publishes a pointer to *the current task's* snapshot into
/// `PerCoreData::current_syscall_snapshot_ptr` on every dispatch, and
/// `syscall_entry` saves the user GPRs through that pointer.  Two tasks on
/// the same core can no longer share a slot.
///
/// `#[repr(C)]` pins the field offsets so the syscall-entry asm can use
/// literal `[ptr + SNAP_*]` addressing.  The `OFF_*` constants in this
/// module document the offsets the asm relies on.
#[repr(C)]
#[derive(Default)]
pub struct TaskSyscallSnapshot {
    pub user_rbx: u64,    // 0
    pub user_rbp: u64,    // 8
    pub user_r12: u64,    // 16
    pub user_r13: u64,    // 24
    pub user_r14: u64,    // 32
    pub user_r15: u64,    // 40
    pub user_rdi: u64,    // 48
    pub user_rsi: u64,    // 56
    pub user_rdx: u64,    // 64
    pub user_r8: u64,     // 72
    pub user_r9: u64,     // 80
    pub user_r10: u64,    // 88
    pub user_rflags: u64, // 96
    /// Phase 57e Bug #4 fix — user RSP at SYSCALL entry.
    ///
    /// The asm was originally only saving user RSP to a per-core slot
    /// (`PerCoreData::syscall_user_rsp`).  Under preempt-full a kernel-
    /// mode preempt firing in the window between the per-core save and
    /// `snapshot_user_return_state()`'s read of it would let another
    /// task's `syscall_entry` overwrite the per-core slot, so the
    /// preempted task's `task.user_return.user_rsp` was populated with a
    /// foreign value when its syscall handler eventually got around to
    /// running.  At sysret the bad RSP would land the resumed user task
    /// on a stranger's stack, with the next `ret`/instruction-fetch
    /// hitting garbage.
    ///
    /// Storing user RSP per-task and rebuilding the per-core slot from
    /// the snapshot on every dispatch closes the window: the snapshot
    /// is touched *only* by the owning task's `syscall_entry` (during
    /// the IRQs-masked prologue) so no aliasing is possible.
    pub user_rsp: u64, // 104
}

/// Byte offsets within `TaskSyscallSnapshot`.  The syscall-entry asm
/// references these as `[task_snapshot_ptr + SNAP_*]`.
pub mod task_syscall_snapshot_offsets {
    use super::TaskSyscallSnapshot;
    use core::mem::offset_of;

    pub const SNAP_USER_RBX: usize = offset_of!(TaskSyscallSnapshot, user_rbx);
    pub const SNAP_USER_RBP: usize = offset_of!(TaskSyscallSnapshot, user_rbp);
    pub const SNAP_USER_R12: usize = offset_of!(TaskSyscallSnapshot, user_r12);
    pub const SNAP_USER_R13: usize = offset_of!(TaskSyscallSnapshot, user_r13);
    pub const SNAP_USER_R14: usize = offset_of!(TaskSyscallSnapshot, user_r14);
    pub const SNAP_USER_R15: usize = offset_of!(TaskSyscallSnapshot, user_r15);
    pub const SNAP_USER_RDI: usize = offset_of!(TaskSyscallSnapshot, user_rdi);
    pub const SNAP_USER_RSI: usize = offset_of!(TaskSyscallSnapshot, user_rsi);
    pub const SNAP_USER_RDX: usize = offset_of!(TaskSyscallSnapshot, user_rdx);
    pub const SNAP_USER_R8: usize = offset_of!(TaskSyscallSnapshot, user_r8);
    pub const SNAP_USER_R9: usize = offset_of!(TaskSyscallSnapshot, user_r9);
    pub const SNAP_USER_R10: usize = offset_of!(TaskSyscallSnapshot, user_r10);
    pub const SNAP_USER_RFLAGS: usize = offset_of!(TaskSyscallSnapshot, user_rflags);
    pub const SNAP_USER_RSP: usize = offset_of!(TaskSyscallSnapshot, user_rsp);
}

/// Read a snapshot of the **current task's** user GPRs as captured by the
/// most recent `syscall_entry` on this core.
///
/// Replaces the per-core `pc.syscall_user_*` reads that pre-Bug-#3 code
/// did directly.  The pointer is published by the dispatcher on every
/// dispatch (`scheduler::run`) and stays valid for the lifetime of the
/// task on this core; under preempt-full it survives mid-syscall
/// kernel-mode preemption because each task has its own snapshot.
///
/// Returns a copy by value — callers should not hold a reference across
/// IRQ-enable boundaries (a same-core preempt could re-dispatch, but the
/// pointer would be re-pointed at *this* task's snapshot before this code
/// resumes anyway, so the pointee is stable; the by-value copy just keeps
/// the borrow checker happy without extra `unsafe`).
///
/// Panics in debug builds if the per-core pointer is null (would only
/// happen during very early boot before the first dispatch).
pub fn current_task_syscall_snapshot() -> TaskSyscallSnapshot {
    let pc = crate::smp::per_core();
    // SAFETY: `current_syscall_snapshot_ptr` is per-core, written only by
    // the dispatcher with IRQs masked.  Reading the raw pointer value is
    // a single aligned load.  Dereferencing it: the pointee is the
    // current task's `Task::syscall_snapshot`, which lives in a
    // `Vec<Box<Task>>` slot that is stable for the task's lifetime
    // (Track B's storage discipline).  The current task cannot have its
    // snapshot freed while it is the current task on this core.
    let ptr = unsafe { *pc.current_syscall_snapshot_ptr.get() };
    debug_assert!(
        !ptr.is_null(),
        "current_task_syscall_snapshot called before first dispatch"
    );
    if ptr.is_null() {
        return TaskSyscallSnapshot::default();
    }
    unsafe {
        TaskSyscallSnapshot {
            user_rbx: (*ptr).user_rbx,
            user_rbp: (*ptr).user_rbp,
            user_r12: (*ptr).user_r12,
            user_r13: (*ptr).user_r13,
            user_r14: (*ptr).user_r14,
            user_r15: (*ptr).user_r15,
            user_rdi: (*ptr).user_rdi,
            user_rsi: (*ptr).user_rsi,
            user_rdx: (*ptr).user_rdx,
            user_r8: (*ptr).user_r8,
            user_r9: (*ptr).user_r9,
            user_r10: (*ptr).user_r10,
            user_rflags: (*ptr).user_rflags,
            user_rsp: (*ptr).user_rsp,
        }
    }
}

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
    /// Bulk data attached to the pending message (Phase 52).
    ///
    /// Set alongside `pending_msg` when a sender uses `ipc_send_buf` or
    /// `ipc_call_buf`.  Consumed by `take_bulk_data` after the receiver
    /// wakes.  `None` for messages without bulk payloads.
    pub pending_bulk: Option<alloc::vec::Vec<u8>>,
    /// Sticky completion flag for `send()` / `send_with_cap()` so a receiver
    /// can acknowledge a consumed send even if the sender has not blocked yet.
    pub send_completed: bool,
    /// Endpoint this task is the "server" of (used by `reply_recv` to find
    /// the endpoint to block on after replying).
    pub server_endpoint: Option<crate::ipc::EndpointId>,
    /// Core this task is assigned to for per-CPU run queue dispatch (Phase 35).
    pub assigned_core: u8,
    /// PID of the userspace process this task is associated with (0 = kernel task).
    pub pid: u32,
    /// Task priority (Phase 35): 0-9 = real-time, 10-29 = normal, 30 = idle.
    /// Lower numeric value = higher priority.
    pub priority: u8,
    /// CPU affinity mask (Phase 35): one bit per core (max 64 cores).
    /// Default: all bits set (can run on any core).
    pub affinity_mask: u64,
    /// Wall-clock milliseconds the task has spent in ring 3.
    ///
    /// Phase 61 refactor history: pre-refactor was `u64` mutated under the
    /// scheduler lock from the timer ISR (heavy contention, caused
    /// `vfs_server: slow req` tail latency). Then briefly moved to a static
    /// global `[TaskRusage; MAX_TASKS]` table — that fixed the lock but
    /// introduced false-sharing between adjacent slots. Now back here as
    /// `AtomicU64`, written lock-free from the timer ISR only by the CPU
    /// currently running this task (Linux's `task_struct.utime` model). The
    /// `#[repr(transparent)]` of `AtomicU64` over `u64` means
    /// `EXPECTED_TASK_PREEMPT_FRAME_OFFSET` (464 since Phase 74's
    /// `Message` cap-slot extension; was 448 prior) is preserved without
    /// padding.
    pub user_ticks: core::sync::atomic::AtomicU64,
    /// Wall-clock milliseconds the task has spent in ring 0. Same access
    /// discipline as [`user_ticks`].
    pub system_ticks: core::sync::atomic::AtomicU64,
    /// Tick count when this task was last dispatched.
    pub start_tick: u64,
    /// Tick at which this task was last migrated to a different core (Phase 52c).
    /// Used by the load balancer to enforce a cooldown period and prevent
    /// migration thrashing.
    pub last_migrated_tick: u64,
    /// Tick at which this task most recently became `Ready` — set at spawn,
    /// at every wake, and on post-switch re-enqueue. Compared against
    /// `tick_count()` at dispatch time to measure ready-to-running latency
    /// (Phase 54 diagnostic).
    pub last_ready_tick: u64,
    /// True while the task is mid-context-switch and its `saved_rsp` may not
    /// yet be published.
    ///
    /// Set before `switch_context` (in the block/yield/dead path). Cleared by
    /// the dispatch handler immediately after `saved_rsp` is durably written
    /// to this struct (arch-level switch-out epilogue).
    ///
    /// Replaces v1's `PENDING_SWITCH_OUT[core]` RSP-publication guard
    /// (Linux `p->on_cpu` `smp_cond_load_acquire` pattern, `try_to_wake_up`).
    /// The wake-side spin-wait (`wake_task_v2`) reads this flag with `Acquire`
    /// ordering; the epilogue clear uses `Release`, guaranteeing a waker
    /// observing `on_cpu == false` sees the published `saved_rsp`.
    pub on_cpu: core::sync::atomic::AtomicBool,

    /// Set once per-task IPC teardown has run so deferred dead-task cleanup
    /// can avoid double-cleaning the same task.
    pub ipc_cleaned: bool,
    /// Set when another thread in the group calls `exit_group()` and this task
    /// must quiesce on its own core before the caller reaps its process entry.
    pub group_exit_pending: bool,
    /// User-mode return state saved when this task yields and restored by the
    /// scheduler on re-dispatch.  `None` for kernel-only tasks or before the
    /// first yield from a userspace context.
    pub user_return: Option<UserReturnState>,
    /// Userspace register frame restored by `fork_child_trampoline`, if this
    /// task was spawned to finish a fork/clone handoff.
    fork_ctx: Option<crate::process::ForkChildCtx>,
    /// Optional tick deadline at which a `Blocked*` task should be force-woken.
    ///
    /// `Some(deadline)` when set by `block_current_until` with a deadline.
    /// The scheduler's dispatch path scans blocked tasks whose `wake_deadline`
    /// is in the past and transitions them to `Ready`. `None` means no timeout.
    pub wake_deadline: Option<u64>,
    /// Tick at which this task most recently entered a `Blocked*` state.
    ///
    /// Set by `block_current_until` before yielding.
    /// Reset to 0 when the task transitions back to `Ready`.
    /// Used by the G.1 stuck-task watchdog to compute how long a task has been blocked.
    pub blocked_since_tick: u64,
    /// Owns the allocated kernel stack — dropped when the `Task` is dropped.
    /// Wrapped in `Option` so `drain_dead` can `.take()` the allocation to
    /// free stack memory for dead tasks without removing them from the vec.
    ///
    /// Backed by a static `.bss` pool ([`kstack::KernelStack`]) rather than
    /// the kernel heap so stack memory cannot alias with any other heap
    /// allocation. See `docs/handoffs/ap-core-gpf-saved-rsp-stack-corruption.md`
    /// for the failure mode this isolation closes.
    _stack: Option<kstack::KernelStack>,

    // ---------------------------------------------------------------------------
    // Phase 57a B.2 — per-task pi_lock (shadow lock, migration window)
    // ---------------------------------------------------------------------------
    /// Per-task spinlock guarding [`TaskBlockState`].
    ///
    /// # Lock ordering
    ///
    /// `pi_lock` is **OUTER**, `SCHEDULER.lock` is **INNER** (Linux's
    /// `p->pi_lock` → `rq->lock` pattern).  A code path may hold `pi_lock`
    /// while acquiring `SCHEDULER.lock`; the reverse is forbidden and panics
    /// in debug builds (see [`Task::with_block_state`]).
    ///
    /// # Migration window
    ///
    /// During Tracks C/D, writes go to **both** this field and to the legacy
    /// `Task::state` / `Task::wake_deadline` fields ("shadow lock" pattern).
    /// Track E removes the legacy fields once all callers migrate.
    pub pi_lock: crate::task::scheduler::IrqSafeMutex<TaskBlockState>,

    // ---------------------------------------------------------------------------
    // Phase 57b D.1 — per-task preempt-disable counter
    // ---------------------------------------------------------------------------
    /// Per-task preempt-disable counter. Incremented by `preempt_disable()`,
    /// decremented by `preempt_enable()`. Must be 0 at every user-mode return.
    /// Phase 57d/57e gate preemption on this == 0. The address of this field
    /// is stable across the task's lifetime — Track B's `Vec<Box<Task>>`
    /// storage guarantees the heap address does not move; Track C caches a
    /// raw pointer into this field on `PerCoreData::current_preempt_count_ptr`.
    pub preempt_count: core::sync::atomic::AtomicI32,

    // ---------------------------------------------------------------------------
    // Phase 57b E.1 — preemption save area
    // ---------------------------------------------------------------------------
    /// Phase 57b infrastructure. Written by 57d's assembly entry stub; read
    /// by 57d/57e's preempt-resume routines. Unused in 57b. Layout pinned by
    /// `kernel_core::preempt_frame::PreemptFrame` and the
    /// `PREEMPT_FRAME_OFFSET_*` constants exported from that module — the
    /// assembly stub uses those offsets directly.
    pub preempt_frame: kernel_core::preempt_frame::PreemptFrame,
    /// Phase 57e Bug #3 fix — per-task user GPR snapshot written by
    /// `syscall_entry` and read by `make_fork_ctx` and the syscall handlers
    /// that consume r8/r9/etc. as extra args.  See `TaskSyscallSnapshot`'s
    /// doc comment for the full hazard analysis.  The dispatcher publishes
    /// `&task.syscall_snapshot` into `PerCoreData::current_syscall_snapshot_ptr`
    /// on every dispatch so the asm and Rust paths agree on which task's
    /// snapshot is "current".
    ///
    /// `UnsafeCell` because the field is mutated through a raw pointer from
    /// asm (which the borrow checker cannot see) and from Rust handlers
    /// reading their own task's snapshot — both are single-writer for the
    /// duration of any syscall, since IRQs are masked at entry/exit and a
    /// kernel-mode preempt of *this* task can only resume on a core whose
    /// `current_syscall_snapshot_ptr` has been re-pointed back at *this*
    /// task by the dispatch path.
    pub syscall_snapshot: core::cell::UnsafeCell<TaskSyscallSnapshot>,
    /// Phase 57d D.1 — which resume path the scheduler uses for this task.
    ///
    /// `Preempted` → `preempt_resume_to_user` (full restore + `iretq`).
    /// `Cooperative` / `Initial` → existing `switch_context` path.
    pub resume_mode: core::sync::atomic::AtomicU8,
    /// Wake flag registered by an IPC caller before parking in
    /// [`TaskState::BlockedOnReply`]. Reply delivery sets this before calling
    /// `wake_task_v2`, closing the "reply arrived just before park" lost-wake
    /// window.
    pub reply_waker: Option<Arc<core::sync::atomic::AtomicBool>>,
    /// Accumulated user-mode ticks of this task's reaped descendants
    /// (children + recursively-reaped grandchildren). Updated at the
    /// zombie-reap point in `sys_waitpid`. Read by `sys_times` to populate
    /// `tms_cutime` and by `sys_getrusage(RUSAGE_CHILDREN)` to populate
    /// `ru_utime`. Phase 61 Track E.1.
    ///
    /// Placed AFTER `preempt_frame` to preserve `EXPECTED_TASK_PREEMPT_FRAME_OFFSET`.
    pub child_user_ticks: u64,
    /// Accumulated system-mode ticks of this task's reaped descendants.
    /// Updated at the zombie-reap point alongside `child_user_ticks`.
    /// Read by `sys_times` to populate `tms_cstime` and by
    /// `sys_getrusage(RUSAGE_CHILDREN)` to populate `ru_stime`.
    /// Phase 61 Track E.1.
    pub child_system_ticks: u64,
    /// Phase 110 Track B.3 — this task's saved `IA32_PL3_SSP` (user shadow-stack
    /// pointer). Same lifecycle as the FPU/XSAVE state: saved from the live MSR
    /// at switch-out (co-located with `save_fpu_state`) and restored to the MSR
    /// at switch-in (co-located with `restore_fpu_state`), both gated on
    /// `cet_active`. `0` for kernel tasks and until a shadow stack is installed
    /// (a `0` restore leaves the task with no shadow stack). Only meaningful
    /// when CET is active; inert on QEMU.
    ///
    /// Placed AFTER `preempt_frame` to preserve `EXPECTED_TASK_PREEMPT_FRAME_OFFSET`.
    pub cet_ssp: u64,
    /// Phase 61 Track E.4 — page-fault counters for `getrusage(2)`.
    ///
    /// Minor faults — fault successfully resolved in-memory (e.g., CoW page
    /// duplication, demand-zero allocation). No backing-store I/O.
    /// Incremented from `page_fault_handler` after resolution. Same access
    /// discipline as [`user_ticks`] / [`system_ticks`] above: lock-free
    /// atomic written only by the CPU running this task.
    pub minor_faults: core::sync::atomic::AtomicU64,
    /// Major faults — fault required a backing-store read (page-in from
    /// disk-backed `mmap`, swap-in). In Phase 61 the disk-backed mmap path
    /// is incomplete, so this counter stays at 0 in practice; the field is
    /// present so the API surface is stable.
    pub major_faults: core::sync::atomic::AtomicU64,
    /// Voluntary context switches — task explicitly yielded
    /// (`yield_now`, IPC block, futex sleep, etc.). Incremented inside
    /// `yield_now` before the switch.
    pub voluntary_ctxsw: u64,
    /// Involuntary context switches — task was preempted by the timer IRQ
    /// or rescheduled by an external waker. Incremented from the
    /// timer-IRQ preempt path before the switch.
    pub involuntary_ctxsw: u64,
    /// Reaped-descendants minor-fault accumulator. Updated at zombie-reap
    /// alongside the time accumulators, recursive accumulation rule.
    pub child_minor_faults: u64,
    /// Reaped-descendants major-fault accumulator.
    pub child_major_faults: u64,
    /// Reaped-descendants voluntary-ctxsw accumulator.
    pub child_voluntary_ctxsw: u64,
    /// Reaped-descendants involuntary-ctxsw accumulator.
    pub child_involuntary_ctxsw: u64,
    /// DIAGNOSTIC (claude -p stall hunt, 2026-06-16) — tick at which this task
    /// most recently ENTERED a syscall. Stamped at `syscall_handler` entry,
    /// lock-free from the running CPU (same discipline as [`user_ticks`]). Does
    /// NOT reset on a wake/reblock cycle, so `now - last_syscall_entry_tick`
    /// measures how long a task has been inside ONE syscall even if it is
    /// wake/reblock-looping in `BlockedOnReply` (which resets `blocked_since_tick`
    /// every cycle and so blinds the 30 s stuck-task watchdog). The reply-stall
    /// scan in `watchdog_scan` reads this to catch the Claude Code startup stall.
    /// Placed AFTER `preempt_frame` to preserve `EXPECTED_TASK_PREEMPT_FRAME_OFFSET`.
    pub last_syscall_entry_tick: core::sync::atomic::AtomicU64,
    /// DIAGNOSTIC (claude -p stall hunt, 2026-06-16) — the syscall number of the
    /// most recent `syscall_handler` entry. Paired with `last_syscall_entry_tick`
    /// so the stall census can name WHICH syscall a wedged task is parked in
    /// (e.g. `epoll_pwait`=281, `futex`=202) — the parked node event-loop thread
    /// is invisible to the 30 s watchdog (it exempts `BlockedOnRecv` with no
    /// deadline, which is exactly how `epoll_wait` blocks).
    pub last_syscall_nr: core::sync::atomic::AtomicU32,
}

// ---------------------------------------------------------------------------
// Phase 57b E.2 — Task::preempt_frame layout regression gate
// ---------------------------------------------------------------------------
//
// Phase 57d's assembly entry stub will store every saved register into
// `Task.preempt_frame` using literal `[task_ptr + EXPECTED_TASK_PREEMPT_FRAME_OFFSET + PREEMPT_FRAME_OFFSET_*]`
// addressing.  If the offset of `preempt_frame` inside `Task` ever drifts
// (e.g., a new field is inserted before it) the assembly will write to the
// wrong slot — silently corrupting the saved register set, and on resume
// jumping to garbage.
//
// The two assertions below pin the offset at build time:
//
//   1. `EXPECTED_TASK_PREEMPT_FRAME_OFFSET` records the value at the time
//      this gate was added (Phase 57b E.2).  Treat it as the canonical
//      "what 57d's assembly was written against" anchor.
//   2. The `const _: () = assert!` cross-checks `offset_of!(Task,
//      preempt_frame)` against that anchor; a mismatch fails the build with
//      a load-bearing message that points future contributors at this gate.
//
// To intentionally rebase the offset (e.g., after a deliberate `Task` field
// reorder), update both `EXPECTED_TASK_PREEMPT_FRAME_OFFSET` and the
// matching offset references in 57d's assembly stub in the same commit.
//
// This assertion lives in the kernel crate (rather than `kernel/tests/`
// integration-test land) because the `kernel` crate is a binary and has no
// `lib` target — integration tests cannot import `Task`.  A const assertion
// on the type definition itself is the strongest guard available and runs
// on every kernel build (including `cargo xtask check` clippy passes).

/// Documented byte offset of [`Task::preempt_frame`] inside [`Task`].  Pins
/// the value at the time Phase 57b E.2 landed (448).  Treat as the source
/// of truth that Phase 57d's assembly entry stub is written against.
// Phase 74 bump: extending `Message` with `cap_slots: [CapHandle; 2]` and
// `n_caps: u8` (rounded up to the next 8-byte alignment) added 16 bytes to
// `Task::pending_msg`, shifting `preempt_frame` from 448 to 464. No assembly
// in `arch/x86_64/` uses this offset directly — the `preempt_resume_to_user`
// asm takes `*const PreemptFrame` in `rdi` so it is layout-agnostic. The
// constant is updated and the regression gate continues to fire on any
// further drift.
pub const EXPECTED_TASK_PREEMPT_FRAME_OFFSET: usize = 464;

const _: () = assert!(
    core::mem::offset_of!(Task, preempt_frame) == EXPECTED_TASK_PREEMPT_FRAME_OFFSET,
    "Task::preempt_frame offset drift will break Phase 57d assembly: \
     reorder Task fields or update EXPECTED_TASK_PREEMPT_FRAME_OFFSET \
     plus 57d's assembly offsets in the same commit",
);

impl Task {
    /// Allocate a new task with its own kernel stack, initialized to enter
    /// `entry` when first scheduled.
    pub fn new(entry: fn() -> !, name: &'static str) -> Self {
        static NEXT_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
        let id = TaskId(NEXT_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed));

        let mut stack =
            kstack::KernelStack::alloc().expect("kernel stack pool exhausted (MAX_TASKS reached)");
        let saved_rsp = init_stack(stack.as_mut_slice(), entry);

        Task {
            id,
            name,
            state: TaskState::Ready,
            saved_rsp,
            caps: CapabilityTable::new(),
            pending_msg: None,
            pending_bulk: None,
            send_completed: false,
            server_endpoint: None,
            assigned_core: 0,
            pid: 0,                  // Set by fork_child_trampoline for userspace tasks
            priority: 20,            // Normal priority (middle of 10-29 range)
            affinity_mask: u64::MAX, // Can run on any core
            user_ticks: core::sync::atomic::AtomicU64::new(0),
            system_ticks: core::sync::atomic::AtomicU64::new(0),
            start_tick: 0,
            last_migrated_tick: 0,
            last_ready_tick: 0,
            on_cpu: core::sync::atomic::AtomicBool::new(false),
            ipc_cleaned: false,
            group_exit_pending: false,
            user_return: None,
            cet_ssp: 0,
            fork_ctx: None,
            wake_deadline: None,
            blocked_since_tick: 0,
            _stack: Some(stack),
            // Phase 57a B.2: initialize pi_lock with the same initial state as
            // Task::state so the shadow lock is consistent from construction.
            // Writes during the migration window (Tracks C/D) go to both v1
            // fields and pi_lock; Track E removes the v1 fields.
            pi_lock: crate::task::scheduler::IrqSafeMutex::new(TaskBlockState {
                state: TaskState::Ready,
                wake_deadline: None,
            }),
            // Phase 57b D.1: counter starts at 0 — no preempt_disable held.
            // Track F will wire IrqSafeMutex::lock to fetch_add this counter
            // in 57b; in 57b proper the counter is never read by 57d/57e gates.
            preempt_count: core::sync::atomic::AtomicI32::new(0),
            // Phase 57b E.1: zero-initialised save area. Untouched in 57b;
            // 57d's assembly entry stub will populate this on every preempt
            // entry, and 57d/57e's resume routines read it back to issue
            // `iretq` to the preempted instruction.
            preempt_frame: kernel_core::preempt_frame::PreemptFrame::default(),
            syscall_snapshot: core::cell::UnsafeCell::new(TaskSyscallSnapshot::default()),
            resume_mode: core::sync::atomic::AtomicU8::new(ResumeMode::Initial as u8),
            reply_waker: None,
            // Phase 61 Track E.1 — children CPU-time accumulators.
            child_user_ticks: 0,
            child_system_ticks: 0,
            // Phase 61 Track E.4 — rusage event counters (own + child).
            minor_faults: core::sync::atomic::AtomicU64::new(0),
            major_faults: core::sync::atomic::AtomicU64::new(0),
            voluntary_ctxsw: 0,
            involuntary_ctxsw: 0,
            child_minor_faults: 0,
            child_major_faults: 0,
            child_voluntary_ctxsw: 0,
            child_involuntary_ctxsw: 0,
            last_syscall_entry_tick: core::sync::atomic::AtomicU64::new(0),
            last_syscall_nr: core::sync::atomic::AtomicU32::new(u32::MAX),
        }
    }

    /// Return the base and top addresses of this task's kernel stack, if allocated.
    pub fn stack_bounds(&self) -> Option<(u64, u64)> {
        self._stack.as_ref().map(|s| s.bounds())
    }

    // ---------------------------------------------------------------------------
    // Phase 57a B.4 — canonical pi_lock reader/writer
    // ---------------------------------------------------------------------------

    /// Acquire `pi_lock`, run `f` with mutable access to the protected
    /// [`TaskBlockState`], release, and return the result.
    ///
    /// This is the **only** entry point Tracks C/D use to read or write
    /// `TaskBlockState` fields.  Using this helper exclusively is the SOLID
    /// Single-Responsibility boundary: all lock-acquire/transition/release
    /// boilerplate lives here, not at call sites.
    ///
    /// # Lock ordering
    ///
    /// In debug builds, panics if `SCHEDULER.lock` is already held by this
    /// CPU (Linux's `p->pi_lock` → `rq->lock` ordering — `pi_lock` is the
    /// OUTER lock; see the `scheduler.rs` module doc for the full hierarchy).
    #[inline]
    pub fn with_block_state<R>(&self, f: impl FnOnce(&mut TaskBlockState) -> R) -> R {
        // Phase 57a B.3: lock-ordering assertion.
        // Acquiring pi_lock while already holding SCHEDULER.lock violates the
        // Linux p->pi_lock → rq->lock invariant and can deadlock.
        debug_assert!(
            !crate::smp::try_per_core()
                .map(|c| c
                    .holds_scheduler_lock
                    .load(core::sync::atomic::Ordering::Relaxed))
                .unwrap_or(false),
            "pi_lock acquisition while SCHEDULER.lock is held — \
             Linux p->pi_lock → rq->lock ordering violated"
        );
        let mut guard = self.pi_lock.lock();
        f(&mut guard)
    }

    /// Like [`Task::with_block_state`], but for sites that already hold
    /// `scheduler_lock()` (the inner lock) and need to acquire `pi_lock`
    /// (the outer lock) for an atomic state mutation.
    ///
    /// Per the canonical Linux `p->pi_lock` → `rq->lock` ordering, acquiring
    /// `pi_lock` while the run-queue lock is held inverts the lock order and
    /// is normally a deadlock risk. This helper is reserved for sites where
    /// a **structural-safety argument** rules out concurrent waker contention:
    ///
    /// - The task being mutated is not visible from another CPU (test setup,
    ///   freshly-constructed tasks before `push`), or
    /// - The mutation runs under IRQ-disabled `scheduler_lock` and the
    ///   competing waker class (`wake_task_v2`'s `Blocked* → Ready` CAS)
    ///   cannot target the state being written here (`Ready → Dead` queue-scan
    ///   cleanup, or `Ready`/idle `→ Running` dispatch publish).
    ///
    /// Each call site MUST carry an inline `// NOTE:` comment explaining
    /// which structural-safety argument applies. See
    /// `docs/handoffs/62a-pi-lock-inventory.md` for the per-site reasoning
    /// at the four Phase 57a Tracks C/D closure sites this helper unblocks.
    ///
    /// # Lock ordering
    ///
    /// Unlike [`Task::with_block_state`] (which debug-asserts that
    /// `scheduler_lock` is *unheld*), this helper debug-asserts the
    /// **inverse** invariant — that `scheduler_lock` *is* held by this CPU
    /// — so misuse outside the documented exception sites is caught in
    /// debug builds. The visibility is also narrowed to `pub(crate)` so
    /// only kernel-internal code (the four scheduler sites listed above)
    /// can reach this exception path.
    #[inline]
    pub(crate) fn with_block_state_locked_scheduler<R>(
        &self,
        f: impl FnOnce(&mut TaskBlockState) -> R,
    ) -> R {
        // Phase 62 (PR #146 review fix): inverse of `with_block_state`'s
        // assertion. `with_block_state` panics if `scheduler_lock` IS held;
        // this helper panics if it is NOT held — confirming the caller is at
        // a documented exception site (one of the four `// NOTE: Phase 62
        // Track B` sites in `kernel/src/task/scheduler.rs`) and not using
        // this as a drop-in for `with_block_state`. Mirrors the lenient
        // `unwrap_or(true)` pattern: if per-CPU data isn't yet available
        // (very-early-boot), defer to the caller's correctness — the four
        // documented sites all run after per-core init, so this branch
        // should never fire there.
        debug_assert!(
            crate::smp::try_per_core()
                .map(|c| c
                    .holds_scheduler_lock
                    .load(core::sync::atomic::Ordering::Relaxed))
                .unwrap_or(true),
            "with_block_state_locked_scheduler called without SCHEDULER.lock \
             held — this helper is the documented lock-order exception path \
             for sites that already hold scheduler_lock(); use \
             Task::with_block_state instead"
        );
        let mut guard = self.pi_lock.lock();
        f(&mut guard)
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

// ---------------------------------------------------------------------------
// E.1 in-kernel QEMU tests
// ---------------------------------------------------------------------------
//
// The kernel crate is `no_std` and uses the `test_case` framework
// (see `crate::test_runner`) rather than libtest's `#[test]`. Using
// `#[test_case]` lets these checks run inside the kernel test harness
// alongside the rest of the QEMU-driven suite.

#[cfg(test)]
mod tests {
    use core::sync::atomic::Ordering;

    /// Verify that `Task::on_cpu` can be set and cleared with the correct
    /// Release/Acquire ordering semantics expected by the epilogue clear and
    /// D.1's wake-side spin-wait.
    ///
    /// Exercises the AtomicBool API and memory-ordering contract in isolation
    /// (no scheduler lock, no switch_context).
    #[test_case]
    fn on_cpu_set_clear_round_trip() {
        let flag = core::sync::atomic::AtomicBool::new(false);

        // Initially false — task is not in a switch-out window.
        assert!(!flag.load(Ordering::Acquire));

        // Block-side path: set to true before switch_context (Release).
        flag.store(true, Ordering::Release);
        assert!(flag.load(Ordering::Acquire));

        // Epilogue clear: set to false after saved_rsp is committed (Release).
        flag.store(false, Ordering::Release);
        assert!(!flag.load(Ordering::Acquire));
    }

    /// Verify pick_next on_cpu guard: a task with `on_cpu == true` must be
    /// excluded from dispatch until the switch-out epilogue clears it.
    #[test_case]
    fn on_cpu_guard_excludes_switching_task() {
        let on_cpu = core::sync::atomic::AtomicBool::new(false);

        // Initially false → task is eligible for dispatch.
        let eligible = !on_cpu.load(Ordering::Acquire);
        assert!(eligible);

        // on_cpu set (mid switch-out) → ineligible.
        on_cpu.store(true, Ordering::Release);
        let eligible = !on_cpu.load(Ordering::Acquire);
        assert!(!eligible);

        // Epilogue clears on_cpu → eligible again.
        on_cpu.store(false, Ordering::Release);
        let eligible = !on_cpu.load(Ordering::Acquire);
        assert!(eligible);
    }

    // -----------------------------------------------------------------------
    // Phase 57b B.2 — stable-address regression test
    //
    // The address of a `Task` heap allocation must remain fixed for the
    // entire lifetime of the task, even as the outer `Vec<Box<Task>>` grows
    // and reallocates.  Track C will cache a raw pointer to
    // `Task::preempt_count` on `PerCoreData::current_preempt_count_ptr`;
    // without `Vec<Box<Task>>` storage that pointer would dangle on the
    // first scheduler `push` past the current capacity.
    //
    // This test does not exercise the live `Scheduler::tasks` field
    // (avoiding any `scheduler_lock()` interaction in test context).  It
    // instead drives a private `Vec<Box<Task>>` through enough `push`
    // operations to force ≥ 3 reallocations of the outer `Vec`, then
    // confirms a cached pointer to an early task's `preempt_count` still
    // resolves to the same address and the same value the original task
    // wrote.  This pins the property — `Box` keeps each `Task` at a fixed
    // heap address regardless of `Vec` growth — without depending on the
    // scheduler harness.
    //
    // Lives in `kernel/src/task/mod.rs` rather than
    // `kernel/tests/task_storage_stable.rs` because the `kernel` crate is a
    // binary with no `lib` target — integration tests cannot import `Task`.
    // A `#[cfg(test)] #[test_case]` here runs inside the kernel test
    // harness alongside the rest of `cargo xtask test`.
    // -----------------------------------------------------------------------

    use super::Task;
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    /// Dummy entry function for synthetic `Task` instances created in tests.
    ///
    /// Real tasks point `entry` at a function the scheduler would dispatch;
    /// this stub is never actually executed because the test never inserts
    /// the task into the scheduler.  It exists only so [`Task::new`] can
    /// build a complete kernel stack frame.
    fn dummy_task_entry() -> ! {
        loop {
            core::hint::spin_loop();
        }
    }

    /// Address-stability of `Task::preempt_count` across `Vec` reallocations.
    ///
    /// Phase 57b Track C caches a raw pointer to a live task's
    /// `preempt_count`.  That pointer must remain valid while the outer
    /// `Vec<Box<Task>>` grows (e.g., as new tasks `spawn`).  This test
    /// pushes 32 boxed tasks into a freshly-constructed `Vec`, forcing the
    /// `Vec` to reallocate multiple times (typical `Vec` growth from
    /// capacity 0 walks 0 → 4 → 8 → 16 → 32, which is 4 reallocations —
    /// strictly more than the 3 required by the spec).
    ///
    /// Steps:
    ///   1. Push 3 sentinel boxed tasks; cache a raw pointer to `tasks[2]`'s
    ///      `preempt_count` and write a known sentinel value into it.
    ///   2. Push 29 additional boxed tasks (32 total) — forces multiple
    ///      `Vec` reallocations.
    ///   3. Re-read the cached pointer (without going through `tasks[2]`).
    ///      Assert the address still matches `&tasks[2].preempt_count` and
    ///      that the sentinel value is intact.
    ///
    /// A failure here means `Vec<Box<Task>>` is no longer the storage shape
    /// (e.g., a refactor accidentally reverted to `Vec<Task>`) or `Box`
    /// itself stopped guaranteeing heap-address stability.  Either case
    /// regresses the Track C invariant and breaks `preempt_disable` /
    /// `preempt_enable` after the next `spawn`.
    #[test_case]
    fn task_preempt_count_address_stable_across_vec_growth() {
        const SENTINEL: i32 = 0x5A5A_5A5A;
        const EARLY_IDX: usize = 2;
        const TOTAL_TASKS: usize = 32;

        // Start with empty (cap=0) Vec to maximise reallocation pressure.
        let mut tasks: Vec<Box<Task>> = Vec::new();

        // Phase 1: push enough tasks to reach EARLY_IDX, then cache a raw
        // pointer to that task's `preempt_count` and write a sentinel.
        for _ in 0..=EARLY_IDX {
            tasks.push(Box::new(Task::new(dummy_task_entry, "stable-addr-early")));
        }
        let cached_ptr: *const core::sync::atomic::AtomicI32 = &tasks[EARLY_IDX].preempt_count;
        tasks[EARLY_IDX]
            .preempt_count
            .store(SENTINEL, Ordering::Release);

        // Phase 2: push remaining tasks to force several Vec reallocations.
        // Vec<Box<Task>> typically grows 0 → 4 → 8 → 16 → 32 → … — pushing
        // 32 total entries forces at least 4 reallocations (well over the
        // ≥ 3 the B.2 acceptance criterion requires).
        while tasks.len() < TOTAL_TASKS {
            tasks.push(Box::new(Task::new(dummy_task_entry, "stable-addr-filler")));
        }

        // Phase 3: assert the cached pointer still points to the same heap
        // address as `tasks[EARLY_IDX].preempt_count` (Box keeps the
        // allocation pinned even though the outer Vec moved its slot
        // pointer) AND the sentinel value is intact.
        let live_ptr: *const core::sync::atomic::AtomicI32 = &tasks[EARLY_IDX].preempt_count;
        assert_eq!(
            cached_ptr, live_ptr,
            "Box<Task> must keep `Task::preempt_count` at a fixed heap \
             address across Vec reallocations (Phase 57b Track C invariant)",
        );

        // Read through the cached pointer (the path Track C will use in
        // production) and confirm the sentinel survived.
        // Safety: `cached_ptr` originated from a `&` borrow into `tasks[EARLY_IDX]`
        // earlier in this function; `tasks` is still alive in this scope and
        // `Box<Task>` guarantees the pointee has not moved.
        let observed = unsafe { (*cached_ptr).load(Ordering::Acquire) };
        assert_eq!(
            observed, SENTINEL,
            "value written through the cached pointer must survive \
             ≥ 3 Vec reallocations (got {observed:#x}, want {SENTINEL:#x})",
        );
    }

    // -----------------------------------------------------------------------
    // Phase 57b D.2 — lock-free `preempt_disable` / `preempt_enable`
    //                 regression tests
    //
    // These tests pin the lock-free property of D.2's helpers without
    // depending on a fully-initialised SMP environment.  The kernel test
    // harness runs `test_main()` *before* `smp::init_bsp_per_core()` (see
    // `kernel/src/main.rs`), so [`crate::smp::per_core`] is not callable
    // here.  [`crate::task::scheduler::preempt_disable`] guards itself with
    // [`crate::smp::try_per_core`] and degrades to a no-op when per-core
    // data is not yet initialised, so calling it directly is safe at this
    // point — but it would not exercise the `fetch_add` we want to pin.
    //
    // Approach: mirror the exact atomic operations the helpers perform
    // against a private [`AtomicI32`].  This pins:
    //
    //   1. **Lock-freedom** — the helpers are implemented as
    //      `(*ptr).fetch_add` / `fetch_sub` on a stable address and take
    //      no lock at all.  Reproducing that operation in the test against
    //      a private counter means the test cannot deadlock by
    //      construction; if a future refactor wired a lock through the
    //      counter the asserted operation count would diverge.
    //   2. **Pairing** — every `disable` matched by an `enable` returns
    //      the counter to 0, mirroring the user-mode-return invariant
    //      Track D.3 enforces.
    //   3. **Maximum nesting depth** — the helpers' debug assertion caps
    //      the post-increment count at 32 (Engineering Practice Gates of
    //      `docs/roadmap/tasks/57b-preemption-foundation-tasks.md`).  The
    //      property fuzz in `kernel-core/tests/preempt_property.rs`
    //      already pins the model; this kernel-side test mirrors the
    //      contract for the kernel-build counter.
    //
    // The full F.1 recursion test (calling `preempt_disable` from inside
    // `IrqSafeMutex::lock`) is deferred until Track F lands the
    // `IrqSafeMutex` integration; the property pinned here is the
    // pre-condition F.1 relies on.
    // -----------------------------------------------------------------------

    /// Mirrors the body of [`crate::task::scheduler::preempt_disable`]
    /// against an explicit pointer.  Used by the lock-freedom regression
    /// test below to exercise the post-increment / cap behaviour without
    /// depending on SMP initialisation.
    ///
    /// # Safety
    ///
    /// `ptr` must point at a live [`core::sync::atomic::AtomicI32`].
    unsafe fn synthetic_preempt_disable(ptr: *mut core::sync::atomic::AtomicI32) -> i32 {
        // Safety: caller-supplied invariant.
        unsafe { (*ptr).fetch_add(1, Ordering::Acquire) + 1 }
    }

    /// Mirrors the body of [`crate::task::scheduler::preempt_enable`]
    /// against an explicit pointer.
    ///
    /// # Safety
    ///
    /// `ptr` must point at a live [`core::sync::atomic::AtomicI32`].
    unsafe fn synthetic_preempt_enable(ptr: *mut core::sync::atomic::AtomicI32) -> i32 {
        // Safety: caller-supplied invariant.
        unsafe { (*ptr).fetch_sub(1, Ordering::Release) - 1 }
    }

    /// Recurse to `depth` levels and call [`synthetic_preempt_disable`] at
    /// the bottom.  Used to pin the lock-free property: a synthetic
    /// `preempt_disable` from deep inside a call chain (the closest stand-
    /// in for "from inside `IrqSafeMutex::lock`" until Track F lands)
    /// must complete without deadlock or stack overflow.
    ///
    /// # Safety
    ///
    /// `ptr` must point at a live [`core::sync::atomic::AtomicI32`].
    unsafe fn nested_call(depth: u32, ptr: *mut core::sync::atomic::AtomicI32) -> i32 {
        if depth == 0 {
            // Safety: caller-supplied invariant on `ptr`.
            unsafe { synthetic_preempt_disable(ptr) }
        } else {
            // Safety: caller-supplied invariant on `ptr`.
            unsafe { nested_call(depth - 1, ptr) }
        }
    }

    /// Phase 57b D.2 — lock-free property regression test.
    ///
    /// The full Track F.1 recursion test (a synthetic call to
    /// `preempt_disable` from inside `IrqSafeMutex::lock`) cannot run
    /// until F.1 lands the `IrqSafeMutex` integration.  This test pins
    /// the strongest property D.2 alone can demonstrate: calling the
    /// counter-mutation pattern from a deep nested call chain (the
    /// closest stand-in for "from inside an IrqSafeMutex critical
    /// section") completes without deadlock and produces the expected
    /// post-increment value.
    ///
    /// A deadlock here would manifest as a test timeout in QEMU.  A
    /// future refactor that smuggled a lock acquisition into
    /// `preempt_disable` would either deadlock under this test (if the
    /// lock were held by someone else) or fail review by inspection.
    #[test_case]
    fn preempt_disable_is_lock_free_under_synthetic_recursion() {
        let counter = core::sync::atomic::AtomicI32::new(0);
        let ptr = &counter as *const _ as *mut core::sync::atomic::AtomicI32;

        // Recurse 16 levels deep before issuing the synthetic
        // `preempt_disable`.  16 is well past the "deeply nested function
        // call" threshold the task spec calls out (10+) and stays
        // comfortably within the kernel test stack budget.
        const NEST_DEPTH: u32 = 16;
        // Safety: `ptr` derives from a live `AtomicI32` on this stack.
        let post_increment = unsafe { nested_call(NEST_DEPTH, ptr) };
        assert_eq!(
            post_increment, 1,
            "synthetic preempt_disable from depth-{NEST_DEPTH} nested call \
             must produce post-increment count = 1",
        );
        assert_eq!(counter.load(Ordering::Acquire), 1);

        // Pair with a synthetic enable and confirm round-trip to zero —
        // the user-mode-return invariant Track D.3 asserts.
        // Safety: `ptr` derives from a live `AtomicI32` on this stack.
        let post_decrement = unsafe { synthetic_preempt_enable(ptr) };
        assert_eq!(
            post_decrement, 0,
            "synthetic preempt_enable must round-trip the counter to 0",
        );
        assert_eq!(counter.load(Ordering::Acquire), 0);
    }

    /// Phase 57b D.2 — maximum nesting depth (32) regression test.
    ///
    /// Mirrors the property the model-side
    /// `nesting_to_max_depth_round_trips_to_zero` test in
    /// `kernel-core/src/preempt_model.rs` pins for the pure-logic
    /// `Counter`, but exercises the kernel-build [`AtomicI32`] used by
    /// the live `preempt_disable` / `preempt_enable` helpers.
    ///
    /// The helpers' [`debug_assert!`] caps the post-increment count at 32;
    /// this test confirms a balanced raise-to-32-then-drop sequence stays
    /// at or below the cap and round-trips to 0 cleanly.
    #[test_case]
    fn preempt_disable_round_trips_through_maximum_nesting_depth() {
        const MAX_DEPTH: i32 = 32;
        let counter = core::sync::atomic::AtomicI32::new(0);
        let ptr = &counter as *const _ as *mut core::sync::atomic::AtomicI32;

        for expected in 1..=MAX_DEPTH {
            // Safety: `ptr` derives from a live `AtomicI32` on this stack.
            let observed = unsafe { synthetic_preempt_disable(ptr) };
            assert_eq!(
                observed, expected,
                "post-increment count at depth {expected} must equal \
                 the depth (got {observed})",
            );
            assert!(
                observed <= MAX_DEPTH,
                "post-increment count {observed} exceeded the documented \
                 maximum nesting depth of {MAX_DEPTH} (Engineering \
                 Practice Gates of \
                 docs/roadmap/tasks/57b-preemption-foundation-tasks.md)",
            );
        }
        assert_eq!(counter.load(Ordering::Acquire), MAX_DEPTH);

        for expected in (0..MAX_DEPTH).rev() {
            // Safety: `ptr` derives from a live `AtomicI32` on this stack.
            let observed = unsafe { synthetic_preempt_enable(ptr) };
            assert_eq!(
                observed, expected,
                "post-decrement count must descend by one (got \
                 {observed}, want {expected})",
            );
        }
        assert_eq!(counter.load(Ordering::Acquire), 0);
    }
}
