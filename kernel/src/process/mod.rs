//! Minimal process abstraction for Phase 5.
//!
//! A `Process` holds the virtual addresses needed to enter userspace and
//! any cleanup information. Full process lifecycle (spawn, wait, exit)
//! is deferred to Phase 6+.
//!
//! # Integration note
//! Add `mod process;` (or `pub mod process;`) to `kernel/src/main.rs` to
//! include this module in the kernel crate.

pub use crate::mm::user_space::{USER_CODE_BASE, USER_STACK_TOP};

/// A minimal userspace process descriptor.
pub struct Process {
    /// Virtual address of the process entry point.
    pub entry: u64,
    /// Virtual address of the process stack top.
    pub stack_top: u64,
}

impl Process {
    /// Create a new process descriptor.
    pub fn new(entry: u64, stack_top: u64) -> Self {
        Process { entry, stack_top }
    }
}

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
    // mov rax, 12  (sys_debug_print)
    0xB8, 0x0C, 0x00, 0x00, 0x00,
    // lea rdi, [rip+0x14]  (points to .msg at offset 32; RIP here = 12)
    0x48, 0x8D, 0x3D, 0x14, 0x00, 0x00, 0x00,
    // mov rsi, 13  (length of "hello world!\n")
    0x48, 0xC7, 0xC6, 0x0D, 0x00, 0x00, 0x00,
    // syscall
    0x0F, 0x05,
    // mov rax, 6  (sys_exit)
    0xB8, 0x06, 0x00, 0x00, 0x00,
    // xor edi, edi
    0x31, 0xFF,
    // syscall
    0x0F, 0x05,
    // ud2  (unreachable, safety net)
    0x0F, 0x0B,
    // .msg: "hello world!\n"
    b'h', b'e', b'l', b'l', b'o', b' ', b'w', b'o', b'r', b'l', b'd', b'!', b'\n',
];
