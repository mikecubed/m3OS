//! Userspace process management — Phase 11 / Phase 14.
//!
//! This module owns the global process table and the types that describe
//! a userspace process's lifecycle.  Kernel threads live in
//! [`crate::task`]; this module is exclusively for ring-3 processes.
//!
//! # Design
//!
//! Each [`Process`] has its own kernel stack (allocated here, leaked so
//! the stack lives for the kernel lifetime) and a unique [`Pid`].  The
//! [`PROCESS_TABLE`] spinlock-protected global table is the single source
//! of truth for all live processes.
//!
//! Phase 14: each process has its own file descriptor table (`fd_table`).
//! FDs 0/1/2 are initialized as stdin/stdout/stderr on process creation.
//! `fork()` deep-clones the parent's FD table into the child.

#![allow(dead_code)]

extern crate alloc;

use alloc::{collections::VecDeque, string::String, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

use spin::Mutex;

// ---------------------------------------------------------------------------
// Current-process tracker (single-CPU)
// ---------------------------------------------------------------------------

/// PID of the userspace process currently running on the CPU.
///
/// 0 = no userspace process is running (kernel task context).
/// Updated by `fork_child_trampoline` and `sys_execve` before entering ring 3.
pub static CURRENT_PID: AtomicU32 = AtomicU32::new(0);

#[allow(unused_imports)]
pub use crate::mm::user_space::{USER_CODE_BASE, USER_STACK_TOP};

// ---------------------------------------------------------------------------
// Re-export stack size from task module so we have a single source of truth.
// ---------------------------------------------------------------------------

use crate::task::KERNEL_STACK_SIZE;

// ---------------------------------------------------------------------------
// PID type and allocator
// ---------------------------------------------------------------------------

/// A process identifier.  PID 0 is reserved for the idle concept and is
/// never assigned to a real process.
pub type Pid = u32;

/// Monotonically increasing PID counter.  Starts at 1 so that PID 0 is
/// always "no process / idle".
static NEXT_PID: AtomicU32 = AtomicU32::new(1);

/// Allocate a fresh PID.  Uses `Relaxed` ordering — PID uniqueness only
/// requires that each call sees a value not yet returned by a previous
/// call, which the atomic fetch-add guarantees regardless of memory order.
fn alloc_pid() -> Pid {
    NEXT_PID.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// File descriptor table (Phase 14 — per-process)
// ---------------------------------------------------------------------------

/// Maximum number of open file descriptors per process.
pub const MAX_FDS: usize = 32;

/// Backing store for an open file descriptor.
#[derive(Clone)]
pub enum FdBackend {
    /// FD 1/2 stdout/stderr — writes go to serial output.
    Stdout,
    /// FD 0 stdin — reads block until data is available from the kernel stdin buffer.
    Stdin,
    /// Read-only static ramdisk file (pointer + length into kernel .rodata).
    Ramdisk {
        content_addr: usize,
        content_len: usize,
    },
    /// Writable tmpfs file, identified by its path (e.g. "foo/bar.txt"
    /// relative to tmpfs root — no leading `/tmp/`).
    Tmpfs { path: String },
    /// Read end of a kernel pipe (Phase 14).
    PipeRead { pipe_id: usize },
    /// Write end of a kernel pipe (Phase 14).
    PipeWrite { pipe_id: usize },
    /// Directory file descriptor (Phase 18).
    Dir { path: String },
    /// /dev/null — reads return EOF, writes are silently discarded (Phase 21).
    DevNull,
}

/// A single open-file entry in the per-process FD table.
#[derive(Clone)]
pub struct FdEntry {
    pub backend: FdBackend,
    pub offset: usize,
    /// True if the file was opened for reading.
    pub readable: bool,
    /// True if the file was opened for writing.
    pub writable: bool,
}

/// Const sentinel for empty FD slots (used in array init).
const NONE_FD: Option<FdEntry> = None;

/// Create a default FD table with stdin(0), stdout(1), stderr(2) wired up.
/// Public accessor for use by the shell task when spawning processes.
pub fn new_fd_table_pub() -> [Option<FdEntry>; MAX_FDS] {
    new_fd_table()
}

/// Increment pipe ref-counts for all pipe FDs in a cloned FD table.
///
/// Must be called after cloning a process's FD table (fork/dup2) so that
/// pipe reader/writer counts stay consistent with the number of open FDs.
pub fn add_pipe_refs(fd_table: &[Option<FdEntry>; MAX_FDS]) {
    for entry in fd_table.iter().flatten() {
        match &entry.backend {
            FdBackend::PipeRead { pipe_id } => crate::pipe::pipe_add_reader(*pipe_id),
            FdBackend::PipeWrite { pipe_id } => crate::pipe::pipe_add_writer(*pipe_id),
            _ => {}
        }
    }
}

/// Close all open file descriptors for a process.
///
/// Decrements pipe ref-counts for any open pipe FDs so that EOF/EPIPE
/// propagates correctly. Called by `sys_exit` before marking the process
/// as a zombie.
pub fn close_all_fds_for(pid: Pid) {
    // Collect pipe IDs under the process table lock, then close them
    // after releasing the lock to avoid holding PROCESS_TABLE while
    // locking PIPE_TABLE.
    let mut readers = alloc::vec::Vec::new();
    let mut writers = alloc::vec::Vec::new();
    {
        let mut table = PROCESS_TABLE.lock();
        let proc = match table.find_mut(pid) {
            Some(p) => p,
            None => return,
        };
        for slot in proc.fd_table.iter_mut() {
            if let Some(entry) = slot.take() {
                match &entry.backend {
                    FdBackend::PipeRead { pipe_id } => readers.push(*pipe_id),
                    FdBackend::PipeWrite { pipe_id } => writers.push(*pipe_id),
                    _ => {}
                }
            }
        }
    }
    for id in readers {
        crate::pipe::pipe_close_reader(id);
    }
    for id in writers {
        crate::pipe::pipe_close_writer(id);
    }
}

/// Create a default FD table with stdin(0), stdout(1), stderr(2) wired up.
fn new_fd_table() -> [Option<FdEntry>; MAX_FDS] {
    let mut table = [NONE_FD; MAX_FDS];
    table[0] = Some(FdEntry {
        backend: FdBackend::Stdin,
        offset: 0,
        readable: true,
        writable: false,
    });
    table[1] = Some(FdEntry {
        backend: FdBackend::Stdout,
        offset: 0,
        readable: false,
        writable: true,
    });
    table[2] = Some(FdEntry {
        backend: FdBackend::Stdout,
        offset: 0,
        readable: false,
        writable: true,
    });
    table
}

// ---------------------------------------------------------------------------
// Signal constants (Phase 14)
// ---------------------------------------------------------------------------

/// Signal numbers (Linux x86_64).
pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGBUS: u32 = 7;
pub const SIGFPE: u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;
pub const SIGCHLD: u32 = 17;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;

/// sigaltstack flag: currently executing on the alt stack.
pub const SS_ONSTACK: u32 = 1;
/// sigaltstack flag: alt stack is disabled.
pub const SS_DISABLE: u32 = 2;
/// Minimum signal stack size (bytes).
pub const MINSIGSTKSZ: u64 = 2048;

/// What to do when a signal is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAction {
    /// Perform the default action for this signal.
    Default,
    /// Ignore the signal.
    Ignore,
    /// Run a user-space signal handler.
    Handler {
        /// Userspace handler function address.
        entry: u64,
        /// Additional signals to block during handler execution.
        mask: u64,
        /// `sa_flags` from `rt_sigaction` (SA_RESTORER, SA_ONSTACK, etc.).
        flags: u64,
        /// Address of the `__restore_rt` trampoline stub (from `sa_restorer`).
        restorer: u64,
    },
}

/// Default action table: terminate or ignore.
pub fn default_signal_action(sig: u32) -> SignalDisposition {
    match sig {
        SIGCHLD => SignalDisposition::Ignore,
        SIGCONT => SignalDisposition::Continue,
        SIGSTOP | SIGTSTP => SignalDisposition::Stop,
        SIGKILL | SIGINT | SIGTERM | SIGHUP | SIGBUS | SIGFPE | SIGSEGV | SIGPIPE | SIGALRM
        | SIGUSR1 | SIGUSR2 => SignalDisposition::Terminate,
        _ => SignalDisposition::Terminate,
    }
}

/// The kernel's resolved action when delivering a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalDisposition {
    Terminate,
    Stop,
    Continue,
    Ignore,
    /// Deliver to a user-space handler via sigframe.
    UserHandler {
        entry: u64,
        mask: u64,
        flags: u64,
        restorer: u64,
    },
}

