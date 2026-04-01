# Phase 28 — ext2 Filesystem

**Status:** Complete
**Source Ref:** phase-28
**Depends on:** Phase 24 (Persistent Storage) ✅, Phase 27 (User Accounts) ✅
**Builds on:** Replaces the FAT32 backend from Phase 24 with ext2; implements the VfsMetadata trait from Phase 27 natively via ext2 inodes
**Primary Components:** kernel/src/fs/ext2, kernel-core/src/fs/, xtask image builder

## Milestone Goal

Replace FAT32 as the primary persistent filesystem with ext2, which natively
stores Unix ownership (uid/gid), permission modes, and timestamps in every inode.
After this phase, file permissions set via `chmod`/`chown` survive reboots and the
FAT32 permissions index file workaround from Phase 27 is no longer needed.

## Why This Phase Exists

FAT32 has no concept of Unix file ownership or permissions. Phase 27 worked around
this with a `.m3os_permissions` overlay file, but that approach is fragile and
non-standard. ext2 stores uid, gid, mode, and timestamps directly in each inode,
making permission persistence a natural property of the filesystem rather than a
bolted-on workaround. Implementing ext2 also teaches the canonical Unix filesystem
layout (superblock, block groups, inode tables, bitmaps) which underpins ext3,
ext4, and many other modern filesystems.

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

## Important Components and How They Work

### ext2 On-Disk Layout

The filesystem is divided into block groups, each containing a copy of the
superblock (in group 0), a block group descriptor table, block and inode bitmaps,
an inode table, and data blocks. The superblock at group 0 holds global metadata:
block size, inode size, inodes per group, blocks per group, and free counts.

### Inode Structure

Each inode stores uid, gid, mode, size, timestamps (mtime), and block pointers.
Direct blocks (0-11) point to data blocks directly. Block 12 is a single-indirect
pointer (points to a block of block pointers). Block 13 is a double-indirect
pointer. This supports files up to ~64 MB with 4K blocks.

### Directory Entries

Directories are stored as linked lists of variable-length entries within data
blocks. Each entry contains an inode number, entry length, name length, file
type, and name. Path resolution walks these entries to map names to inode numbers.

### Block/Inode Bitmap Management

In-memory copies of the block and inode bitmaps track allocation state. On file
creation, free bits are located and set; on deletion, bits are cleared. The
superblock's free counts are updated on each allocation/deallocation and written
back on sync/unmount.

### VfsMetadata Trait Implementation

ext2 implements the `VfsMetadata` trait by returning native inode fields directly
(uid, gid, mode, size, mtime), eliminating the need for the FAT32 permissions
overlay file.

## How This Builds on Earlier Phases

- **Replaces Phase 24 (Persistent Storage):** swaps the FAT32 filesystem backend for ext2 on the same virtio-blk device
- **Extends Phase 27 (User Accounts):** implements the VfsMetadata trait natively, replacing the FAT32 permissions index file workaround
- **Reuses Phase 24 (Persistent Storage):** continues to use the virtio-blk driver and MBR partition parsing
- **Extends kernel-core:** adds host-testable ext2 data structure definitions and parsing

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

- [Phase 28 Task List](./tasks/28-ext2-filesystem-tasks.md)

## How Real OS Implementations Differ

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
- Our implementation covers the core ext2 specification (revision 0) which is
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
