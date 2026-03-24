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
    /// FD 0 stdin — reads return EAGAIN until stdin integration (Track E).
    Stdin,
    /// Read-only static ramdisk file (pointer + length into kernel .rodata).
    Ramdisk {
        content_addr: usize,
        content_len: usize,
    },
    /// Writable tmpfs file, identified by its path (e.g. "foo/bar.txt"
    /// relative to tmpfs root — no leading `/tmp/`).
    Tmpfs { path: String },
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
    /// Per-process file descriptor table (Phase 14).
    ///
    /// FDs 0/1/2 are stdin/stdout/stderr.  `fork()` deep-clones this table.
    pub fd_table: [Option<FdEntry>; MAX_FDS],
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
            brk_current: 0,
            mmap_next: 0,
            fd_table: new_fd_table(),
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
        brk_current: 0,
        mmap_next: 0,
        fd_table: new_fd_table(),
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
        brk_current,
        mmap_next,
        fd_table: new_fd_table(),
    };
    PROCESS_TABLE.lock().insert(proc);
    pid
}

/// Create a new process entry inheriting the parent's FD table.
///
/// Used by `sys_fork` to deep-clone the parent's file descriptors into
/// the child process (Phase 14, P14-T003).
pub fn spawn_process_with_cr3_and_fds(
    ppid: Pid,
    entry_point: u64,
    user_stack_top: u64,
    cr3: x86_64::PhysAddr,
    brk_current: u64,
    mmap_next: u64,
    fd_table: [Option<FdEntry>; MAX_FDS],
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
        brk_current,
        mmap_next,
        fd_table,
    };
    PROCESS_TABLE.lock().insert(proc);
    pid
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
