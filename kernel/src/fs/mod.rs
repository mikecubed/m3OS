//! Filesystem services — Phase 8.
//!
//! Provides a VFS routing layer and a read-only ramdisk backend so kernel
//! tasks can open and read files by name without touching hardware I/O.
//!
//! # Module layout
//!
//! - [`protocol`] — IPC message labels and data conventions shared by all
//!   filesystem servers and their clients.
//! - [`ramdisk`] — static embedded files and the `fat_server` message handler.
//! - [`vfs`] — path routing and the `vfs_server` message handler.
//!
//! # Phase 8 scope
//!
//! All filesystem servers are kernel tasks sharing the kernel address space.
//! No ring-3 processes, no real disk I/O, no writeable filesystem.  The goal
//! is to validate the IPC contract and the VFS / backend split; moving to
//! real storage is Phase 9+.

pub mod protocol;
pub mod ramdisk;
pub mod vfs;
