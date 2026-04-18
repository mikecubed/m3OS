# Phase 24 — Persistent Storage

**Aligned Roadmap Phase:** Phase 24
**Status:** Complete
**Source Ref:** phase-24

## Overview

Phase 24 gives m3OS persistent block storage via a virtio-blk disk. The current
storage model uses an **ext2 filesystem mounted at `/`** as the root filesystem.
Init mounts the ext2 volume at boot (`mount /dev/blk0 / ext2`), making all paths
— `/etc`, `/var`, `/home` — persistent across reboots (backed by the QEMU disk
image `disk.img`).

The original Phase 24 implementation used a 64 MB FAT32 partition mounted at
`/data`. The FAT32 path routing (`/data/...` → FAT32 volume) remains as a legacy
fallback, but all new code and service configuration targets the ext2 root.

**What persists where:**

| Path | Backend | Persistent? | Notes |
|---|---|---|---|
| `/bin`, `/sbin` | Ramdisk overlay | No | Built-in binaries; overlays ext2 |
| `/etc/services.d/` | ext2 (root) | Yes | Service definitions |
| `/var/log/messages` | ext2 (root) | Yes | syslogd output |
| `/var/log/kern.log` | ext2 (root) | Yes | Kernel log archive |
| `/var/run/` | ext2 (root) | Yes | Runtime state (PID files, status) |
| `/tmp` | tmpfs | No | Cleared on reboot |
| `/data/...` | FAT32 (legacy) | Yes | Legacy mount; not used by services |

**Checking storage health**: `df /` shows ext2 capacity and usage. `stat <path>`
confirms whether a file is on the ext2 volume. If init reports "/ mount failed",
the ext2 partition may be corrupted — recreate the disk with `cargo xtask clean`
followed by `cargo xtask run`.

## Architecture

```
userspace                     kernel                          QEMU
─────────                     ──────                          ────
open("/etc/foo", O_CREAT) →  sys_linux_open()
                              ├── ext2 inode lookup
                              └── Ext2Volume::write()
                                  └── virtio_blk::write_sectors()  →  disk.img
```

All persistent storage I/O flows through three layers:
1. **Syscall routing** — paths are resolved against the ext2 root filesystem.
   The ramdisk overlays `/bin` and `/sbin` (built-in binaries take precedence).
   Legacy `/data/...` paths are routed to the FAT32 volume if mounted.
2. **ext2 volume driver** (`kernel/src/fs/ext2.rs`) — superblock, block groups,
   inode CRUD, directory operations, block allocation
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
| rdi | source path pointer (e.g. `/dev/blk0`) |
| rsi | target mount point path pointer |
| rdx | filesystem type string pointer (`"ext2"` or `"vfat"`) |

Returns 0 on success, negative errno on failure:
- `-EINVAL` if fstype is not `"ext2"` or `"vfat"`
- `-ENODEV` if no matching partition found on virtio-blk
- `-EIO` if mount fails

### Supported Filesystem Types

- **`"ext2"`** — probes the MBR partition table for a Linux partition (type 0x83)
  and mounts it via the ext2 volume driver. Init uses this at boot:
  `mount("/dev/blk0", "/", "ext2")`.
- **`"vfat"`** (legacy) — probes for a FAT32 partition (type 0x0B or 0x0C) and
  mounts it as a `Fat32Volume` at `/data`.

### VFS Path Routing

Path routing combines the ext2 root with ramdisk overlays and legacy FAT32:
- `/bin/...`, `/sbin/...` → ramdisk first (built-in binaries overlay ext2)
- `/tmp/...` → tmpfs
- `/data/...` → FAT32 volume (legacy, if mounted)
- Everything else → ext2 root filesystem

The ext2 root is the primary persistent store. The ramdisk overlay ensures that
built-in kernel binaries (linked into the kernel image) are always available even
if the ext2 disk is missing or corrupt.

