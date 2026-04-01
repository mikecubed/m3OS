# Phase 28 — ext2 Filesystem: Task List

**Status:** Complete
**Source Ref:** phase-28
**Depends on:** Phase 24 (Persistent Storage) ✅, Phase 27 (User Accounts) ✅
**Goal:** Replace FAT32 as the primary persistent filesystem with ext2, providing
native Unix ownership (uid/gid), permission modes, and timestamps in every inode.
File permissions survive reboots without the FAT32 `.m3os_permissions` workaround.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | kernel-core ext2 data structures and parsing | — | ✅ Done |
| B | Inode reading and block pointer traversal | A | ✅ Done |
| C | Directory entry parsing and path resolution | B | ✅ Done |
| D | Read-only file access through VFS | C | ✅ Done |
| E | Block/inode bitmap management and allocation | B | ✅ Done |
| F | File and directory write operations | D, E | ✅ Done |
| G | VfsMetadata trait implementation (chmod/chown) | D | ✅ Done |
| H | Build system: ext2 image creation | — | ✅ Done |
| I | VFS mount integration and FAT32 removal | F, G, H | ✅ Done (FAT32 overlay kept as fallback) |
| J | Validation and acceptance testing | I | ✅ Done |

### Implementation Notes

- **ext2 revision 0**: Original ext2 specification. No journal (ext3), no extents
  (ext4), no extended attributes. Sufficient for a toy OS.
- **Block size**: 4096 bytes (4K). Most common ext2 block size, simplifies page alignment.
- **Block pointers**: Direct blocks (0-11), single-indirect (12), double-indirect (13).
  Handles files up to ~64 MB with 4K blocks. Triple-indirect deferred.
- **Directory format**: Linked-list directory entries (original ext2 format). No htree
  indexing -- linear scan is fine for our directory sizes.
- **Bitmap management**: Block and inode bitmaps kept in memory for active block groups.
- **Superblock writeback**: Free block/inode counts updated on every allocation/deallocation.
- **No crash recovery**: ext2 has no journal. Unclean shutdown may leave filesystem
  inconsistent. Acceptable for a toy OS.
- **Single block group initially**: 64 MB partition with 4K blocks yields ~2 block groups.

## Prerequisite Analysis

Current state (post-Phase 27):
- FAT32 volume driver in `kernel/src/fs/fat32.rs` with full read/write support
- `.m3os_permissions` index file overlay for Unix metadata on FAT32
- `VfsMetadata` trait defined in VFS layer -- designed for ext2 forward-compatibility
- `check_permission()` helper in VFS uses only the trait, not backend internals
- Block device abstraction: `crate::blk::read_sectors()` / `write_sectors()` via virtio-blk
- MBR partition parsing in `kernel-core/src/fs/mbr.rs`
- xtask builds a 64 MB FAT32 disk image with MBR partition table

---

## Track A — kernel-core ext2 Data Structures and Parsing

Define ext2 on-disk structures in `kernel-core` so they are host-testable.

### A.1 — Define `Ext2Superblock` struct

**File:** `kernel-core/src/fs/ext2.rs`
**Symbol:** `Ext2Superblock`
**Why it matters:** The superblock contains all filesystem geometry needed to locate every other structure on disk.

**Acceptance:**
- [x] Matches on-disk layout: `inodes_count`, `blocks_count`, `free_blocks_count`, `free_inodes_count`, `first_data_block`, `log_block_size`, `blocks_per_group`, `inodes_per_group`, `magic` (0xEF53), etc.
- [x] Total size: 1024 bytes at byte offset 1024 on disk

### A.2 — Define `Ext2BlockGroupDescriptor` struct

**File:** `kernel-core/src/fs/ext2.rs`
**Symbol:** `Ext2BlockGroupDescriptor`
**Why it matters:** Block group descriptors locate the bitmaps and inode tables for each block group.

**Acceptance:**
- [x] 32-byte struct: `block_bitmap`, `inode_bitmap`, `inode_table`, `free_blocks_count`, `free_inodes_count`, `used_dirs_count`, padding