// ---------------------------------------------------------------------------
// Process state
// ---------------------------------------------------------------------------

/// Lifecycle state of a userspace process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Waiting to be scheduled onto the CPU.
    Ready,
    /// Currently executing on the CPU.
    Running,
    /// Blocked waiting for a resource (I/O, IPC, …).
    Blocked,
    /// Stopped by a signal (SIGSTOP/SIGTSTP).
    Stopped,
    /// Exited but not yet reaped by its parent.
    Zombie,
}

// ---------------------------------------------------------------------------
// Process descriptor
// ---------------------------------------------------------------------------

/// Descriptor for a single userspace process.
pub struct Process {
    /// This process's unique identifier.
    pub pid: Pid,
    /// Parent PID.  0 means no parent (init or an orphan).
    pub ppid: Pid,
    /// Current lifecycle state.
    pub state: ProcessState,
    /// Root physical address of this process's page table.
    ///
    /// `None` means the process has not yet been assigned a dedicated
    /// address space and shares the kernel's mappings (pre-Phase 12).
    pub page_table_root: Option<x86_64::PhysAddr>,
    /// Top of this process's kernel-mode stack (virtual address).
    pub kernel_stack_top: u64,
    /// Userspace entry-point virtual address.
    pub entry_point: u64,
    /// Initial userspace RSP: the virtual address of `argc` on the ABI stack,
    /// as returned by `setup_abi_stack`.  This is **not** the raw top of the
    /// stack allocation — it points into the stack, below the argv/envp data.
    pub user_stack_top: u64,
    /// Exit code written when the process transitions to [`ProcessState::Zombie`].
    pub exit_code: Option<i32>,
    /// Signal that caused the process to stop (set when transitioning to Stopped).
    pub stop_signal: u32,
    /// True after waitpid has reported this stop; prevents re-reporting.
    pub stop_reported: bool,
    /// Current program break (heap top). 0 = not yet initialized.
    ///
    /// Set to BRK_BASE on first `sys_brk(0)` call; grows upward as
    /// the process requests more heap via `sys_brk(new_addr)`.
    pub brk_current: u64,
    /// Next virtual address available for anonymous `mmap` allocations.
    ///
    /// Initialized to ANON_MMAP_BASE on first use; grows upward with
    /// each allocation. Kept per-process so fork children start fresh.
    pub mmap_next: u64,
    /// Process group ID (Phase 14). Defaults to own PID.
    pub pgid: Pid,
    /// Per-process file descriptor table (Phase 14).
    ///
    /// FDs 0/1/2 are stdin/stdout/stderr.  `fork()` deep-clones this table.
    pub fd_table: [Option<FdEntry>; MAX_FDS],
    /// Bitfield of pending signals (bit N = signal N is pending).
    pub pending_signals: u64,
    /// Bitfield of blocked signals (bit N = signal N is blocked from delivery).
    /// SIGKILL (9) and SIGSTOP (19) can never be blocked.
    pub blocked_signals: u64,
    /// Per-signal action table (Default or Ignore).
    pub signal_actions: [SignalAction; 32],
    /// Alternate signal stack base address (0 = disabled).
    pub alt_stack_base: u64,
    /// Alternate signal stack size in bytes.
    pub alt_stack_size: u64,
    /// Alternate signal stack flags (SS_DISABLE, SS_ONSTACK).
    pub alt_stack_flags: u32,
    /// Current working directory (Phase 18). Defaults to "/".
    pub cwd: String,
}