## Headless Operator Workflow

The supported headless/reference path verifies storage from the running system
rather than from host-side image tooling:

```bash
mount
df /
df -h /
echo "storage-ok" > /root/storage-test
cat /root/storage-test
rm /root/storage-test
stat /var/log/messages
```

These checks answer the three operator questions that matter for Phase 53:

| Question | Command | Expected result |
|---|---|---|
| Is the writable filesystem mounted? | `mount`, `df /` | ext2 mounted at `/` |
| Can I write persistent data? | `echo ... > /root/storage-test` | create/read/remove succeeds |
| Are service-backed files really on persistent storage? | `stat /var/log/messages` | file exists on the ext2 root |

Interpret the results against the current image model:
- `/` is the persistent ext2 root backed by `disk.img`
- `/tmp` is tmpfs and is cleared on reboot
- `/bin` and `/sbin` are ramdisk overlays, so built-in binaries stay available
  even if the ext2 image is damaged

When storage-backed workflows fail, inspect the same evidence trail the services
use:
- `mount` and `df` to confirm the ext2 root is present and has space
- serial output from init/syslogd for mount or `/var/log` creation failures
- `/var/log/messages` and `/var/log/kern.log` if syslogd was able to start

If the ext2 image is corrupted or the writable path is no longer trustworthy,
recreate it with `cargo xtask run --fresh` (or `cargo xtask clean` followed by a
normal run).

## Phase 55 Additions (NVMe)

Phase 24 shipped VirtIO-blk as the only supported block device. Phase 55
adds NVMe (`kernel/src/blk/nvme.rs`) as the first non-VirtIO storage path.
NVMe is architecturally close to VirtIO — submission and completion queues,
doorbell registers, MSI/MSI-X completion — which made it the highest-leverage
first real-hardware storage target.

- **Bring-up.** `nvme_probe` matches on PCI class `01:08:02`, claims the
  device via the hardware-access layer, maps BAR0, executes a controller
  reset bounded by `CAP.TO`, programs the admin queue (`AQA`/`ASQ`/`ACQ`),
  and enables the controller.
- **Identify.** Identify Controller records model/serial/firmware strings;
  Identify Namespace records the namespace capacity and LBA format.
- **I/O queue pair.** A single I/O CQ + SQ pair is created (qid=1, 64
  entries). Read and Write commands use Physical Region Page (PRP) lists
  with an overflow PRP-list page for transfers spanning more than two
  pages.
- **Completion path.** MSI-X is installed when available (the QEMU NVMe
  controller advertises it); the handler drains both admin and I/O
  completion queues by walking the phase bit and wakes blocked tasks via
  `wake_task`. A polling fallback exists if MSI allocation fails.
- **Dispatch policy.** `blk/mod.rs` dispatches `read_sectors` /
  `write_sectors` to NVMe when `NVME_READY` is set, else VirtIO-blk. A
  proper multi-device block layer is deferred — Phase 55 is deliberately
  simple.
- **Smoke test.** At end of probe, the driver writes a deterministic
  512-byte pattern to LBA 0, reads it back, and compares. On mismatch
  it clears `NVME_READY` so the dispatch layer falls back to VirtIO-blk
  instead of silently corrupting data.
- **Operator usage.** `cargo xtask run --device nvme` attaches a 64 MiB
  NVMe drive at `target/nvme.img` and the kernel logs
  `nvme data-path smoke OK (512B round-trip at LBA 0)` when bring-up
  succeeds.

NVMe pure-logic types (`NvmeCommand`, `NvmeCompletion`, `NvmeCap`, opcode
constants) live in `kernel-core/src/nvme.rs` and are host-testable. See
[Phase 55 — Hardware Substrate](./55-hardware-substrate.md) for the driver
architecture, the reference matrix (including the IOMMU caveat for
physical-hardware NVMe), and how NVMe plugs into the hardware-access layer.
