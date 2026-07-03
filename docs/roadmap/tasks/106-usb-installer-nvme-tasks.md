# Phase 106 — USB Installer & NVMe Install: Task List

**Status:** Planned
**Source Ref:** phase-106
**Depends on:** Phase 82/87 (AHCI fork-and-retry root bootstrap + writable ext2) ✅, Phase 92a (USB mass-storage with writable `/mnt/usb`) ✅, Phase 55b (ring-3 NVMe driver) ✅, Phase 98 (GUI-workstation re-charter + bare-metal validation strategy) ✅. **Gate:** the NVMe-root milestone is gated on **bare-metal NVMe root boot validated** per the [bare-metal validation strategy](../../appendix/bare-metal-validation.md).
**Goal:** Climb the M1→M3 ladder from "boots a flashed read-only image" to "installs m3OS onto the Dell's internal NVMe": **M1** a single combined GPT(ESP+ext2) USB image that boots writable from USB; **M2** an NVMe root boot mirroring the AHCI path, with `nvme-rw`/`nvme-persist` gates passing in QEMU; **M3** an on-device installer that writes the NVMe from a USB-resident image and creates the first user. Reuse the Phase 82 `bootstrap_ring3_root_disk` template, the Phase 87 writable-ext2 path, the Phase 92a `usb-storage` write path + `usb_ext2_base_lba` GPT probe, and the Phase 55b ring-3 NVMe driver. HW rungs follow the Phase 98 bare-metal protocol (`docs/appendix/bare-metal-validation.md`) and land as `Validated-on-HW (run N, date)`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Combined GPT(ESP+ext2) USB image + USB-ext2 root bootstrap (M1) | — | Planned |
| B | NVMe root boot + `nvme-rw`/`nvme-persist` gates (M2) | — | Planned |
| C | On-device installer: raw USB→NVMe copy, then partition-aware `mkfs` (M3) | A, B | Planned |
| D | First-user / account setup on the installed rootfs (M3) | C | Planned |
| E | Validation: QEMU gates + bare-metal sign-off | A, B, C, D | Planned |

---

## Track A — Combined Writable USB Image + USB-ext2 Root Bootstrap (M1)

### A.1 — Combined GPT(ESP + ext2) image builder

**File:** `xtask/src/main.rs`
**Symbol:** new `build_combined_usb_image` (composing `create_uefi_image` + `create_data_disk` content via `create_gpt_disk`); `cmd_image` gains a `--combined` arm
**Why it matters:** Today `cmd_image` emits two separate files — `create_uefi_image` (GPT+ESP kernel) and `create_data_disk` (a separate MBR+ext2 `disk.img`) — and no combiner lays both on one disk, so a USB-only boot has no rootfs partition. One GPT disk with `[ESP FAT] + [ext2 rootfs]` is the M1 medium.