### A.3 — Define `Ext2Inode` struct

**File:** `kernel-core/src/fs/ext2.rs`
**Symbol:** `Ext2Inode`
**Why it matters:** Inodes store all file metadata and block pointers -- they are the core ext2 data unit.

**Acceptance:**
- [x] 128-byte struct (rev 0): `mode`, `uid`, `size`, `atime`/`ctime`/`mtime`/`dtime`, `gid`, `links_count`, `blocks`, `flags`, `block[15]`

### A.4 — Define `Ext2DirEntry` struct

**File:** `kernel-core/src/fs/ext2.rs`
**Symbol:** `Ext2DirEntry`
**Why it matters:** Directory entries link filenames to inodes and must be parsed with proper alignment handling.

**Acceptance:**
- [x] `inode` (u32), `rec_len` (u16), `name_len` (u8), `file_type` (u8), variable-length `name`

### A.5 — Implement superblock parsing

**File:** `kernel-core/src/fs/ext2.rs`
**Symbol:** `Ext2Superblock::parse`
**Why it matters:** Invalid superblocks must be detected early to prevent cascading errors.

**Acceptance:**
- [x] Validates magic (0xEF53), extracts fields, computes derived values
- [x] `Ext2Error` enum defined with `BadMagic`, `UnsupportedRevision`, `InvalidBlockSize`, `IoError`, `OutOfSpace`, `NotFound`, etc.

### A.6 — Implement block group descriptor parsing

**File:** `kernel-core/src/fs/ext2.rs`
**Symbol:** `Ext2BlockGroupDescriptor::parse`, `parse_table`
**Why it matters:** The descriptor table is needed to locate bitmaps and inode tables for every block group.

**Acceptance:**
- [x] Single descriptor and table parsing from byte slices

### A.7 — Implement inode parsing with helpers

**File:** `kernel-core/src/fs/ext2.rs`
**Symbol:** `Ext2Inode::parse`, `is_dir`, `is_regular`, `permission_mode`, `file_type`
**Why it matters:** Inode type and permission helpers are used throughout the VFS and permission enforcement layers.

**Acceptance:**
- [x] Parse from byte slice; helper methods for type and permission checks

### A.8 — Host-side unit tests

**File:** `kernel-core/src/fs/ext2.rs` (tests module)
**Why it matters:** Parsing correctness is critical -- a single off-by-one corrupts every file access.

**Acceptance:**
- [x] Tests for superblock, block group descriptor, inode, and directory entry parsing
- [x] Error cases: bad magic, truncated input, zero-length entries
- [x] All pass via `cargo test -p kernel-core`

---

## Track B — Inode Reading and Block Pointer Traversal

Implement the core logic for reading inodes and following block pointers to
locate file data.

### B.1 — Implement inode location helpers

**File:** `kernel-core/src/fs/ext2.rs`
**Symbol:** `inode_block_group`, `inode_index_in_group`
**Why it matters:** Correct 1-based inode number translation is needed to locate any inode on disk.

**Acceptance:**
- [x] Inode N in block group `(N-1) / inodes_per_group`, index `(N-1) % inodes_per_group`
- [x] Unit tests pass

### B.2 — Implement `read_inode()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `read_inode`
**Why it matters:** Every file and directory access begins with reading the target inode from disk.

**Acceptance:**
- [x] Uses block group descriptor to find inode table, reads sectors, parses inode

### B.3 — Implement direct block pointer resolution

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `read_data_block`
**Why it matters:** Direct blocks handle the first 48 KiB of every file, which covers most small files entirely.

**Acceptance:**
- [x] Logical blocks 0-11 return `inode.block[logical_block]`; block 0 means sparse hole

### B.4 — Implement single-indirect resolution

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** Single-indirect extends file capacity from 48 KiB to ~4 MiB.

**Acceptance:**
- [x] Reads indirect block from `inode.block[12]`, indexes into it
- [x] Indirect block cached for sequential access

