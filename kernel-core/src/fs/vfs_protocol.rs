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
//! | [`VFS_OPEN`] | Open file by path | bulk=path, data[0]=flags, data[1]=path_len | data[0]=handle \| (file_size << 32), reply_bulk=`VFS_STAT_REPLY_SIZE` stat header |
//! | [`VFS_READ`] | Read from handle | data[0]=handle, data[1]=offset, data[2]=count | data[0]=bytes_read, reply_bulk=data |
//! | [`VFS_CLOSE`] | Close handle | data[0]=handle | label=0 ack |
//! | [`VFS_STAT_PATH`] | Stat resolved path | bulk=path, data[0]=path_len | reply_bulk=`VFS_STAT_REPLY_SIZE` bytes + optional symlink target |
//! | [`VFS_LIST_DIR`] | List directory entries | bulk=path, data[0]=path_len, data[1]=offset, data[2]=count | data[0]=bytes_written \| (next_offset << 32), reply_bulk=dirent bytes |
//! | [`VFS_ACCESS_PATH`] | Check resolved path existence | bulk=path, data[0]=path_len | label=0 on success |
//! | [`VFS_MOUNT_POLICY`] | Resolve mount policy | bulk=target\|\|fstype, data[0]=target_len, data[1]=fstype_len | data[0]=policy action |
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
/// Reply:   label = 0 on success (negative errno on error),
///          `data[0]` packs the opaque service handle in the low 32 bits and
///          the file size (clamped to `u32::MAX`) in the high 32 bits — the
///          kernel unpacks both to seed the FdBackend::VfsService handle.
///          The low 32 bits are further split by `vfs_server` into a 16-bit
///          generation counter (upper) and a 16-bit slot index (lower); a
///          stale `VFS_CLOSE` whose generation no longer matches the live
///          slot is rejected as `EBADF` without affecting the recycled
///          handle's file.
///
/// Phase 88 (Track B.1/D): the reply ALSO carries a reply bulk of
/// `VFS_STAT_REPLY_SIZE` bytes (the same stat header as `VFS_STAT_PATH`), so the
/// kernel seeds a full, consistent `fstat` snapshot (inode, mode, uid, gid,
/// nlink, size, blocks, a/m/c times) onto the fd at open time instead of
/// resolving the inode a second time through the in-kernel ext2 engine.
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
///
/// Request: bulk = UTF-8 path bytes, `data[0]` = path length,
///          `data[1]` = starting entry offset, `data[2]` = max reply bulk bytes.
/// Reply:   label = 0 on success (negative errno on error),
///          `data[0]` packs `bytes_written` in the low 32 bits and the next
///          entry offset to resume from in the high 32 bits.
///          Reply bulk holds the `getdents64` record bytes.
pub const VFS_LIST_DIR: u64 = 14;

/// Check whether a resolved path exists in the migrated namespace.
pub const VFS_ACCESS_PATH: u64 = 15;

/// Resolve mount policy for a target/fstype pair.
pub const VFS_MOUNT_POLICY: u64 = 16;

/// Resolve umount policy for a target path.
pub const VFS_UMOUNT_POLICY: u64 = 17;

// ---------------------------------------------------------------------------
// Phase 93 — ext2 write authority. These ops make `vfs_server` the SINGLE
// owner of the ext2 root (reads AND writes), eliminating the dual-engine
// read-incoherence hazard (kernel `EXT2_VOLUME` vs vfs_server `Ext2State`).
// The kernel routes mutating ext2 syscalls through these ops when the `vfs`
// service is registered, and falls back to its in-kernel engine otherwise.
//
// All write ops are **path-based** (like `VFS_STAT_PATH`/`VFS_ACCESS_PATH`)
// rather than handle-based, so they share the path-resolution authority the
// service already owns and never need writable open-handle state.
// ---------------------------------------------------------------------------

/// Read file data by resolved path + byte offset.
///
/// Used by the kernel's `Ext2Disk` read path so that a writer reading back its
/// own writable fd sees the coherent vfs_server view (not a stale kernel
/// block-cache snapshot).
///
/// Request: bulk = UTF-8 path bytes, `data[0]` = path length,
///          `data[1]` = byte offset, `data[2]` = max bytes.
/// Reply:   label = 0 on success (negative errno on error),
///          `data[0]` = bytes actually read, reply bulk = file data.
pub const VFS_PREAD: u64 = 18;