**Acceptance:**
- [x] `cargo xtask image --combined` produces a single GPT disk file (`m3os-usb.img`) with exactly two partitions: an EFI System Partition (FAT, unsigned bootloader + kernel — the `--sign` path's `create_fat_filesystem` recipe) and a Linux partition (ext2 — the rootfs partition lifted from the freshly built `disk.img` at its 1 MiB MBR offset, so `populate_ext2_files` content carries over unchanged). Real-image probe: ESP @ LBA 34 (FAT jump `EB 3C 90`), ext2 root @ LBA 34850 (magic `53EF`).
- [x] The ext2 partition does **not** start at LBA 0; its start LBA is discoverable by the GPT-scan in `usb_ext2_base_lba` — the `combined_gpt_image_is_kernel_probe_discoverable` host test builds a synthetic combined image and replays the kernel's exact probe (protective-MBR `0xEE` → `EFI PART` → 128-byte entry walk → ext2 magic at `first_lba + 2`).
- [ ] `dd`-ing the image to a stick and pointing QEMU's `usb-storage` at it enumerates an ESP **and** an ext2 partition (asserted by `usb-root-smoke`, Track E — pends A.2–A.4).

### A.2 — Root slot 0 accepts a `usbN.block` backend

**File:** `kernel/src/blk/remote.rs`
**Symbol:** `is_registered` (the root slot-0 auto-discovery chain)
**Why it matters:** `is_registered` auto-discovers `"nvme.block"` then `"ahci.block"` for the root slot; the boot USB registers `usb0.block`, which today can only back a *secondary* `/mnt/usb` mount. Promoting it to the root role is the load-bearing kernel change for a writable USB root.

**Acceptance:**
- [ ] When no `nvme.block`/`ahci.block` is present and a trusted `/drivers/` process has registered `usb0.block`, `is_registered()` adopts it into root slot 0 (owner-gate unchanged — an untrusted registrant is still rejected and logged).
- [ ] The existing `nvme.block`-then-`ahci.block` priority is preserved when those are present (a host/kernel-test asserts USB is only the last-resort root backend).
- [ ] `MAX_REMOTE_BLOCK` and the per-`dev_id` paths (`read_sectors_dev`/`write_sectors_dev`/`flush_dev`) are untouched.

### A.3 — GPT-aware root mount

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** the `mount` (165) `VFS_MOUNT_EXT2_ROOT` arm; reuse `usb_ext2_base_lba`
**Why it matters:** The root mount path uses `crate::blk::mbr::probe_ext2()` + `mount_ext2(base_lba)` (whole-disk / MBR), so it cannot find an ext2 partition that lives **after** an ESP on a GPT stick; the secondary-mount path already solves this with `usb_ext2_base_lba`.

**Acceptance:**
- [ ] When the root backend is a `usbN.block` GPT device, `VFS_MOUNT_EXT2_ROOT` resolves the ext2 base LBA via `usb_ext2_base_lba(dev_id)` and mounts via `mount_ext2`/`mount_dev` at that LBA.
- [ ] The legacy virtio-blk / MBR whole-disk root path (`probe_ext2()` → `base_lba`) is unchanged — existing root-mount gates stay green.
- [ ] A bad/missing ext2 partition fails the mount cleanly (`ENODEV`/`EIO`), never a panic.

### A.4 — `bootstrap_ring3_root_disk` forks the USB storage stack

**File:** `userspace/init/src/main.rs`
**Symbol:** `bootstrap_ring3_root_disk`, `init_main` (the root-mount sequence)
**Why it matters:** On a failed root mount, init forks only `/drivers/ahci` today; a USB-only boot needs `/drivers/xhci` + `/drivers/usb-storage` brought up so `usb0.block` registers, then a retry of the root mount against the USB device.

**Acceptance:**
- [ ] On root-mount failure, `bootstrap_ring3_root_disk` (or a sibling) forks `/drivers/xhci` then `/drivers/usb-storage`, polls for `usb0.block`, and retries the root mount within the bounded retry loop (extends the existing 15×100 ms loop with USB-bring-up headroom).
- [ ] On success, init logs `init: / mounted (ext2 via ring-3 usb0.block)` and proceeds to a **writable** root (not `add_builtin_defaults`' ramdisk fallback).
- [ ] On a normal virtio/NVMe/AHCI root (first mount already succeeded) this path is never reached — a no-op detour, asserted by the unchanged existing root gates.

### A.5 — USB-root service-config baseline

**File:** `userspace/init/src/main.rs`
**Symbol:** `add_builtin_defaults` / `BUILTIN_CONFIGS`
**Why it matters:** A writable USB root means `/etc/services.d` is now present on the stick, so the boot can use the on-disk configs instead of the minimal ramdisk `BUILTIN_CONFIGS`; the fallback must remain correct for a still-unmountable stick.

**Acceptance:**
- [ ] When the USB root mounts writable, init reads `/etc/services.d` from it (the ramdisk `BUILTIN_CONFIGS` fallback is taken only when the root is still unmountable).
- [ ] The comment block at `add_builtin_defaults` documenting the "USB root is future work — slot 0 only auto-discovers nvme/ahci" limitation is updated to reflect A.2/A.3 landing.

---

## Track B — NVMe Root Boot + Persistence Gates (M2)

### B.1 — `bootstrap_ring3_root_disk` forks `/drivers/nvme`

**File:** `userspace/init/src/main.rs`
**Symbol:** `bootstrap_ring3_root_disk` (NVMe arm)
**Why it matters:** The kernel root slot already prefers `nvme.block` (`is_registered`), and the Phase 55b driver registers it — but init never forks `/drivers/nvme`, so an NVMe-rooted boot can't bring the driver up. This mirrors the Phase 82 AHCI fork-and-retry exactly.

**Acceptance:**
- [ ] On root-mount failure, init forks `/drivers/nvme` and retries `mount("/dev/blk0", "/", "ext2")` within the bounded loop, alongside the AHCI/USB arms.
- [ ] On success, init logs `init: / mounted (ext2 via ring-3 nvme.block)`.
- [ ] `execve` failure of `/drivers/nvme` logs the negative errno (matching the existing AHCI diagnostic) and falls through, not hangs.

### B.2 — Route the real rootfs to the QEMU NVMe controller

**File:** `xtask/src/main.rs`
**Symbol:** `qemu_args_with_devices_resolved` (the `data_disk` routing block), `DeviceSet`
**Why it matters:** `--device nvme` attaches a *scratch* second drive (`target/nvme.img`) today; the `nvme-rw`/`nvme-persist` gates need the **real ext2 rootfs** placed behind the NVMe controller, exactly as `devices.ahci` routes the rootfs to `ich9-ahci`.

**Acceptance:**
- [ ] A device-flag (e.g. `nvme-root`) routes the real `disk.img` rootfs behind the QEMU `nvme` controller (the `-drive if=none,id=…` + `-device nvme,drive=…` chain), not as a scratch second drive.
- [ ] The default virtio-blk and the `--device ahci` rootfs routings are unchanged (a unit test asserts the emitted QEMU args for each routing).

### B.3 — `nvme-rw` gate

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_nvme_rw_smoke` (analog of `cmd_ahci_rw_smoke`)
**Why it matters:** Proves the ring-3 NVMe **write** path: a payload write that round-trips `blk::remote::write_sectors` → `do_write_ipc` → the ring-3 `nvme_driver` `handle_write`, the NVMe analog of the always-on `ahci-rw-smoke`.

**Acceptance:**
- [ ] Boots the NVMe-rooted image, logs in, runs an ext2-coherence write of ≥200 KiB to a file on `/`, and a fresh process byte-verifies the read-back (a truncated-write regression fails the gate).
- [ ] Asserts `init: / mounted (ext2 via ring-3 nvme.block)` in the boot log.
- [ ] Always-on in CI (mirrors `ahci-rw-smoke`, since default smoke only exercises in-kernel virtio-blk); skip-with-reason without musl if it needs it.

### B.4 — `nvme-persist` gate

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_nvme_persist_smoke` (analog of `cmd_ahci_persist_smoke`)
**Why it matters:** The reboot-persistence proof — a durable on-disk write must survive a re-mount, exercising the `BLK_FLUSH` IPC path on NVMe.

**Acceptance:**
- [ ] Two-boot gate against the same NVMe disk: boot 1 writes a marker to `/`, idles past one periodic write-back flush, QEMU is torn down; boot 2 re-mounts ext2 fresh and re-reads the marker.
- [ ] Asserts boot 1 logged **no** `[blk] remote block flush failed`.
- [ ] Always-on in CI (mirrors `ahci-persist-smoke`).

### B.5 — `M3OS_NVME_REGRESSION` gate documentation

**Files:**
- `AGENTS.md` (pre-push opt-in gate table)
- `docs/roadmap/README.md` (Phase 106 row + mermaid node)

**Symbol:** the `M3OS_NVME_REGRESSION` row; the Phase 106 summary row
**Why it matters:** Keeps the new gates discoverable and the roadmap accurate per the documentation policy; `nvme-rw`/`nvme-persist` parallel the always-on AHCI gates.

**Acceptance:**
- [ ] `M3OS_NVME_REGRESSION=1` row added to the `AGENTS.md` gate table covering `nvme-rw`/`nvme-persist` (+`usb-root-smoke`/`nvme-install-smoke`), with the same skip-vs-pass wording as the `M3OS_AHCI_REGRESSION` row.
- [ ] `docs/roadmap/README.md` has the Phase 106 table row and a mermaid node depending on Phases 82/87/92a/55b.

---

## Track C — On-Device Installer: USB → NVMe (M3)

### C.1 — Installer crate scaffold + four-place wiring

**Files:**
- `userspace/installer/Cargo.toml`, `userspace/installer/src/main.rs` (new)
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`bins` array in `build_userspace`)
- `kernel/src/fs/ramdisk.rs` (`include_bytes!` + `BIN_ENTRIES`)

**Symbol:** `main` (installer entry)
**Why it matters:** Missing any wiring point means the binary is not built, not embedded, or not found at runtime (per "Adding a New Userspace Binary"). It is invoked on demand (not a daemon), so no `services.d` config. `needs_alloc = true`.

**Acceptance:**
- [ ] `cargo xtask check` builds `installer`; it is embedded in the ramdisk and `execve`-able by path.
- [ ] Defines a `#[global_allocator]` (`syscall_lib::heap::BrkAllocator`) and enables the `alloc` feature on `syscall-lib`.

### C.2 — Capability-gated raw cross-`dev_id` block syscalls

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** new `SYS_BLK_RAW_READ` / `SYS_BLK_RAW_WRITE` (thin wrappers over `blk::read_sectors_dev` / `blk::write_sectors_dev`)
**Why it matters:** The installer must read sectors from the boot USB `dev_id` and write them to the NVMe `dev_id`; the per-`dev_id` block I/O exists in `kernel/src/blk` but is not exposed to userspace, and raw cross-device writes are too destructive to be ambient.

**Acceptance:**
- [ ] New raw read/write syscalls move sectors between an arbitrary `dev_id` and a userspace buffer, wrapping `blk::read_sectors_dev`/`write_sectors_dev`.
- [ ] The write syscall is **access-checked** against an installer capability — a process without it gets `EPERM` (a kernel/host test asserts the reject; the installer with the capability succeeds).
- [ ] Out-of-range `dev_id` or sector counts return `EINVAL`, never a panic or OOB.

### C.3 — Raw image copy (USB → NVMe) + reboot

**File:** `userspace/installer/src/main.rs`
**Symbol:** `dd_copy` (the streaming raw-block copy loop)
**Why it matters:** The first-cut installer: a `dd`-style byte-for-byte copy of the combined image from the boot USB onto the NVMe, then a reboot into the installed disk — the simplest correct path to a writable internal install.

**Acceptance:**
- [ ] Streams the full combined image USB→NVMe in bounded chunks via the C.2 syscalls, with a progress indicator, then issues `flush_dev` on the NVMe `dev_id`.
- [ ] Refuses to run if source and destination `dev_id` resolve to the same device, or if the destination is smaller than the source (logged, non-destructive abort).
- [ ] After copy + flush, triggers the reboot; on next boot the NVMe carries the same GPT(ESP+ext2) layout the USB held.

### C.4 — On-device GPT writer + ESP/FAT creator (partition-aware follow-on)

**File:** `userspace/installer/src/main.rs` (+ a host-tested partition module)
**Symbol:** `write_gpt` / `create_esp`
**Why it matters:** A partition-aware install sizes the rootfs to the target disk (a raw copy wastes everything past the image size); it must write a GPT + a protective MBR and lay down an ESP FAT with the bootloader/kernel.

**Acceptance:**
- [ ] Writes a valid GPT (protective MBR + primary/backup headers + partition entries) onto the NVMe with an ESP + a Linux partition; the produced GPT parses with `usb_ext2_base_lba`-style logic.
- [ ] Creates a FAT ESP and copies the bootloader + kernel into it; the firmware can boot the resulting disk (validated in `nvme-install-smoke`, Track E).

### C.5 — On-device `mkfs.ext2`

**Files:**
- `kernel-core/src/fs/ext2.rs`
- `userspace/installer/src/main.rs`

**Symbol:** new `format_ext2` orchestration (composing the existing `Ext2Superblock::write_into` / `Ext2BlockGroupDescriptor::write_into` / `Ext2Inode::write_into` serializers)
**Why it matters:** `kernel-core::fs::ext2` reads and writes an **existing** filesystem but cannot **create** one — there is no orchestration that lays out a superblock, BGD table, block/inode bitmaps, inode table, root inode, and `lost+found` from scratch. This is the genuinely new pure-logic capability.

**Acceptance:**
- [ ] `format_ext2` produces a blank rev-1 ext2 image (correct group geometry, primary + backup superblocks, BGD table, zeroed-then-marked bitmaps, root inode + `lost+found`) sized to a given block count.
- [ ] A host test formats an in-memory image, then **re-mounts it through the existing reader and round-trips a written file** (write a file → fresh mount → read it back identical) — the falsifiable proof the format is valid.
- [ ] The installer can `format_ext2` the NVMe Linux partition, then copy the rootfs files into it (an alternative to the C.3 raw copy).

---

## Track D — First-User / Account Setup (M3)

### D.1 — Create root + first-user credentials

**Files:**
- `userspace/installer/src/main.rs`
- the installed rootfs `/etc/passwd` + `/etc/shadow`

**Symbol:** reuse `adduser` / `passwd` (PBKDF2 via `crypto-lib`)
**Why it matters:** The installed NVMe system must come up with a real account, not the image's autologin; reuse the existing multi-user tooling rather than new auth crypto.

**Acceptance:**
- [ ] The installer (or a one-shot first-boot step) creates a root credential and a first-user account in the installed `/etc/passwd` + `/etc/shadow`, hashed via the existing `passwd`/`crypto-lib` path.
- [ ] The first user's home directory is seeded on the installed rootfs.
- [ ] No new password-hashing code is introduced — `adduser`/`passwd` are reused.

### D.2 — Disable image autologin on the installed rootfs

**File:** `userspace/installer/src/main.rs` (rootfs post-processing)
**Symbol:** the installed `services.d` / login config
**Why it matters:** The build image autologs in for smoke runs; the installed workstation must present a login prompt to the created user.

**Acceptance:**
- [ ] The installed rootfs login config presents a login prompt (the build-image autologin marker is removed/flipped) so the first user must authenticate.
- [ ] `nvme-install-smoke` (Track E) asserts the installed system reaches a login with the created user present.

---

## Track E — Validation & Bare-Metal Sign-off

### E.1 — `usb-root-smoke` (M1 QEMU arm)

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_usb_root_smoke`
**Why it matters:** QEMU *can* model a USB stick, so the M1 "writable ext2 root from USB" claim is CI-testable even though the Dell boot is not.

**Acceptance:**
- [ ] Builds the combined image (A.1), attaches it as a QEMU `usb-storage` device, boots, and asserts `init: / mounted (ext2 via ring-3 usb0.block)`.
- [ ] Writes a file under `/` and byte-verifies the read-back from a fresh process (proving the root is **writable**, not the ramdisk fallback) — a regression to read-only fails the gate.
- [ ] Runs at a timeout sized for a fresh-disk USB boot (floored ≥ 360 s).

### E.2 — `nvme-install-smoke` (M3 QEMU arm)

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_nvme_install_smoke`
**Why it matters:** The end-to-end installer proof QEMU can model — install from a USB-attached image to a blank NVMe, then boot the NVMe alone.

**Acceptance:**
- [ ] Boots from a USB-attached combined image, runs `userspace/installer` against a blank NVMe scratch disk (raw copy and/or partition-aware), and flushes.
- [ ] Tears down QEMU, relaunches with **only** the NVMe attached, and asserts the installed system boots to a login with the created first user present in `/etc/passwd`.
- [ ] Fails fast on any kernel-fatal marker (reuses the global fatal-line scan).

### E.3 — Bare-metal sign-off (M1 + M3 HW rungs)

**File:** `scripts/installer-baremetal.md` (new — a results appendix, generalized from `scripts/ure-baremetal-usb.md`)
**Symbol:** the recorded bare-metal runs
**Why it matters:** QEMU cannot model "the Dell boots writable from its own internal NVMe" or "the installer writes the Dell's NVMe"; the Phase 98 protocol supplies the substitute evidence standard.

> **Status: operator-owned** — requires physical access to the Dell Precision 5560 (Tiger Lake). Follows `docs/appendix/bare-metal-validation.md`: USB boot → `usb-logsink` boot.log + AMT SOL capture; assert serial sentinels; reboot into the installed NVMe.

**Acceptance:**
- [ ] **M1:** booting the combined image on the Dell from USB mounts a writable ext2 root — captured `init: / mounted (ext2 via ring-3 usb0.block)` + a write/read-back over the log sink; recorded as `Validated-on-HW (run N, date) — Dell Precision 5560`.
- [ ] **M2:** the Dell boots writable from its internal NVMe (`init: / mounted (ext2 via ring-3 nvme.block)`) — `Validated-on-HW (run N, date)`.
- [ ] **M3:** the installer writes the Dell's internal NVMe from a USB-resident image and the machine reboots into the installed NVMe system with the created first user logging in — `Validated-on-HW (run N, date)`; evidence (boot.log excerpt / photo) committed and referenced.

---

## Documentation Notes

- This phase is **glue, not new subsystems**: the writable ext2 engine (Phase 87), the `usb-storage` write path + `usb_ext2_base_lba` GPT probe (Phase 92a), the ring-3 NVMe driver (Phase 55b), and the `bootstrap_ring3_root_disk` fork-and-retry template (Phase 82) all pre-exist — record that Track A/B mostly *wire existing parts together*, and the only genuinely new pure-logic capability is the on-device `mkfs.ext2` orchestration in `kernel-core::fs::ext2` (Track C.5).
- The one load-bearing kernel data-path change is **root slot 0 accepting a `usbN.block` backend** (`is_registered`, Track A.2) — the analog of the Phase 82 D.2 `ahci.block` root acceptance; keep the trusted-owner gate intact and note the priority order (`nvme.block` → `ahci.block` → `usb0.block`).
- `nvme-rw`/`nvme-persist` are deliberate analogs of the always-on `ahci-rw`/`ahci-persist` gates — when they land, cross-reference the AHCI gates so a future reader sees the pattern.
- HW rungs use the Phase 98 `Validated-on-HW (run N, date)` convention, **never** a bare "Complete"; the QEMU arms (`usb-root-smoke`/`nvme-install-smoke`/`nvme-rw`/`nvme-persist`) carry the CI half so the un-modelable remainder (the Dell booting its own internal disk) is as small as possible.
- Prefer exact files/symbols over directories as these land; update this list's checkboxes as tracks complete, and record each bare-metal run in `scripts/installer-baremetal.md`.