### B.5 — Implement double-indirect resolution

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** Double-indirect extends file capacity to ~64 MiB, needed for large source files and libraries.

**Acceptance:**
- [x] Reads double-indirect block from `inode.block[13]`, then indirect, then data block

### B.6 — Implement `read_file_data()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `read_file_data`
**Why it matters:** This is the main data read path used by every file read syscall.

**Acceptance:**
- [x] Reads bytes from file at given offset into buffer
- [x] Handles partial first/last blocks and EOF

---

## Track C — Directory Entry Parsing and Path Resolution

Build directory traversal on top of inode reading.

### C.1 — Implement `read_directory_entries()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `read_directory_entries`
**Why it matters:** Directory listing (ls) and path lookup both depend on parsing the directory entry linked list.

**Acceptance:**
- [x] Reads directory inode data blocks, parses variable-length `Ext2DirEntry` records
- [x] Skips deleted entries (inode == 0)

### C.2 — Implement `lookup_in_directory()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `lookup_in_directory`
**Why it matters:** Single-component directory lookup is the building block for full path resolution.

**Acceptance:**
- [x] Scans entries for matching name, returns inode number or `NotFound`

### C.3 — Implement `resolve_path()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `resolve_path`
**Why it matters:** Every file operation starts with resolving a path string to an inode number.

**Acceptance:**
- [x] Splits path on `/`, resolves from root inode (inode 2)
- [x] Handles absolute paths, `.`, `..`

### C.4 — Implement `stat_inode()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `stat_inode`
**Why it matters:** The stat syscall needs to populate FileMetadata from native ext2 inode fields.

**Acceptance:**
- [x] Returns `FileMetadata` with uid, gid, mode, size, mtime from ext2 inode

---

## Track D — Read-Only File Access Through VFS

Wire ext2 reading into the VFS layer so userspace can open and read ext2 files.

### D.1 — Define `Ext2Volume` and mount

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `Ext2Volume`
**Why it matters:** The volume struct holds all mounted filesystem state and is the entry point for all ext2 operations.

**Acceptance:**
- [x] Holds `base_lba`, parsed superblock, block group descriptors, block size
- [x] `Ext2Volume::mount(base_lba)` validates superblock and initializes volume

### D.2 — Register ext2 as VFS backend

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `EXT2_VOLUME`
**Why it matters:** The VFS must route path-based requests to the correct filesystem backend.

**Acceptance:**
- [x] Global `Mutex<Option<Ext2Volume>>` registered for path prefix routing

### D.3 — Implement ext2 open

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** Opening files creates the file descriptor entries that all subsequent I/O uses.

**Acceptance:**
- [x] Resolves path, reads inode, creates FD entry with inode number and offset 0

### D.4 — Implement ext2 read

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** File reading is the most common filesystem operation.

**Acceptance:**
- [x] Reads bytes via `read_file_data()`, advances offset

### D.5 — Implement ext2 close

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** Proper close releases FD entries for reuse.

**Acceptance:**
- [x] Releases FD entry

### D.6 — Implement ext2 readdir

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** Directory listing is needed for `ls` and shell tab completion.

**Acceptance:**
- [x] Returns list of names for a directory inode; wired into FILE_LIST handler

### D.7 — Implement ext2 fstat

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** Stat must return real ext2 metadata for permission enforcement and `ls -l` display.

**Acceptance:**
- [x] Populates stat struct with real uid, gid, mode, size, mtime from ext2 inode

---

## Track E — Block and Inode Bitmap Management

Implement allocation and freeing of blocks and inodes for write support.

### E.1 — Implement `read_block_bitmap()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `read_block_bitmap`
**Why it matters:** The block bitmap tracks which data blocks are free for allocation.

**Acceptance:**
- [x] Reads block bitmap for a block group; each bit = one block (1=used, 0=free)

### E.2 — Implement `allocate_block()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `allocate_block`
**Why it matters:** Every file write that extends a file needs new data blocks.

