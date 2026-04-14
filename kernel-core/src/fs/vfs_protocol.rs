//! VFS service IPC protocol — Phase 54.
//!
//! Defines the message labels and data layout shared between the kernel
//! syscall handler (which acts as the IPC client on behalf of userspace apps)
//! and the ring-3 `vfs_server` process.
//!
//! # Operation labels
//!
//! | Label | Operation | Request | Reply |
//! |---|---|---|---|
//! | [`VFS_OPEN`] | Open file by path | bulk=path, data[0]=flags, data[1]=path_len | data[0]=handle |
//! | [`VFS_READ`] | Read from handle | data[0]=handle, data[1]=offset, data[2]=count | data[0]=bytes_read, reply_bulk=data |
//! | [`VFS_CLOSE`] | Close handle | data[0]=handle | label=0 ack |
//!
//! # Reply bulk data
//!
//! `VFS_READ` replies carry file content via the IPC reply-bulk mechanism
//! (Phase 54).  The server stores data in its `pending_bulk` slot before
//! replying; `endpoint::reply()` transfers it to the caller.

/// Open a file by path (read-only for Phase 54 first slice).
///
/// Request: bulk = UTF-8 path bytes, `data[0]` = open flags, `data[1]` = path length.
/// Reply:   label = 0 on success (negative errno on error), `data[0]` = opaque service handle.
pub const VFS_OPEN: u64 = 10;

/// Read bytes from an open handle.
///
/// Request: `data[0]` = handle, `data[1]` = byte offset, `data[2]` = max bytes.
/// Reply:   label = 0 on success (negative errno on error),
///          `data[0]` = bytes actually read,
///          reply bulk = file data.
pub const VFS_READ: u64 = 11;

/// Close a handle.
///
/// Request: `data[0]` = handle.
/// Reply:   label = 0 (ack).
pub const VFS_CLOSE: u64 = 12;

/// Maximum bytes per single VFS_READ reply bulk payload.
pub const VFS_MAX_READ: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_distinct() {
        assert_ne!(VFS_OPEN, VFS_READ);
        assert_ne!(VFS_OPEN, VFS_CLOSE);
        assert_ne!(VFS_READ, VFS_CLOSE);
    }

    #[test]
    fn max_read_is_block_aligned() {
        assert!(VFS_MAX_READ > 0);
        assert_eq!(VFS_MAX_READ % 512, 0);
    }
}
