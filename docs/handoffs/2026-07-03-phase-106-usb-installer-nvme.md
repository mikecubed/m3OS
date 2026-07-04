# Handoff — Phase 106: USB Installer & NVMe Install

**Date:** 2026-07-03 (living doc — update on every session working this phase)
**Branch:** `feat/phase-106-usb-storage-multisector` (off `main`)
**State:** IN PROGRESS.
- **Track A (M1)** ✅ merged — PR #294 (`40a9e685`). Combined GPT(ESP+ext2)
  USB image + USB-ext2 root bootstrap. `usb-root-smoke` green.
- **Track B (M2)** ✅ merged — PR #295 (`9510a0a1`). NVMe root boot +
  `nvme-rw` / `nvme-persist` gates green.
- **Track C (M3)** 🟡 foundation merged — PR #296 (`13d1cf6e`): C.1
  installer scaffold + C.2 capability-gated raw block syscalls + C.3 raw
  `dd`-copy installer + kernel root-slot-release fix. The former
  `nvme-install-smoke` blockers are **fixed** on
  `feat/phase-106-usb-storage-multisector` (see below) and the gate is
  **GREEN end-to-end** (2026-07-03: USB boot → ~40 s 1 GiB sparse copy →
  reboot → NVMe-alone boot to a live shell over `nvme.block`), wired into
  pre-push behind `M3OS_NVME_INSTALL_REGRESSION=1`. C.4/C.5 pending.
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

- **PR #296 — Track C foundation (C.1+C.2+C.3).** Details below.

### Merged via PR #296 — Track C foundation

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

## The former blocker — `nvme-install-smoke` (Track C / E.2) — FIXED

The gate `cmd_nvme_install_smoke` (`SMOKE_EXIT_NVME_INSTALL_SMOKE_FAILED=103`)
is a **two-boot** oracle: boot 1 attaches both the combined USB image and a
**blank** NVMe, runs `/sbin/installer` (USB→NVMe copy), reboots; boot 2
attaches **only** the NVMe and asserts a serial login. It is wired into
pre-push behind `M3OS_NVME_INSTALL_REGRESSION=1` (900 s timeout).