**Acceptance:**
- [x] Scans bitmap for free bit, sets it, updates descriptor and superblock free counts
- [x] Returns `OutOfSpace` if no free blocks

### E.3 — Implement `free_block()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `free_block`
**Why it matters:** Deleting files and truncating must return blocks to the free pool.

**Acceptance:**
- [x] Clears bitmap bit, updates free counts

### E.4 — Implement `read_inode_bitmap()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `read_inode_bitmap`
**Why it matters:** The inode bitmap tracks which inodes are available for new file creation.

**Acceptance:**
- [x] Reads inode bitmap for a block group

### E.5 — Implement `allocate_inode()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `allocate_inode`
**Why it matters:** Creating files and directories requires allocating new inodes.

**Acceptance:**
- [x] Scans inode bitmap, sets bit, updates free counts; tries specified group first

### E.6 — Implement `free_inode()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `free_inode`
**Why it matters:** Deleting files must return inodes to the free pool.

**Acceptance:**
- [x] Clears inode bitmap bit, updates free counts

### E.7 — Implement `write_superblock()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `write_superblock`
**Why it matters:** Updated free counts must be persisted to disk for consistency across reboots.

**Acceptance:**
- [x] Flushes superblock and block group descriptor table to disk

---

## Track F — File and Directory Write Operations

Implement creating, writing, and deleting files and directories on ext2.

### F.1 — Implement `write_inode()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `write_inode`
**Why it matters:** Every metadata change (permissions, size, timestamps) requires writing the inode back to disk.

**Acceptance:**
- [x] Serializes inode to bytes and writes to correct inode table location

### F.2 — Implement `allocate_data_block()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `allocate_data_block`
**Why it matters:** Growing files requires allocating and linking new data blocks into the inode's block pointer hierarchy.

**Acceptance:**
- [x] Allocates block and assigns to direct, indirect, or double-indirect slot
- [x] Allocates indirect/double-indirect blocks as needed

### F.3 — Implement `write_file_data()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `write_file_data`
**Why it matters:** This is the main data write path used by the write syscall.

**Acceptance:**
- [x] Writes data at offset, allocates blocks as needed, updates inode size

### F.4 — Implement `add_directory_entry()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `add_directory_entry`
**Why it matters:** Creating files and directories requires adding entries to the parent directory.

**Acceptance:**
- [x] Finds space in directory data blocks (reuses padding or allocates new block)
- [x] Updates directory inode size

### F.5 — Implement `create_file()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `create_file`
**Why it matters:** File creation is needed for `open(O_CREAT)`, `touch`, and editor save-as.

**Acceptance:**
- [x] Allocates inode, initializes metadata, adds directory entry, returns inode number

### F.6 — Implement `create_directory()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `create_directory`
**Why it matters:** `mkdir` creates directories with proper `.` and `..` entries.

**Acceptance:**
- [x] Allocates inode, allocates data block with `.` and `..` entries, updates parent link count

### F.7 — Implement `truncate_file()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `truncate_file`
**Why it matters:** Overwriting a file with `O_TRUNC` must free all existing data blocks.

**Acceptance:**
- [x] Frees all data, indirect, and double-indirect blocks; sets size and blocks to 0

### F.8 — Implement `remove_directory_entry()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `remove_directory_entry`
**Why it matters:** Deleting files requires removing the directory entry and merging rec_len with the previous entry.

**Acceptance:**
- [x] Sets inode to 0, merges rec_len per standard ext2 deletion

### F.9 — Implement `delete_file()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `delete_file`
**Why it matters:** File deletion is needed for `rm`, `unlink`, and temporary file cleanup.

**Acceptance:**
- [x] Truncates, frees inode, removes directory entry; only frees inode if link count reaches 0

### F.10 — Implement `delete_directory()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `delete_directory`
**Why it matters:** `rmdir` must verify the directory is empty before removing it.

**Acceptance:**
- [x] Verifies directory empty (only `.` and `..`), deletes, decrements parent link count

