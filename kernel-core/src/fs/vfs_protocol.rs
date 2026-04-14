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
//! | [`VFS_STAT_PATH`] | Stat resolved path | bulk=path, data[0]=path_len | reply_bulk=`VFS_STAT_REPLY_SIZE` bytes + optional symlink target |
//! | [`VFS_LIST_DIR`] | List directory entries | bulk=path, data[0]=path_len, data[1]=offset, data[2]=count | data[0]=packed(next_offset, bytes), reply_bulk=dirent bytes |
//! | [`VFS_ACCESS_PATH`] | Check resolved path existence | bulk=path, data[0]=path_len | label=0 on success |
//! | [`VFS_MOUNT_POLICY`] | Resolve mount policy | bulk=target||fstype, data[0]=target_len, data[1]=fstype_len | data[0]=policy action |
//! | [`VFS_UMOUNT_POLICY`] | Resolve umount policy | bulk=target, data[0]=target_len | data[0]=policy action |
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

/// Stat a resolved path.
pub const VFS_STAT_PATH: u64 = 13;

/// Serialize one `getdents64` batch for a resolved directory path.
pub const VFS_LIST_DIR: u64 = 14;

/// Check whether a resolved path exists in the migrated namespace.
pub const VFS_ACCESS_PATH: u64 = 15;

/// Resolve mount policy for a target/fstype pair.
pub const VFS_MOUNT_POLICY: u64 = 16;

/// Resolve umount policy for a target path.
pub const VFS_UMOUNT_POLICY: u64 = 17;

/// Maximum bytes per single VFS_READ reply bulk payload.
pub const VFS_MAX_READ: usize = 4096;

/// Reply-bulk size for `VFS_STAT_PATH`.
///
/// Base layout: 11 little-endian `u64` values:
/// 1. node kind
/// 2. mode
/// 3. uid
/// 4. gid
/// 5. inode number
/// 6. size
/// 7. nlink
/// 8. blksize
/// 9. atime
/// 10. mtime
/// 11. ctime
///
/// If `node kind == VFS_NODE_SYMLINK`, the reply bulk appends the raw symlink
/// target bytes immediately after this fixed-size header.
pub const VFS_STAT_REPLY_SIZE: usize = 11 * core::mem::size_of::<u64>();

pub const VFS_NODE_FILE: u64 = 1;
pub const VFS_NODE_DIR: u64 = 2;
pub const VFS_NODE_SYMLINK: u64 = 3;

pub const VFS_MOUNT_EXT2_ROOT: u64 = 1;
pub const VFS_MOUNT_VFAT_DATA: u64 = 2;
pub const VFS_UMOUNT_EXT2_ROOT: u64 = 3;
pub const VFS_UMOUNT_VFAT_DATA: u64 = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_distinct() {
        assert_ne!(VFS_OPEN, VFS_READ);
        assert_ne!(VFS_OPEN, VFS_CLOSE);
        assert_ne!(VFS_READ, VFS_CLOSE);
        assert_ne!(VFS_STAT_PATH, VFS_LIST_DIR);
        assert_ne!(VFS_ACCESS_PATH, VFS_MOUNT_POLICY);
        assert_ne!(VFS_MOUNT_POLICY, VFS_UMOUNT_POLICY);
    }

    #[test]
    fn max_read_is_block_aligned() {
        assert!(VFS_MAX_READ > 0);
        assert_eq!(VFS_MAX_READ % 512, 0);
    }

    #[test]
    fn stat_reply_is_word_aligned() {
        assert_eq!(VFS_STAT_REPLY_SIZE % core::mem::size_of::<u64>(), 0);
    }

    #[test]
    fn node_kinds_and_policy_actions_are_distinct() {
        assert_ne!(VFS_NODE_FILE, VFS_NODE_DIR);
        assert_ne!(VFS_NODE_FILE, VFS_NODE_SYMLINK);
        assert_ne!(VFS_NODE_DIR, VFS_NODE_SYMLINK);
        assert_ne!(VFS_MOUNT_EXT2_ROOT, VFS_MOUNT_VFAT_DATA);
        assert_ne!(VFS_UMOUNT_EXT2_ROOT, VFS_UMOUNT_VFAT_DATA);
    }
}