The original blocker report ("256-sector raw reads fail / cap below the
block-IPC max") was **misdiagnosed**. A manual boot-1 replication with full
serial capture showed the real chain:

1. **Throughput, not size.** The inline BOT path chunks at
   `MAX_BOT_SECTORS=7` (the `USB_MSG_MAX=4096` inline-reply budget), so one
   256-sector request = 37 SCSI commands = 111 IPC round-trips — a ~1 GiB
   image copy can never fit a gate window under TCG. **Fix:** a persistent
   64 KiB shm bounce buffer (`USB_STORAGE:shm-bounce-ok sectors=128`
   sentinel) + `MAX_SHM_SECTORS=128`-sector SCSI commands whose data stage
   is one zero-copy `SubmitShmTransfer` (2 commands / 6 round-trips per
   256-sector request). 64 KiB per stage because the xHCI server programs
   the stage as a **single Normal TRB** (17-bit length field, max
   128 KiB − 1) — no TRB chaining needed at 64 KiB. Setup failure falls
   back to the inline path; ≤7-sector tails stay inline.
2. **Concurrent-instance BOT collision** (the real source of the original
   `read-failed` reports, observed killing the copy at ~12%): on a USB-root
   boot, init's bootstrap fork serves `usb0.block`, while the service
   manager's `usb_storage` daemon (`restart=on-failure`, `max_restart=5`)
   keeps probing the SAME device (GET_MAX_LUN / TEST UNIT READY / INQUIRY
   are raw BOT commands on the same bulk pipes), failing to register the
   taken name, exiting 1, and being restarted into the collision again.
   **Fix (single-daemon guard):** before ANY device traffic, a fresh
   instance checks the service registry (`usb{k}.block` lookup — the kernel
   drops a dead owner's entries, so a hit means a live daemon) and exits
   **0** when every discovered device is already served; a lost
   registration race also exits 0. Multi-stick topologies still work: only
   claimed devices are skipped.
3. **Serial log flood:** `device_host.dma_map_shm` logged at INFO per
   transfer (~33 k lines per copy) — demoted to DEBUG, same as the Phase
   106 `dma_alloc` demotion.
4. **xHCI completion-wait budget** (the residual mid-copy flake): the
   bulk-event wait (`wait_for_bulk_out_event`) gave up after ~400 ms of
   sleep-polls; a 64 KiB DMA under TCG scheduling jitter occasionally
   exceeded that, and the abandoned TD's late completion desynced the
   shared event ring (cascading CBW/INQUIRY failures at a random LBA).
   Raised to 5000 sleep-polls (≥5 s) — only a genuinely dead transfer
   fails, and failing then IS correct. Deliberately no retry-at-SCSI
   layer: retrying after an abandoned transfer risks stale-event
   off-by-one attribution, the worse failure.
5. **Detach false-positive:** the C.4 reconcile treated ONE failed
   `NextAttach` as a hot-unplug, so a transient glitch made the daemon
   serving the root unmount and exit. `device_detached_confirmed` now
   requires two verdicts 300 ms apart.

---

## Validation status (2026-07-04) + two repo-level discoveries

The full battery ran manually against `feat/phase-106-usb-storage-multisector`
(PR #297; matrix posted as a PR comment): 12 suite passes, 5 suite failures
that all pass in isolation (the hook's documented flake pattern), and **one
persistent failure that reproduces identically on `main`**:

- **`usb-storage-dual-smoke` is broken on `main`** (pre-existing): times out
  waiting for `mass-storage devices — multi-device mode`. Suspects: the wait
  pattern has an **em-dash in a single-shot startup line** (see the serial
  gotchas below — multi-byte sentinels split under lossy decode, and the
  Phase 100 `RENDER_FP` per-frame compositor spam interleaves with early
  boot), or the second stick misses the ~600 ms discovery stability window.
  Needs its own investigation; consider an ASCII sentinel emitted more than
  once.
- **This clone's pushes were not running the QEMU battery.** `core.hooksPath`
  pointed at a stale March-era `.git/hooks/pre-push` that only ran
  `cargo xtask check` — every push since then skipped smoke-test / kernel
  tests / regression / all env-gated arms. Fixed by re-running `./setup.sh`
  (now `.githooks`). Assume any "hook-verified" claim between March and
  2026-07-04 from this machine only covered `check`.

---

## Remaining Phase 106 work

- **C.5 — on-device `mkfs.ext2`** ✅ **pure-logic core landed** (2026-07-04,
  branch `feat/phase-106-c5-mkfs-ext2`). New `kernel-core/src/fs/ext2_format.rs`:
  - `format_ext2(io, params)` lays down a complete rev-1 filesystem — primary +
    per-group backup superblocks (with the primary-at-offset-1024 vs
    backup-at-offset-0 asymmetry for >1 KiB blocks), BGD table, block/inode
    bitmaps (metadata + tail bits marked), inode tables, root + `lost+found`.
    FILETYPE-only feature set, 128-byte inodes, no `sparse_super`/journal.
  - `Ext2Fs` — a mounted-for-write handle: bitmap block/inode allocation +
    `create_file` (direct/indirect/double-indirect), `create_dir`,
    `create_symlink` (inline + block), `flush`.
  - `BlockIo` write seam (dual of the read path's `BlockReader`); the installer's
    `0x117x` raw syscalls back it directly.
  - `Ext2Superblock::write_full_into` added to `ext2.rs` (the existing
    `write_into` is a partial writeback helper; format needs the full struct).
  - **11 host tests** round-trip written content back through the **existing
    `ext2.rs` reader** (small/indirect/double-indirect files, dir tree +
    symlink, 4 KiB blocks, dir-block spill), **plus a real `e2fsck -fn`
    external-validator test** (skips-with-reason if absent; ran+passed here).
  - **Remaining C.5 (installer populate):** deferred to C.4 — see below.
- **C.4 — on-device GPT writer + ESP/FAT creator** (partition-aware follow-on to
  the raw copy). Lets the installer lay a fresh GPT + ESP sized to the target
  disk instead of a byte-for-byte image clone. **C.5's installer-populate arm
  lands here:** format the C.4-created Linux partition with `format_ext2`, then
  copy the source rootfs into it via `Ext2Fs::create_*` (needs a source-fs
  reader over the raw syscalls). The pure-logic pieces are done and validated.
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
- **One xHCI shm transfer = one Normal TRB.** `submit_bulk_iova` programs the
  whole `SubmitShmTransfer` stage as a single TRB; the TRB length field is 17
  bits (max 128 KiB − 1), so a 128 KiB (256-sector) stage cannot be one TRB.
  Keep shm data stages ≤ 64 KiB (`MAX_SHM_SECTORS=128`) or implement chained
  TRBs first.
- **Never let two processes drive BOT on the same device.** A BOT command is
  2–3 bulk transfers; the xHCI server serializes *transfers*, not commands, so
  a second process's innocent-looking probe (GET_MAX_LUN → TUR → INQUIRY)
  interleaves mid-command and corrupts both streams. The usb-storage
  single-daemon guard (registry lookup before any device traffic, clean exit 0)
  is what keeps the service-manager restarts out of the root-serving
  instance's pipes — preserve it when touching the daemon's startup.
- **Gate-invisible progress:** the smoke gates don't echo guest serial; when a
  copy "hangs", `du -h` on the target image (allocated blocks) vs
  `--apparent-size` distinguishes "writes flowing", "reading a zero stretch
  (sparse skip)", and "dead".

---

## Next actions (suggested order)

1. ~~Land PR #296~~ ✅ merged (`13d1cf6e`).
2. ~~Harden `usb-storage` multi-sector transfers + green `nvme-install-smoke`~~
   — shm bounce path + single-daemon guard on
   `feat/phase-106-usb-storage-multisector`; gate wired behind
   `M3OS_NVME_INSTALL_REGRESSION=1`.
3. **C.5 on-device `mkfs.ext2`** (host-tested pure logic) → **C.4** GPT/ESP writer.
4. **Track D** first-user setup (reuse `adduser`/`passwd`; disable autologin).
5. **Track E** bare-metal M1/M3 on the Dell (operator-owned).
