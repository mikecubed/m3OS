# Handoff — Phase 106: USB Installer & NVMe Install

**Date:** 2026-07-03 (living doc — update on every session working this phase)
**Branch:** `feat/phase-106-installer` (off `main`; head `6838f8ba`)
**State:** IN PROGRESS.
- **Track A (M1)** ✅ merged — PR #294 (`40a9e685`). Combined GPT(ESP+ext2)
  USB image + USB-ext2 root bootstrap. `usb-root-smoke` green.
- **Track B (M2)** ✅ merged — PR #295 (`9510a0a1`). NVMe root boot +
  `nvme-rw` / `nvme-persist` gates green.
- **Track C (M3)** 🟡 foundation landed on `feat/phase-106-installer`
  (**PR #296**, open): C.1 installer scaffold + C.2 capability-gated raw
  block syscalls + C.3 raw `dd`-copy installer + kernel root-slot-release
  fix. The end-to-end **`nvme-install-smoke` gate is written but WIP /
  not-in-CI**, blocked on a USB-storage driver limitation (below).
- **Tracks D / E** — not started (D first-user; E bare-metal sign-off).

**Charter:** `docs/roadmap/106-usb-installer-nvme.md`
**Tasks:** `docs/roadmap/tasks/106-usb-installer-nvme-tasks.md`

---

## Where things stand

### Merged to `main`

- **PR #294 — Track A (M1).** Host-side combiner `image --combined` lays one
  GPT disk `[protective MBR | GPT | ESP FAT (kernel+bootloader) | ext2
  rootfs]` (reuses `create_gpt_disk` + `populate_ext2_files`).
  `bootstrap_ring3_root_disk` now forks `/drivers/xhci` + `/drivers/usb-storage`
  on a failed root mount, waits for `usb0.block`, and the kernel root slot 0 +
  `VFS_MOUNT_EXT2_ROOT` accept a `usbN.block` backend **at the GPT base LBA**
  (not just whole-disk MBR). Gate: `usb-root-smoke` (`M3OS_USB_ROOT_REGRESSION=1`).
- **PR #295 — Track B (M2).** `bootstrap_ring3_root_disk` gained a Stage-1
  `/drivers/nvme` fork arm before AHCI/USB; the xtask data-disk router can place
  the real ext2 rootfs behind a QEMU `nvme` controller (`DeviceSet.nvme_root`).
  Gates `nvme-rw` + `nvme-persist` (`M3OS_NVME_REGRESSION=1`) are direct analogs
  of the always-on `ahci-rw`/`ahci-persist` gates and pass.

### On `feat/phase-106-installer` (PR #296, open) — Track C foundation

**C.1 — installer scaffold (four-place new-binary wiring).**
`userspace/installer` (`/sbin/installer`), workspace member, xtask `bins`
entry `("installer","installer",true)`, ramdisk `SBIN_ENTRIES` entry
(mounts at `/sbin`). No service config — it is invoked, not a daemon.

**C.2 — capability-gated raw cross-`dev_id` block syscalls (`0x117x`).**
ABI pinned in `kernel-core/src/installer.rs` (host-tested):
`SYS_BLK_RESOLVE_DEV=0x1170`, `SYS_BLK_RAW_READ=0x1171`,
`SYS_BLK_RAW_WRITE=0x1172`, `SYS_BLK_RAW_FLUSH=0x1173`,
`INSTALLER_EXEC_PATH="/sbin/installer"`, `SECTOR_BYTES=512`,
`MAX_SECTORS_PER_RAW_REQUEST=256`, `raw_count_ok(count)`.
Kernel dispatch (`kernel/src/arch/x86_64/syscall/mod.rs`): each raw syscall is
access-checked via `is_installer_process()` (`is_current_exec_path("/sbin/installer")`
— the unforgeable exec-path trust model, identical to the `/drivers/` device-host
gate; a non-installer caller gets `EPERM`). `raw_request_bytes()` bounds `count`
via `raw_count_ok`→`EINVAL`, rejects `dev_id > u32::MAX`→`EINVAL` and an
unregistered secondary→`ENODEV`. `dev0` routes to `read_sectors`/`write_sectors`
(root slot); a secondary `dev_id` to `read_sectors_dev`/`write_sectors_dev`.

**C.3 — raw `dd`-copy installer + kernel root-slot-release fix.**
- `userspace/installer/src/main.rs`: reads source `dev_id 0` LBA0 (checks
  `0x55AA` + `0xEE@450` = protective MBR/GPT), LBA1 (`"EFI PART"`), derives the
  copy span from the backup-header LBA at GPT-header offset 32
  (`alt_lba` → `total_sectors = alt_lba + 1`, so exactly `0..=alt_lba`, never a
  whole physical stick). Resolves `TARGET_SERVICE="nvme.block"` to a secondary
  `dev_id`, size-guards by probe-reading the target's last-needed sector (QEMU
  nvme rejects out-of-range LBA → real capacity check), then a **sparse** copy
  loop in `CHUNK_SECTORS=256` (128 KiB) chunks: all-zero source chunks are read
  but skipped (target is zero-filled), so only data + GPT/ext2 metadata is
  written. Flushes the target, `reboot(RESTART)` (skipped under `--no-reboot`).
  Serial sentinels: `INSTALLER:start/source/copy/progress/done/rebooting/error`.
