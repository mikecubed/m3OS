# Standing checklist — next Dell (Precision 5560) session

> **Running a session?** Use the sequenced runbook:
> [2026-07-09 Dell validation session](./2026-07-09-dell-validation-session.md)
> — objectives, per-test expected serial lines, pass/fail bars, and a
> failure→cause map, ordered by value. This file stays the standing *checklist*
> the runbook works down.

A running list of everything waiting on physical access to the reference laptop, so
bench time gets batched instead of spent one item per session. **Add items here when
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

## Phase 110 — Real-Hardware Security (KPTI Meltdown + PCID on metal)

Track A (KPTI) is merged and live on every QEMU boot, but QEMU TCG models
**no** speculation and advertises **no** PCID/INVPCID, so two arms are inherently
bare-metal-only. The Precision 5560 (Intel Tiger Lake) has both PCID and INVPCID
and is Meltdown-susceptible with KPTI off, so it is the right target.

- [ ] **A.6 — Meltdown PoC reject** (`Validated-on-HW`, never a bare "Complete").
      Boot `M3OS_MITIGATIONS=full` on the Dell, run a ported public Meltdown PoC:
      it must **leak** kernel memory with KPTI off (`M3OS_MITIGATIONS=off`) and
      **fail** to leak with it on. Record the run (`run N, YYYY-MM-DD`) + the
      capture path. This is the whole point of Track A — QEMU can never prove it.
- [ ] **A.5 — PCID scheme is live on real silicon.** Boot the default image and
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
- [ ] **A.5 — PCID perf bound.** With PCID active under `M3OS_MITIGATIONS=full`,
      the smoke suite must be **≤30 %** slower than `M3OS_MITIGATIONS=off` (the
      Phase 84 bound the naive full-flush KPTI cannot meet). Capture both wall
      times; if the delta exceeds 30 %, the same-address-space re-dispatch
      no-flush optimization (a documented A.5 follow-up: per-CPU last-CR3 cache)
      is the next lever. Also worth a same-boot A/B: temporarily force the
      fallback (mask PCID in `probe_pcid`) to measure the recovery the tags buy.
- [ ] **B.3 — CET user shadow stacks are live on real silicon.** The whole B.3
      substrate is dormant on QEMU (TCG models no CET); Tiger Lake has `CET_SS`,
      so the Dell is the only place the active path runs. Boot the default image
      and confirm: the `[sec]` line flips to `cet(active=true supported=true)`
      (QEMU is `active=false supported=false`), every `[sec] CR4.CET enabled` +
      per-AP `IA32_U_CET` re-assert logs, and `m3ctl mitigations status` prints
      `CET: enabled (user shadow stacks)`. A clean boot-to-login here is the
      first proof the shadow-stack **enable + per-task SSP + context-switch
      save/restore + the shadow-stack PTE encoding** are all correct — a wrong
      encoding or a stale SSP restore shows up immediately as a `#CP` kill or a
      `#PF` on the first ring-3 `CALL`. **Watch for:** the fork **CoW-of-shadow-
      stack** interaction (a child's first shadow-stack push CoW-duplicating the
      RO+Dirty page — m3OS's generic CoW may need a shadow-stack-aware arm,
      unlike Linux's explicit copy), and **nested-signal** shadow-stack handling
      (the single-slot `cet_signal_ssp` covers non-nested; nesting needs the
      `RSTORSSP`-token path modeled in `kernel_core::cet::shadow_stack_restore_token`).
- [ ] **B.3 — CET catches a real ROP/overwrite.** Port (or write) a tiny
      return-address-overwrite PoC: with CET **on** it must fault `#CP` (the
      `control_protection_fault_body` kill: `userspace #CP (CET control-protection)
      … process killed`); with CET off (mask `CET_SS` in `probe_cet`) the same
      overwrite returns into the planted address. `Validated-on-HW (run N, date)`,
      the CFI analogue of the A.6 Meltdown PoC. Skip-with-reason under QEMU TCG.

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

## Phase 106 — USB Installer (when the combined image lands)

- [ ] **M1 rung** — boot the combined GPT(ESP+ext2) image from USB and confirm a
      *writable* ext2 root (not the ramdisk fallback); record per protocol.
