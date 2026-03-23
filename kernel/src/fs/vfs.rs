//! VFS routing layer — Phase 8 (`vfs_server` handler logic).
//!
//! # Phase 8 behaviour
//!
//! In Phase 8 the VFS is a pure pass-through: it receives file IPC messages
//! from kernel-task clients and forwards them directly to the single filesystem
//! backend — the in-memory ramdisk ([`crate::fs::ramdisk`]).  There is no
//! path rewriting, no permission checking, and no mount-point selection.
//!
//! # Phase 9+ plans
//!
//! When the project gains a real disk and a writable filesystem, the VFS will
//! consult a mount table to select the correct backend for a given path prefix.
//! For example:
//!
//! - `/`     → ramdisk (read-only initrd)
//! - `/tmp`  → tmpfs backend
//! - `/home` → ext2 / FAT backend over a block device
//!
//! [`handle`] will inspect `msg.data[0]` (the name pointer / fd) together with
//! a registered mount table and dispatch to the appropriate backend's `handle`
//! function.
//!
//! # Why keep a separate `vfs` module in Phase 8?
//!
//! Even though the pass-through is a single call, the separation is valuable:
//!
//! 1. **Routing boundary** — `vfs` owns path dispatch; `ramdisk` owns file
//!    data.  Clients only ever call `vfs::handle`; they never import ramdisk
//!    directly.  This contract is validated by P8-T008.
//!
//! 2. **Zero-cost refactor** — Phase 9 mount-table logic slots in here without
//!    touching `ramdisk.rs` or any client call site.
//!
//! 3. **Test seam** — tests can substitute a mock backend by targeting
//!    `vfs::handle` and verifying routing behaviour independently of the
//!    ramdisk implementation.

#![allow(dead_code)]

use crate::ipc::Message;

/// Handle one `vfs_server` IPC message by routing it to the ramdisk backend.
///
/// # Phase 8 limitation
///
/// There is exactly one backend (the ramdisk), so this is a direct forward.
/// Phase 9+ will inspect the file path and consult a mount table to select
/// the appropriate backend before dispatching.
pub fn handle(msg: &Message) -> Message {
    crate::fs::ramdisk::handle(msg)
}