- **Root-slot-release fix** (`kernel/src/blk/remote.rs`): when the USB image
  boots but NVMe should take over, the auto-adopted root slot 0 must be
  releasable. Added `ROOT_SKIP_MASK` (`SKIP_NVME/AHCI/USB` bits), rewrote the
  `is_registered()` auto-discovery chain to a `try_lookup` closure that skips a
  candidate whose bit is set, and added `release_root_and_skip()` (clears slot 0,
  clears `REMOTE_BLOCK_REGISTERED` + mask bit 0, sets the skip bit; no-op if the
  slot was explicitly-registered or holds no auto-service). Wired into the
  `VFS_MOUNT_EXT2_ROOT` `ENODEV` path via `crate::blk::release_root_and_skip()`.
  **Verified non-regressing:** the C.3 push ran the pre-push battery with
  `M3OS_USB_ROOT_REGRESSION=1 M3OS_NVME_REGRESSION=1` and passed (exit 0), so
  normal USB/NVMe root mounting is unaffected.

Host tests added along the way: `kernel_core::installer` ABI+bounds;
`qemu_args_with_nvme_root_routes_rootfs_to_nvme`,
`nvme_gates_assert_root_mounted_over_nvme_block`,
`combined_gpt_image_is_kernel_probe_discoverable`.

---

## The one real blocker — `nvme-install-smoke` (Track C / E.2)

The gate `cmd_nvme_install_smoke` (`SMOKE_EXIT_NVME_INSTALL_SMOKE_FAILED=103`,
`M3OS_NVME_INSTALL_REGRESSION` shape) is a **two-boot** oracle: boot 1 attaches
both the combined USB image and a **blank** NVMe, runs `/sbin/installer`
(USB→NVMe copy), reboots; boot 2 attaches **only** the NVMe and asserts a serial
login. It is committed as finished scaffold but **marked WIP / not wired into
CI**, blocked on:

1. **USB-storage 256-sector raw reads FAIL.** 1-sector raw reads over
   `usb0.block` work (the installer's LBA0/LBA1 GPT probe succeeds —
   diagnostic showed `lba1=4546492050415254` = `"EFI PART"`), but the first
   256-sector (128 KiB) copy read returns an error (`INSTALLER:error
   read-failed lba=0`). **The USB BOT read path caps well below the block-IPC
   `MAX_SECTORS_PER_REQUEST=256`.** This is a `usb-storage` driver-hardening
   problem, **not** an installer bug — the installer, the raw syscalls, the GPT
   parse, the target resolve, and the copy loop are all correct and were
   observed running live.
2. **Dual `usb-storage` instance restart flakiness** — the two-drive boot-1
   topology (USB stick + blank NVMe, both needing controllers) has a transient
   restart window.
3. **TCG raw-copy slowness** — a full sector-by-sector image copy under TCG is
   slow; the sparse-copy optimization mitigates but does not eliminate it.

**Fix direction:** harden the ring-3 `usb-storage` BOT `READ(10)` path to honor
multi-sector (up to 256) requests — chunk internally to the controller's max
transfer if needed, but present the full `read_sectors_dev` count to the caller.
Once large reads are stable, revisit the dual-instance restart window, then wire
the gate into CI behind `M3OS_NVME_INSTALL_REGRESSION=1`.

---

## Remaining Phase 106 work

- **C.4 — on-device GPT writer + ESP/FAT creator** (partition-aware follow-on to
  the raw copy). Lets the installer lay a fresh GPT + ESP sized to the target
  disk instead of a byte-for-byte image clone.
