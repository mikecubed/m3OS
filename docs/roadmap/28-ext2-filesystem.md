# Phase 28 - ext2 Filesystem

## Milestone Goal

Replace FAT32 as the primary persistent filesystem with ext2, which natively
stores Unix ownership (uid/gid), permission modes, and timestamps in every inode.
After this phase, file permissions set via `chmod`/`chown` survive reboots and the
FAT32 permissions index file workaround from Phase 27 is no longer needed.

## Learning Goals

- Understand on-disk filesystem layout: superblock, block group descriptors,
  inode tables, block/inode bitmaps.
- Learn how inodes map file metadata (permissions, ownership, size, timestamps)
  to data blocks via direct, indirect, and double-indirect pointers.
- See how directory entries are stored as linked lists within data blocks.
- Understand block allocation strategies and free-space management.

## Feature Scope

### Kernel Changes

- **ext2 superblock parsing**: read and validate the superblock at block group 0.
  Extract block size, inode size, inodes per group, blocks per group, etc.
- **Block group descriptor table**: parse the descriptor table to locate inode
  tables and bitmaps for each block group.
- **Inode reading**: read inodes by number, extracting uid, gid, mode, size,
  timestamps, and block pointers (direct, indirect, double-indirect).
- **Directory traversal**: parse directory entries (linked-list format) to
  resolve paths to inode numbers.
- **File reading**: follow block pointers to read file data. Support direct
  blocks (0-11), single-indirect (12), and double-indirect (13). Triple-indirect
  is deferred (handles files up to ~64 MB with 4K blocks, sufficient for a toy OS).
- **Block and inode allocation**: maintain in-memory copies of block and inode
  bitmaps. Allocate/free blocks and inodes on file creation/deletion.
- **File writing**: allocate data blocks, update inode block pointers, update
  file size. Support append and overwrite.
- **Directory creation and deletion**: allocate inodes for new directories,
  create `.` and `..` entries, link into parent directory.
- **File deletion**: free data blocks and the inode, remove directory entry,
  update bitmaps.
- **Implement `VfsMetadata` trait** (from Phase 27): return native inode metadata
  (uid, gid, mode, size, mtime) directly — no overlay needed.
- **chmod/chown**: update inode fields directly on disk.
- **Mount as `/data`**: replace the FAT32 mount with ext2 on the same virtio-blk
  device.
- **Superblock writeback**: update free block/inode counts in the superblock
  on allocation/deallocation. Write back on sync/unmount.

### Userspace Changes

- No userspace changes required — ext2 is transparent to userspace programs.
  The VFS layer and `VfsMetadata` trait abstract the filesystem backend.

### Build System Changes

- **xtask image builder**: use `mkfs.ext2` (host tool) to create the ext2
  partition in the disk image instead of `mkfs.fat`. Populate `/etc/passwd`,
  `/etc/shadow`, `/etc/group`, and home directories on the ext2 image.
- **Partition layout**: either replace the FAT32 partition with ext2, or use a
  second partition (MBR supports 4 primary partitions). Single ext2 partition is
  simpler.
- Remove the FAT32 permissions index file (`.m3os_permissions`) — no longer
  needed.

### kernel-core Changes

- Add ext2 data structure definitions to `kernel-core/src/fs/`: superblock,
  block group descriptor, inode, directory entry. These are host-testable.
- Add parsing and serialization functions with unit tests.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 24 (Persistent Storage) | virtio-blk driver and block device abstraction |
| Phase 27 (User Accounts) | VfsMetadata trait that ext2 implements natively |

## Implementation Outline

1. Add ext2 on-disk structure definitions to `kernel-core` with unit tests.
2. Implement superblock and block group descriptor parsing.
3. Implement inode reading and block pointer traversal (direct + indirect).
4. Implement directory entry parsing and path resolution.
5. Implement read-only file access through the VFS layer.
6. Implement block/inode bitmap management (allocation and freeing).
7. Implement file creation, writing, truncation, and deletion.
8. Implement directory creation and deletion.
9. Implement `VfsMetadata` trait: chmod/chown modify inode fields directly.
10. Update xtask to create ext2 images with `mkfs.ext2`.
11. Mount ext2 as `/data` instead of FAT32.
12. Migrate `/etc/passwd`, `/etc/shadow`, `/etc/group` to ext2.
13. Remove FAT32 permissions index file code.

## Acceptance Criteria

- Boot mounts an ext2 partition at `/data` (or `/`).
- Files created with `touch`/`edit` are stored on ext2 and survive reboot.
- `ls -l` (or fstat) shows correct uid, gid, and permission mode from ext2 inodes.
- `chmod 600 /etc/shadow` persists across reboot.
- `chown user:user /home/user/file` persists across reboot.
- Permission enforcement from Phase 27 works identically with ext2 backend.
- `cargo xtask check` passes.
- The ext2 image can be mounted and inspected on the host with standard Linux
  tools (`mount -o loop`, `debugfs`, `ls -la`).
- No regressions in existing functionality.

## Companion Task List

- [Phase 28 Task List](./tasks/28-ext2-filesystem-tasks.md) *(to be created)*

## How Real OS Implementations Differ

Real ext2/3/4 implementations have:
- **ext3/ext4 journaling**: crash recovery via a write-ahead log. ext2 has no
  journal — an unclean shutdown requires `e2fsck` to repair.
- **Extent trees** (ext4): replace block pointer lists for better large-file
  performance.
- **Triple-indirect blocks**: ext2 supports triple-indirect for files >64 MB.
  We defer this — double-indirect handles up to ~64 MB with 4K blocks.
- **Sparse superblock**: backup superblocks only in certain block groups.
- **Directory indexing** (ext3+ htree): hash-based directory lookup for large
  directories. We use linear scan.
- **Extended attributes** (xattr): ACLs, SELinux labels, etc.
- **Block group locality**: real implementations try to allocate files near
  their directory's block group for locality. We use simple first-fit.
- **Delayed allocation** (ext4): allocate blocks at writeback time, not at
  write time, for better layout.
- **Online resize and defragmentation**.

Our implementation covers the core ext2 specification (revision 0) which is
sufficient for persistent Unix-style file storage with native permissions.

## Deferred Until Later

- ext3/ext4 journaling
- Triple-indirect block pointers
- Extended attributes (xattr)
- Directory hash tree indexing
- Sparse superblock backups
- Block group locality heuristics
- Online filesystem resize
- `fsck` / filesystem repair tool
- Symbolic links (stored in inode for short links, in data blocks for long)
- Hard link count management beyond basic tracking
- File timestamps beyond mtime (atime, ctime updates)
