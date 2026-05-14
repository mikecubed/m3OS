//! Epoll subsystem public surface (Phase 66 Track D.2).
//!
//! Today the epoll instance table and most of the syscall handlers still
//! live in `kernel/src/arch/x86_64/syscall/mod.rs`. This module exists so
//! that the *public* teardown wrapper called by `kernel/src/process/mod.rs`
//! no longer reaches into the arch-specific syscall path — keeping
//! `process` decoupled from the arch layer.
//!
//! Carving the rest of the epoll subsystem out of `syscall/mod.rs` is a
//! follow-up phase. For now `epoll_free_pub` forwards to the existing
//! implementation by way of `pub(crate)` visibility on `epoll_free`.
#![allow(dead_code)]

/// Decrement the epoll instance's refcount; free the slot when it
/// reaches zero. Called from process-cleanup paths in
/// `kernel/src/process/mod.rs` (close-on-exec and full-fd-table
/// teardown).
pub fn epoll_free_pub(instance_id: usize) {
    crate::arch::x86_64::syscall::epoll_free_internal(instance_id);
}