impl Process {
    /// Backward-compatible constructor kept while `main.rs` still uses the
    /// old two-argument form.  Will be removed once the call sites are
    /// updated.
    #[deprecated(note = "Use spawn_process() instead")]
    pub fn new(entry: u64, stack_top: u64) -> Self {
        let kstack_top = alloc_kernel_stack();
        let pid = alloc_pid();
        Process {
            pid,
            ppid: 0,
            state: ProcessState::Ready,
            page_table_root: None,
            kernel_stack_top: kstack_top,
            entry_point: entry,
            user_stack_top: stack_top,
            exit_code: None,
            stop_signal: 0,
            stop_reported: false,
            brk_current: 0,
            mmap_next: 0,
            pgid: pid,
            fd_table: new_fd_table(),
            pending_signals: 0,
            blocked_signals: 0,
            signal_actions: [SignalAction::Default; 32],
            alt_stack_base: 0,
            alt_stack_size: 0,
            alt_stack_flags: 0,
            cwd: String::from("/"),
        }
    }
}

// ---------------------------------------------------------------------------
// Kernel-stack allocator
// ---------------------------------------------------------------------------

/// Allocate a kernel stack for a new process and return its top address.
///
/// The stack is a heap-allocated `[u8; KERNEL_STACK_SIZE]` array that is
/// intentionally leaked (`Box::into_raw`) so the memory is never freed.
/// Process cleanup (and proper stack deallocation) is deferred to a later
/// phase.
fn alloc_kernel_stack() -> u64 {
    // Allocate as a fixed-size array so the allocation is contiguous and
    // properly aligned.  Box::into_raw prevents the destructor from running.
    let stack: alloc::boxed::Box<[u8; KERNEL_STACK_SIZE]> =
        alloc::boxed::Box::new([0u8; KERNEL_STACK_SIZE]);
    let ptr = alloc::boxed::Box::into_raw(stack) as *mut u8;
    // The top of the stack is one byte past the last element, aligned down
    // to a 16-byte boundary for the SysV AMD64 ABI (kernel-stack RSP0 and
    // SYSCALL stack must be 16-byte aligned for call instructions).
    let top = (ptr as u64) + KERNEL_STACK_SIZE as u64;
    top & !15
}

