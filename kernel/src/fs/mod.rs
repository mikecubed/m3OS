//! Filesystem services — Phase 8 / Phase 13.
//!
//! Provides a VFS routing layer, a read-only ramdisk backend, and a
//! writable in-memory tmpfs so kernel tasks and userspace processes can
//! open, read, write, and manage files.
//!
//! # Module layout
//!
//! - [`protocol`] — IPC message labels and data conventions shared by all
//!   filesystem servers and their clients.
//! - [`ramdisk`] — static embedded files and the `fat_server` message handler.
//! - [`tmpfs`] — RAM-backed writable filesystem mounted at `/tmp` (Phase 13).
//! - [`vfs`] — path routing and the `vfs_server` message handler.

pub mod fat32;
pub mod protocol;
pub mod ramdisk;
pub mod tmpfs;
pub mod vfs;