- **C.5 — on-device `mkfs.ext2`** — the only genuinely new pure-logic capability
  in this phase. `kernel-core::fs::ext2` has the structure **serializers**
  (`Ext2Superblock::write_into`, `Ext2BlockGroupDescriptor::write_into`,
  `Ext2Inode::write_into`) + the read path + kernel-side allocators, but nothing
  **orchestrates a from-scratch format** (group geometry, superblock + backups,
  BGD table, block/inode bitmaps, root inode, `lost+found`). Add it as
  host-tested pure logic with a round-trip test (format → re-mount → write+read a
  file), keeping the kernel boundary thin.
- **Track D — first-user / account setup.** Wire `adduser`/`passwd`
  (PBKDF2/`crypto-lib`) into the installer or a one-shot first-boot: create
  root + first-user `/etc/passwd`+`/etc/shadow`, seed the home dir, **disable the
  image's autologin** so the installed NVMe system presents a login. No new
  crypto — reuse existing tooling.
- **Track E — validation / bare-metal sign-off.** Keep `usb-root-smoke` /
  `nvme-rw` / `nvme-persist` green; green `nvme-install-smoke` once the USB
  blocker is fixed; run the Phase 98 bare-metal protocol on the Dell for M1 (USB
  boot) and M3 (real NVMe install) and record `Validated-on-HW (run N, date)`.
  Operator-owned — needs physical access (see `docs/handoffs/next-dell-session.md`).

---

## Gotchas learned this phase (don't re-discover)

- **NVMe self-test was destructive.** The ring-3 `nvme_driver` wrote a bring-up
  sentinel to LBA 0, clobbering a real rootfs MBR when routed as the root disk.
  Fixed in `userspace/drivers/nvme/src/main.rs`: preread LBA0, run write+read-back
  against it, **restore** the original bytes. Keep any block-driver self-test
  non-destructive.
- **Serial DMA-alloc flood.** A per-request `device_host.dma_alloc` `log::info!`
  emitted ~14k lines/boot on the NVMe I/O path and starved the persist gate's
  prompt matching. Demoted to `log::debug!` in
  `kernel/src/syscall/device_host.rs`. Watch for hot-path INFO logs on new block
  drivers.
- **QEMU device-slot collisions.** The nvme controller targets a sentinel BDF;
  xhci took the same slot in the multi-device install topology. Pin it:
  `-device nvme,...,addr=0x4`.
- **Serial-pattern matching (the recurring one):**
  - Multi-byte UTF-8 (em-dash `—`) can split across a `read()` boundary under
    lossy decode and **never match**. Use ASCII-only sentinel patterns.
  - `serial_buf` trims to a 48 KB tail, so an **early** pattern can be evicted
    before the first `.contains()` on a fast boot. Prefer waiting on a
    **tail-stable** sentinel that only appears once the thing you care about
    happened (e.g. `m3OS login:` only prints if root mounted).
  - Gate stdout goes to the harness's `> …out` redirect file, not the
    background-task capture file — grep the **right** file.
- **`vfs_server: "no ext2 partition found"` is a RED HERRING** on any GPT image.
  `vfs_server` only does MBR probing and bails on GPT (same on `usb-root-smoke`);
  login still works via the kernel ext2 fallback. Not a bug — don't chase it.
- **The root-slot release is timing-dependent.** A slow NVMe bring-up can register
  `nvme.block` *after* init already mounted USB, so no release is needed that boot
  — don't assert `"releasing + skipping it"` unconditionally in a gate; assert on
  login-reached.
- **Merge workflow:** `gh pr merge N --squash --delete-branch --admin`. GitHub
  **closes** (does not retarget) stacked PRs when their base branch is deleted —
  rebase stacked branches onto `main` around a merge.
- **Kill stale QEMU:** `pkill -9 -f "qemu-system-x86_6[4] -bios"` (the bracket
  keeps `pkill` from matching itself).

---

## Next actions (suggested order)

1. **Land PR #296** (Track C foundation) — title updated to cover C.1+C.2+C.3.
2. **Harden `usb-storage` multi-sector reads** so 256-sector raw reads succeed;
   then green `nvme-install-smoke` and wire it into CI.
3. **C.5 on-device `mkfs.ext2`** (host-tested pure logic) → **C.4** GPT/ESP writer.
4. **Track D** first-user setup (reuse `adduser`/`passwd`; disable autologin).
5. **Track E** bare-metal M1/M3 on the Dell (operator-owned).
