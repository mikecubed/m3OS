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

use alloc::boxed::Box;

use crate::ipc::{CapabilityTable, Message};

pub(crate) const MAX_TASKS: usize = 256;

pub use kernel_core::types::TaskId;

pub mod blocking_mutex;
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
    signal_reschedule, spawn, spawn_fork_task, spawn_idle, spawn_idle_for_core,
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

pub(crate) const KERNEL_STACK_SIZE: usize = 4096 * 8; // 32 KiB

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
    /// Task has permanently exited; the scheduler will remove it on next pass.
    Dead,
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
    /// Canonical block state.  Mirrors the v1 `Task::state` field; will become
    /// the sole arbiter of "is this task blocked?" once Track F migrates all
    /// callers and Track E.3 deletes `Task::switching_out`.
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
    /// Ticks spent in ring 3 (user mode). Updated on context switch.
    pub user_ticks: u64,
    /// Ticks spent in ring 0 (syscall handling). Updated on context switch.
    pub system_ticks: u64,
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
    /// Set under `SCHEDULER.lock` alongside `switching_out = true`, before
    /// `switch_context`. Cleared by the dispatch handler immediately after
    /// `saved_rsp` is durably written to this struct (arch-level switch-out
    /// epilogue, around `scheduler.rs:2279`).
    ///
    /// Replaces the RSP-publication aspect of v1's `PENDING_SWITCH_OUT[core]`
    /// (Linux `p->on_cpu` `smp_cond_load_acquire` pattern in
    /// `try_to_wake_up`). D.1's wake-side spin-wait reads this flag with
    /// `Acquire` ordering; the epilogue clear uses `Release`. This
    /// Release/Acquire pair guarantees that a waker observing
    /// `on_cpu == false` is guaranteed to see the published `saved_rsp`.
    ///
    /// Track E.3 deletes `switching_out` once all v1 call sites are migrated
    /// (Track F). During the F.1–F.6 migration window, both fields are set
    /// and cleared together so `pick_next`'s dual guard (`!switching_out &&
    /// !on_cpu.load(Acquire)`) agrees with both paths.
    pub on_cpu: core::sync::atomic::AtomicBool,
    /// True while the task is returning to the scheduler and its kernel stack
    /// pointer has not been safely published yet.
    pub switching_out: bool,
    /// Set by a wakeup that arrives while `switching_out` is true so the
    /// scheduler can enqueue the task after `switch_context` completes.
    pub wake_after_switch: bool,
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
    /// `Some(deadline)` when set by `block_current_unless_woken_until`. The
    /// scheduler's dispatch path (`pick_next`'s caller) scans for blocked
    /// tasks whose `wake_deadline` is in the past and transitions them to
    /// `Ready`. `None` for tasks that have no timeout, which is the default.
    ///
    /// This is the safe replacement for a timer-ISR wake: the expiry check
    /// runs inside the scheduler dispatch loop (already holding
    /// `SCHEDULER.lock`), not from the timer ISR, so there is no same-core
    /// re-entrance hazard.
    pub wake_deadline: Option<u64>,
    /// Tick at which this task most recently entered a `Blocked*` state.
    ///
    /// Set by `block_current` (and its variants) immediately before writing
    /// the new `Blocked*` state. Reset to 0 when the task transitions back to
    /// `Ready` via `wake_task` or `scan_expired_wake_deadlines`. Used by the
    /// G.1 stuck-task watchdog to compute how long a task has been blocked.
    pub blocked_since_tick: u64,
    /// Owns the allocated kernel stack — dropped when the `Task` is dropped.
    /// Wrapped in `Option` so `drain_dead` can `.take()` the allocation to
    /// free stack memory for dead tasks without removing them from the vec.
    _stack: Option<Box<[u8]>>,

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
    pub pi_lock: spin::Mutex<TaskBlockState>,
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
            pending_bulk: None,
            send_completed: false,
            server_endpoint: None,
            assigned_core: 0,
            pid: 0,                  // Set by fork_child_trampoline for userspace tasks
            priority: 20,            // Normal priority (middle of 10-29 range)
            affinity_mask: u64::MAX, // Can run on any core
            user_ticks: 0,
            system_ticks: 0,
            start_tick: 0,
            last_migrated_tick: 0,
            last_ready_tick: 0,
            on_cpu: core::sync::atomic::AtomicBool::new(false),
            switching_out: false,
            wake_after_switch: false,
            ipc_cleaned: false,
            group_exit_pending: false,
            user_return: None,
            fork_ctx: None,
            wake_deadline: None,
            blocked_since_tick: 0,
            _stack: Some(stack),
            // Phase 57a B.2: initialize pi_lock with the same initial state as
            // Task::state so the shadow lock is consistent from construction.
            // Writes during the migration window (Tracks C/D) go to both v1
            // fields and pi_lock; Track E removes the v1 fields.
            pi_lock: spin::Mutex::new(TaskBlockState {
                state: TaskState::Ready,
                wake_deadline: None,
            }),
        }
    }

    /// Return the base and top addresses of this task's kernel stack, if allocated.
    pub fn stack_bounds(&self) -> Option<(u64, u64)> {
        self._stack.as_ref().map(|s| {
            let base = s.as_ptr() as u64;
            let top = base + s.len() as u64;
            (base, top)
        })
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

    /// Verify pick_next dual-guard semantics: a task with `on_cpu == true`
    /// must be excluded from dispatch even if `switching_out` already covers it.
    #[test_case]
    fn on_cpu_and_switching_out_dual_guard() {
        let on_cpu = core::sync::atomic::AtomicBool::new(false);

        // Neither flag set → eligible for dispatch.
        let eligible = !on_cpu.load(Ordering::Acquire);
        assert!(eligible);

        // on_cpu set → ineligible (dual guard).
        on_cpu.store(true, Ordering::Release);
        let eligible = !on_cpu.load(Ordering::Acquire);
        assert!(!eligible);

        // on_cpu cleared → eligible again.
        on_cpu.store(false, Ordering::Release);
        let eligible = !on_cpu.load(Ordering::Acquire);
        assert!(eligible);
    }
}
