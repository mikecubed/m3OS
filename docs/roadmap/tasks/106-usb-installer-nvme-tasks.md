# Phase 106 — USB Installer & NVMe Install: Task List

**Status:** In progress — Track A (M1) ✅ merged (PR #294), Track B (M2) ✅ merged (PR #295), Track C foundation (C.1/C.2/C.3) ✅ merged (PR #296), and **`nvme-install-smoke` is GREEN end-to-end** (2026-07-03) on `feat/phase-106-usb-storage-multisector`: usb-storage multi-sector transfers ride a 64 KiB shm bounce (`SubmitShmTransfer`, 128-sector SCSI commands), a single-daemon guard stops concurrent-instance BOT probe collisions, the xHCI bulk completion-wait budget is jitter-proof, and the gate is in pre-push behind `M3OS_NVME_INSTALL_REGRESSION=1`. Remaining: C.4/C.5 (partition-aware GPT/ESP writer + on-device `mkfs.ext2`), Track D (first-user), Track E (bare-metal sign-off). See `docs/handoffs/2026-07-03-phase-106-usb-installer-nvme.md`.
**Source Ref:** phase-106
**Depends on:** Phase 82/87 (AHCI fork-and-retry root bootstrap + writable ext2) ✅, Phase 92a (USB mass-storage with writable `/mnt/usb`) ✅, Phase 55b (ring-3 NVMe driver) ✅, Phase 98 (GUI-workstation re-charter + bare-metal validation strategy) ✅. **Gate:** the NVMe-root milestone is gated on **bare-metal NVMe root boot validated** per the [bare-metal validation strategy](../../appendix/bare-metal-validation.md).
**Goal:** Climb the M1→M3 ladder from "boots a flashed read-only image" to "installs m3OS onto the Dell's internal NVMe": **M1** a single combined GPT(ESP+ext2) USB image that boots writable from USB; **M2** an NVMe root boot mirroring the AHCI path, with `nvme-rw`/`nvme-persist` gates passing in QEMU; **M3** an on-device installer that writes the NVMe from a USB-resident image and creates the first user. Reuse the Phase 82 `bootstrap_ring3_root_disk` template, the Phase 87 writable-ext2 path, the Phase 92a `usb-storage` write path + `usb_ext2_base_lba` GPT probe, and the Phase 55b ring-3 NVMe driver. HW rungs follow the Phase 98 bare-metal protocol (`docs/appendix/bare-metal-validation.md`) and land as `Validated-on-HW (run N, date)`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Combined GPT(ESP+ext2) USB image + USB-ext2 root bootstrap (M1) | — | ✅ Merged (PR #294) — `usb-root-smoke` green |
| B | NVMe root boot + `nvme-rw`/`nvme-persist` gates (M2) | — | ✅ Merged (PR #295) — both gates green |
| C | On-device installer: raw USB→NVMe copy, then partition-aware `mkfs` (M3) | A, B | 🟢 C.1/C.2/C.3 merged (PR #296); `nvme-install-smoke` GREEN; C.5 mkfs.ext2 merged (PR #299, e2fsck-validated); **C.4 + C.5-populate landed** (`installer --part` + `nvme-install-part-smoke`) |
| D | First-user / account setup on the installed rootfs (M3) | C | 🟢 Landed — `installer --part` first-user prompts (default; `--no-user` opts out); `nvme-install-part-smoke` logs in as the created user |
| E | Validation: QEMU gates + bare-metal sign-off | A, B, C, D | 🟡 M1/M2 QEMU arms green; M3 gate GREEN in pre-push (`M3OS_NVME_INSTALL_REGRESSION=1`); HW rungs operator-owned |

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
- [x] When no `nvme.block`/`ahci.block` is present and a trusted `/drivers/` process has registered `usb0.block`, `is_registered()` adopts it into root slot 0 (owner-gate unchanged — an untrusted registrant is still rejected and logged).
- [x] The existing `nvme.block`-then-`ahci.block` priority is preserved when those are present (a host/kernel-test asserts USB is only the last-resort root backend).
- [x] `MAX_REMOTE_BLOCK` and the per-`dev_id` paths (`read_sectors_dev`/`write_sectors_dev`/`flush_dev`) are untouched.

### A.3 — GPT-aware root mount

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** the `mount` (165) `VFS_MOUNT_EXT2_ROOT` arm; reuse `usb_ext2_base_lba`
**Why it matters:** The root mount path uses `crate::blk::mbr::probe_ext2()` + `mount_ext2(base_lba)` (whole-disk / MBR), so it cannot find an ext2 partition that lives **after** an ESP on a GPT stick; the secondary-mount path already solves this with `usb_ext2_base_lba`.

**Acceptance:**
- [x] When the root backend is a `usbN.block` GPT device, `VFS_MOUNT_EXT2_ROOT` resolves the ext2 base LBA via `usb_ext2_base_lba(dev_id)` and mounts via `mount_ext2`/`mount_dev` at that LBA.
- [x] The legacy virtio-blk / MBR whole-disk root path (`probe_ext2()` → `base_lba`) is unchanged — existing root-mount gates stay green.
- [x] A bad/missing ext2 partition fails the mount cleanly (`ENODEV`/`EIO`), never a panic.

### A.4 — `bootstrap_ring3_root_disk` forks the USB storage stack

**File:** `userspace/init/src/main.rs`
**Symbol:** `bootstrap_ring3_root_disk`, `init_main` (the root-mount sequence)
**Why it matters:** On a failed root mount, init forks only `/drivers/ahci` today; a USB-only boot needs `/drivers/xhci` + `/drivers/usb-storage` brought up so `usb0.block` registers, then a retry of the root mount against the USB device.

**Acceptance:**
- [x] On root-mount failure, `bootstrap_ring3_root_disk` (or a sibling) forks `/drivers/xhci` then `/drivers/usb-storage`, polls for `usb0.block`, and retries the root mount within the bounded retry loop (extends the existing 15×100 ms loop with USB-bring-up headroom).
- [x] On success, init logs `init: / mounted (ext2 via ring-3 usb0.block)` and proceeds to a **writable** root (not `add_builtin_defaults`' ramdisk fallback).
- [x] On a normal virtio/NVMe/AHCI root (first mount already succeeded) this path is never reached — a no-op detour, asserted by the unchanged existing root gates.

### A.5 — USB-root service-config baseline

**File:** `userspace/init/src/main.rs`
**Symbol:** `add_builtin_defaults` / `BUILTIN_CONFIGS`
**Why it matters:** A writable USB root means `/etc/services.d` is now present on the stick, so the boot can use the on-disk configs instead of the minimal ramdisk `BUILTIN_CONFIGS`; the fallback must remain correct for a still-unmountable stick.

**Acceptance:**
- [x] When the USB root mounts writable, init reads `/etc/services.d` from it (the ramdisk `BUILTIN_CONFIGS` fallback is taken only when the root is still unmountable).
- [x] The comment block at `add_builtin_defaults` documenting the "USB root is future work — slot 0 only auto-discovers nvme/ahci" limitation is updated to reflect A.2/A.3 landing.

---

## Track B — NVMe Root Boot + Persistence Gates (M2)

### B.1 — `bootstrap_ring3_root_disk` forks `/drivers/nvme`

**File:** `userspace/init/src/main.rs`
**Symbol:** `bootstrap_ring3_root_disk` (NVMe arm)
**Why it matters:** The kernel root slot already prefers `nvme.block` (`is_registered`), and the Phase 55b driver registers it — but init never forks `/drivers/nvme`, so an NVMe-rooted boot can't bring the driver up. This mirrors the Phase 82 AHCI fork-and-retry exactly.

**Acceptance:**
- [x] On root-mount failure, init's `bootstrap_ring3_root_disk` forks `/drivers/nvme` as **Stage 1** (before AHCI/USB — matching the kernel root slot's `nvme > ahci > usb` priority) and retries the root mount within the bounded 15×100 ms loop; the driver exits cleanly with no controller, so it's a no-op on non-NVMe roots.
- [x] On success, init logs `init: / mounted (ext2 via ring-3 nvme.block)` (the `nvme-rw`/`nvme-persist` boot-log sentinel).
- [x] `execve` failure of `/drivers/nvme` falls through to the AHCI/USB stages via the shared `fork_driver` diagnostic (no hang).

### B.2 — Route the real rootfs to the QEMU NVMe controller

**File:** `xtask/src/main.rs`
**Symbol:** `qemu_args_with_devices_resolved` (the `data_disk` routing block), `DeviceSet`
**Why it matters:** `--device nvme` attaches a *scratch* second drive (`target/nvme.img`) today; the `nvme-rw`/`nvme-persist` gates need the **real ext2 rootfs** placed behind the NVMe controller, exactly as `devices.ahci` routes the rootfs to `ich9-ahci`.

**Acceptance:**
- [x] `DeviceSet.nvme_root` routes the real `disk.img` rootfs behind the QEMU `nvme` controller (`-drive if=none,id=nvmeroot0` + `-device nvme,serial=deadbeef,drive=nvmeroot0`), distinct from the scratch `devices.nvme` drive.
- [x] The default virtio-blk and `--device ahci` routings are unchanged; `qemu_args_with_nvme_root_routes_rootfs_to_nvme` asserts the emitted args (nvmeroot0 drive + nvme device, no virtio/AHCI/scratch-nvme0).

### B.3 — `nvme-rw` gate

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_nvme_rw_smoke` (analog of `cmd_ahci_rw_smoke`)
**Why it matters:** Proves the ring-3 NVMe **write** path: a payload write that round-trips `blk::remote::write_sectors` → `do_write_ipc` → the ring-3 `nvme_driver` `handle_write`, the NVMe analog of the always-on `ahci-rw-smoke`.

**Acceptance:**
- [x] `nvme-rw-smoke` boots the NVMe-rooted image, logs in, runs the ext2-coherence 200 KiB write + fresh-process byte-verify over `nvme.block`. **PASSED 22s.** (Uncovered + fixed a real bug: the driver's bring-up self-test wrote LBA 0 destructively, clobbering the rootfs MBR — now save-and-restore.)
- [x] Asserts `init: / mounted (ext2 via ring-3 nvme.block)` (guarded by the `nvme_gates_assert_root_mounted_over_nvme_block` unit test).
- [x] Always-on in CI (mirrors `ahci-rw-smoke`); SKIP-with-reason without a musl cross-compiler.

### B.4 — `nvme-persist` gate

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_nvme_persist_smoke` (analog of `cmd_ahci_persist_smoke`)
**Why it matters:** The reboot-persistence proof — a durable on-disk write must survive a re-mount, exercising the `BLK_FLUSH` IPC path on NVMe.

**Acceptance:**
- [x] `nvme-persist-smoke` — two-boot gate against the same NVMe disk (marker write + flush drain, teardown, fresh remount + re-read). **PASSED 10s.**
- [x] Asserts boot 1 logged **no** `[blk] remote block flush failed`.
- [x] Always-on in CI (mirrors `ahci-persist-smoke`). *(Also demoted the per-request `device_host.dma_alloc` kernel log INFO→DEBUG — the NVMe I/O path allocs a landing buffer per request and flooded the serial ~14k lines/boot, starving prompt matching.)*

### B.5 — `M3OS_NVME_REGRESSION` gate documentation

**Files:**
- `AGENTS.md` (pre-push opt-in gate table)
- `docs/roadmap/README.md` (Phase 106 row + mermaid node)

**Symbol:** the `M3OS_NVME_REGRESSION` row; the Phase 106 summary row
**Why it matters:** Keeps the new gates discoverable and the roadmap accurate per the documentation policy; `nvme-rw`/`nvme-persist` parallel the always-on AHCI gates.

**Acceptance:**
- [x] `M3OS_NVME_REGRESSION=1` row added to the `AGENTS.md` gate table covering `nvme-rw`/`nvme-persist` + a `regression-gates.md` section each.
- [x] `docs/roadmap/README.md` Phase 106 row updated for Track B (the mermaid node + phase dependencies landed with the phase charter).

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
- [x] `cargo xtask check` builds `installer` (workspace member + xtask `bins` entry `needs_alloc=true`); embedded via `INSTALLER_ELF` in `SBIN_ENTRIES` at `/sbin/installer`.
- [x] Defines `#[global_allocator]` (`BrkAllocator`) + `syscall-lib` `alloc` feature; depends on `kernel-core` for the shared `installer` ABI module.

### C.2 — Capability-gated raw cross-`dev_id` block syscalls

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** new `SYS_BLK_RAW_READ` / `SYS_BLK_RAW_WRITE` (thin wrappers over `blk::read_sectors_dev` / `blk::write_sectors_dev`)
**Why it matters:** The installer must read sectors from the boot USB `dev_id` and write them to the NVMe `dev_id`; the per-`dev_id` block I/O exists in `kernel/src/blk` but is not exposed to userspace, and raw cross-device writes are too destructive to be ambient.

**Acceptance:**
- [x] `SYS_BLK_RAW_READ`/`SYS_BLK_RAW_WRITE` (0x1171/0x1172) move sectors between a `dev_id` and a user buffer (`dev_id 0` = root → `blk::read_sectors`/`write_sectors`; `1..` → `read_sectors_dev`/`write_sectors_dev`), copying through a bounded (≤128-sector / 64 KiB) heap buffer; plus `SYS_BLK_RESOLVE_DEV` (0x1170) registers/looks up a `dev_id` by service name.
- [x] All three raw syscalls are access-checked against the installer's unforgeable exec path (`is_current_exec_path("/sbin/installer")` — the `/drivers/`-gate trust model; a non-installer caller gets `EPERM`). *(The gate + I/O run live under C.3's `nvme-install-smoke`; the ABI + bounds logic are host-tested in `kernel_core::installer`.)*
- [x] `raw_request_bytes` validates: `count` in `1..=128` (`raw_count_ok`, host-tested) → else `EINVAL`; `dev_id > u32::MAX` → `EINVAL`; an unregistered secondary `dev_id` → `ENODEV`; never a panic/OOB (the byte length cannot overflow at ≤128 sectors).

### C.3 — Raw image copy (USB → NVMe) + reboot

**File:** `userspace/installer/src/main.rs`
**Symbol:** `dd_copy` (the streaming raw-block copy loop)
**Why it matters:** The first-cut installer: a `dd`-style byte-for-byte copy of the combined image from the boot USB onto the NVMe, then a reboot into the installed disk — the simplest correct path to a writable internal install.

**Acceptance:**
- [x] `program_main` derives the exact copy span from the source's own GPT (backup-header LBA at offset 32 = last meaningful sector, so `0..=alt_lba`, not a whole physical stick), resolves the NVMe target by service name, streams in ≤128 KiB chunks (**sparse: all-zero source chunks are read but not written**, since the target is zero-filled — cuts the write round-trips to the real-data + GPT/ext2-metadata blocks), and flushes the target via the new `SYS_BLK_RAW_FLUSH`. Progress logged every ~10%.
- [x] Aborts non-destructively (logs `INSTALLER:error …`, no partial write) if the target resolves to the boot device (`target-is-source`) or if a probe read at the source's last-needed sector fails (`target-too-small` — a real capacity check via the target's out-of-range-LBA rejection, no capacity syscall needed).
- [x] After copy + flush, issues `reboot(RESTART)` (skipped under `installer --no-reboot`); the written NVMe carries the identical GPT(ESP+ext2) layout. *(Proven end-to-end 2026-07-03: `nvme-install-smoke` GREEN — the former blockers were the usb-storage inline-path throughput, a concurrent-instance BOT probe collision, and the xHCI bulk completion-wait budget; all fixed. In pre-push behind `M3OS_NVME_INSTALL_REGRESSION=1`.)*

### C.4 — On-device GPT writer + ESP copy (partition-aware follow-on)

**Files:**
- `kernel-core/src/fs/gpt.rs` *(new — pure-logic GPT builder + CRC-verified parser)*
- `kernel-core/src/fs/ext2_populate.rs` *(new — populate walker + write-back block cache; the C.5 populate arm)*
- `userspace/installer/src/main.rs` (`installer --part`)

**Symbol:** `build_gpt` / `GptPlan::for_target` / `parse_gpt` (kernel-core); `install_part` (installer)
**Why it matters:** A partition-aware install sizes the rootfs to the target disk (a raw copy wastes everything past the image size); it must write a GPT + a protective MBR and lay down an ESP FAT with the bootloader/kernel. *(Design note: the ESP is laid down by a same-span raw copy of the source ESP rather than a from-scratch FAT format — the FAT's geometry is partition-relative and its `hidden sectors` field is the unchanged partition start LBA, so the copy is exactly as valid and needs no on-device FAT formatter; only the rootfs partition grows.)*

**Acceptance:**
- [x] Writes a valid GPT (protective MBR + primary/backup headers + partition entries + CRC32s) onto the NVMe with an ESP + a Linux partition grown to the target's last usable LBA; the produced GPT parses with `usb_ext2_base_lba`-style logic *(host test replays the kernel's exact `gpt_ext2_scan`)*, with the **independent `gpt` crate** *(the gate's host-side cross-check)*, and passes **`sgdisk --verify`** *(external-validator host test, skip-with-reason)*.
- [x] The target carries a FAT ESP with the bootloader + kernel (same-span raw copy, sparse); the firmware boots the resulting disk (validated in `nvme-install-part-smoke`, Track E).
- [x] Fails closed: a CRC-corrupt source GPT, a missing ESP/Linux partition, an unreadable source ext2, a too-small target, or `target-is-source` all abort before (or without) touching the target — `INSTALLER:error part-*` sentinels.

### C.5 — On-device `mkfs.ext2` 🟢 pure-logic core landed (2026-07-04)

**Files:**
- `kernel-core/src/fs/ext2_format.rs` *(new module; `ext2.rs` gained `Ext2Superblock::write_full_into`)*
- `userspace/installer/src/main.rs` *(populate integration — deferred to C.4, see below)*

**Symbol:** new `format_ext2` orchestration + `Ext2Fs` write handle, composing the existing `ext2.rs` serializers (`Ext2Inode::write_into`, `Ext2BlockGroupDescriptor::write_into`, the new full-superblock serializer) and driving them through a `BlockIo` device seam.
**Why it matters:** `kernel-core::fs::ext2` reads and writes an **existing** filesystem but cannot **create** one — there is no orchestration that lays out a superblock, BGD table, block/inode bitmaps, inode table, root inode, and `lost+found` from scratch. This is the genuinely new pure-logic capability.

**Acceptance:**
- [x] `format_ext2` produces a blank rev-1 ext2 image (correct group geometry, primary + per-group backup superblocks with the primary-vs-backup 1024-byte offset asymmetry, BGD table, zeroed-then-marked bitmaps, root inode + `lost+found`) sized to a given block count. Feature set is FILETYPE-only; 128-byte inodes; no `sparse_super`/journal/resize-inode. Geometry validated for degenerate-volume rejection. *(11 host tests in `ext2_format::tests`.)*
- [x] A host test formats an in-memory image, then **re-mounts it through the existing `ext2.rs` reader and round-trips written content** — small file, indirect + double-indirect file, a directory tree with a symlink, 4 KiB blocks, and 60-file dir-block spill all read back byte-identical via `resolve_path`/`read_file_data`/`read_symlink_target`. **Plus** an external-validator test that runs real `e2fsck -fn` on the formatted+populated image and asserts a clean exit (skips-with-reason when `e2fsck` is absent; **ran and passed** on this host).
- [x] The installer can `format_ext2` the NVMe Linux partition, then copy the rootfs files into it (an alternative to the C.3 raw copy). **Landed with C.4** (`installer --part`): `kernel-core/src/fs/ext2_populate.rs` walks the source rootfs through the existing `BlockReader` read path and re-creates the tree via `Ext2Fs::create_*` (mode/uid/gid/timestamps preserved; `lost+found` skipped; a corrupt source's dir cycle terminates via a visited set). IO rides `WriteBackBlockIo` — an LRU write-back cache for metadata read-modify-writes + a contiguous-run coalescer that leaves as single ≤256-sector raw writes (`BlockIo::write_block_run`) — so the populate doesn't cost one IPC round trip per block. Host tests: cross-block-size tree equality, byte-identical cached-vs-direct equivalence, `e2fsck -fn` on a cache-populated target.

---

## Track D — First-User / Account Setup (M3)

### D.1 — Create root + first-user credentials

**Files:**
- `userspace/installer/src/main.rs` (`first_user_prompts` / `shadow_line` / `apply_first_user`)
- `kernel-core/src/fs/ext2_populate.rs` (`populate_from_reader_filtered`), `ext2_format.rs` (`Ext2Fs::lookup`)

**Symbol:** reuse the `passwd` lib's `$sha256i$` chain (`build_hash_field` + `syscall_lib::sha256::hash_password_iterated` + `getrandom` salt — the exact `adduser`/`passwd`/`login` recipe)
**Why it matters:** The installed NVMe system must come up with a real account, not the image's well-known seeded credentials; reuse the existing multi-user tooling rather than new auth crypto.

**Acceptance:**
- [x] `installer --part` prompts on the console (root password / username / user password, echo off via the `adduser` termios pattern) **before any target write**, then writes fresh `/etc/passwd` + `/etc/shadow` (+ `/etc/group`) hashed via the existing `passwd`-lib path. The populate **filters** the image's credential files + `/home/user` off the target (the `Ext2Fs` writer is write-once, so exclusion-then-rewrite is the replacement mechanism; host-tested). `--no-user` opts out.
- [x] The first user's home directory (`/home/<name>`, 0700, uid/gid 1000) is seeded on the installed rootfs, `.profile` carried over from the image's seeded account when present.
- [x] No new password-hashing code is introduced — the `passwd` lib + `syscall_lib::sha256` are reused.

### D.2 — Installed rootfs presents a real login (image credentials replaced)

**File:** `userspace/installer/src/main.rs` (`FIRST_USER_SKIP_PATHS` + `apply_first_user`)
**Symbol:** the filtered populate + fresh credential files
**Why it matters:** The serial image boots to an interactive `login` already (there is no literal serial autologin marker — the smoke images use `smoke-runner` mode, and the graphical gate is the separate `/etc/m3os-graphical-only`); the real exposure is the image's **well-known seeded credentials** (`root:root`, `user:user`). The installed workstation must require the operator-chosen accounts instead.

**Acceptance:**
- [x] The installed rootfs presents a login prompt whose valid credentials are the installer-created ones — the image's seeded `/etc/passwd`/`/etc/shadow`/`/etc/group` are never copied in first-user mode, so `root:root`/`user:user` do not authenticate on the installed system.
- [x] `nvme-install-part-smoke` (Track E) asserts the installed system reaches a login **as the created user** (`mike`/created password) and that the account is present in the installed `/etc/passwd` (uid/home/shell asserted).

---

## Track E — Validation & Bare-Metal Sign-off

### E.1 — `usb-root-smoke` (M1 QEMU arm)

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_usb_root_smoke`
**Why it matters:** QEMU *can* model a USB stick, so the M1 "writable ext2 root from USB" claim is CI-testable even though the Dell boot is not.

**Acceptance:**
- [x] Builds the combined image (A.1), attaches it as a QEMU `usb-storage` device, boots, and asserts `init: / mounted (ext2 via ring-3 usb0.block)`. *(Green — Track A, PR #294; in pre-push behind `M3OS_USB_ROOT_REGRESSION=1`.)*
- [x] Writes a file under `/` and byte-verifies the read-back from a fresh process (proving the root is **writable**, not the ramdisk fallback) — a regression to read-only fails the gate. *(Green — `echo`/`cat` round-trip on `/home/usbprobe.txt`.)*
- [ ] Runs at a timeout sized for a fresh-disk USB boot (floored ≥ 360 s). *(Not yet — the gate does not floor its timeout; pre-push invokes it at 300 s, which has been sufficient in practice.)*

### E.2 — `nvme-install-smoke` (M3 QEMU arm)

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_nvme_install_smoke`
**Why it matters:** The end-to-end installer proof QEMU can model — install from a USB-attached image to a blank NVMe, then boot the NVMe alone.

**Acceptance:**
- [x] Boots from a USB-attached combined image, runs `userspace/installer` against a blank NVMe scratch disk (raw copy and/or partition-aware), and flushes. *(Green 2026-07-03 — ~40 s sparse copy over the usb-storage shm bulk path; in pre-push behind `M3OS_NVME_INSTALL_REGRESSION=1`.)*
- [x] Tears down QEMU, relaunches with **only** the NVMe attached, and asserts the installed system boots to a login. *(Green 2026-07-03 — `init: / mounted (ext2 via ring-3 nvme.block)` + live shell. The "created first user present in `/etc/passwd`" arm landed with Track D in `nvme-install-part-smoke`: boot 2 logs in as the installer-created user and asserts the `/etc/passwd` entry.)*
- [ ] Fails fast on any kernel-fatal marker (reuses the global fatal-line scan). *(Not yet — on failure the gate dumps filtered `INSTALLER:`/driver/`[xhci]` serial lines instead.)*

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
