# Hardware-Validation Campaign — the 100–110 arc

**Status:** Living plan (created 2026-07-10)
**Companion to:** [bare-metal validation strategy](./bare-metal-validation.md) (the *how* — status
convention, capture toolkit, per-phase protocol) and
[next-dell-session.md](../handoffs/next-dell-session.md) (the *running bench checklist* the
runbook works down). This doc is the **whole-arc inventory + campaign plan** — the single map
of what still needs real hardware across phases 100–110, **including the phases that are not
yet built** (which the bench checklist omits because they have no HW arms written yet).

## Why this exists

QEMU models none of the GUI-workstation-arc hardware (Intel LPSS I2C, the Elan I2C-HID touchpad,
the AX201/CNVi radio, ACPI battery/thermal/lid, S3/S0ix, the laptop audio codec, AMD-Vi, "the
screen shows the greeter"), so phases 100→110 ship **skip-with-reason** CI gates and their true
completion is gated on recorded hardware runs (see the strategy doc). Most of the arc's *code*
has landed and is QEMU-green — but only **Phase 110** has actually been driven to
`Validated-on-HW`. The rest of the hardware validation was deferred, and until now the gap was
recorded only in scattered places (per-gate notes in `AGENTS.md` / `regression-gates.md`, the
per-phase task docs, `next-dell-session.md` for the *built* phases only). This doc consolidates
it so bench time and the remaining driver work can be planned together.

> **Doc-hygiene note (known, not yet fixed):** the per-phase `**Status:**` headers at the top of
> `docs/roadmap/1NN-*.md` are stale for several phases (101/103/105/106/107 read "Planned" but are
> substantially landed + QEMU-green). The authoritative status is the README milestone table
> (`docs/roadmap/README.md:553`) and the handoffs — reflected below. Reconciling those headers is a
> tracked follow-up.

## Status at a glance

Code = is the subsystem implemented in the tree. HW = recorded hardware validation state.

| Phase | Subsystem | Code | HW validation | Blocking hardware |
|---|---|---|---|---|
| **100** | Bare-metal GUI session (greeter, USB HID, WC framebuffer) | ✅ merged (#272) | ❌ **open** — greeter on panel, USB kbd/mouse behind dock hub, WC blit-latency, idle CPU | Dell panel; USB mouse + keyboard + hub |
| **101** | ACPI namespace + AML interpreter + `acpid` | ✅ mostly (A–E) | ❌ **open** — Dell DSDT/SSDT capture, `acpid` on metal, FADT boot line, lid SCI | Dell (real DSDT + lid) |
| **102** | I2C-HID touchpad (Intel LPSS DesignWare) | ❌ **not built** | — (needs the driver first) | Dell Elan touchpad (`DLL0945`) |
| **103** | Laptop power (battery/AC, backlight, thermal, HWP, S3) | ✅ (A–F; S3 QEMU-green) | ❌ **open** — *entirely* bare-metal: battery/AC, unplug-flip, backlight dim, HWP range, S3-on-firmware | Dell battery + charger + panel + HWP CPU |
| **104** | Wi-Fi Intel AX201/CNVi + WPA2 supplicant | ❌ **not built** | — (needs the driver first; only MediaTek mt792x shipped) | Dell AX201 radio + a WPA2 AP |
| **105** | Native GUI toolkit + core apps + settings | ✅ core (A–E, D.4) | mostly QEMU-ok; settings Wi-Fi picker waits on 104; D.5 on-metal | (rides 100/103/104 bench runs) |
| **106** | USB installer + NVMe install | ✅ (A, B; C foundation) | ❌ **open** — M1 USB-boot-to-writable-root, M3 install-to-internal-NVMe | Dell internal NVMe + USB stick |
| **107** | Networked + ed25519-signed packages | ✅ (A–D) | **no hardware** (off-metal); only a live-HTTPS arm (owner creates the repo) | none |
| **108** | HP OmniBook / AMD Strix Point bring-up | ❌ **not built** | — (a different machine) | HP OmniBook (MT7925, AMD-Vi, AMD I2C-HID) |
| **109** | Bare-metal audio (HDA vs SoundWire+SOF) | ⚠️ HDA built (P80); **codec path unscoped** | ❌ **open** — determine the Dell codec, then validate HDA **or** build SoundWire | Dell audio codec (may not be HDA) |
| **110** | Real-hardware security (KPTI/PCID/CET/argon2id) | ✅ | ✅ **Validated-on-HW (2026-07-09/10)** — KPTI+PCID+CET live, PCID perf 2.7%, ROP `#CP`-kill, fork-CoW, **nested-signal fixed**; remaining: **Secure Boot on metal** (Track D) | Dell Secure-Boot firmware |
| **111** | Remote debugging (kgdb / ptrace) | ✅ Complete (merged) | ❌ on-metal arms open — kgdb over COM2, async break, panic hook, ptrace/m3gdbserver | USB-serial adapter (COM2); a NIC for remote ptrace |

## The three kinds of remaining work

**A. Validate-on-Dell — code exists, just needs a recorded bench run.** The bulk of the gap.
Flash → boot → assert the phase's serial sentinels / capture a photo → record `Validated-on-HW`.
Phases: **100, 101, 103, 106, 109 (scoping arm), 111**, plus **110 Secure Boot**. These are
batchable — see the sequencing below. Their exact arms live in
[next-dell-session.md](../handoffs/next-dell-session.md) (100/101/103/106/110/111).

**B. Build-then-validate — the driver is missing.** Real implementation work, not just
validation, and the two things standing between this and a self-contained usable Dell:
- **102 — I2C-HID touchpad.** The Dell's only built-in pointer (there is no PS/2 or I2C-HID
  driver in the tree today; Phase 100 drives an *interim USB* mouse). Needs an Intel LPSS
  DesignWare I2C controller + I2C-HID transport + multitouch parse → `mouse_server`. **Depends on
  101** (the touchpad's I2C address + GpioInt come from ACPI `_CRS`), so it must follow the 101
  DSDT capture.
- **104 — AX201 Wi-Fi.** The Dell's only built-in NIC (no Ethernet port). Needs an `iwx`-style
  AX201/CNVi driver → `RemoteNic` **plus** a running WPA2 supplicant/connect daemon (`wifi-core`
  is only a config parser today). Until this lands, the Dell has no built-in network — bench
  capture relies on the RTL8156 USB-Ethernet dongle.

**C. Scope-first.** **109 audio** is a scoping risk before it is a validation task: modern Tiger
Lake often routes audio over **SoundWire + SOF DSP**, where the Phase 80 HDA driver may not bind.
The first arm is a cheap on-metal probe (what codec/bus does the Dell expose?) — do it during any
bench session — which then decides "validate HDA" vs "build a SoundWire driver."

**Off-metal / effectively done:** 105 core (QEMU-validated; only the Wi-Fi picker waits on 104),
107 (no hardware; owner just creates the public package repo + secret), 110 (done bar Secure
Boot). **108** is a whole different machine (HP OmniBook / AMD Strix) — sequence it after the Dell
line when that hardware is on hand.

## Recommended sequencing

1. **Batch the ready Dell bench arms** (one or two sessions, value-ordered — the machine is
   already the reference target from the Phase 110 work):
   - **101 ACPI capture first** — dump the Dell DSDT/SSDTs. It is a *prerequisite input* for both
     103 (real `_BST`/`_BIF` battery objects) and 102 (touchpad `_CRS`), so it unblocks the most
     downstream work per minute of bench time.
   - **103 power** — the highest-value headline (a usable daily-driver laptop) and *entirely*
     bare-metal (battery/AC, charger-unplug flip, backlight dim photo, HWP range, S3 round-trip).
   - **100 GUI/USB** — resolve the open tier-2 (behind-hub) keyboard/mouse capture + the greeter
     photo; this also exercises 105's toolkit on the panel.
   - **106 NVMe install** — M1 USB-boot-to-root + M3 install-to-internal-NVMe (the one arm that
     writes `nvme0n1`, so do it deliberately).
   - **111 kgdb** — needs a USB-serial adapter on COM2; fold in if the adapter is on hand.
   - **110 Secure Boot** — the last 110 arm; standalone.
2. **Build 102 (touchpad, after the 101 capture) and 104 (Wi-Fi)** — the two driver-writing
   efforts that make the Dell self-contained (real pointer + built-in network). 104 also unblocks
   105's settings Wi-Fi picker and 111's remote-ptrace-over-Wi-Fi arm.
3. **109 audio** — run the codec-scoping probe during step 1; schedule the validate-or-build work
   once the bus is known.
4. **108 (AMD OmniBook)** — when that machine is available.

## Hardware inventory a full campaign needs

- **Dell Precision 5560 / Tiger Lake** (primary): real battery + charger + backlight panel +
  HWP-capable CPU (103); PCID+INVPCID + `CET_SS` (110, done); AX201/CNVi Wi-Fi (104); Elan
  I2C-HID touchpad (102); internal NVMe (106); Secure-Boot firmware (110 D); a **USB-serial
  adapter** wired to COM2 (`0x2F8`) for kgdb (111). Note it is `rdcl_no=true` (**Meltdown-immune**),
  so a *positive* Meltdown-leak demo (110 A.6) needs pre-`rdcl_no` silicon or a KVM CPU model
  without `rdcl_no` — not this laptop.
- **Peripherals:** USB mouse + USB keyboard + USB hub (100); an RTL8156 `0bda:8156` USB-Ethernet
  dongle (bench network before 104 lands; also 106/111); a USB audio (UAC) device and/or a real
  HDA codec (109).
- **HP OmniBook Ultra 14 / AMD Strix Point** (108): MT7925 Wi-Fi, AMD-Vi, AMD I2C-HID.

## Recording results

Follow the [status convention](./bare-metal-validation.md): a validated arm becomes
`Validated-on-HW (run N, YYYY-MM-DD) — <machine>; evidence: <pointer>` in the phase's task doc /
README table, and the corresponding checkbox in `next-dell-session.md` is ticked with the capture
path. When a phase's HW arms are *added* (e.g. once 102/104 land), add them to
`next-dell-session.md` and update this table.
