# Phase 28 — ext2 Filesystem

## Overview

Phase 28 replaces the FAT32 permissions overlay with a native ext2
filesystem implementation. ext2 stores Unix ownership (uid/gid),
permission modes, and timestamps directly in inodes, eliminating the
`.m3os_permissions` index file workaround from Phase 27.

The implementation covers ext2 revision 0 with 4K blocks, direct +
single-indirect + double-indirect block pointers, linked-list directory
entries, and block/inode bitmap management. The ext2 partition is
mounted at `/` and serves as the primary persistent filesystem.

## On-Disk Structures

All on-disk structures are defined in `kernel-core/src/fs/ext2.rs` for
host testability. Each struct has a `parse(bytes)` constructor that
validates the input and a `write_into(buf)` method for serialization.

### Superblock

Located at byte offset 1024 from the start of the partition (always
within the first two sectors regardless of block size). Parsed by
`Ext2Superblock::parse()`, which validates `magic == 0xEF53` and
rejects `log_block_size > 2` (only 1K/2K/4K blocks supported).

Key fields:

| Field | Purpose |
|---|---|
| `inodes_count` / `blocks_count` | Total inodes and blocks on the volume |
| `free_blocks_count` / `free_inodes_count` | Updated on every alloc/free |
| `log_block_size` | Block size = `1024 << log_block_size` (always 2 for 4K) |
| `blocks_per_group` / `inodes_per_group` | Block group geometry |
| `first_data_block` | 0 for 4K blocks, 1 for 1K blocks |
| `magic` | `0xEF53` |
| `rev_level` | 0 for original ext2; Rev 1 fields read if >= 1 |
| `first_ino` / `inode_size` | Rev 0 defaults: 11 / 128 bytes |

Derived: `block_group_count = (blocks_count - first_data_block).div_ceil(blocks_per_group)`.

### Block Group Descriptor

32 bytes per group. The descriptor table is stored in the block
immediately after the superblock. Parsed by
`Ext2BlockGroupDescriptor::parse_table()`.

| Field | Purpose |
|---|---|
| `block_bitmap` | Block number of this group's block bitmap |
| `inode_bitmap` | Block number of this group's inode bitmap |
| `inode_table` | Block number of the first block of the inode table |
| `free_blocks_count` / `free_inodes_count` | Per-group free counts |
| `used_dirs_count` | Number of directories in this group |

### Inode

128 bytes (Rev 0). Stores all file metadata natively — no overlay
needed.

| Field | Purpose |
|---|---|
| `mode` (u16) | File type (upper 4 bits) + permissions (lower 12 bits) |
| `uid` (u16) / `gid` (u16) | Owner identity |
| `size` (u32) | File size in bytes |
| `atime` / `ctime` / `mtime` / `dtime` (u32) | Timestamps |
| `links_count` (u16) | Hard link count |
| `blocks` (u32) | Count of 512-byte blocks allocated |
| `block` ([u32; 15]) | 12 direct + 1 indirect + 1 double-indirect + 1 triple-indirect |

Helper methods: `is_dir()`, `is_regular()`, `is_symlink()`,
`permission_mode()`, `file_type()`.

Inode numbering is 1-based. Inode 2 is always the root directory.
Location helpers:

```
block_group  = (inode_num - 1) / inodes_per_group
index_in_grp = (inode_num - 1) % inodes_per_group
```

### Directory Entry

Variable-length linked-list format. Each entry:

| Field | Size | Purpose |
|---|---|---|
| `inode` | 4 bytes | Target inode number (0 = deleted) |
| `rec_len` | 2 bytes | Total entry size including padding |
| `name_len` | 1 byte | Actual name length |
| `file_type` | 1 byte | 1=regular, 2=directory, 7=symlink |
| `name` | variable | Up to 255 bytes, no null terminator |

`Ext2DirEntry::parse_block()` walks a raw block by advancing `rec_len`
bytes at a time. Entries with `inode == 0` are skipped (deleted).

## Block Pointer Hierarchy

File data is located through a three-level block pointer scheme in the
inode's `block[15]` array:

```
block[0..11]   → 12 direct block pointers        (up to 48K with 4K blocks)
block[12]      → single-indirect block            (+4M: 1024 pointers × 4K)
block[13]      → double-indirect block            (+4G: 1024² pointers × 4K)
block[14]      → triple-indirect (NOT IMPLEMENTED)
```

`resolve_block(inode, logical_block)` traverses this hierarchy.
A physical block number of 0 indicates a sparse (hole) block — reads
return zeros. With 4K blocks and double-indirect support, the maximum
file size is approximately 4 GB (far exceeding the 64 MB partition).

Triple-indirect is declared (`EXT2_TIND_BLOCK = 14`) but returns an
error if reached.

## Bitmap Management

Block and inode bitmaps are one block each per block group. Each bit
represents one block or inode (1 = used, 0 = free).

### Allocation

`allocate_block(preferred_group)` and `allocate_inode(preferred_group)`:

