//! IPC syscall handlers.
//!
//! Userspace IPC syscalls use numbers `0x1100..=0x1109`, which are translated
//! to internal IPC dispatch numbers `1..=10`.  This avoids colliding with
//! Linux-compatible syscall numbers (1=write, 2=open, etc.).
//!
//! The dispatch is now handled inline in the flat match table in `mod.rs`
//! for QEMU TCG performance.  This file is retained for documentation.