// ---------------------------------------------------------------------------
// Process table
// ---------------------------------------------------------------------------

/// Global process table.  All modifications go through `PROCESS_TABLE.lock()`.
pub static PROCESS_TABLE: Mutex<ProcessTable> = Mutex::new(ProcessTable::new());

/// The kernel's process table — a flat list of all live [`Process`] entries.
pub struct ProcessTable {
    processes: Vec<Process>,
}

impl ProcessTable {
    /// Create an empty process table.
    ///
    /// `const fn` so it can be used to initialise the `static`.
    pub const fn new() -> Self {
        ProcessTable {
            processes: Vec::new(),
        }
    }

    /// Insert `proc` into the table and return its PID.
    pub fn insert(&mut self, proc: Process) -> Pid {
        let pid = proc.pid;
        self.processes.push(proc);
        pid
    }

    /// Find the process with the given PID (immutable borrow).
    pub fn find(&self, pid: Pid) -> Option<&Process> {
        self.processes.iter().find(|p| p.pid == pid)
    }

    /// Find the process with the given PID (mutable borrow).
    pub fn find_mut(&mut self, pid: Pid) -> Option<&mut Process> {
        self.processes.iter_mut().find(|p| p.pid == pid)
    }

    /// Iterate over all live processes.
    pub fn iter(&self) -> impl Iterator<Item = &Process> {
        self.processes.iter()
    }