### F.11 — Wire ext2 write operations into VFS

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** The VFS layer dispatches file write/create/delete syscalls to the ext2 backend.

**Acceptance:**
- [x] FILE_WRITE, FILE_CREATE, FILE_DELETE, FILE_MKDIR, FILE_RMDIR handlers implemented

---

## Track G — VfsMetadata Trait Implementation (chmod/chown)

Implement native ext2 metadata operations -- no overlay file needed.

### G.1 — Implement `VfsMetadata::metadata()` for ext2

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** Permission enforcement reads file metadata through this trait method.

**Acceptance:**
- [x] Returns native uid, gid, mode, size, mtime from ext2 inode (no `.m3os_permissions`)

### G.2 — Implement `set_metadata()` for ext2

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** chmod and chown must persist changes directly in the ext2 inode.

**Acceptance:**
- [x] Resolves path to inode, updates uid/gid/mode, writes inode back to disk

### G.3 — Implement `set_timestamps()` for ext2

**File:** `kernel/src/fs/ext2.rs`
**Why it matters:** Accurate mtime is critical for `make` incremental builds (Phase 32).

**Acceptance:**
- [x] Updates mtime field in inode on write operations

### G.4 — Verify fstat returns correct ext2 metadata

**Component:** fstat syscall with ext2 backend
**Why it matters:** Programs like `ls -l` and `stat` must show real ownership and permissions.

**Acceptance:**
- [x] uid, gid, mode, size, mtime all come from ext2 inode via VfsMetadata trait

---

## Track H — Build System: ext2 Image Creation

Update xtask to create ext2 disk images instead of FAT32.

### H.1 — Replace `mkfs.fat` with `mkfs.ext2`

**File:** `xtask/src/main.rs`
**Why it matters:** The data partition must be ext2 format for native Unix metadata support.

**Acceptance:**
- [x] `mkfs.ext2 -b 4096 -L m3data` creates 64 MB ext2 filesystem

### H.2 — Update MBR partition type

**File:** `xtask/src/main.rs`
**Why it matters:** The kernel scans MBR partition types to find and mount the correct filesystem.

**Acceptance:**
- [x] Partition type byte changed from 0x0B (FAT32) to 0x83 (Linux/ext2)

### H.3 — Populate ext2 image with initial files

**File:** `xtask/src/main.rs`
**Why it matters:** System configuration files must exist with correct ownership and permissions at boot.

**Acceptance:**
- [x] `/etc/passwd`, `/etc/shadow`, `/etc/group` with correct content
- [x] `/root` (mode 0o700, uid=0), `/home/user` (mode 0o755, uid=1000, gid=1000)

### H.4 — Verify ext2 image validity

**Component:** Build system
**Why it matters:** A corrupted filesystem image would cause boot failures or data corruption.

**Acceptance:**
- [x] `e2fsck -n` passes on the built image

---

## Track I — VFS Mount Integration and FAT32 Cleanup

Switch the running kernel to mount ext2 instead of FAT32 and remove the
permissions overlay workaround.

### I.1 — Update MBR partition scanning

**File:** `kernel-core/src/fs/mbr.rs`
**Why it matters:** The kernel must recognize ext2 partition type 0x83 during boot.

**Acceptance:**
- [x] `find_partition()` recognizes type 0x83 as ext2

### I.2 — Update kernel boot filesystem init

**File:** `kernel/src/main.rs` (boot sequence)
**Why it matters:** The kernel must mount the ext2 partition as the primary data filesystem.

**Acceptance:**
- [x] Scans MBR for ext2 partition, mounts as `Ext2Volume` at `/data`
- [x] Falls back to FAT32 for backwards compatibility

### I.3 — Update VFS routing

**File:** `kernel/src/fs/vfs.rs`
**Why it matters:** Path-based routing must direct requests to the new ext2 backend.

**Acceptance:**
- [x] `/data/*` requests routed to ext2 backend

### I.4 — Remove FAT32 permissions overlay

