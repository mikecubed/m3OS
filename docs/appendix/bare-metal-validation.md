# Bare-Metal Validation Strategy

**Aligned Roadmap Phase:** Phase 98 (Track A.5), reused by every HW-only phase in the GUI-workstation arc (99→110)
**Status:** Adopted (Phase 98 deliverable; the standing protocol for the 99→110 hardware-only arc)
**Source Ref:** phase-98

## Why this exists

m3OS's entire quality story has been **always-on, falsifiable CI gates** — a serial `Wait` on a sentinel, a host test, or a QMP/PPM screendump assertion. That story does not survive contact with the next arc. **QEMU models none of the new hardware**: there is no emulator for the Intel LPSS DesignWare I2C controller, the Elan I2C-HID touchpad, the Intel AX201/CNVi radio, ACPI battery/thermal/lid behavior, S3/S0ix suspend, the laptop's audio codec path, AMD-Vi on real AMD silicon, or "the screen actually shows the greeter" on a physical panel. Every phase from 99→110 that touches hardware will therefore ship a **skip-with-reason** CI gate at best.

Without a deliberate substitute, HW phases get marked "Complete" on a single uncaptured manual run with no regression coverage — which is **exactly the claim-vs-validated drift Phase 98 exists to retire**. This document is the substitute: a repeatable manual protocol, an evidence convention, and a status convention so a hardware phase carries real, recorded validation.

## The status convention

HW-only phases do **not** use a bare `Complete`. They use:

> **Validated-on-HW (run N, YYYY-MM-DD)** — `<machine>`; evidence: `<captured-artifact pointer>`

where `run N` increments on each recorded validation (so a regression that needs re-validation is visible), `<machine>` is the reference machine (`Dell Precision 5560 / Tiger Lake` or `HP OmniBook Ultra 14-fd0xxx / Strix Point`), and the evidence pointer is a committed capture (see below). A phase with no recorded HW run stays **Planned** or **Implemented (HW-unvalidated)** — never `Complete`.

## The capture toolkit (what we already have)

These came out of Phase 96's bare-metal bring-up and are the building blocks every HW phase reuses:

- **`cargo xtask run --usb-passthrough <vid:pid>`** — hands a *physical* USB device (a dongle, a USB mouse/keyboard, a USB- Ethernet/Wi-Fi adapter) to a QEMU guest via `usb-host`, so an in-the-loop iteration runs against real silicon while the existing serial harness captures logs. The primary fast-iteration path where the device is USB-attachable. (Caveat — Phase 96 found a host-owned device's bulk-IN stream may not forward under passthrough; cold-owned bare metal is the fallback.)
- **AMT Serial-over-LAN (SOL)** — for **pre-network** panic/boot logs on a port-less laptop, the Intel ME redirects COM1 (`0x3F8`, where m3OS already logs) over Ethernet; capture from a second machine with `amtterm`. Runbook: `scripts/ure-vfio-validate.md`.
- **`usb-logsink` boot.log** — the resident `usb-logsink` daemon writes the kernel dmesg to `/mnt/usb0/boot.log` on a writable USB ext2 partition, so the full boot log survives on removable media even with no network.
- **Network log sink** — `scripts/m3os-logsink.sh` on a second machine tails the target's `syslogd`/console stream over UDP/`ssh` into one file, for **post-network** live observability.

## The per-phase protocol

For each HW phase, the task doc's validation track follows this loop:

1. **Iterate against passthrough where possible.** If the device is USB-attachable (mice, keyboards, the `ure` dongle, a USB Wi-Fi adapter), use `--usb-passthrough` for the tight loop and the serial harness for assertions, exactly as `ure-smoke` does.
2. **Boot the physical reference machine** from the USB image (`cargo xtask image` → `dd` to USB, UEFI boot). Capture **pre-network** logs over AMT SOL and the **post-network** stream over the network sink + the `usb-logsink` boot.log.
3. **Assert the phase's sentinels** in the captured log (the same string sentinels the QEMU gates use — e.g. `[remote_nic] up=true`, `[dhcp] bound`, an `I2C_HID:` claim line, a touchpad PointerEvent count). A serial sentinel proves the code path ran.
4. **Prove "the screen shows X"** — there is **no QMP screendump on bare metal** (that path is QEMU-only). Two acceptable methods:
   - **On-device render assertion** — the app/compositor computes a cheap hash or changed-scanline count of its own output and prints it over the log sink (the on-metal analog of the `claude_tui_render_arm` / `less-render-probe` PPM band-diff). Falsifiable without a camera.
   - **Photographic capture** — a dated photo of the panel committed as the evidence artifact, for cases an on-device assertion can't cover (panel color, backlight level).
5. **Record the run.** Append a dated entry to the phase's validation track and/or a results appendix in the relevant `scripts/*-validate.md` runbook, set the README/task-doc Status to `Validated-on-HW (run N, date)`, and commit the captured artifact (or its pointer).

## Evidence convention (where captures live)

- Serial/console captures and `boot.log` excerpts: a results appendix in the phase's runbook under `scripts/` (generalize `scripts/ure-vfio-validate.md`), referenced from the task doc.
- On-device render-assertion output: the sentinel line in the captured log is the evidence; quote it in the task-doc acceptance checkbox.
- Photos: committed under the phase's evidence directory (kept small), referenced by path from the task doc.
- The README Status string carries the `run N, date, machine` so the map shows validation freshness at a glance.

## What stays in CI

Everything CI *can* still test stays a real gate — and HW phases should maximize this surface so the un-testable remainder is as small as possible:

- **Host tests** for all pure logic (AML opcode decode + `_CRS` descriptor parse on captured DSDT bytes; the I2C-HID descriptor + multitouch report parse; the `iwx` command/notification codec + the supplicant state machine; the `index.m3idx` parse + ed25519 verify + the dep solver; control-method evaluation on captured ACPI objects).
- **QEMU gates** for everything with a model (`smp-smoke` at higher `-smp`, the demand-fault soak, `nvme-rw`/`nvme-persist`, the GUI-session integration where a QEMU framebuffer suffices, the packaging index/sig/solver arms).
- **Skip-with-reason gates** for the HW-only datapaths (mirroring `tls-smoke`/`wifi-smoke`/`ure-smoke`), so the gate is present and self-documenting and flips to PASS on the machine where the device exists.

The discipline: a HW phase is **Validated-on-HW** only when the host-testable + QEMU-testable surface is green in CI **and** a recorded physical run cleared the un-modelable remainder.
