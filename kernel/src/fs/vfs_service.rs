//! VFS-service teardown surface (Phase 66 Track D.4).
//!
//! The ring-3 VFS-service IPC plumbing still lives inside
//! `kernel/src/arch/x86_64/syscall/mod.rs`. This module exists so the
//! *public* teardown wrapper called by `kernel/src/process/mod.rs` no
//! longer reaches into the arch-specific syscall path.
//!
//! Carving the rest of the vfs-service module out of `syscall/mod.rs` is
//! a follow-up phase. For now `vfs_service_close_pub` forwards to the
//! existing private implementation.
#![allow(dead_code)]

/// Tell the ring-3 VFS service that `service_handle` has been closed.
/// Called after the process layer has confirmed under `PROCESS_TABLE`
/// that the handle being closed was the last alias.
pub fn vfs_service_close_pub(service_handle: u64) {
    crate::arch::x86_64::syscall::vfs_service_close_internal(service_handle);
}
