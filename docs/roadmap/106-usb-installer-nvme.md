# Phase 106 - USB Installer & NVMe Install

**Status:** Planned
**Source Ref:** phase-106
**Depends on:** Phase 82/87 (AHCI fork-and-retry root bootstrap + writable ext2) ✅, Phase 92a (USB mass-storage with writable `/mnt/usb`) ✅, Phase 55b (ring-3 NVMe driver) ✅, Phase 98 (the GUI-workstation re-charter + the bare-metal validation strategy) ✅. **Gate:** the NVMe-root install milestone (M2/M3) is unblocked only once **bare-metal NVMe root boot is validated on the Dell** per the [Phase 98 Track A bare-metal validation strategy](../appendix/bare-metal-validation.md) (the USB-ext2 root milestone M1 has no such gate).
**Builds on:** Reuses the Phase 82 D.3 fork-and-retry root-bootstrap template (`bootstrap_ring3_root_disk` in `userspace/init`), the Phase 87 writable-ext2 data path (`allocate_inode`/`allocate_data_block`/`write_inode` + `BLK_FLUSH`), the Phase 92a `usb-storage` BOT `WRITE(10)` path and the GPT-aware `usb_ext2_base_lba` probe, and the Phase 55b ring-3 NVMe driver (`nvme.block` → `RemoteBlockDevice`). Adds the missing combined-image build, the USB/NVMe root bootstraps, and the on-device install tooling.
**Primary Components:** `xtask/src/main.rs` (a combined GPT(ESP+ext2) image builder + `nvme-rw`/`nvme-persist`/`usb-root`/`nvme-install` gates), `userspace/init/src/main.rs` (`bootstrap_ring3_root_disk` generalized to USB + NVMe), `kernel/src/blk/remote.rs` (root slot-0 accepts a `usbN.block`/`nvme.block` backend), `kernel/src/arch/x86_64/syscall/mod.rs` (GPT-aware root mount + new installer-scoped raw-block syscalls), `userspace/installer` (new — the on-device installer), `userspace/drivers/nvme` (root bring-up path)

## Milestone Goal

m3OS climbs the milestone ladder from "boots a flashed read-only image" to "**installs itself onto the Dell's internal NVMe**." Three rungs: **M1** — a single self-contained USB image that boots the laptop with a **writable** ext2 root (not the read-only ramdisk fallback we have today); **M2** — the laptop boots writable from the **internal NVMe**, with `nvme-rw`/`nvme-persist` gates passing in QEMU; **M3** — an on-device **installer** that writes the NVMe from a USB-resident image, installs the boot path, creates the first user, and reboots into the installed system. A fully working, writable filesystem booted from USB (M1) is the acceptable first milestone; M2/M3 are the workstation-grade payoff.

## Why This Phase Exists