**File:** `kernel/src/fs/fat32.rs`
**Why it matters:** The overlay is no longer needed since ext2 stores permissions natively.

**Acceptance:**
- [x] `.m3os_permissions` handling removed from FAT32 (or kept as fallback for FAT32-only setups)

### I.5 — Update init

**File:** `userspace/init/src/main.rs`
**Why it matters:** Init chmod calls for FAT32 overlay are unnecessary with native ext2 permissions.

**Acceptance:**
- [x] FAT32-specific init-time chmod calls removed; tmpfs/ramdisk chmod retained

### I.6 — Verify existing filesystem paths

**Component:** End-to-end VFS path validation
**Why it matters:** All user-facing file paths must continue to work with the new backend.

**Acceptance:**
- [x] `/data/etc/passwd`, file creation, reading, writing, deletion all work on ext2

---

## Track J — Validation and Acceptance Testing

### J.1 — ext2 mount acceptance

**Acceptance:**
- [x] Boot mounts ext2 partition at `/data`; kernel log shows superblock parsed with magic 0xEF53

### J.2 — Password file acceptance

**Acceptance:**
- [x] `cat /etc/passwd` works; `cat /etc/shadow` denied for non-root

### J.3 — File metadata acceptance

**Acceptance:**
- [x] `ls -l`/fstat shows correct uid, gid, mode from ext2 inodes

### J.4 — Persistence acceptance

**Acceptance:**
- [x] Files created with editor survive reboot

### J.5 — chmod persistence acceptance

**Acceptance:**
- [x] `chmod 600 /etc/shadow` modifies ext2 inode mode; persists across reboot

### J.6 — chown persistence acceptance

**Acceptance:**
- [x] `chown user:user` modifies ext2 inode uid/gid; persists across reboot

### J.7 — Permission enforcement acceptance

**Acceptance:**
- [x] Phase 27 permission enforcement works identically on ext2

### J.8 — Host mount acceptance

**Acceptance:**
- [x] ext2 image mountable on host with `mount -o loop`; `ls -la` shows correct ownership/permissions

### J.9 — Lint and format

**Acceptance:**
- [x] `cargo xtask check` passes

### J.10 — kernel-core tests

**Acceptance:**
- [x] `cargo test -p kernel-core` passes (all ext2 parsing tests)

### J.11 — QEMU boot validation

**Acceptance:**
- [x] Full login cycle, file operations, permission enforcement work without panics

### J.12 — Documentation

**File:** `docs/28-ext2-filesystem.md`
**Why it matters:** Documents the ext2 on-disk layout and VFS integration for future maintainers.

**Acceptance:**
- [x] Covers ext2 layout, superblock/inode/directory formats, block pointers, bitmap management, VFS integration

---

## Deferred Until Later

These items are explicitly out of scope for Phase 28:

- **ext3/ext4 journaling** -- crash recovery via write-ahead log
- **Triple-indirect block pointers** -- only needed for files > 64 MB
- **Extended attributes (xattr)** -- ACLs, SELinux labels, etc.
- **Directory hash tree indexing** (htree) -- for large directories
- **Sparse superblock backups** -- backup superblocks in select block groups
- **Block group locality heuristics** -- allocate near parent directory
- **Online filesystem resize**
- **`e2fsck` / filesystem repair tool** -- would require a userspace utility
- **Symbolic links** -- stored in inode (short) or data blocks (long)
- **Hard link support beyond basic `links_count`** tracking
- **File timestamps beyond mtime** -- atime/ctime update policies
- **Large file support** (>2 GB via `size_high` in rev 1 inodes)
- **File holes / sparse file optimization**

---

## Documentation Notes

- Phase 28 replaces the FAT32 `.m3os_permissions` overlay from Phase 27 with
  native ext2 inode metadata, making file ownership and permissions first-class
  filesystem features.
- The `VfsMetadata` trait from Phase 27 is now implemented natively by ext2,
  validating the forward-compatible design.
- The ext2 data structure definitions in `kernel-core` are host-testable via
  `cargo test -p kernel-core`.
