//! Userspace process management — Phase 11.
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
//! Process cleanup (freeing kernel stacks, reaping page tables) is
//! deferred to a later phase.

#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use spin::Mutex;

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
    /// Top of the userspace stack virtual address.
    pub user_stack_top: u64,
    /// Exit code written when the process transitions to [`ProcessState::Zombie`].
    pub exit_code: Option<i32>,
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
    // SAFETY: ptr is valid for KERNEL_STACK_SIZE bytes; we never free it.
    // The top of the stack is one byte past the last element.
    (ptr as u64) + KERNEL_STACK_SIZE as u64
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
    };
    PROCESS_TABLE.lock().insert(proc);
    pid
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