    /// Remove and return the process with the given PID (reap it).
    ///
    /// Returns `None` if no process with that PID exists.
    pub fn reap(&mut self, pid: Pid) -> Option<Process> {
        if let Some(pos) = self.processes.iter().position(|p| p.pid == pid) {
            Some(self.processes.swap_remove(pos))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Public helper
// ---------------------------------------------------------------------------

/// Create a new process entry with a fresh kernel stack and an allocated PID.
///
/// The process is inserted into [`PROCESS_TABLE`] in the [`ProcessState::Ready`]
/// state but is **not** handed to the scheduler.  Callers are responsible for
/// scheduling the process separately (Phase 12+).
pub fn spawn_process(ppid: Pid, entry_point: u64, user_stack_top: u64) -> Pid {
    let kstack_top = alloc_kernel_stack();
    let pid = alloc_pid();
    let proc = Process {
        pid,
        ppid,
        state: ProcessState::Ready,
        page_table_root: None,
        kernel_stack_top: kstack_top,
        entry_point,
        user_stack_top,
        exit_code: None,
        stop_signal: 0,
        stop_reported: false,
        brk_current: 0,
        mmap_next: 0,
        pgid: pid,
        fd_table: new_fd_table(),
        pending_signals: 0,
        blocked_signals: 0,
        signal_actions: [SignalAction::Default; 32],
        alt_stack_base: 0,
        alt_stack_size: 0,
        alt_stack_flags: 0,
        cwd: String::from("/"),
    };
    PROCESS_TABLE.lock().insert(proc);
    pid
}

/// Create a new process entry with a known page-table root and kernel stack.
///
/// Used by `sys_fork` to register the child process before spawning the
/// fork-child kernel task.  Inherits `brk_current` and `mmap_next` from the
/// parent so the child's heap and mmap state are consistent with the copied
/// address space.
pub fn spawn_process_with_cr3(
    ppid: Pid,
    entry_point: u64,
    user_stack_top: u64,
    cr3: x86_64::PhysAddr,
    brk_current: u64,
    mmap_next: u64,
) -> Pid {
    let kstack_top = alloc_kernel_stack();
    let pid = alloc_pid();
    let proc = Process {
        pid,
        ppid,
        state: ProcessState::Ready,
        page_table_root: Some(cr3),
        kernel_stack_top: kstack_top,
        entry_point,
        user_stack_top,
        exit_code: None,
        stop_signal: 0,
        stop_reported: false,
        brk_current,
        mmap_next,
        pgid: pid,
        fd_table: new_fd_table(),
        pending_signals: 0,
        blocked_signals: 0,
        signal_actions: [SignalAction::Default; 32],
        alt_stack_base: 0,
        alt_stack_size: 0,
        alt_stack_flags: 0,
        cwd: String::from("/"),
    };
    PROCESS_TABLE.lock().insert(proc);
    pid
}

/// Create a new process entry inheriting the parent's FD table.
///
/// Used by `sys_fork` to deep-clone the parent's file descriptors into
/// the child process (Phase 14, P14-T003).
/// `inherit_pgid`: if non-zero, use this as the child's pgid (for fork);
/// if zero, default to the child's own pid (for exec/spawn).
#[allow(clippy::too_many_arguments)]
pub fn spawn_process_with_cr3_and_fds(
    ppid: Pid,
    entry_point: u64,
    user_stack_top: u64,
    cr3: x86_64::PhysAddr,
    brk_current: u64,
    mmap_next: u64,
    fd_table: [Option<FdEntry>; MAX_FDS],
    inherit_pgid: Pid,
) -> Pid {
    let kstack_top = alloc_kernel_stack();
    let pid = alloc_pid();
    let pgid = if inherit_pgid != 0 { inherit_pgid } else { pid };
    let proc = Process {
        pid,
        ppid,
        state: ProcessState::Ready,
        page_table_root: Some(cr3),
        kernel_stack_top: kstack_top,
        entry_point,
        user_stack_top,
        exit_code: None,
        stop_signal: 0,
        stop_reported: false,
        brk_current,
        mmap_next,
        pgid,
        fd_table,
        pending_signals: 0,
        blocked_signals: 0,
        signal_actions: [SignalAction::Default; 32],
        alt_stack_base: 0,
        alt_stack_size: 0,
        alt_stack_flags: 0,
        cwd: String::from("/"),
    };
    PROCESS_TABLE.lock().insert(proc);
    pid
}

// ---------------------------------------------------------------------------
// Foreground process group (Phase 14, Track G)
// ---------------------------------------------------------------------------

/// The PID of the foreground process group. Ctrl-C/Ctrl-Z signals
/// are delivered to all processes in this group.
pub static FG_PGID: AtomicU32 = AtomicU32::new(0);

/// Send a signal to all processes in a process group.
pub fn send_signal_to_group(pgid: Pid, sig: u32) {
    let pids: alloc::vec::Vec<Pid> = {
        let table = PROCESS_TABLE.lock();
        table
            .iter()
            .filter(|p| p.pgid == pgid)
            .map(|p| p.pid)
            .collect()
    };
    for pid in pids {
        send_signal(pid, sig);
    }
}

// ---------------------------------------------------------------------------
// Signal helpers (Phase 14)
// ---------------------------------------------------------------------------

/// Send a signal to a process by PID. Sets the pending bit.
///
/// SIGCONT is special: it also resumes a stopped process.
/// SIGKILL and SIGSTOP cannot be caught or ignored.
pub fn send_signal(pid: Pid, sig: u32) -> bool {
    if sig == 0 || sig > 63 {
        return false;
    }
    let mut table = PROCESS_TABLE.lock();
    let proc = match table.find_mut(pid) {
        Some(p) => p,
        None => return false,
    };

    if sig == SIGCONT {
        // SIGCONT resumes a stopped process.
        if proc.state == ProcessState::Stopped {
            proc.state = ProcessState::Ready;
            log::info!("[signal] SIGCONT → pid {} (resumed)", pid);
        }
        // Clear any pending SIGSTOP/SIGTSTP.
        proc.pending_signals &= !(1u64 << SIGSTOP) & !(1u64 << SIGTSTP);
        return true;
    }

    proc.pending_signals |= 1u64 << sig;
    true
}

/// Check and deliver pending signals for a process.
///
/// Called on the return-to-userspace path. Returns the action to take.
/// Clears the delivered signal's pending bit.
pub fn dequeue_signal(pid: Pid) -> Option<(u32, SignalDisposition)> {
    let mut table = PROCESS_TABLE.lock();
    let proc = table.find_mut(pid)?;

    if proc.pending_signals == 0 {
        return None;
    }

    // Find the lowest-numbered pending signal that is not blocked.
    let deliverable = proc.pending_signals & !proc.blocked_signals;
    if deliverable == 0 {
        return None;
    }
    let sig = deliverable.trailing_zeros();
    if sig >= 64 {
        return None;
    }
    proc.pending_signals &= !(1u64 << sig);

    // Determine disposition.
    let action = if sig < 32 {
        proc.signal_actions[sig as usize]
    } else {
        SignalAction::Default
    };

    let disposition = match action {
        SignalAction::Ignore => {
            // SIGKILL and SIGSTOP cannot be ignored.
            if sig == SIGKILL || sig == SIGSTOP {
                default_signal_action(sig)
            } else {
                SignalDisposition::Ignore
            }
        }
        SignalAction::Default => default_signal_action(sig),
        SignalAction::Handler {
            entry,
            mask,
            flags,
            restorer,
        } => {
            // SIGKILL and SIGSTOP always use default action regardless.
            if sig == SIGKILL || sig == SIGSTOP {
                default_signal_action(sig)
            } else {
                SignalDisposition::UserHandler {
                    entry,
                    mask,
                    flags,
                    restorer,
                }
            }
        }
    };

    Some((sig, disposition))
}

/// Deliver SIGCHLD to the parent of the given child PID.
pub fn send_sigchld_to_parent(child_pid: Pid) {
    let ppid = {
        let table = PROCESS_TABLE.lock();
        table.find(child_pid).map(|p| p.ppid).unwrap_or(0)
    };
    if ppid != 0 {
        send_signal(ppid, SIGCHLD);
    }
}

// ---------------------------------------------------------------------------
// Fork child support
// ---------------------------------------------------------------------------

/// Context passed from `sys_fork` to `fork_child_trampoline`.
struct ForkChildCtx {
    pid: Pid,
    user_rip: u64,
    user_rsp: u64,
}

/// Queue of fork-child contexts, consumed by `fork_child_trampoline`.
///
/// Uses a `VecDeque` for O(1) pop-from-front semantics.
static FORK_CHILD_QUEUE: Mutex<VecDeque<ForkChildCtx>> = Mutex::new(VecDeque::new());

/// Push a fork-child context so `fork_child_trampoline` can consume it.
pub fn push_fork_ctx(pid: Pid, user_rip: u64, user_rsp: u64) {
    FORK_CHILD_QUEUE.lock().push_back(ForkChildCtx {
        pid,
        user_rip,
        user_rsp,
    });
}

/// Kernel-task entry point for a fork child.
///
/// Pops the next fork context from `FORK_CHILD_QUEUE`, switches the process's
/// CR3 if set, updates the kernel stack in TSS/MSR, sets `CURRENT_PID`, then
/// enters ring 3 at the forked RIP with rax=0.
pub fn fork_child_trampoline() -> ! {
    // Pop the context set up by sys_fork / run_elf_and_report.
    let ctx = FORK_CHILD_QUEUE
        .lock()
        .pop_front()
        .expect("fork_child_trampoline: FORK_CHILD_QUEUE is empty — missing push_fork_ctx");

    CURRENT_PID.store(ctx.pid, Ordering::Relaxed);

    // Look up page table root and kernel stack for this process.
    let (cr3_phys, kstack_top) = {
        let table = PROCESS_TABLE.lock();
        let p = table.find(ctx.pid).expect("fork child: process not found");
        (p.page_table_root, p.kernel_stack_top)
    };

    // Update TSS.RSP0 and SYSCALL_STACK_TOP for this process's kernel stack.
    unsafe {
        crate::arch::x86_64::gdt::set_kernel_stack(kstack_top);
        // SAFETY: SYSCALL_STACK_TOP is written once per context switch.
        *(core::ptr::addr_of_mut!(crate::arch::x86_64::syscall::SYSCALL_STACK_TOP)) = kstack_top;
    }

    // If the child has its own page table, switch CR3.
    if let Some(cr3) = cr3_phys {
        unsafe {
            use x86_64::{
                registers::control::{Cr3, Cr3Flags},
                structures::paging::{PhysFrame, Size4KiB},
                PhysAddr,
            };
            let frame: PhysFrame<Size4KiB> =
                PhysFrame::containing_address(PhysAddr::new(cr3.as_u64()));
            Cr3::write(frame, Cr3Flags::empty());
        }
    }
    // Enter ring 3 at the parent's post-fork RIP with rax=0 (child return value).
    unsafe { crate::arch::enter_userspace_with_retval(ctx.user_rip, ctx.user_rsp, 0) }
}

// ---------------------------------------------------------------------------
// HELLO_BIN — kept for main.rs compatibility (remove when main.rs is updated)
// ---------------------------------------------------------------------------

/// Embedded hello-world userspace binary (raw x86_64 machine code).
///
/// When loaded at USER_CODE_BASE (0x400000) and executed in ring 3, this program:
/// 1. Calls sys_debug_print (syscall 12) with "hello world!\n"
/// 2. Calls sys_exit (syscall 6) with exit code 0
///
/// Layout (flat binary, position-independent via RIP-relative string ref):
///   offset  0: mov rax, 12        (B8 0C 00 00 00)
///   offset  5: lea rdi, [rip+20]  (48 8D 3D 14 00 00 00) → points to .msg at offset 32
///   offset 12: mov rsi, 13        (48 C7 C6 0D 00 00 00) → "hello world!\n" = 13 bytes
///   offset 19: syscall            (0F 05)
///   offset 21: mov rax, 6         (B8 06 00 00 00)
///   offset 26: xor edi, edi       (31 FF)
///   offset 28: syscall            (0F 05)
///   offset 30: ud2                (0F 0B)
///   offset 32: "hello world!\n"   (68 65 6C 6C 6F 20 77 6F 72 6C 64 21 0A) 13 bytes
pub const HELLO_BIN: &[u8] = &[
    // offset  0: mov rax, 12  (sys_debug_print)
    0xB8, 0x0C, 0x00, 0x00, 0x00,
    // offset  5: lea rdi, [rip+0x14]  (points to .msg at offset 32; RIP at offset 12)
    0x48, 0x8D, 0x3D, 0x14, 0x00, 0x00, 0x00,
    // offset 12: mov rsi, 13  (length of "hello world!\n")
    0x48, 0xC7, 0xC6, 0x0D, 0x00, 0x00, 0x00, // offset 19: syscall
    0x0F, 0x05, // offset 21: mov rax, 6  (sys_exit)
    0xB8, 0x06, 0x00, 0x00, 0x00, // offset 26: xor edi, edi
    0x31, 0xFF, // offset 28: syscall
    0x0F, 0x05, // offset 30: ud2  (unreachable, safety net)
    0x0F, 0x0B, // offset 32: .msg "hello world!\n"
    b'h', b'e', b'l', b'l', b'o', b' ', b'w', b'o', b'r', b'l', b'd', b'!', b'\n',
];