`cargo xtask image` produces a UEFI-bootable GPT+ESP disk (the bootloader's `create_uefi_image` lays the kernel into an ESP FAT partition) that `dd`s to a USB stick and boots the Dell — but the **rootfs is a separate MBR+ext2 `disk.img`** built by `create_data_disk`. On a USB-only boot there is no second disk, so the first `mount("/dev/blk0", "/", "ext2")` in `init_main` fails, `bootstrap_ring3_root_disk` finds no AHCI/NVMe controller, and init falls back to a **read-only ramdisk root** with a RAM `/tmp` (`add_builtin_defaults`). ext2-on-USB is writable today only as a **secondary** `/mnt/usb` mount (the Phase 92a path), never as `/`. That is not a workstation: nothing the user does survives a reboot, and there is no path to the internal disk.

A "real-hardware workstation" means **installing to the internal NVMe** — partition it, write a rootfs, install the EFI boot entry, create the first account — not living on a flashed read-only stick. The substrate is almost all present (a writable ext2 engine, a `usb-storage` write path, a ring-3 NVMe driver, a GPT partition probe, per-`dev_id` block I/O); what is missing is the **glue**: one combined image, the two root bootstraps, and the installer. This phase supplies exactly that glue and nothing more.

## Learning Goals

- How a single bootable medium carries both the firmware-visible **ESP (FAT, kernel)** and the OS **rootfs (ext2)** on one GPT, and why the rootfs partition does not start at LBA 0 (so the root mount must be **GPT-partition-aware**, not whole-disk).
- How a microkernel keeps the root block device behind a **ring-3 driver**: the kernel's root slot auto-discovers a named block service (`nvme.block`/`ahci.block`/`usb0.block`) and forwards sector I/O over IPC, so "boot from NVMe" and "boot from USB" differ only in which driver registers the backend.
- Why an **installer** on a microkernel is mostly userspace policy: it reads sectors from one `dev_id` and writes them to another over capability-gated raw-block syscalls, then arranges for the firmware to boot the new medium — the kernel only enforces the access check.
- The difference between a **raw image copy** (`dd`-style, byte-for-byte, simplest) and a **partition-aware install** (create a GPT, format an ESP, run an on-device `mkfs.ext2`, copy files) — and why creating a filesystem from scratch (superblock + BGD + inode table + bitmaps) is materially harder than writing into an existing one.
- The **bare-metal validation discipline** (Phase 98 Track A.5): QEMU can model NVMe and a USB stick but cannot model "the Dell boots from its own internal disk," so the headline rungs carry `Validated-on-HW (run N, date)` evidence, not a bare `Complete`.

## Feature Scope

### Track A — Combined writable USB image + USB-ext2 root bootstrap (M1)

`cmd_image` emits two separate files today (`create_uefi_image` → the GPT+ESP kernel image; `create_data_disk` → a separate MBR+ext2 `disk.img`); **no combiner lays both on one disk**. Track A adds a host-side combiner that builds **one GPT disk** with an `[ESP FAT (kernel + bootloader)] + [ext2 rootfs]` layout — reusing the existing `create_gpt_disk` GPT/protective-MBR plumbing (currently only used to wrap the signed ESP) and the `populate_ext2_files` rootfs content. Then it generalizes `bootstrap_ring3_root_disk` so that, when the root mount fails, init also forks `/drivers/xhci` + `/drivers/usb-storage`, waits for `usb0.block`, and mounts `/dev/usb0`'s **GPT ext2 partition** as `/`. The GPT-aware `usb_ext2_base_lba` probe and the `usb0.block` backend already exist (Phase 92a); the load-bearing new work is making the **kernel root mount accept a `usbN.block` backend at a non-zero base LBA** — i.e. `blk::remote` root slot 0 and `VFS_MOUNT_EXT2_ROOT` learn to route to a USB device and use the GPT base LBA, instead of only the whole-disk `probe_ext2()` path.

### Track B — NVMe root boot + persistence gates (M2)

The kernel root slot already auto-discovers `nvme.block` first (`is_registered()`), and the Phase 55b NVMe driver registers it — but `init` only ever forks `/drivers/ahci` on a failed root mount, and `--device nvme` attaches a *scratch* second drive rather than routing the real rootfs to NVMe. Track B mirrors the Phase 82 AHCI path: `bootstrap_ring3_root_disk` also forks `/drivers/nvme` and retries, the xtask data-disk router can place the real ext2 rootfs behind the QEMU NVMe controller, and two new gates — `nvme-rw` and `nvme-persist` — are written as direct analogs of the passing `ahci-rw`/`ahci-persist` gates (a payload write that round-trips `write_sectors`→`do_write_ipc`→the ring-3 NVMe `handle_write`, and a two-boot reboot-persistence check against the same NVMe disk with a `BLK_FLUSH` drain between boots).

### Track C — On-device installer: USB → NVMe (M3)

A new ring-3 binary `userspace/installer`. **First cut (raw-image copy):** a capability-gated `dd`-style copy that reads the combined image from the boot USB `dev_id` and writes it sector-for-sector onto the NVMe `dev_id`, then arranges the reboot into NVMe. The per-`dev_id` block I/O already exists in the kernel (`blk::read_sectors_dev`/`write_sectors_dev`/`flush_dev`); Track C adds a **new installer-scoped raw read/write syscall pair** over both `dev_id`s, **access-checked** so only a capability-holding installer can do raw cross-device block I/O (an unprivileged process gets `EPERM`). **Follow-on (partition-aware):** an on-device GPT writer + ESP/FAT creator + an **on-device `mkfs.ext2`** that builds a fresh rootfs in place (sized to the disk), then copies files into it. The on-device `mkfs` is the genuinely new kernel-core capability: `kernel-core::fs::ext2` can read **and** write an *existing* filesystem but cannot **create** a superblock/BGD/inode-table/bitmaps today — the structure serializers (`Ext2Superblock::write_into` etc.) exist as building blocks, but nothing orchestrates a from-scratch format.

### Track D — First-user / account setup (M3)

The installed system must come up with a real account, not the image's autologin. Track D wires the installer (or a one-shot first-boot setup) into the existing multi-user stack: create `/etc/passwd`/`/etc/shadow` entries via the Phase-era `adduser`/`passwd` tooling, set the root and first-user credentials (PBKDF2/`crypto-lib`), seed the home directory, and disable the image's autologin so the installed NVMe system presents a login. No new auth crypto — reuse `passwd`/`adduser`.

### Track E — Validation & bare-metal sign-off

`nvme-rw`/`nvme-persist` are always-on QEMU gates (the ext2 write + reboot-persistence proofs M2 needs). `usb-root-smoke` boots a combined image attached as a QEMU `usb-storage` stick and asserts a **writable** ext2 root (the M1 proof QEMU *can* model). `nvme-install-smoke` boots from a USB-attached combined image, runs the installer onto a blank NVMe scratch disk, reboots QEMU with **only** the NVMe, and asserts the installed system + the created first user. The headline rungs that QEMU cannot model — M1 (the Dell boots writable from USB) and M3 (a real install to the Dell's internal NVMe) — follow the Phase 98 bare-metal protocol (`docs/appendix/bare-metal-validation.md`) and land as `Validated-on-HW (run N, date)`.

## Important Components and How They Work

### `xtask/src/main.rs` — the combined image builder

A new builder composes `create_uefi_image`'s ESP-with-kernel and `create_data_disk`'s ext2 rootfs onto **one** GPT via the existing `create_gpt_disk` protective-MBR + GPT plumbing (today it only wraps the signed ESP). The output is a single `m3os-usb.img` with two GPT partitions: an EFI System Partition (FAT, the bootloader + kernel) and a Linux partition (ext2, the rootfs populated by `populate_ext2_files`). `dd` it to a stick and the firmware finds the ESP while m3OS finds the ext2 partition by GPT scan — no separate `disk.img`.

### `kernel/src/blk/remote.rs` — root slot accepts USB / NVMe

`is_registered()` is the root slot-0 auto-discovery: it looks up `"nvme.block"` then `"ahci.block"` in the IPC registry, owner-gated to `/drivers/` processes, and caches the endpoint. Track A extends this discovery chain so a `usbN.block` backend can also back the root slot (when no nvme/ahci is present and the boot device is the USB stick), preserving the trusted-owner gate. `MAX_REMOTE_BLOCK = 4` and the per-`dev_id` `read_sectors_dev`/`write_sectors_dev`/`flush_dev`/`do_write_ipc_dev` paths are unchanged — the installer's raw cross-device copy rides them directly.

### `kernel/src/arch/x86_64/syscall/mod.rs` — GPT-aware root mount + raw-block syscalls

The root mount path (`VFS_MOUNT_EXT2_ROOT`) currently calls `crate::blk::mbr::probe_ext2()` (whole-disk / MBR) and `mount_ext2(base_lba)`. Track A teaches it to use the **GPT** base LBA — the same `usb_ext2_base_lba(dev_id)` logic the `/mnt/usb` secondary-mount path already uses (it parses the protective MBR → `EFI PART` header → partition entries → ext2-magic probe). Track C adds the **new installer-scoped raw read/write syscalls**, capability-checked against an installer capability so a non-installer process cannot do raw cross-`dev_id` block writes; they thin-wrap `blk::read_sectors_dev`/`write_sectors_dev`.

### `userspace/installer` (new) — the on-device installer

A ring-3 binary, wired through the four-place new-binary procedure (workspace member, xtask `bins`, ramdisk `BIN_ENTRIES`, no service config — it is invoked, not a daemon). The raw-copy path opens the boot USB `dev_id` and the NVMe `dev_id`, streams sectors USB→NVMe over the capability-gated raw syscalls (with a progress meter), flushes, and triggers the reboot. The partition-aware path drives the on-device GPT writer + `mkfs.ext2` + file copy. The first-user step (Track D) runs before the final reboot.

### `kernel-core::fs::ext2` — the on-device `mkfs` building blocks

The pure-logic ext2 module has the structure **serializers** the format needs — `Ext2Superblock::write_into`, `Ext2BlockGroupDescriptor::write_into`, `Ext2Inode::write_into` — plus the read path and the existing kernel-side allocators (`allocate_inode`, `allocate_data_block`, `write_inode`). What it lacks is the **orchestration** that lays out a blank ext2 (compute group geometry, write the superblock + backup superblocks, the BGD table, the block/inode bitmaps, a root inode, and `lost+found`). Track C's follow-on adds that as host-tested pure logic (a freshly-formatted image must re-mount and round-trip a file), keeping the kernel boundary thin.

## How This Builds on Earlier Phases

- **Reuses the Phase 82 D.3 fork-and-retry template** — `bootstrap_ring3_root_disk` forks `/drivers/ahci` and retries `mount("/dev/blk0", "/", "ext2")`; Track A/B generalize it to fork `/drivers/usb-storage` (+`/drivers/xhci`) and `/drivers/nvme`, with the same bounded-retry loop.
- **Reuses the Phase 87 writable-ext2 path** — the `allocate_inode`/`allocate_data_block`/`write_inode` + deferred-metadata + `BLK_FLUSH` machinery that `ahci-rw`/`ahci-persist` validate; `nvme-rw`/`nvme-persist` are the NVMe analogs, and the writable USB/NVMe root depends on this write path working over `RemoteBlockDevice`.
- **Reuses the Phase 92a USB mass-storage path** — the `usb-storage` BOT `WRITE(10)` data phase, the `usb0.block` registration, and the GPT-aware `usb_ext2_base_lba` probe (built for `/mnt/usb`); Track A promotes that same backend to the **root** role.
- **Reuses the Phase 55b ring-3 NVMe driver** — `nvme.block` → `RemoteBlockDevice` is already the highest-priority root backend in `is_registered()`; Track B only adds the init fork path and the xtask routing so the real rootfs sits behind it.
- **Sits in the Phase 98 GUI-workstation arc** — chartered as "the M1→M3 ladder," gated on bare-metal NVMe root being reachable, and bound to the Phase 98 bare-metal validation strategy + status convention for its HW rungs.

## Implementation Outline

1. **Track A** — add the combined GPT(ESP+ext2) image builder to `xtask` (reuse `create_gpt_disk` + `populate_ext2_files`); extend `bootstrap_ring3_root_disk` to fork `/drivers/xhci`+`/drivers/usb-storage` and retry the root mount via `/dev/usb0`; teach `blk::remote` root slot 0 + `VFS_MOUNT_EXT2_ROOT` to accept a `usbN.block` backend at the GPT base LBA; add `usb-root-smoke`.
2. **Track B** — extend `bootstrap_ring3_root_disk` to fork `/drivers/nvme` and retry; route the real ext2 rootfs to the QEMU NVMe controller in the xtask data-disk emitter; write `nvme-rw` + `nvme-persist` gates as analogs of the AHCI gates; add the `M3OS_NVME_REGRESSION` row to `AGENTS.md`.
3. **Track C** — scaffold `userspace/installer` (four-place wiring); add the capability-gated raw read/write syscalls in `kernel/src/arch/x86_64/syscall/mod.rs` over `blk::*_sectors_dev`; implement the raw USB→NVMe copy + reboot; (follow-on) implement the on-device GPT writer + `kernel-core::fs::ext2` `mkfs` orchestration + file copy; add `nvme-install-smoke`.
4. **Track D** — wire `adduser`/`passwd` into the installer/first-boot to create root + first-user credentials, seed the home dir, and disable the image autologin on the installed rootfs.
5. **Track E** — keep `nvme-rw`/`nvme-persist`/`usb-root-smoke`/`nvme-install-smoke` green in CI; run the bare-metal protocol for M1 (USB boot of the Dell) and M3 (real NVMe install) and record `Validated-on-HW` evidence.

## Acceptance Criteria

- **M1:** `cargo xtask image --combined` produces a **single** GPT disk with an ESP(FAT, kernel) + ext2(rootfs) layout; `usb-root-smoke` boots that image as a QEMU `usb-storage` stick and asserts the serial sentinels `init: / mounted (ext2 via ring-3 usb0.block)` and a write+read-back of a file under `/` (proving the root is **writable**, not the ramdisk fallback). The bare-metal rung: booting the same image on the Dell from USB mounts a writable ext2 root — `Validated-on-HW (run N, date)` per `docs/appendix/bare-metal-validation.md`, evidence captured via `usb-logsink` boot.log + AMT SOL.
- **M2:** with the rootfs routed to the QEMU NVMe controller, `init` forks `/drivers/nvme`, the kernel logs `init: / mounted (ext2 via ring-3 nvme.block)`, and `nvme-rw` (a ≥200 KiB file write + fresh-process byte-verify read-back over `nvme.block`) and `nvme-persist` (two-boot marker write → `BLK_FLUSH` drain → reboot → re-mount → marker re-read, with no `[blk] remote block flush failed`) both PASS in QEMU. The bare-metal rung: the Dell boots writable from its internal NVMe — `Validated-on-HW (run N, date)`.
- **M3:** `nvme-install-smoke` boots a USB-attached combined image, runs `userspace/installer` against a blank NVMe scratch disk, reboots QEMU with **only** the NVMe attached, and asserts the installed system boots to a login with the created first user present in `/etc/passwd`. The bare-metal rung: the installer writes the Dell's internal NVMe from a USB-resident image and the machine reboots into the installed NVMe system with a created first user — `Validated-on-HW (run N, date)`.
- **Access control:** every installer-scoped raw-block-write target is capability-checked — a non-installer process invoking the raw cross-`dev_id` write syscall gets `EPERM` (host/kernel-test asserted), so the installer's destructive power is not ambient.
- The combined image, the USB/NVMe root bootstraps, and the installer are documented; `AGENTS.md` carries the `M3OS_NVME_REGRESSION` gate row with the same skip-vs-pass semantics as the existing AHCI rows.

## Companion Task List

- [Phase 106 Task List](./tasks/106-usb-installer-nvme-tasks.md)

## How Real OS Implementations Differ

- Real installers (Debian `debian-installer`, Fedora `anaconda`, `calamares`) are large, scriptable, partition-aware GUIs that run `libparted` + `mkfs.ext4`/`mkfs.btrfs`, install a bootloader (GRUB/systemd-boot) and write a UEFI **NVRAM `BootXXXX` variable** via `efibootmgr`. Phase 106's first cut is closer to **Raspberry Pi Imager / balenaEtcher** (a raw `dd` of a prebuilt image) and relies on the firmware's removable-media boot path rather than writing an EFI boot variable; the partition-aware follow-on adds the `mkfs`-from-scratch capability those tools take for granted.
- Production `mkfs.ext4` handles dozens of features (journaling, extents, 64-bit, flex_bg, resize_inode, metadata checksums); the on-device `mkfs.ext2` here targets the **bring-up subset** — a plain rev-1 ext2 with the exact geometry m3OS's reader/writer already mount.
- Real OSes treat the rootfs as resizable, encryptable (LUKS), and journaled, often atop LVM or with A/B update partitions and rollback; this phase ships a single fixed ext2 partition with no resize, encryption, or journaling.
- Mature installers verify image integrity (signature/checksum) and support network installs; here the combined image is built and trusted locally, and a networked/signed image pull is deferred to the Phase 107 packaging arc.

## Deferred Until Later

- **EFI boot-variable management** — writing/ordering UEFI `BootXXXX` NVRAM entries (the `efibootmgr` equivalent) so the installed NVMe is a first-class boot target independent of removable-media fallback; the raw-copy first cut relies on firmware removable-media boot order.
- **The partition-aware installer follow-on** if M3 ships the raw-image copy first — the on-device GPT writer, ESP/FAT creator, and `kernel-core::fs::ext2` `mkfs` orchestration land as a follow-on once the raw path is HW-validated.
- **ext4 / journaling / xattr / resize / LVM / LUKS encryption / swap** — the broader storage-depth backlog (Phase 98 accepted-deferred); this phase is plain unencrypted ext2 only.
- **Network / signed image install** — pulling the install image over the Phase 107 networked-packaging stack rather than from a locally-built USB stick.
- **A TUI/GUI installer** on the Phase 105 native toolkit — the first installer is a serial/console-driven tool; a graphical installer is a later polish item.
- **A/B update partitions + rollback** and **multi-disk / RAID layouts** — out of scope for the single-disk workstation install.
