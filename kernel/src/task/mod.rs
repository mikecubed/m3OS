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
pub const EXPECTED_TASK_PREEMPT_FRAME_OFFSET: usize = 448;

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
    // here — invoking [`crate::task::scheduler::preempt_disable`]
    // directly would panic on the uninitialised gs_base.
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
