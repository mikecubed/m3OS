# Standing checklist — next Dell (Precision 5560) session

> **Running a session?** Use the sequenced runbook:
> [2026-07-09 Dell validation session](./2026-07-09-dell-validation-session.md)
> — objectives, per-test expected serial lines, pass/fail bars, and a
> failure→cause map, ordered by value. This file stays the standing *checklist*
> the runbook works down.

A running list of everything waiting on physical access to the reference laptop, so
bench time gets batched instead of spent one item per session. **This checklist covers only
phases with *built* HW arms** (100/101/103/106/110/111); the whole-arc plan — including the
not-yet-built phases (102 touchpad, 104 Wi-Fi, 108, 109) and the recommended sequencing — is
[hw-validation-campaign.md](../appendix/hw-validation-campaign.md). **Add items here when
work lands that needs a hardware arm; check items off (with the capture path / run
record) when a session validates them.** Protocol: [bare-metal validation
strategy](../appendix/bare-metal-validation.md). Workflow facts (SSH capture idiom,
image flashing, `[userspace]` new-image gate): [2026-06-30
handoff](./2026-06-30-phase-100-bare-metal-gui-hw-validation.md) §6.

## Phase 100 — Bare-Metal GUI Session (PR #272, merged)

- [ ] **usbhub tier-2 capture** — the decision capture for the `d094d87a` fix
      (bounded RPC + retry). `echo 'dmesg | grep usbhub' | ssh root@<ip> > usbhub.log`,
      then branch per [handoff §1](./2026-06-30-phase-100-bare-metal-gui-hw-validation.md#1-resume-here--open-issue-tier-2-behind-hub-keyboardmouse-not-yet-enumerated).
      Expected good path: `bound hub → has N downstream ports → port X device
      connected → reset+enabled → child enumerated … class=3`.
- [ ] **USB keyboard types** — after tier-2 enumerates: `usbhid.log` shows
      `role=KEYBOARD` (confirms the `149b7210` classifier) and keys echo (runbook arm 4).
- [ ] **xHCI cpu-hog confirm** — verify the residual ~2 s `cpu-hog … /drivers/xhci`
      storm is gone after the completion-wait spin-phase fix (this branch); check
      `/proc/loadavg` flat at idle (runbook arm 5).
- [ ] **Runbook arms 1 and 3** — greeter photo artifact; WC blit-latency ratio
      (`scripts/phase-100-bare-metal-validate.md` §4).

## Phase 101 — ACPI Platform Foundation (fixture capture + HW arms)

- [ ] **Capture the Dell's ACPI tables** — the Track A/B/C host tests currently run
      on QEMU/synthetic fixtures; the charter wants them on the real DSDT. Boot the
      stick, then from the m3OS side dump DSDT/SSDTs via the `usb-logsink` boot.log
      path (or boot any Linux USB and `sudo acpidump > dell-5560-acpi.dat`). Land the
      dump under `kernel-core/tests/fixtures/acpi/` and re-point the
      `find_by_hid("DLL0945")` / touchpad-`_CRS` tests at it.
- [ ] **acpid on metal** — boot log should show `ACPI_SMOKE:namespace-built` +
      `sci-armed` from the real firmware (check `dmesg | grep acpid` /
      `grep ACPI_SMOKE`). Record node/skipped counts; skips reveal which AML
      constructs the Dell's tables need beyond the subset.
- [ ] **FADT boot line on metal** — `[acpi] FADT: DSDT …, SCI_INT …, PM1a_EVT …`
      with a non-zero DSDT pointer (D.1 `Validated-on-HW` arm).
- [ ] **Lid-switch SCI** — close/open the lid; expect the kernel demux →
      `acpid` GPE/fixed dispatch to log it (D.3/D.4 HW arm; charter's
      lid `Validated-on-HW` item). Power button press is the fallback arm.

## Phase 103 — Laptop Power Management (battery/backlight/HWP/S3 on metal)

Merged 2026-07-02 (PRs #286–#293). The phase's headline claim — a usable
daily-driver laptop — is **entirely bare-metal**: QEMU q35 has no battery, no
charger, no backlight panel, and no HWP MSRs, so `powerd` runs a no-device
posture in CI and every live datapath carries a `Validated-on-HW (run N, date)`
status (Track G.3). The Precision 5560 (Tiger Lake) is the target. Several arms
piggyback on the Block-1 boot and the Phase 101 ACPI capture above — batch them
in one pass.

- [ ] **Battery + AC read correctly** — on the default boot the `POWERD:ready`
      line reads `battery=<path> ac=<path> zones=<N> mech=hwp` (vs the QEMU
      `battery=none ac=assumed-online zones=0 mech=none`), and `m3ctl power
      status` prints a real `battery: <pct>%` / `ac: online` / `thermal: <temp>` /
      `governor: conservative (mech hwp, target N)`.
- [ ] **AC flips on charger unplug** (Track G.3 headline arm) — pull the charger:
      expect `Notify(ADP,0x80)` + a `_BST` re-read, AC online→offline, and the
      percentage decreasing on the next `m3ctl power status`. Replug → online again.
- [ ] **Backlight brightness** — `m3ctl backlight <pct>` / `up` / `down` drives
      `_BCM` and **visibly dims the panel** (capture a photo); `_BQC` reads it
      back. On the panel-less VM this is `backlight: no device`; restore-on-resume
      joins the S3 arm.
- [ ] **HWP P-states** — boot line `cpufreq: HWP enabled, perf range <lo>..<hi>`
      with a real Tiger Lake range (vs the QEMU no-HWP no-op); the governor target
      programs `IA32_HWP_REQUEST` (package-wide on Tiger Lake — per-core IPI
      broadcast is a known residual).
- [ ] **S3 suspend/resume on real firmware (stretch)** — `m3ctl power suspend`
      → quiesce (AP park + blk drain) → `_PTS(3)` + SLP_TYP write → wake →
      `_WAK(3)` → `POWERD:resume` → a live shell + disk + re-applied brightness;
      **or** it fails closed to a live session (an acceptable stretch outcome —
      record it). Residuals: GPE re-arm, framebuffer re-mode-set on the real GPU,
      S0ix. (QEMU S3 round trip is green incl. PS/2 re-init; the Dell arm pends.)
- [ ] **Real `_BST`/`_BIF` fixture swap** — the Track A host tests run on
      synthetic Dell-shaped packages; capture the real battery objects during the
      Phase 101 DSDT dump and re-point the decode tests at them (shares that capture).

## Phase 110 — Real-Hardware Security (KPTI Meltdown + PCID on metal)

Track A (KPTI) is merged and live on every QEMU boot, but QEMU TCG models
**no** speculation and advertises **no** PCID/INVPCID, so two arms are inherently
bare-metal-only. The Precision 5560 (Intel Tiger Lake) has both PCID and INVPCID
and is Meltdown-susceptible with KPTI off, so it is the right target.

- [x] **A.6 — Meltdown PoC: immune silicon** — `Validated-on-HW (run 2,
      2026-07-09) — Dell Precision 5560 / Tiger Lake`. **This CPU is
      `rdcl_no=true` (Meltdown-immune in hardware)**, so `mitigations=auto`
      correctly leaves **KPTI OFF** and `m3ctl mitigations status` reads
      `Meltdown: Not affected`. `/bin/meltdown-poc` (`userspace/meltdown-poc`)
      runs its `channel=CALIBRATED` control then correctly reports
      `MELTDOWN_POC:NO-LEAK` (an earlier uniform-`0xdb`×16 "leak" was a
      stuck-hot cache-slot artifact — the PoC was hardened to require a
      *non-uniform* recovery). **The leak-on-B / no-leak-on-A A/B is NOT
      demonstrable on this silicon** — both arms are no-leak because the CPU is
      immune; a positive leak demo needs Meltdown-**susceptible** (pre-`rdcl_no`)
      silicon or a KVM CPU model without `rdcl_no`. Evidence:
      [CET/Meltdown handoff §0.7](./2026-07-09-cet-boot-hang-on-tiger-lake.md#07--validation-result-20260709-delltiger-lake--cet-b3-closed).
- [x] **A.5 — PCID scheme is live on real silicon** — `Validated-on-HW (run 2,
      2026-07-09) — Dell / Tiger Lake` (evidence:
      [CET/Meltdown handoff §0.7](./2026-07-09-cet-boot-hang-on-tiger-lake.md)):
      `m3ctl mitigations status` shows `KPTI PCID: active (kernel/user PCID,
      no-flush)`; the tagged-CR3 no-flush trampolines + both-PCID `INVPCID`
      shootdown ran clean (no CR3 `#GP`/triple-fault at ring-3 entry). To
      reconfirm: boot the default image and
      confirm the A.5 fallback flips to *active* on PCID hardware: the `[sec]`
      line reads `pcid(active=true supported=true)` (vs the QEMU
      `active=false supported=false`), every `[sec] AP CR4… CR4.PCIDE enabled`,
      and `m3ctl mitigations status` prints `KPTI PCID: active (kernel/user PCID,
      no-flush)`. This is the first boot the tagged-CR3 + no-flush trampolines +
      both-PCID `INVPCID` shootdown actually execute — so it doubles as the
      functional proof of the whole A.5 asm/CR3 path (QEMU only ever ran the
      fallback). Watch for any CR3 `#GP` / triple-fault at first ring-3 entry
      (a PCIDE-ordering bug) or a wedged CoW/demand-fault loop (a missed
      user-PCID invalidation).
- [x] **A.5 — PCID perf bound** — `Validated-on-HW (2026-07-10) — Dell / Tiger
      Lake`. `/bin/perf-bench` (3M-iteration `getpid()` round-trip):
      image C (`full`, PCID+KPTI+CET on) `ns_per_syscall=6128`; image B (`off`)
      `ns_per_syscall=5967`. Overhead `(6128 − 5967)/5967 = 2.7 %` — **≪ 30 %,
      PASS** by a wide margin, confirming PCID hides almost all of the KPTI
      CR3-switch cost (a naive full-flush KPTI could not meet this). `ns_off < ns_full`
      also self-confirms image B really booted the mitigations-off kernel. No need
      for the per-CPU last-CR3 lever. (image B: `B-mitigations-off-fixbranch.img`,
      sha256 `63064824…`, off `fix/cet-nested-signal-ssp` @ `653d7ec9`.)
- [x] **B.3 — CET user shadow stacks are live on real silicon** —
      `Validated-on-HW (run 2, 2026-07-09) — Dell / Tiger Lake`. Boots clean
      under CET → greeter → login → compositor → terminal → fork/exec;
      `m3ctl mitigations status` reads `CET: enabled (user shadow stacks)` and
      the `[sec]` line reads `cet(active=true supported=true)`. The earlier boot
      hang was root-caused and **five** real-silicon CET bugs fixed (AP
      `CR0.WP`-before-`CR4.CET`, init SSP arming, signal-delivery SSP seeding,
      userspace-retpoline↔CET incompatibility, and fork eager-copy of
      shadow-stack pages). RESOLVED analysis:
      [2026-07-09 CET boot-hang handoff](./2026-07-09-cet-boot-hang-on-tiger-lake.md).
      Original acceptance detail retained — The whole B.3
      substrate is dormant on QEMU (TCG models no CET); Tiger Lake has `CET_SS`,
      so the Dell is the only place the active path runs. Boot the default image
      and confirm: the `[sec]` line flips to `cet(active=true supported=true)`
      (QEMU is `active=false supported=false`), every `[sec] CR4.CET enabled` +
      per-AP `IA32_U_CET` re-assert logs, and `m3ctl mitigations status` prints
      `CET: enabled (user shadow stacks)`. A clean boot-to-login here is the
      first proof the shadow-stack **enable + per-task SSP + context-switch
      save/restore + the shadow-stack PTE encoding** are all correct — a wrong
      encoding or a stale SSP restore shows up immediately as a `#CP` kill or a
      `#PF` on the first ring-3 `CALL`. **Stress arms (dedicated PoCs shipped this session):**
      (1) fork **CoW-of-shadow-stack** — **fixed** by Fix #5 (eager copy of
      RO+Dirty shadow-stack pages); regression-confirm with **`/bin/fork-cet-poc`**
      (expect `FORK_CET_POC:PASS`; a `FORK_CET_POC:FAIL` + a child `#CP` kill would
      be a regression). (2) **nested-signal** shadow-stack handling — the
      single-slot `cet_signal_ssp` covers non-nested delivery; deep nesting is the
      **open follow-up** (`RSTORSSP`-token path in
      `kernel_core::cet::shadow_stack_restore_token`). Exercise with
      **`/bin/nested-sig-cet-poc`** (expect `NESTED_SIG_POC:PASS`; a `#CP` kill
      confirms the per-frame-SSP redesign is needed).
      **Run-2 results (2026-07-10, Dell/Tiger Lake):** 4a **PASS**
      (`FORK_CET_POC:PASS` — Fix #5 holds on HW). 4b **FAIL — confirmed bug**:
      `nested-sig-cet-poc` `#CP`-killed on the *nested* handler's `ret`
      (`pid=45 rip=0x2014b3 err=0x1`); follow-up:
      [2026-07-10 nested-signal SSP `#CP`](./2026-07-10-cet-nested-signal-ssp-followup.md).
- [x] **B.3 — CET catches a real ROP/overwrite** — `Validated-on-HW (run 2,
      2026-07-09) — Dell / Tiger Lake`: `/bin/rop-cet-poc`'s overwrite was
      `#CP`-killed (no `ROP_CET_POC:PWNED`; `dmesg` shows `[int] userspace #CP …
      process killed`). Details below. PoC **scaffolded and
      shipping** as `/bin/rop-cet-poc` (`userspace/rop-cet-poc`, built canary-off
      via `-Zstack-protector=none`): its naked `vulnerable()` does a precise
      return-address overwrite (`mov [rsp],rdi; ret`, asm-verified). With CET
      **on** it must fault `#CP` (the `control_protection_fault_body` kill:
      `userspace #CP (CET control-protection) … process killed`) and never print
      `ROP_CET_POC:PWNED`; with CET off (mask `CET_SS` in `probe_cet`, or boot
      `M3OS_MITIGATIONS=off`) the overwrite returns into the planted address →
      `ROP_CET_POC:PWNED`. The CFI analogue of
      the A.6 Meltdown PoC (validated — see the check-off above). Skip-with-reason under QEMU TCG.

## Phase 111 — Remote Debugging (kgdb / ptrace on metal)

The whole phase is merged and QEMU-green; these arms prove the *bare-metal* value
prop (QEMU's own gdbstub is blind on real silicon, which is why the in-kernel
kgdb stub exists). All are `#[cfg]`-gated features — build with
`M3OS_KERNEL_FEATURES=kgdb` (or `ptrace`) for the debug image.

- [ ] **kgdb over a physical COM2** — the Dell has no DE-9; use a USB-serial
      adapter wired to COM2 (`0x2F8`), or expose COM2 another way. Boot the `kgdb`
      image (freezes at `KGDB:waiting`), attach a raw-RSP client (or real `gdb`)
      over the serial link, set a breakpoint at a kernel fn (`nm` addr +
      `0x10000000000`), continue, confirm the hit + register/memory read-back.
      This is the arm QEMU cannot substitute for.
- [ ] **kgdb async break on metal** — with the guest running, send Ctrl-C
      (`0x03`) on the serial link; confirm it breaks into the stub at the
      interrupted RIP (the BSP timer-tick poll works the same on real silicon).
- [ ] **kgdb panic hook** — trigger a real panic on the `kgdb` image; confirm it
      drops into the stub (`KGDB:panic`) instead of a dead halt — the highest-value
      bare-metal use (live post-mortem of a driver/IRQ/SMP crash).
- [ ] **ptrace / m3gdbserver on metal** — mostly hardware-agnostic (worked in
      QEMU), but run one real session: `m3gdbserver <port> <prog>` on the Dell,
      attach a host RSP client over the (Phase 104) Wi-Fi or a USB-Ethernet dongle,
      breakpoint + step + continue-to-exit. Optional: a `-g` (DWARF) userspace
      build + real `gdb` for source-level stepping (the D.4 follow-on).

## Phase 106 — USB Installer + NVMe (Track E bare-metal sign-off)

Merged 2026-07-03/05 (PRs #294–#307). Track E — **M1** (USB boot) + **M3** (real
NVMe install) — is the only open Phase 106 item; operator-owned, needs physical
access. QEMU proves both in CI (`usb-root-smoke`, `nvme-install-smoke`,
`nvme-install-part-smoke`); the Dell is the sign-off.

- [ ] **M1 — USB boot to a writable root** — boot the combined GPT(ESP+ext2)
      image from USB; confirm a *writable* ext2 root (not the ramdisk fallback):
      OVMF loads the ESP, init brings up xhci + usb-storage, the GPT scan finds the
      ext2 partition, and a serial login writes + reads back a file.
- [ ] **M3 — install to the internal NVMe** — the one arm that deliberately writes
      `nvme0n1` (the internal system disk — **not** the USB `/dev/sda`). On the
      USB-booted system run `/sbin/installer --part` targeting the NVMe; watch
      `INSTALLER:mode/layout/target/gpt-written/esp-copied/format/populate` with no
      `INSTALLER:error part-*`, answer the first-user account prompt, then reboot
      **with the USB removed** → the NVMe boots alone to a login as the created
      first user (the `--raw` sparse-copy arm is the fallback).
