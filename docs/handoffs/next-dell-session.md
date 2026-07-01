# Standing checklist — next Dell (Precision 5560) session

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

## Phase 101 — ACPI Platform Foundation (fixture capture)

- [ ] **Capture the Dell's ACPI tables** — the Track A/B/C host tests currently run
      on QEMU/synthetic fixtures; the charter wants them on the real DSDT. Boot the
      stick, then from the m3OS side dump DSDT/SSDTs via the `usb-logsink` boot.log
      path (or boot any Linux USB and `sudo acpidump > dell-5560-acpi.dat`). Land the
      dump under `kernel-core/tests/fixtures/acpi/` and re-point the
      `find_by_hid("DLL0945")` / touchpad-`_CRS` tests at it.

## Phase 106 — USB Installer (when the combined image lands)

- [ ] **M1 rung** — boot the combined GPT(ESP+ext2) image from USB and confirm a
      *writable* ext2 root (not the ramdisk fallback); record per protocol.
