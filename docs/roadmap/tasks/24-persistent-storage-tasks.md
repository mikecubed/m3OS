# Phase 24 — Persistent Storage: Task List

**Depends on:** Phase 18 (Directory and VFS) ✅, Phase 15 (Hardware Discovery) ✅, Phase 17 (Memory Reclamation) ✅
**Goal:** Give m3OS a persistent block device so files survive reboots. A virtio-blk
driver handles raw sector I/O; MBR partition parsing locates the FAT32 data partition;
the existing `fat_server` gains a write path. After this phase, writing a file to
`/data` and rebooting leaves it visible on the next boot. See
[Phase 24 roadmap doc](../24-persistent-storage.md) for full details.

## Prerequisite Analysis

Current state (post-Phase 23):

- **PCI enumeration** (`kernel/src/pci/mod.rs`): complete — `PCI_DEVICES` holds up
  to 64 discovered devices with BARs, vendor/device IDs, interrupt lines
- **virtio-net reference** (`kernel/src/net/virtio_net.rs`): working virtio legacy
  driver — PCI discovery, feature negotiation, virtqueue setup, synchronous polling;
  provides the pattern for virtio-blk
- **VFS server** (`kernel/src/fs/vfs.rs`): stateless routing layer that forwards all
  FILE_* requests to the "fat" endpoint; no mount table yet
- **FAT server** (`kernel/src/main.rs`): `fat_server_task()` delegates to
  `crate::fs::ramdisk::handle()` — read-only, static embedded files
- **Ramdisk** (`kernel/src/fs/ramdisk.rs`): compile-time embedded filesystem tree;
  supports FILE_OPEN, FILE_READ, FILE_CLOSE, FILE_LIST — no mutations
- **tmpfs** (`kernel/src/fs/tmpfs.rs`): writable in-memory filesystem at `/tmp`;
  provides reference for write/mkdir/unlink patterns
- **Syscall table** (`kernel/src/arch/x86_64/syscall.rs`): Linux-compatible numbers
  through 318; syscall 165 (`mount`) is unimplemented
- **xtask** (`xtask/src/main.rs`): builds single-partition UEFI image; already uses
  `fatfs` crate for FAT32 formatting; QEMU launched with virtio-net but no
  virtio-blk drive
- **Init process** (`userspace/init/src/main.rs`): ring-3 PID 1, forks ion shell;
  no mount calls yet

## Track Layout

| Track | Scope | Dependencies |
|---|---|---|
| A | xtask: disk image creation and QEMU drive argument | — |
| B | virtio-blk driver (PCI discovery, virtqueue, sector I/O) | — |
| C | MBR partition parsing | B |
| D | FAT32 on-disk driver (BPB parsing, read/write via block device) | B, C |
| E | VFS mount table and sys_mount | D |
| F | Init integration and shell mount builtin | E |
| G | Validation, tests, and documentation | All |

---

## Track A — xtask: Disk Image and QEMU Configuration

Create a `disk.img` data disk with a single MBR FAT32 partition and pass it to QEMU
as a virtio-blk device. The existing UEFI image (containing the ESP, bootloader, and
kernel) remains separate; `disk.img` is an empty FAT32 volume that the OS will mount
at `/data`.

| Task | Description | Status |
|---|---|---|
| P24-T001 | Add `create_data_disk()` function in `xtask/src/main.rs`: create a raw `disk.img` file (64 MB default) with an MBR partition table containing one FAT32 partition (type 0x0C) starting at LBA 2048 (1 MB offset). Use the `fatfs` crate to format the partition as FAT32 with 4 KB clusters. | ✅ |
| P24-T002 | Update `cmd_image()` to call `create_data_disk()` after building the UEFI image, placing `disk.img` alongside the existing UEFI image in the output directory. | ✅ |
| P24-T003 | Update `qemu_args()` to add `-drive file=disk.img,format=raw,if=virtio` so QEMU exposes the data disk as a virtio-blk PCI device (vendor 0x1AF4, device 0x1001). | ✅ |
| P24-T004 | Verify: run `cargo xtask image` and confirm `disk.img` is created; run `fsck.fat` on the data partition and confirm it reports clean. Verify QEMU boots without regression and PCI scan logs the new virtio-blk device. | ✅ |