/// Write file data by resolved path at a byte offset, allocating blocks as
/// needed and growing the file. Mirrors the kernel engine's `write_file_data`.
///
/// Request: bulk = UTF-8 path bytes followed by the data bytes,
///          `data[0]` = path length, `data[1]` = byte offset,
///          `data[2]` = data length.
/// Reply:   label = 0 on success (negative errno on error),
///          `data[0]` packs `bytes_written` in the low 32 bits and the new
///          file size in the high 32 bits.
pub const VFS_WRITE: u64 = 19;

/// Truncate a file by resolved path to `length` bytes (freeing data blocks
/// when shrinking to zero; growth via subsequent writes).
///
/// Request: bulk = UTF-8 path bytes, `data[0]` = path length,
///          `data[1]` = new length.
/// Reply:   label = 0 on success (negative errno on error).
pub const VFS_TRUNCATE: u64 = 20;

/// Create a regular file, directory, or symlink in a parent directory.
///
/// The IPC message has only four `data` words, so the create parameters are
/// packed:
///
/// Request: bulk = parent path bytes || name bytes || symlink-target bytes,
///          `data[0]` packs parent-path length (low 32) and name length
///          (high 32); `data[1]` packs `mode` (low 16 bits), `kind`
///          (bits 16..18: [`VFS_NODE_FILE`]/[`VFS_NODE_DIR`]/
///          [`VFS_NODE_SYMLINK`]), and symlink-target length (high 32, only for
///          [`VFS_NODE_SYMLINK`]); `data[2]` = uid; `data[3]` = gid.
/// Reply:   label = 0 on success (negative errno on error),
///          `data[0]` = new inode number.
pub const VFS_CREATE: u64 = 21;

/// Remove a directory entry (file or empty directory) by parent + name.
///
/// Request: bulk = parent path bytes || name bytes, `data[0]` = parent path
///          length, `data[1]` = name length, `data[2]` = 1 to require a
///          directory (rmdir), 0 to require a non-directory (unlink).
/// Reply:   label = 0 on success (negative errno on error).
pub const VFS_UNLINK: u64 = 22;

/// Rename/move an entry from `old` to `new` (both absolute resolved paths).
///
/// Request: bulk = old path bytes || new path bytes, `data[0]` = old path
///          length, `data[1]` = new path length.
/// Reply:   label = 0 on success (negative errno on error).
pub const VFS_RENAME: u64 = 23;

/// Hard-link `new` to the existing target inode at the resolved `target` path.
///
/// Request: bulk = target path bytes || new-parent path bytes || new-name
///          bytes, `data[0]` = target path length, `data[1]` = new-parent
///          path length, `data[2]` = new-name length.
/// Reply:   label = 0 on success (negative errno on error).
pub const VFS_LINK: u64 = 24;

/// Set inode attributes (chmod / chown / utimes) through the `vfs_server` — the
/// single ext2 write owner — so the change is coherent with the server's own
/// block cache and the kernel path-metadata (stat) cache it backs (Phase 89). A
/// direct kernel-engine inode write would leave the server's cached inode block
/// stale (the dual-engine hazard Phase 88 eliminated for data writes).
///
/// Only `data[0..3]` carry payload — the IPC engine reserves `data[3]` for the
/// reply-cap handle it hands the receiver. uid/gid pack to 16 bits each (the
/// ext2 inode width) and ctime is set to "now" by the server, so neither needs a
/// dedicated word.
///
/// Request: bulk = path bytes; `data[0]` = path length;
///          `data[1]` = `(uid << 48) | (gid << 32) | (mode << 16) | mask`
///          (uid/gid/mode/mask each 16 bits; `mask` selects which fields to
///          apply — see `VFS_SETATTR_*`);
///          `data[2]` = `(atime << 32) | mtime`.
/// Reply:   label = 0 on success (negative errno on error).
pub const VFS_SETATTR: u64 = 25;

/// `VFS_SETATTR` field-mask bits — which attributes the request applies.
pub const VFS_SETATTR_MODE: u64 = 1 << 0;
pub const VFS_SETATTR_UID: u64 = 1 << 1;
pub const VFS_SETATTR_GID: u64 = 1 << 2;
pub const VFS_SETATTR_ATIME: u64 = 1 << 3;
pub const VFS_SETATTR_MTIME: u64 = 1 << 4;

/// Node-kind selector encoded in `VFS_CREATE` `data[2]` bits 16..18.
pub const VFS_CREATE_KIND_SHIFT: u32 = 16;

