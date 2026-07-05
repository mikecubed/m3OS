# Handoff — Phase 106: USB Installer & NVMe Install

**Date:** 2026-07-05 (living doc — update on every session working this phase)
**Branch:** `fix/phase-106-bug2-followups` (off `main`) — the active feature
branch (Bug 2 follow-up cleanups). All Phase 106 tracks A–D merged (PRs
#294–#306).
**State:** IN PROGRESS.
- **Track A (M1)** ✅ merged — PR #294 (`40a9e685`). Combined GPT(ESP+ext2)
  USB image + USB-ext2 root bootstrap. `usb-root-smoke` green.
- **Track B (M2)** ✅ merged — PR #295 (`9510a0a1`). NVMe root boot +
  `nvme-rw` / `nvme-persist` gates green.
- **Track C (M3)** 🟡 in progress:
  - **C.1–C.3** ✅ merged — PR #296 (`13d1cf6e`): installer scaffold +
    capability-gated raw block syscalls + raw `dd`-copy installer + kernel
    root-slot-release fix.
  - **`nvme-install-smoke`** ✅ **GREEN end-to-end** — PR #297 (merged): USB
    boot → ~40 s 1 GiB sparse copy → reboot → NVMe-alone boot to a live shell
    over `nvme.block`; in pre-push behind `M3OS_NVME_INSTALL_REGRESSION=1`.
  - **C.5 on-device `mkfs.ext2`** ✅ **merged** — PR #299 (`09921df3`).
    `kernel-core/src/fs/ext2_format.rs` (`format_ext2` + `Ext2Fs` writer +
    `BlockIo` seam). See below.
  - **C.4 + C.5 installer-populate arm** ✅ **landed on PR #302**
    (`feat/phase-106-c4-partition-installer`): pure-logic GPT builder/parser
    (`kernel-core/src/fs/gpt.rs`, sgdisk-validated), populate walker +
    write-back block cache (`kernel-core/src/fs/ext2_populate.rs`,
    e2fsck-validated), `installer --part` (fresh target-sized GPT + ESP copy +
    on-device `format_ext2` + file-level populate), and the
    `nvme-install-part-smoke` gate (same `M3OS_NVME_INSTALL_REGRESSION=1`
    env var; host-side gpt-crate + e2fsck cross-checks between the boots).
- **Track D** ✅ landed (`feat/phase-106-track-d-first-user`): `installer
  --part` first-user setup — console prompts (root password / username /
  user password, echo off) before any target write; the populate filters the
  image's `/etc/passwd`/`/etc/shadow`/`/etc/group` + `/home/user` off the
  target (`populate_from_reader_filtered`) and fresh ones are written via the
  existing `passwd`-lib `$sha256i$` chain (`getrandom` salt; no new crypto).
  `/home/<user>` seeded (0700, uid/gid 1000, `.profile` carried over).
  `--no-user` opts out; raw mode is a byte-clone by definition.
  `nvme-install-part-smoke` drives the prompts and boot 2 logs in **as the
  created user** + asserts the `/etc/passwd` entry (the E.2 deferred arm).
- **Track E** — bare-metal sign-off not started (operator-owned).
- **CI reliability** ✅ — a four-cause chain that made every PR's checks red/flaky
  was root-caused and fixed this session (PRs #298/#300/#301, all merged). The
  regression suite is now **deterministically** reliable, not just timeout-padded.
  Details under "CI reliability" below — read it before touching the regression
  harness or the compositor's `RENDER_FP` emission.

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

## CI reliability — four root causes fixed (2026-07-04)

The `Build` (on `main`) and `PR Check` GitHub Actions had been red/flaky "for a
while." This was **four distinct bugs**, all now fixed and merged. The last one
is the important durable lesson — read it before touching the regression harness
or the compositor.

1. **Font before compile** (PR #298). The kernel `include_bytes!`s the
   **gitignored** Nerd Font `xtask/assets/fonts/term.ttf` (Phase 100), but
   `build.yml`/`pr.yml`/`release.yml` ran `cargo xtask check` (compiles the
   kernel) **before** their `fetch-fonts` step → fresh checkout can't compile.
   Fix: `cmd_check` calls `ensure_font_asset()` first (idempotent, silent when
   the font's already present).
2. **Per-step timeouts too tight** (PR #298). Added
   `M3OS_CI_TIMING_MULT: "3"` to the `pr.yml`/`build.yml` QEMU jobs (the
   `scaled_secs` knob nightly-stress already used) to absorb shared-runner
   jitter.
3. **Unscaled global timeout** (PR #300). The multiplier scaled per-**step**
   waits but not the per-**test** global (`cmd_regression`: `timeout =
   args.timeout_secs.unwrap_or(test.timeout_secs)`), and every step deadline is
   `step_deadline.min(global_deadline)` — so the global silently capped the
   per-step scaling for **late-step** tests (`security-floor` step 19,
   `su user`). Fix: scale the global through the same `scaled_secs` (180 s →
   540 s at 3×).
4. **`RENDER_FP` serial flood defeats the prompt matcher** (PR #301) — *the
   actual reliability killer, not runner speed.* The compositor emits a
   `RENDER_FP frame=… rows_changed=0 hash=…` fingerprint line **every compose
   frame, continuously**, even on a static screen. With the `ure: HB` and
   `USB_HID:idle` heartbeats it floods the shared serial console **after** the
   shell prompt. The smoke-step `# `/`$ ` detector then fails **both** ways: the
   strict matcher (`prompt_suffix_end`) needs the buffer to *end with* the
   prompt (flood lands after it), and the idle fallback
   (`idle_prompt_fallback_matches`) needs 750 ms of quiet (flood never idles).
   So the prompt is *present but never recognized* and the wait times out no
   matter how long — **flakily**, because whether a lucky idle gap appears
   depends on runner load (which is why the timeout work in #298/#300 only
   half-helped). The tell: the serial tail at every failure is 100 %
   `RENDER_FP`/`ure`/`dhcpv6` with no visible prompt. Fix: `strip_background_noise`
   now strips `BACKGROUND_HEARTBEAT_PREFIXES` (`RENDER_FP `, `ure: HB `,
   `USB_HID:idle`) so the cleaned buffer ends with the prompt and the strict
   matcher fires **deterministically, independent of runner load**. Test-proven:
   `prompt_recognized_through_compositor_render_fp_flood` reproduces a prompt
   under 300 frames of flood — fails before, passes after. #298/#300 are kept as
   orthogonal margin for genuinely-slow ops.

**Also discovered this session:** this clone's `core.hooksPath` had drifted to a
stale March-era `.git/hooks/pre-push` that only ran `cargo xtask check` — every
push from this machine skipped smoke/test/regression for months. Fixed by
re-running `./setup.sh`. Assume any "hook-verified" claim from this machine
between March and 2026-07-04 only covered `check`.

**FIXED (2026-07-04): usb-storage BOT/xHCI error recovery — one abandoned
transfer no longer poisons the pipe for good.** The failure it addressed
(hit twice on 2026-07-04, both during install-gate image copies; 0
occurrences across green runs — all-or-nothing): `usb-storage: BLK_READ
shm transport-fail lba=<n>` on some innocent transfer, then `[xhci]
bulk-OUT non-success cc=6` (STALL) and **every** subsequent ≥8-sector
(shm-path) transfer on the device failing, including the kernel's own
rootfs reads. Chain: a transfer exceeds the 5 s bulk-event wait (host
page-cache stalls under battery-load TCG — QEMU reads the USB image
synchronously while the ~1 GiB NVMe target floods write-back) → the
daemon abandons it mid-BOT exchange → the device is left phase-desynced
→ it STALLs the next CBW → the STALL **halts the xHCI endpoint**, and
nothing cleared it; the abandoned TD's late completion event additionally
got mis-attributed to the NEXT transfer (the wait matched events by
(slot, dci) only). Three-layer fix:

1. **Event attribution** (`xhci/src/controller.rs`): the bulk completion
   waits now match by **TRB pointer** — a stale event from an abandoned
   TD is consumed + discarded with a `[xhci] stale transfer event
   discarded` line instead of being credited to the current transfer.
   (Also: bulk-IN transport failures now print their cc — they used to
   be silent, which is why the first failure in the logs had no xhci
   line.)
2. **xHCI endpoint recovery** (`Controller::recover_endpoint`, exposed
   as `UsbRequest::RecoverEndpoint`): Stop Endpoint → Reset Endpoint →
   Set TR Dequeue Pointer to the producer enqueue (xHCI §4.6.8–4.6.10;
   Context State Error tolerated per step) + a stale-event sweep. New
   host-tested command-TRB builders in `kernel-core usb/xhci/trb.rs`.
3. **BOT reset recovery + retry-once** (`usb-storage bot_recover`): on a
   TRANSPORT failure (never a CSW status failure — that's the device
   answering) the daemon runs `RecoverEndpoint` on both pipes, then the
   class-standard Bulk-Only Mass Storage Reset + `CLEAR_FEATURE
   (ENDPOINT_HALT)` on both endpoints (BOT §5.3.4 — via the existing
   `ControlRequest` plumbing), and retries the failed SCSI command
   exactly once. Protects even retry-less callers (the kernel's own
   rootfs reads); the installer's 3-attempt raw retries remain the outer
   belt.

The trigger (a >5 s host stall) is environmental and can recur; the
sentinel chain to look for on the next occurrence is `transport-fail` →
`usb-storage: BOT reset recovery ok — retrying command` → the copy
proceeding. Ruled out while diagnosing: the restart-looping
service-manager `xhci` instance (config-space scans only, never claims —
5-7 restarts per boot, benign), the second `usb_storage` instance (its
`shm_create`/`shm_map` are process-local; the single-daemon guard exits
it before any BOT traffic), and the Track D changes themselves (the
failing raw-arm data path is byte-identical to C.3's).

**"Bug 2" — FULLY RESOLVED and MERGED to `main` (strand ~1/4 → 0; three
distinct IPC races fixed) (2026-07-05).** PR #305 (squash-merged as `25261a1a`;
branch `investigate/phase-106-bug2-lost-wakeup` deleted). Follow-up cleanups
✅ **done on `fix/phase-106-bug2-followups`** (2026-07-05): the
`run_qemu_gate_retry_once` pre-push guard (all four uses) and the
`KNOWN-FLAKE: Phase 106 Bug 2` boot-serial banner are removed; the task name
is now set on `exec` (`intern_task_name` + `set_current_task_name`, so
diagnostics never show `fork-child` for exec'd drivers again); the copy-fault
re-pend (race C) is mirrored into `ipc_recv_msg_timeout` / `ipc_try_recv_msg`
/ `ipc_recv_with_caps`, with matching drain-first guards added to `recv_msg`
/ `recv_msg_nowait` / `recv_msg_with_deadline` (without which a re-pended
message would be overwritten by the next queued-sender delivery — the race
(B) mechanism). The bug is **not** in USB/xhci/storage and **not** an
`ep.senders` lost-wake (an
interim `ep.senders` BSP backstop was written, disproven by ground-truth, and
**reverted** — see "false trails"). Three distinct core-IPC races in the same
install strand were found and fixed:

- **(A) stale-wake in `deliver_message` + plain `wake_task_v2`.** The pair are
  two separate `scheduler_lock` sections, so under `-smp 4` the target can run on
  another core, consume the delivered message, and re-block on its *next* IPC
  between them; the delayed state-only `wake_task_v2` then CASes that unrelated
  re-block `Blocked* → Ready` without setting its `woken` flag, and
  `block_current_until` returns a bogus `DeadlineExpired` (dropping the request/
  reply). Two instances — the **reply path** (`reply()`, loud:
  `reply_v2:deadline_expired_no_deadline`) and the **request-delivery** `call`/
  `send` hand-offs (quiet). Fixed by gating all six real-message deliver+wake
  sites with `wake_task_v2_if(x, |t| t.pending_msg.is_some())`.
- **(B) stale `ep.receivers` entry → double-delivery overwrite (the ~1/19
  residual).** `recv_msg_with_notif`'s message-return-after-block path returned
  `RECV_KIND_MESSAGE` **without** the `ep.receivers.retain(remove)` that every
  other return path performs. When woken by a message via
  `block_current_on_notif_v2`'s `has_pending_message` self-revert (not a
  `call_msg` pop), the server stayed enqueued in `ep.receivers`; a later
  `call_msg` popped it again and `deliver_message` **overwrote** the unconsumed
  `pending_msg`, orphaning the first request's reply cap → xhci idle in recv
  holding an unanswered `Reply` cap. Pinned with a `#[track_caller]`
  `deliver_message`-overwrite probe (caller = `endpoint.rs:632` Some-arm, then
  `:1112` call_msg after a partial fix). Fixed by (1) draining `pending_msg`
  first at the top of `recv_msg_with_notif`, and (2) adding the missing
  `retain(remove)` to the message-return-after-block path.
- **(C) copy-to-user failure orphaning the cap (defensive).** `ipc_recv_msg`
  dequeues the message *before* the userspace copy, so a rare header/bulk
  copy-to-user failure (OOM / demand-fault) dropped the message with its reply
  cap still held. Hardened to **re-pend** the message (and bulk) before the
  `u64::MAX` return so the driver re-loops cleanly. (Unconfirmed in the wild —
  belt-and-suspenders alongside (B).)

*Ground truth (the two decisive diagnostics — keep both).*
1. `[replystall]` (`kernel/src/task/scheduler.rs reply_stall_scan`) reports,
   per stranded caller, the **reply-cap holder** pid + **name** + state +
   parked syscall nr + `holder_pending` + **`holder_reply_caps`** (how many
   `Reply(*)` caps the holder owns). Classes: `STRANDED-NO-HOLDER`,
   `STRANDED-DEAD-HOLDER`, `STRANDED-SERVER-STUCK`. It writes
   `nvme-install-boot1-serial.log` and prints `KNOWN-FLAKE: Phase 106 Bug 2`.
2. The scheduler's own `[sched] wake_task_v2 on BlockedOnReply … has_pending_msg=false`
   and `[sched] spurious block wake … site=reply_v2:deadline_expired_no_deadline`
   warnings — pre-existing instrumentation that fires exactly on this bug.

   NB: the captured `holder_name=fork-child` is a **stale task name** — the
   drivers are exec'd from a forked child whose name isn't updated on `exec`;
   `pid=4 = /drivers/xhci`, `pid=5 = /drivers/usb-storage` (confirmed by the
   `elf: mapped pid=N binary=…` lines). Do not read "fork-child" as a distinct
   process class. (Cosmetic fix worth doing: set the task name on `exec`.)

*What actually happens (chain + mechanism).* Chain:
`installer child (SYS_BLK_RAW_READ 0x1171) → usb-storage (pid5, ipc_call
0x110d, BlockedOnReply) → xhci (pid4)`. In the captured deadlock, **xhci is
idle in its notif-bound recv (`0x110e`, BlockedOnNotif) still holding exactly
one unconsumed `Reply(usb-storage)` cap** (`holder_reply_caps=1`,
`holder_pending=false`), and usb-storage is `BlockedOnReply` on it forever.
Immediately upstream in the serial, for the SAME usb-storage task:
`wake_task_v2 on BlockedOnReply … caller=endpoint.rs:1400 has_pending_msg=false`
(that call site is the `wake_task_v2(caller)` **inside `reply()`**, one line
after `deliver_message(caller, reply_msg)` at endpoint.rs:1398 — so the reply's
`deliver_message` left **no** pending message), immediately followed by
`spurious block wake … reply_v2:deadline_expired_no_deadline outcome=DeadlineExpired
woken_flag=false pending_at_clear=false` (the client's reply-block returned
`DeadlineExpired` although reply_v2 passes **no deadline**, with the `woken`
flag false). Net: a reply is issued but its message never lands and the client's
block returns a spurious timeout — the client abandons/re-blocks, the server's
reply obligation is dropped (cap still held), deadlock. The Bug 1
transport-fail/BOT-recovery path is the *trigger* (it drives the high-rate
`call`/`reply` churn that exposes the race), not the cause.

*The fix (applied — `kernel/src/ipc/endpoint.rs`, `reply()`).* This is the
**reply-path analog** of a race already fixed elsewhere. On 2026-05-14 the
`reply_v2:deadline_expired_no_deadline` signature was root-caused and fixed for
the *deadline-scanner* wake site (`drive_expired_wake_deadlines`) on branch
`fix/wake-task-v2-precondition-race` (handoff
`docs/handoffs/2026-05-13-reply-v2-deadline-residual-race.md`) by converting its
`wake_task_v2` to `wake_task_v2_if(id, |t| t.wake_deadline == Some(expected))` —
a precondition re-checked atomically under the CAS's own locks. **`reply()`'s
wake at `endpoint.rs` never got that treatment** and remained a plain
`wake_task_v2(caller)`. The mechanism: `reply()` does `deliver_message(caller)`
then `wake_task_v2(caller)` as two separate `scheduler_lock` sections (the
`preempt_disable` bracket only gates the *local* core, not other cores). Under
`-smp`, between them the caller can run on another core, self-revert on the reply
payload we just delivered, consume it, and **re-block on its NEXT request** with
a fresh waker and `pending_msg == None`. The delayed state-only `wake_task_v2`
then CASes that unrelated re-block `BlockedOnReply → Ready` **without** setting
its `woken` flag, so `block_current_until` (`scheduler.rs:3843-3848`) resumes it
with `woken == false` and returns `DeadlineExpired` although no deadline was set
— the caller abandons the wait (`call_msg` surfaces `u64::MAX`), the server's
reply obligation is dropped, and the chain deadlocks. (`deliver_message` is
*not* at fault — it `find`s by unique `TaskId` and always writes; a kernel
backstop can't help because the payload is already consumed.) **Fix:** gate the
re-enqueue on the payload still being pending —
`wake_task_v2_if(caller, |t| t.pending_msg.is_some())`. True exactly when THIS
reply is the one the caller is still parked on; false for the racy re-block, so
the stale CAS (and both warnings) never happen. No legit wake is lost: a caller
that already consumed the reply is `Running`/`Ready` → `AlreadyAwake`.

*Then the symmetric strand.* Guarding only `reply()` closed the reply-path race
(the `reply_v2` warnings **and** the usb-storage transport-fail/BOT-recovery
churn — themselves a manifestation of it — vanished) but the load-stress loop
still stranded ~1/7, now **warning-free**, at the same terminal state: xhci idle
in recv holding an unconsumed `Reply(usb-storage)` cap it never processed. That
is the **request-delivery** analog: the same stale-wake on the `deliver_message
+ wake_task_v2` pair that hands a *request* to a server blocked in recv, so the
wake lands on the server's next recv-block (no `reply_v2` warning because the
victim is `BlockedOnNotif`, not `BlockedOnReply`). Fix: the **same precondition
guard** applied to all six real-message `deliver_message + wake_task_v2` sites
(the "Phase 57e Bug #12 minimum atomic" pairs) in `endpoint.rs` — `reply()` plus
the five send / `call`-hand-off deliveries. The `complete_send`+wake sender-wake
(different precondition: send-completion, not `pending_msg`) and the
`deliver_message_and_wake` error-sentinel paths are deliberately **not** touched.

*False trails (recorded so they aren't re-walked).*
- *"xhci idle in recv holding an unanswered reply = unreachable paradox"* —
  wrong framing; it IS reachable, via the lost reply-delivery above.
- *`ep.senders` lost-wake + BSP `stranded_sender_owners` backstop* — written,
  then disproven: the holder holds a `Reply` cap, so the request was
  **received**, never queued on `senders`. **Reverted.** (A first cut also
  regressed by waking `BlockedOnReply` owners — the very `reply_v2` spurious
  wake above — a useful confirmation that poking this path is dangerous.)
- *nvme `wait_completion` unbounded `irq.wait()`* — real hardening (a
  completion wait must never depend on the IRQ), **kept** in
  `userspace/drivers/nvme/src/io.rs`, but not this strand.

*Repro + validation.* Intermittent under CPU+IO load: 4 CPU spinners + a `dd`
loop, then `M3OS_SMOKE_SERIAL_DUMP=1 cargo xtask nvme-install-part-smoke
--timeout 300` in a loop, grepping the boot-1 serial for `STRANDED-`. Strand
rate as each race was closed: **pre-fix ~1/4** → **`reply()`-guard only ~1/7**
→ **all-6 deliver+wake guards ~1/19** (race A closed: the
`reply_v2:deadline_expired_no_deadline` / `wake_task_v2 on BlockedOnReply …
has_pending_msg=false` warnings went to 0) → **+ the recv fix (B) 0/25**, with a
`#[track_caller]` `deliver_message`-overwrite probe silent across all 25 runs
(direct confirmation the double-delivery is gone) → **+ copy-fault hardening (C),
probes removed 0/20**. Notably, once (B) landed the usb-storage
transport-fail/BOT-recovery churn **disappeared entirely** across all runs — the
IPC race had been corrupting the transfers themselves, so fixing it removed the
whole cascade, not just the terminal strand. `cargo xtask check` green; pre-push
smoke-test (`ipc-wake`) + regression pass. ~~The `run_qemu_gate_retry_once`
guard stays as belt-and-suspenders.~~ (Removed 2026-07-05 on the follow-ups
branch — with the strand at 0 the guard only masked genuine regressions.)

*Diagnostics retained; probes removed.* The durable `[replystall]` diagnostic
(now with `holder_name` + `holder_reply_caps`) stays. The three **temporary**
probes used to pin race (B) — the `#[track_caller]` `deliver_message`-overwrite
warning, and the recv `Some`-arm orphan warning — were **removed** after the fix
validated. ~~`holder_name=fork-child` is a stale post-`exec` task name … set
the task name on `exec`~~ ✅ done (follow-ups branch). ~~The same copy-fault
gap (C) exists in the sibling `ipc_recv_msg_timeout` / `ipc_try_recv_msg`
recv variants~~ ✅ done (follow-ups branch, incl. `ipc_recv_with_caps` and the
prerequisite drain-first guards in the three endpoint recv paths).

**`usb-storage-dual-smoke` — ROOT-CAUSED and FIXED (2026-07-05,
`fix/phase-106-bug2-followups` session).** The failing wait was the first one
(`mass-storage devices — multi-device mode`), and the mechanism is the
handoff's original em-dash suspect — but with the real trigger pinned:
`spawn_serial_reader` reads QEMU stdout in chunks and `append_serial_chunk`
decodes **each chunk independently** with `from_utf8_lossy`. QEMU dribbles
serial output in real time, so the reader's `read()`s return **tiny (3-6
byte) chunks** (measured with a temporary `M3OS_CHUNK_LOG` boundary logger —
in one captured run the em-dash at bytes 46202..46204 sat wholly inside
chunk `[46201,46206)` and the gate PASSED; a boundary one byte later would
have split it). With ~4-byte chunks a 3-byte UTF-8 char straddles a boundary
roughly every other run, the halves decode to U+FFFD, and the pattern never
matches even though the raw serial provably contains the banner — a
per-run coin flip that read as "fails identically" (3 consecutive failures
that day). The guest side was healthy all along: banner + both
`usb0.block`/`usb1.block` registrations appear in every raw capture, and
once the matcher survives the banner the whole gate passes (login, both
mounts, distinct per-stick content). **Fix:** the reader thread now holds
back an incomplete UTF-8 suffix (≤3 bytes, `utf8_incomplete_suffix_len`)
until its continuation bytes arrive, so no sent chunk ever splits a
multi-byte char — this hardens EVERY serial-pattern gate, not just this one
(the "use ASCII-only sentinels" gotcha is now defense-in-depth rather than a
correctness requirement). Host tests: `utf8_incomplete_suffix_len_cases` +
`serial_pattern_with_em_dash_survives_chunk_split` (brute-forces the banner
split at every byte). Also fixed: `usb-hid`'s `enumerate_once` printed
`bound HID device (proto 80)` for mass-storage sticks/hubs it actually
`Ignore`s (the poll loop never touched them — but the line sent this
investigation down a false trail; the print is now role-gated).
Investigation by-catch worth keeping: probes replicating the gate's exact
QEMU argv proved serial login input works on this topology at every timing
(immediate, +25 s idle-first-write, 204 s of 5 s echo-canaries, fresh
first-boot disk), and QEMU auto-inserts a USB hub in the 5-device topology
(one stick enumerates tier-2 behind it via `usbhub`) — both fine.

---

## Remaining Phase 106 work

- **C.5 — on-device `mkfs.ext2`** ✅ **merged — PR #299** (`09921df3`).
  New `kernel-core/src/fs/ext2_format.rs`:
  - `format_ext2(io, params)` lays down a complete rev-1 filesystem — primary +
    per-group backup superblocks (with the primary-at-offset-1024 vs
    backup-at-offset-0 asymmetry for >1 KiB blocks), BGD table, block/inode
    bitmaps (metadata + tail bits marked), inode tables, root + `lost+found`.
    FILETYPE-only feature set, 128-byte inodes, no `sparse_super`/journal.
    `derive` uses `checked_mul` for `inodes_count` (no u32 overflow).
  - `Ext2Fs` — a mounted-for-write handle: bitmap block/inode allocation
    (byte-wise `bitmap_find_clear`, not O(n²)) + `create_file`
    (direct/indirect/double-indirect), `create_dir`, `create_symlink`
    (inline + block), `flush`. **No rollback** — a partial `create_*` failure
    leaves a logical inconsistency; callers (installer populate) must abort +
    reformat (documented on the type). `open()` **fails closed** on an
    incompatible/corrupt volume (validates `inode_size == 128`, geometry, and
    that on-disk BGD pointers match the derived `geo.*_block(g)` offsets; the
    dir-entry scan guards `used > rec_len` against underflow).
  - `BlockIo` write seam (dual of the read path's `BlockReader`); the installer's
    `0x117x` raw syscalls back it directly.
  - `Ext2Superblock::write_full_into` added to `ext2.rs` (the existing
    `write_into` is a partial writeback helper; format needs the full struct).
  - **15 host tests** round-trip written content back through the **existing
    `ext2.rs` reader** (small/indirect/double-indirect files, dir tree +
    symlink, 4 KiB blocks, dir-block spill), a byte-scan cross-check, three
    fail-closed negative tests (bad inode_size / bad BGD / corrupt dir entry),
    **plus a real `e2fsck -fn` external-validator test** (skips-with-reason if
    absent; ran+passed on the dev host). Two independent opus reviews (one
    brute-forced the byte-scan equivalence over all inputs).
  - **Remaining C.5 (installer populate):** ✅ landed with C.4 — below.
- **C.4 — partition-aware installer** ✅ **landed — PR #302**
  (`feat/phase-106-c4-partition-installer`, 2026-07-04). `installer --part`
  lays a fresh GPT + ESP sized to the target disk instead of a byte-for-byte
  image clone:
  - `kernel-core/src/fs/gpt.rs` — pure-logic GPT builder (`build_gpt` +
    `GptPlan::for_target`: source ESP span kept, Linux partition grown to the
    target's last usable LBA; CRC32 IEEE; `sector_writes()` yields the exact
    write list) + `parse_gpt`, a **CRC-verified** parser (stricter than the
    kernel probe — the installer fails closed on a corrupt source). 9 host
    tests incl. a kernel `gpt_ext2_scan` replica and an `sgdisk --verify`
    external validator (ran+passed on the dev host).
  - `kernel-core/src/fs/ext2_populate.rs` — `populate_from_reader` walks the
    source rootfs via the existing `BlockReader` read path and re-creates
    every dir/file/symlink through the C.5 `Ext2Fs` writer (cross-block-size;
    mode/uid/gid/timestamps preserved; `lost+found` skipped; visited-set
    terminates a corrupt source's dir cycle). `WriteBackBlockIo` — LRU
    write-back cache for metadata read-modify-writes + contiguous-run
    coalescer emitting single ≤256-sector `write_block_run` raw requests
    (new `BlockIo` default method) — >2× fewer device write requests,
    byte-identical image (both test-asserted). 5 host tests incl. `e2fsck
    -fn` on a cache-populated target.
  - `userspace/installer` `--part` mode: CRC-verified source-GPT parse →
    source ext2 mount (read-only, before any target write) → target resolve
    + `target-is-source` guard → capacity probe (LBA bisection over raw
    reads — no capacity syscall needed) → fresh GPT → sparse same-span ESP
    copy (the FAT's geometry is partition-relative; `hidden sectors` = the
    unchanged start LBA, so a raw copy stays valid — no on-device FAT
    formatter needed) → `format_ext2` (source's block size — the geometry
    the kernel root mount is proven on) → populate → `Ext2Fs::flush` +
    cache flush + `BLK_FLUSH` → reboot. New sentinels:
    `INSTALLER:mode/layout/target/gpt-written/esp-copied/format/populate`,
    `INSTALLER:error part-*`.
  - **Gate `nvme-install-part-smoke`** (same `M3OS_NVME_INSTALL_REGRESSION=1`;
    second pre-push invocation): boot 1 runs `--part`; between the boots the
    gate host-verifies the written disk — grown-root-span math vs the source
    image's own GPT, an **independent `gpt`-crate parse** of the installed
    GPT, `skipped=0` in the populate sentinel, and `e2fsck -fn` of the
    extracted rootfs partition; boot 2 (NVMe alone → live login) unchanged.
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

- **A device-status error reply used to wedge the whole `dev_id` (fixed with
  C.4).** `blk::remote`'s outer read/write wrappers treated ANY inner error —
  including a decoded `status != Ok` reply from a perfectly live driver — as
  an IPC/driver failure: `on_ipc_error*` latched `is_restarting()`, the
  restart wait timed out (a healthy driver never re-registers), and **every
  later request on that `dev_id` blocked out the full 1 s restart budget and
  failed**. Never seen before C.4 because no green path ever produced a
  legitimate error reply; the `--part` installer's capacity probe (deliberate
  out-of-range reads) hit it instantly — one failing probe bricked all
  subsequent reads *and* writes (`part-gpt-write-failed lba=0`), and the
  bisection collapsed to `known_good+1` (every probe "failed"). Fix:
  `restart_suspected(err_byte)` (kernel-core `driver_ipc::block`,
  host-tested) — only transport-shaped failures (`DriverRestarting`, the
  `0xFF` no-endpoint/decode convention) may trigger the restart dance; a
  decoded status error passes through untouched. If a gate ever shows "first
  error on a device, then everything fails with ~1 s stalls", check for a
  path that bypasses this classification.

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
  - ~~Multi-byte UTF-8 (em-dash `—`) can split across a `read()` boundary under
    lossy decode and **never match**.~~ **Fixed at the decode layer**
    (2026-07-05): `spawn_serial_reader` now carries an incomplete UTF-8
    suffix to the next chunk, so split multi-byte chars decode intact.
    ASCII-only sentinels remain good practice, but are no longer a
    correctness requirement.
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
3. ~~**C.5 on-device `mkfs.ext2`**~~ ✅ merged (PR #299) → ~~**C.4** GPT/ESP
   writer + populate~~ ✅ landed (PR #302, `installer --part` +
   `nvme-install-part-smoke`).
4. ~~**Track D** first-user setup~~ ✅ landed — `installer --part` prompts +
   filtered populate + fresh credentials via the passwd-lib chain;
   `nvme-install-part-smoke` logs in as the created user. (Note: there is no
   literal serial-autologin marker to strip — the serial image boots to an
   interactive `login`; D.2's substance was replacing the well-known seeded
   `root:root`/`user:user` credentials, which first-user mode never copies.)
5. ~~Bug 2 follow-up cleanups~~ ✅ on `fix/phase-106-bug2-followups` (PR #307):
   retry guard + KNOWN-FLAKE banner removed, task name set on exec,
   copy-fault re-pend mirrored into the sibling recv variants.
6. ~~**`usb-storage-dual-smoke` pre-existing failure**~~ ✅ root-caused and
   fixed (same session; see the ROOT-CAUSED section above): reader-side
   UTF-8 carry so serial chunks never split a multi-byte char, plus the
   role-gated `usb-hid` bind log.
7. **Track E** bare-metal M1/M3 on the Dell (operator-owned) — the only
   remaining Phase 106 work.