---

## Track B — virtio-blk Driver

Implement a synchronous virtio-blk driver in the kernel. This follows the same legacy
virtio interface used by virtio-net: PCI BAR0 I/O ports, feature negotiation, single
request virtqueue with spin-polling completion.

| Task | Description | Status |
|---|---|---|
| P24-T005 | Create `kernel/src/blk/mod.rs` and `kernel/src/blk/virtio_blk.rs`. Add module declaration in `kernel/src/main.rs`. Define constants: `VIRTIO_BLK_VENDOR` (0x1AF4), `VIRTIO_BLK_DEVICE` (0x1001), virtio status bits (ACKNOWLEDGE, DRIVER, FEATURES_OK, DRIVER_OK), virtio-blk request types (IN=0, OUT=1, FLUSH=4). | ✅ |
| P24-T006 | Implement `find_virtio_blk_device() -> Option<PciDevice>`: iterate the cached `PCI_DEVICES` list for vendor/device match. Return the first matching device. | ✅ |
| P24-T007 | Implement virtio legacy device initialization: read BAR0 I/O port base from PCI config; perform reset → ACKNOWLEDGE → DRIVER status progression; read device features (capacity in sectors from offset 0x14); negotiate features (accept SIZE_MAX if offered); set DRIVER_OK. Store total sector count in a module-level static. | ✅ |
| P24-T008 | Implement virtqueue setup: select queue 0, read queue size from BAR, allocate contiguous physical pages for descriptor table + available ring + used ring (follow virtio-net's allocation pattern); write queue PFN to BAR; store queue state in a `BlkVirtqueue` struct. | ✅ |
| P24-T009 | Define `VirtioBlkReq` struct (C repr, packed): `type_: u32`, `reserved: u32`, `sector: u64` — this is the 16-byte request header placed in the first descriptor of each I/O chain. Define the status byte constants: OK=0, IOERR=1, UNSUPP=2. | ✅ |
| P24-T010 | Implement `read_sectors(lba: u64, count: u32, buf: &mut [u8]) -> Result<(), BlkError>`: build a 3-descriptor chain (header [device-readable] → data buffer [device-writable] → status byte [device-writable]); add to available ring; kick queue via BAR notify register; spin-poll used ring until completion; check status byte; return data in `buf`. | ✅ |
| P24-T011 | Implement `write_sectors(lba: u64, count: u32, buf: &[u8]) -> Result<(), BlkError>`: same 3-descriptor chain but header type=OUT and data buffer is device-readable. Spin-poll and check status. | ✅ |
| P24-T012 | Implement `pub fn init()`: call `find_virtio_blk_device()`, initialize device, set up virtqueue, set `VIRTIO_BLK_READY` atomic flag. Log device capacity and PCI address. Guard with `call_once` pattern to prevent re-initialization. | ✅ |
| P24-T013 | Call `blk::init()` from `kernel_main()` after `pci::init()` and before `init_task()` spawns servers. Verify: boot in QEMU and confirm log message showing virtio-blk device detected with correct capacity. | ✅ |

---

## Track C — MBR Partition Parsing

Parse the MBR partition table from sector 0 of the virtio-blk device to locate the
FAT32 data partition. This gives the starting LBA and sector count needed by the FAT32
driver.

| Task | Description | Status |
|---|---|---|
| P24-T014 | Create `kernel/src/blk/mbr.rs`. Define `MbrPartitionEntry` struct (16 bytes): `status` (u8), `chs_start` ([u8; 3]), `part_type` (u8), `chs_end` ([u8; 3]), `lba_start` (u32 LE), `sector_count` (u32 LE). Define `MbrHeader` covering bytes 446..510 of sector 0 (four partition entries) plus the 0x55AA signature at bytes 510..512. | ✅ |
| P24-T015 | Implement `parse_mbr(sector0: &[u8; 512]) -> Result<[MbrPartitionEntry; 4], MbrError>`: validate 0x55AA signature; extract four partition entries; return them. | ✅ |
| P24-T016 | Implement `find_fat32_partition(entries: &[MbrPartitionEntry; 4]) -> Option<(u64, u64)>`: scan entries for type 0x0B (FAT32) or 0x0C (FAT32 LBA); return `(lba_start, sector_count)` of the first match. Log the found partition offset and size. | ✅ |
| P24-T017 | Implement `pub fn probe() -> Option<(u64, u64)>`: read sector 0 via `virtio_blk::read_sectors()`, parse MBR, find and return the FAT32 partition location. This is the entry point called during mount. | ✅ |
| P24-T018 | Add kernel-core unit tests for MBR parsing: construct a synthetic 512-byte sector with known partition entries and 0x55AA signature; verify `parse_mbr()` extracts correct LBA and sizes; verify missing signature returns error; verify no FAT32 partition returns None. | ✅ |

---

## Track D — FAT32 On-Disk Driver

Implement a FAT32 filesystem driver that reads and writes through the virtio-blk
device. This replaces the ramdisk backend for the `/data` mount point. The driver
parses the BPB, walks cluster chains, allocates free clusters, and manages directory
entries.

| Task | Description | Status |
|---|---|---|
| P24-T019 | Create `kernel/src/fs/fat32.rs`. Define `Fat32Bpb` struct covering the FAT32 BIOS Parameter Block: `bytes_per_sector` (u16), `sectors_per_cluster` (u8), `reserved_sectors` (u16), `num_fats` (u8), `total_sectors_32` (u32), `fat_size_32` (u32), `root_cluster` (u32), `fs_info_sector` (u16). Parse from the first sector of the partition. | ✅ |
| P24-T020 | Implement `Fat32Volume` struct holding parsed BPB fields, the partition's base LBA (from MBR), computed FAT region start LBA, data region start LBA, and total cluster count. Implement `Fat32Volume::mount(base_lba: u64) -> Result<Self, Fat32Error>`: read sector 0 of the partition, parse BPB, compute region offsets, validate signature (0xAA55 at offset 510, "FAT32   " at offset 82). | ✅ |
| P24-T021 | Implement cluster-to-LBA translation: `fn cluster_to_lba(&self, cluster: u32) -> u64` — converts a FAT32 cluster number to the absolute disk LBA using `data_start_lba + (cluster - 2) * sectors_per_cluster`. | ✅ |
| P24-T022 | Implement FAT table reading: `fn read_fat_entry(&self, cluster: u32) -> Result<u32, Fat32Error>` — compute which sector of the FAT contains the entry, read that sector via virtio-blk, extract the 28-bit entry value. Cache the most recently read FAT sector to reduce I/O. | ✅ |
| P24-T023 | Implement cluster chain walking: `fn read_chain(&self, start_cluster: u32) -> Result<Vec<u32>, Fat32Error>` — follow FAT entries from `start_cluster` until end-of-chain marker (>= 0x0FFFFFF8). Limit chain length to prevent infinite loops. | ✅ |
| P24-T024 | Implement file reading: `fn read_file(&self, start_cluster: u32, offset: usize, buf: &mut [u8]) -> Result<usize, Fat32Error>` — walk the cluster chain, skip clusters before `offset`, read data sectors via virtio-blk, copy into `buf`. Handle partial cluster reads at start and end of range. | ✅ |
| P24-T025 | Implement directory entry parsing: define `Fat32DirEntry` struct (32 bytes) matching the FAT32 directory entry format (name [11], attr, cluster_hi [2], cluster_lo [2], file_size [4]). Implement `fn read_dir(&self, dir_cluster: u32) -> Result<Vec<Fat32DirEntry>, Fat32Error>` — read all sectors in the directory's cluster chain, parse 32-byte entries, skip deleted (0xE5) and LFN (attr 0x0F) entries, stop at 0x00 terminator. | ✅ |
| P24-T026 | Implement path lookup: `fn lookup(&self, path: &str) -> Result<Fat32DirEntry, Fat32Error>` — split path by `/`, walk from root cluster through directory entries matching each component (case-insensitive 8.3 comparison). Return the final entry. | ✅ |
| P24-T027 | Implement FAT table writing: `fn write_fat_entry(&mut self, cluster: u32, value: u32) -> Result<(), Fat32Error>` — read the FAT sector, update the 28-bit entry (preserving high 4 bits), write the sector back. Update both FAT copies if `num_fats == 2`. | ✅ |
| P24-T028 | Implement free cluster allocation: `fn alloc_cluster(&mut self) -> Result<u32, Fat32Error>` — scan FAT entries starting from a hint (last allocated + 1, wrapping) for a zero entry; mark it as end-of-chain (0x0FFFFFF8); return the cluster number. Track the hint in `Fat32Volume` to avoid O(n) scans on sequential allocation. | ✅ |
| P24-T029 | Implement cluster chain extension: `fn extend_chain(&mut self, last_cluster: u32) -> Result<u32, Fat32Error>` — allocate a new cluster, update the previous last cluster's FAT entry to point to it. Used when a file grows beyond its current allocation. | ✅ |
| P24-T030 | Implement file writing: `fn write_file(&mut self, start_cluster: u32, offset: usize, data: &[u8]) -> Result<(u32, usize), Fat32Error>` — walk existing chain to the write offset, allocate new clusters as needed, write data sectors via virtio-blk, return updated start cluster and new file size. Handle both append and overwrite cases. | ✅ |
| P24-T031 | Implement directory entry creation: `fn create_entry(&mut self, dir_cluster: u32, name: &str, attr: u8) -> Result<Fat32DirEntry, Fat32Error>` — format name as 8.3 (pad with spaces, uppercase), find a free slot (0x00 or 0xE5) in the directory's cluster chain, write the new 32-byte entry. Allocate a new cluster for the directory if full. For directories (attr & 0x10), allocate a cluster and create `.` and `..` entries. | ✅ |
| P24-T032 | Implement `create_file()`: call `create_entry()` with attr=0x20 (ARCHIVE), no initial cluster allocation (zero-length file). Implement `mkdir()`: call `create_entry()` with attr=0x10, allocate initial cluster, write `.` and `..` entries pointing to self and parent. | ✅ |
| P24-T033 | Implement `unlink()`: look up the entry, free all clusters in its chain (set FAT entries to 0), mark the directory entry as deleted (0xE5 in first byte). For directories, verify empty (only `.` and `..`) before deleting. | ✅ |
| P24-T034 | Implement `rename()`: look up source entry, create new entry in destination directory with same attributes/cluster/size, delete old entry. If destination already exists, unlink it first. | deferred |
| P24-T035 | Implement the `handle()` function for FAT32 IPC: accept FILE_OPEN, FILE_READ, FILE_WRITE, FILE_CLOSE, FILE_LIST, FILE_MKDIR, FILE_UNLINK, FILE_RENAME messages; dispatch to the corresponding `Fat32Volume` methods. Maintain an open-file table mapping fd numbers to (start_cluster, offset, size, dir_cluster) tuples. | ✅ (implemented via direct syscall routing instead of IPC) |

---

## Track E — VFS Mount Table and sys_mount

Extend the VFS server with a mount table that routes paths to different filesystem
backends. Add the `sys_mount` syscall so userspace can trigger mounting.

| Task | Description | Status |
|---|---|---|
| P24-T036 | Add a mount table to `kernel/src/fs/vfs.rs`: a static array of `MountEntry { path: [u8; 64], backend: MountBackend, active: bool }` entries (max 8 mounts). `MountBackend` enum: `Ramdisk`, `Tmpfs`, `Fat32Disk`. Pre-populate with `"/" → Ramdisk` and `"/tmp" → Tmpfs`. | ✅ (implemented via direct syscall path routing with `fat32_relative_path()` instead of separate mount table) |
| P24-T037 | Update VFS path routing: when a FILE_OPEN/FILE_READ/etc request arrives, find the longest-prefix mount match in the mount table; strip the mount prefix from the path; forward to the corresponding backend handler. | ✅ (path routing in syscall layer: open, read, write, mkdir, unlink, getdents64, fstat, lseek) |
| P24-T038 | Add `mount_fat32(mount_point: &str) -> Result<(), VfsError>`: probe MBR via `blk::mbr::probe()`, initialize `Fat32Volume::mount()` with the partition's base LBA, register in the mount table at the given path. Store the `Fat32Volume` in a `Mutex<Option<Fat32Volume>>` static. | ✅ |
| P24-T039 | Implement `sys_mount` at syscall number 165: extract `source`, `target`, `fstype` string pointers from registers; if `fstype == "vfat"`, call `mount_fat32(target)`; return 0 on success, -EINVAL/-ENODEV on failure. Only the `"vfat"` fstype is supported initially. | ✅ |
| P24-T040 | Wire `sys_mount` into the syscall dispatch table in `kernel/src/arch/x86_64/syscall.rs`. Add the `mount()` wrapper to `userspace/syscall-lib/src/lib.rs` with appropriate constants. | ✅ |

---

## Track F — Init Integration and Shell Mount Builtin

Wire the mount call into the boot sequence and add a `mount` shell builtin for
visibility.

| Task | Description | Status |
|---|---|---|
| P24-T041 | Update `userspace/init/src/main.rs`: after server startup, call `mount("/dev/blk0", "/data", "vfat", 0, ptr::null())`. Log success or failure. The shell should see `/data` as a writable directory after boot. | ✅ |
| P24-T042 | Add a `mount` builtin to the shell (or as a standalone coreutils binary) that prints the active mount table. Implement via a new syscall or by reading a `/proc/mounts`-style file from the VFS. Minimal approach: hardcode output based on what init mounted. | deferred |
| P24-T043 | Verify end-to-end: boot in QEMU, run `echo hello > /data/test.txt`, reboot (`cargo xtask run` twice), verify `/data/test.txt` contains "hello" on the second boot. | deferred (requires interactive QEMU) |
| P24-T044 | Verify host-side visibility: after QEMU shutdown, mount `disk.img` on the host (via `losetup -P` + `mount` or `mtools mcopy`) and confirm the file written from inside m3OS is readable. | deferred (requires interactive QEMU + host mount) |

---

## Track G — Validation, Tests, and Documentation

| Task | Description | Status |
|---|---|---|
| P24-T045 | Add kernel-core unit tests for FAT32 BPB parsing: construct a synthetic BPB sector, verify field extraction (bytes_per_sector, sectors_per_cluster, fat_size_32, root_cluster), verify invalid signature is rejected. | ✅ |
| P24-T046 | Add kernel-core unit tests for directory entry parsing: construct 32-byte FAT32 directory entries with known 8.3 names, verify parsing extracts correct name, attributes, cluster number, and file size; verify deleted (0xE5) and LFN (0x0F) entries are skipped. | ✅ |
| P24-T047 | Add a QEMU integration test (`tests/fat32_write.rs` or similar): boot, write a file to `/data`, read it back, verify contents match. Test mkdir, unlink, and rename operations. | deferred (requires QEMU integration test harness extension) |
| P24-T048 | Test multi-cluster files: write a file larger than 4 KB (one cluster) to `/data`, read it back, verify no corruption. Verify the FAT chain is correct by examining `disk.img` on the host. | deferred (requires interactive QEMU) |
| P24-T049 | Run `cargo xtask check` — clippy clean, rustfmt clean, all existing tests pass. Verify no regressions in boot, shell, networking, or signal handling. | ✅ |
| P24-T050 | Document the virtio-blk driver in `docs/`: PCI discovery, legacy virtio status negotiation, virtqueue layout (descriptor/available/used rings), request descriptor chain format, synchronous polling model. | ✅ |
| P24-T051 | Document the FAT32 on-disk layout in `docs/`: BPB field locations, FAT region vs data region, cluster-to-LBA mapping, directory entry format, write path (alloc cluster → update FAT → write data → update dir entry → flush). | ✅ |
| P24-T052 | Document `sys_mount` ABI and the mount table in `docs/`: syscall arguments, supported fstypes, VFS routing with longest-prefix match. Explain why synchronous write-through is safe here but unacceptable in production (no page cache, no writeback). | ✅ |

---

## Related

- [Phase 24 Design Doc](../24-persistent-storage.md)
