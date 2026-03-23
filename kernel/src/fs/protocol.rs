//! File service IPC protocol — Phase 8.
//!
//! Defines the message labels and data layout shared by `vfs_server` and
//! `fat_server` (and any future filesystem backends).  All three operations
//! use the Phase 6 [`Message`] type directly; no heap allocation is required
//! on the IPC path.
//!
//! # Operation labels (sent by clients to `vfs_server`)
//!
//! | Label | Operation | Request data | Reply data |
//! |---|---|---|---|
//! | [`FILE_OPEN`] | Open a file by path | `data[0]`=name ptr, `data[1]`=name len | `data[0]`=fd, or `u64::MAX` on error |
//! | [`FILE_READ`] | Read bytes from an open file | `data[0]`=fd, `data[1]`=offset, `data[2]`=max len | `data[0]`=content ptr (0=error), `data[1]`=actual bytes (0=EOF or error) |
//! | [`FILE_CLOSE`] | Close a file descriptor | `data[0]`=fd | label=0 (ack) |
//!
//! # Phase 8 limitations
//!
//! - File descriptors are simply file-table indices, not per-process handles.
//!   All clients share the same namespace (acceptable because Phase 8 has only
//!   kernel tasks, all in the same address space).
//! - `FILE_READ` returns a pointer into the static ramdisk content rather than
//!   copying bytes into a client-owned buffer.  This only works while clients
//!   share the kernel address space.  Phase 9+ will introduce page-capability
//!   grants so ring-3 clients can receive data safely.
//! - `FILE_CLOSE` is a no-op in Phase 8 (fds are stateless indices).

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// Operation labels (client → vfs_server)
// ---------------------------------------------------------------------------

/// Open a file by name.
///
/// Request: `data[0]` = kernel pointer to UTF-8 name bytes,
///          `data[1]` = name length (≤ [`MAX_NAME_LEN`]).
/// Reply:   `data[0]` = file descriptor (u64 index), or `u64::MAX` on error.
pub const FILE_OPEN: u64 = 1;

/// Read bytes from an open file.
///
/// Request: `data[0]` = fd, `data[1]` = byte offset, `data[2]` = max bytes.
/// Reply:   `data[0]` = pointer to content (kernel address),
///          `data[1]` = actual bytes available from offset
///                      (capped to max bytes and file length; 0 at EOF),
///          or `data[0]` = 0 (null ptr) on error (bad fd or offset past end).
///          Note: `data[1]` = 0 alone is NOT an error — it indicates EOF.
///          Always check `data[0]` (the pointer) to distinguish error from EOF.
pub const FILE_READ: u64 = 2;

/// Close a file descriptor.
///
/// Request: `data[0]` = fd (ignored in Phase 8 — fds are stateless).
/// Reply:   label = 0 (ack).
pub const FILE_CLOSE: u64 = 3;

// ---------------------------------------------------------------------------
// Constraints
// ---------------------------------------------------------------------------

/// Maximum byte length of a filename accepted by the file service.
pub const MAX_NAME_LEN: usize = 64;

/// Maximum number of bytes returned by a single `FILE_READ` call.
pub const MAX_READ_LEN: usize = 4096;
