# Phase 24 — Persistent Storage

**Aligned Roadmap Phase:** Phase 24
**Status:** Complete
**Source Ref:** phase-24

## Overview

Phase 24 gives m3OS a persistent block device: a 64 MB FAT32 data partition on a
virtio-blk disk, mounted at `/data` during boot. Files written to `/data` survive
reboots because they are stored on the QEMU disk image (`disk.img`) rather than in RAM.

## Architecture

```
userspace                     kernel                          QEMU
─────────                     ──────                          ────
open("/data/x", O_CREAT)  →  sys_linux_open()
                              ├── fat32_relative_path()
                              └── Fat32Volume::create_file()
                                  └── virtio_blk::write_sectors()  →  disk.img
```

All persistent storage I/O flows through three layers:
1. **Syscall routing** — `fat32_relative_path()` matches `/data/...` paths and
   dispatches to the FAT32 volume driver (same pattern as `/tmp` → tmpfs)
2. **FAT32 volume driver** (`kernel/src/fs/fat32.rs`) — BPB parsing, cluster chain
   management, directory entry CRUD
3. **virtio-blk driver** (`kernel/src/blk/virtio_blk.rs`) — sector-level I/O via
   legacy virtio PCI interface

## virtio-blk Driver

### PCI Discovery

The driver scans the PCI device list for vendor `0x1AF4` (Red Hat / virtio) with
device ID `0x1001` (legacy virtio-blk) or `0x1042` (transitional). BAR0 provides
the legacy I/O port base address.

### Legacy Virtio Status Negotiation

```
Reset (write 0 to status register)
  → ACKNOWLEDGE (0x01)
  → DRIVER (0x02)
  → Feature negotiation (read device features, write driver features)
  → FEATURES_OK (0x08) — transitional devices only
  → DRIVER_OK (0x04)
```

### Virtqueue Layout

A single request queue (queue index 0) is used for all I/O. The queue consists of
three physically contiguous regions:

| Region | Size | Alignment |
|---|---|---|
| Descriptor table | 16 × queue_size bytes | 16 bytes |
| Available ring | 4 + 2 × queue_size + 2 bytes | 2 bytes |
| Used ring | 4 + 8 × queue_size + 2 bytes | 4096 bytes |

The PFN (page frame number) of the allocation is written to the Queue Address register.

### Request Descriptor Chain

Each block I/O request uses a 3-descriptor chain:

1. **Header** (device-readable, 16 bytes): `VirtioBlkReq { type_: u32, reserved: u32, sector: u64 }`
   - `type_ = 0` for reads (IN), `type_ = 1` for writes (OUT)
2. **Data buffer** (device-writable for reads, device-readable for writes)
3. **Status byte** (device-writable, 1 byte): 0 = success, 1 = I/O error, 2 = unsupported

### Synchronous Polling

After adding the descriptor chain to the available ring and kicking the device
(writing queue index to the notify register), the driver spin-polls the used ring
`idx` field until it advances. This is a blocking operation — acceptable for a toy
OS but would need interrupt-driven completion in production.

## FAT32 On-Disk Layout

### BIOS Parameter Block (BPB)

Parsed from the first sector of the partition (byte offsets within sector):

| Offset | Size | Field |
|---|---|---|
| 11 | 2 | bytes_per_sector (must be 512) |
| 13 | 1 | sectors_per_cluster |
| 14 | 2 | reserved_sectors |
| 16 | 1 | num_fats (usually 2) |
| 32 | 4 | total_sectors_32 |
| 36 | 4 | fat_size_32 (sectors per FAT) |
| 44 | 4 | root_cluster |
| 48 | 2 | fs_info_sector |
| 510 | 2 | signature (0x55AA) |

### Disk Regions

```
┌─────────────┐ LBA 0 (partition start)
│ Boot sector │ (BPB)
├─────────────┤ LBA reserved_sectors
│ FAT 1       │ fat_size_32 sectors
├─────────────┤
│ FAT 2       │ fat_size_32 sectors (if num_fats == 2)
├─────────────┤ data_start_lba
│ Data region │ clusters 2, 3, 4, ...
└─────────────┘
```

### Cluster-to-LBA Mapping

```
absolute_lba = data_start_lba + (cluster - 2) × sectors_per_cluster
```

### Directory Entry Format (32 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 11 | 8.3 name (space-padded, uppercase) |
| 11 | 1 | attributes (0x10=dir, 0x20=archive, 0x0F=LFN) |
| 20 | 2 | cluster_hi (high 16 bits of start cluster) |
| 26 | 2 | cluster_lo (low 16 bits of start cluster) |
| 28 | 4 | file_size |

Special first-byte values: `0x00` = end of directory, `0xE5` = deleted entry.

### Write Path

1. **Allocate cluster** — scan FAT for a zero entry starting from alloc_hint
2. **Update FAT** — write end-of-chain marker (or chain link) to both FAT copies
3. **Write data** — write cluster data sectors via virtio-blk
4. **Update directory entry** — write new start_cluster and file_size to the parent directory

All writes are synchronous write-through (no page cache, no writeback). This is
correct for a toy OS but would be unacceptably slow in production. A real OS would
use a buffer cache with periodic writeback and fsync barriers.

## sys_mount Syscall

### ABI

| Register | Value |
|---|---|
| rax | 165 (SYS_MOUNT) |
| rdi | source path pointer (ignored for vfat) |
| rsi | target mount point path pointer |
| rdx | filesystem type string pointer |

Returns 0 on success, negative errno on failure:
- `-EINVAL` if fstype is not "vfat"
- `-ENODEV` if no FAT32 partition found on virtio-blk
- `-EIO` if FAT32 mount fails

### Supported Filesystem Types

Only `"vfat"` (FAT32) is currently supported. The syscall probes the MBR partition
table on the virtio-blk device, finds the first FAT32 partition (type 0x0B or 0x0C),
and mounts it as a `Fat32Volume`.

### VFS Path Routing

Instead of a traditional mount table, path routing is implemented directly in the
syscall layer using prefix matching:
- `/tmp/...` → tmpfs (existing)
- `/data/...` → FAT32 volume (Phase 24)
- Everything else → ramdisk

The `Fat32Disk` fd backend stores the relative path, start cluster, file size, and
parent directory cluster for each open file, enabling read/write/seek operations.