/// Legacy 4 KiB (one ext2 block) size baseline. Phase 87 split the read-reply cap
/// into `VFS_MAX_PREAD` (64 KiB) and the write-request cap into `VFS_MAX_PWRITE`,
/// so `VFS_READ`/`VFS_PREAD` replies are no longer bounded by this constant — it
/// now only serves as the minimum/sanity floor those larger caps are validated
/// against (see the asserts below).
pub const VFS_MAX_READ: usize = 4096;

/// Phase 87 — maximum bytes per single `VFS_PREAD` reply bulk payload.
///
/// Decoupled from (and larger than) `VFS_MAX_READ`: a read reply travels in the
/// unbounded bulk `Vec` (capped only by the IPC `MAX_BULK_LEN` = 80 KiB), not in
/// the small fixed request buffer, so reads can be served in 64 KiB chunks
/// instead of one 4 KiB block per round-trip. With vfs_server's contiguous-run
/// coalescing this collapses a multi-MiB read into a few multi-block requests.
/// Must stay `<= MAX_BULK_LEN` and 512-aligned.
pub const VFS_MAX_PREAD: usize = 64 * 1024;

/// Phase 87 — maximum bytes per single `VFS_WRITE` request (path + data),
/// raised from `VFS_MAX_READ` (4 KiB). The write request packs the path AND the
/// data into one bulk buffer that lands in vfs_server's `recv_buf`, so this also
/// sizes that buffer (now heap-allocated, not a stack array). A 64 KiB write
/// chunk lets vfs_server write up to ~16 blocks per request — 16x fewer IPC
/// round-trips, and the per-`write_file_data` inode flush is amortized over the
/// whole chunk instead of per 4 KiB block. Must stay `<= MAX_BULK_LEN` (80 KiB)
/// and 512-aligned.
pub const VFS_MAX_PWRITE: usize = 64 * 1024;

/// Reply-bulk size for `VFS_STAT_PATH` (and the `VFS_OPEN` reply bulk — Phase 88
/// Track B.1/D: the same header is returned on open so the kernel can seed a
/// complete, consistent `fstat` snapshot onto the fd without a second ext2
/// resolve).
///
/// Base layout: 12 little-endian `u64` values:
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
/// 12. blocks (count of 512-byte blocks allocated — `st_blocks`)
///
/// If `node kind == VFS_NODE_SYMLINK`, the reply bulk appends the raw symlink
/// target bytes immediately after this fixed-size header.
pub const VFS_STAT_REPLY_SIZE: usize = 12 * core::mem::size_of::<u64>();

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
    fn write_op_labels_are_unique() {
        // Phase 93 — the ext2-write authority ops must not collide with each
        // other or with any pre-existing label.
        let labels = [
            VFS_OPEN,
            VFS_READ,
            VFS_CLOSE,
            VFS_STAT_PATH,
            VFS_LIST_DIR,
            VFS_ACCESS_PATH,
            VFS_MOUNT_POLICY,
            VFS_UMOUNT_POLICY,
            VFS_PREAD,
            VFS_WRITE,
            VFS_TRUNCATE,
            VFS_CREATE,
            VFS_UNLINK,
            VFS_RENAME,
            VFS_LINK,
        ];
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b, "VFS protocol labels must be distinct");
            }
        }
    }

    #[test]
    fn max_read_is_block_aligned() {
        assert!(VFS_MAX_READ > 0);
        assert_eq!(VFS_MAX_READ % 512, 0);
    }

    #[test]
    fn max_pread_is_valid() {
        // 512-aligned, larger than a single 4 KiB block, and within the IPC
        // bulk-reply ceiling (MAX_BULK_LEN = 80 KiB in kernel/src/ipc/mod.rs).
        assert_eq!(VFS_MAX_PREAD % 512, 0);
        assert!(VFS_MAX_PREAD >= VFS_MAX_READ);
        assert!(VFS_MAX_PREAD <= 81920);
    }

    #[test]
    fn max_pwrite_is_valid() {
        // The write-side analog of `max_pread_is_valid`. VFS_MAX_PWRITE sizes
        // vfs_server's heap `recv_buf` (MAX_BULK_BUF) and bounds the write chunk
        // (`data.len().min(VFS_MAX_PWRITE - path_len)`); it must stay 512-aligned
        // and within the IPC bulk ceiling (MAX_BULK_LEN = 80 KiB) so a path+data
        // request can never overflow the receive buffer.
        assert_eq!(VFS_MAX_PWRITE % 512, 0);
        assert!(VFS_MAX_PWRITE >= VFS_MAX_READ);
        assert!(VFS_MAX_PWRITE <= 81920);
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