1. Start scanning at the preferred group (typically the parent
   directory's group)
2. Wrap around through all groups if the preferred group is full
3. Scan the bitmap for the first clear bit
4. Set the bit, write the bitmap back to disk
5. Decrement free counts in both the block group descriptor and
   superblock
6. Call `flush_metadata()` to persist changes

### Deallocation

`free_block(block_num)` and `free_inode(inode_num)`:

1. Compute the group and bit index
2. Detect double-free (bit already clear)
3. Clear the bit, write bitmap back
4. Increment free counts
5. Call `flush_metadata()`

### Metadata Flush

`flush_metadata()` writes the updated superblock (at partition offset
1024) and the block group descriptor table back to disk. Called after
every allocation/deallocation.

## Ext2Volume

The kernel-side driver (`kernel/src/fs/ext2.rs`) wraps all I/O in the
`Ext2Volume` struct:

```rust
pub struct Ext2Volume {
    base_lba: u64,
    pub superblock: Ext2Superblock,
    pub bgd_table: Vec<Ext2BlockGroupDescriptor>,
    pub block_size: u32,
    sectors_per_block: u32,
    superblock_raw: Vec<u8>,
}
```

A global `EXT2_VOLUME: Mutex<Option<Ext2Volume>>` holds the mounted
volume.

### Mount

`Ext2Volume::mount(base_lba)`:

1. Read 2 sectors at `base_lba + 2` (byte offset 1024)
2. Parse superblock, validate magic
3. Compute BGD table location: block 1 for 4K blocks (since the
   superblock fits within block 0)
4. Read and parse the block group descriptor table

### Block I/O

All disk access goes through two functions:

```rust
fn read_block(&self, block_num: u32) -> Result<Vec<u8>, Ext2Error>
fn write_block(&self, block_num: u32, data: &[u8]) -> Result<(), Ext2Error>
```

These convert block numbers to LBAs (`base_lba + block_num * sectors_per_block`) and delegate to `crate::blk::read_sectors` /
`write_sectors` (the virtio-blk driver).

## File Operations

### Read

`read_file_data(inode, offset, buf)` iterates block by block, calling
`resolve_block` for each logical block, reading the physical block, and
copying bytes into the caller's buffer. Handles partial first/last
blocks and EOF.

### Write

`write_file_data(inode_num, inode, offset, data)` allocates new blocks
as needed via `allocate_data_block`, does read-modify-write for partial
blocks, and updates `inode.size` if writing past EOF.

### Create File

`create_file(parent_inode_num, name, mode, uid, gid)`:

1. Allocate a new inode (prefer the parent's block group)
2. Initialize: `S_IFREG | mode`, `links_count = 1`, timestamps
3. Write the inode
4. Add a directory entry in the parent

### Create Directory

`create_directory(parent_inode_num, name, mode, uid, gid)`:

1. Check for `AlreadyExists`
2. Allocate inode with `links_count = 2`
3. Allocate one data block, write `.` and `..` entries
4. Add entry in parent, increment parent's `links_count`
5. Increment `used_dirs_count` in the block group descriptor

### Delete File

`delete_file(parent_inode_num, name)`:

1. Resolve the child inode
2. Decrement `links_count`
3. If count reaches 0: truncate (free all data blocks), free inode
4. Remove the directory entry

### Delete Directory

`delete_directory(parent_inode_num, name)`:

1. Verify the directory is empty (only `.` and `..`)
2. Truncate and free inode
3. Remove directory entry
4. Decrement parent's `links_count` and `used_dirs_count`

### Truncate

`truncate_file(inode_num, inode)` frees all data blocks through all
three levels of indirection (direct, indirect children + indirect block,
double-indirect children + intermediate blocks + dind block). Sets
`size = 0`, `blocks = 0`.

## Directory Operations

### Entry Addition

`add_directory_entry` searches existing blocks for slack space within
entries (where `rec_len` exceeds the actual entry size). If space is
found, the existing entry is shrunk and the new entry fills the gap.
If no slack exists, a new block is allocated.

### Entry Removal

`remove_directory_entry` finds the target entry. If it is not the first
entry in its block, the previous entry's `rec_len` is extended to absorb
it (standard ext2 deletion). If it is the first entry, the `inode` field
is zeroed.

### Path Resolution

`resolve_path(path)` splits on `/`, starts from inode 2 (root), and
walks each component via `lookup_in_directory`. Handles `.` (skip) and
`..` (parent). Returns the final inode number.

## Metadata Operations

ext2 stores uid, gid, and mode natively in inodes:

- `metadata(path)` — resolves path, reads inode, returns
  `(uid, gid, mode, size, mtime)`
- `set_metadata(path, uid, gid, mode)` — resolves path, updates inode
  fields preserving the file type bits (upper 4 bits of mode), writes
  inode back

These are called by the `chmod` and `chown` syscalls via
`get_ext2_meta()` and the `data_chmod` / `data_chown` helpers in
`syscall.rs`. No `.m3os_permissions` overlay is needed.

## VFS Integration

### FD Backend

The `FdBackend` enum in `kernel/src/process/mod.rs` has an `Ext2Disk`
variant:

```rust
Ext2Disk {
    path: String,
    inode_num: u32,
    file_size: u32,
    parent_inode: u32,
}
```

### Path Routing

The syscall layer routes filesystem operations via `ext2_root_path()`,
which excludes `/tmp/*` (always tmpfs) and maps everything else to ext2
when mounted:

1. `/tmp/*` — tmpfs
2. `/data/*` (ext2 mounted) — ext2 (legacy path compatibility)
3. Any non-`/tmp` path (ext2 mounted, not on ramdisk) — ext2
4. Ramdisk paths (`/bin/*`, `/sbin/*`) — read-only ramdisk
5. Write/create on non-ramdisk path — ext2 (fallback creation)

### Mount Syscall

`sys_linux_mount` with `fstype = "ext2"` at mountpoint `/`:

1. Calls `crate::blk::mbr::probe_ext2()` to find the ext2 partition LBA
2. Calls `crate::fs::ext2::mount_ext2(base_lba)`
3. Stores the volume in `EXT2_VOLUME`

### Root Directory Listing

`getdents64` at `/` merges entries from ext2 root + ramdisk overlays
(deduplicated via `BTreeSet`) + virtual mount points (`/tmp`, `/data`).

## kernel-core vs kernel Split

| Layer | Contents |
|---|---|
| `kernel-core/src/fs/ext2.rs` | On-disk structs, `parse`/`write_into` methods, inode location helpers, `Ext2Error`, constants, host-side unit tests |
| `kernel/src/fs/ext2.rs` | `Ext2Volume`, all I/O methods, `EXT2_VOLUME` global, bitmap management, file/directory CRUD, metadata ops |

The split ensures all parsing logic is testable on the host via
`cargo test -p kernel-core`. The kernel crate adds hardware access
(virtio-blk reads/writes via `crate::blk`).

## Block Device Interface

ext2 accesses disk through the virtio-blk driver:

```
crate::blk::read_sectors(lba: u64, count: usize, buf: &mut [u8])
crate::blk::write_sectors(lba: u64, count: usize, data: &[u8])
```

The driver uses the legacy virtio 0.9.5 I/O port interface via PCI
BAR0. `VIRTIO_BLK_READY: AtomicBool` gates I/O until PCI init
completes.

Block-to-LBA conversion: `lba = base_lba + block_num * sectors_per_block`.

## Disk Image Creation

`xtask/src/main.rs` creates the ext2 disk image at build time:

1. Create a 64 MB zeroed `disk.img`
2. Write MBR partition table: type `0x83` (Linux), start LBA 2048
3. Extract partition area to a temp file
4. Format: `mkfs.ext2 -b 4096 -L m3data -O none -r 0 -q`
5. Populate via `debugfs -w`:
   - Create directories: `bin`, `sbin`, `etc`, `root`, `home`,
     `home/user`, `tmp`, `var`, `dev`
   - Write `/etc/passwd`, `/etc/shadow`, `/etc/group`
   - Set inode uid/gid/mode via `sif` commands (e.g.,
     `sif etc/shadow mode 0x8180` for 0600, `sif home/user uid 1000`)
6. Validate with `e2fsck -n -f` (read-only check)
7. Copy partition back into `disk.img` at the correct offset

The image is attached to QEMU as `-drive file=disk.img,format=raw,if=virtio`.

If `disk.img` already exists, xtask preserves it to maintain data
persistence across rebuilds.

## How ext2 Replaces the FAT32 Overlay

| Aspect | Phase 27 (FAT32) | Phase 28 (ext2) |
|---|---|---|
| Permission storage | `.m3os_permissions` index file | Native inode fields |
| Ownership storage | In-memory `BTreeMap` + index file | Native `uid`/`gid` in inode |
| Persistence | Write index file on every chmod/chown | Write inode block on change |
| Default metadata | Hardcoded `(0, 0, 0o755)` if not in index | Always from inode |
| Timestamp support | None | `mtime` in inode |
| Image creation | `mkfs.fat` | `mkfs.ext2` + `debugfs` |
| Partition type | `0x0B` (FAT32) | `0x83` (Linux) |

The FAT32 driver and overlay code are retained as a fallback but are
not used when ext2 is mounted.

## Known Limitations

- **No journaling** — unclean shutdown may leave the filesystem
  inconsistent (ext3/ext4 journaling is deferred)
- **No triple-indirect blocks** — files larger than ~4 GB are not
  supported (irrelevant on a 64 MB partition)
- **No symlink resolution** — `is_symlink()` exists but no follow logic
- **No ftruncate via fd** — truncation only works via `O_TRUNC` on open
- **No 64-bit file sizes** — `size_high` is parsed but not combined
  with `size`
- **No HTree directories** — linear scan only (fine for small
  directories)
- **No extended attributes** — no xattr, no ACLs beyond rwxrwxrwx
- **Single superblock copy** — no backup superblocks read or maintained
- **Block size limited to 4K** — 1K and 2K are parsed but untested
