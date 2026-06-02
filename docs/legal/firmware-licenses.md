# Firmware Licenses

This document records the redistribution terms and license compliance for every
proprietary firmware blob that m3OS ships or stages. It is the **prerequisite**
for committing any real vendor blob to the tree (Phase 81 Task F.3): Tracks A.4
and E.1 are deliberately written to need **no** committed vendor blob (synthetic
crafted fixtures for the host-tested parsers; operator-supplied bytes on
hardware) precisely so this review is never bypassed.

> **Status:** No firmware blob bytes are committed to this repository. The
> mt792x driver's `firmware_blob()` returns `None` by default and the build
> degrades gracefully (`MT792X_FW:absent:…`, Wi-Fi disabled). An operator stages
> the license-cleared blobs under `kernel/initrd/lib/firmware/` to enable Wi-Fi.

## MediaTek mt792x Wi-Fi (MT7921 / MT7922 / MT7925)

The MediaTek mt76 firmware blobs are distributed in the upstream
[`linux-firmware`](https://gitlab.com/kernel-firmware/linux-firmware) tree under
the `mediatek/` path. In `linux-firmware`'s `WHENCE` file they are marked
**Redistributable** under MediaTek's own terms (not the GPL) — shippable
unmodified, the same redistribution model as Intel's `iwlwifi`. The exact
`WHENCE` clause is:

```
Files: mediatek/mt7961/WIFI_MT7961_patch_mcu_1_2_hdr.bin
Files: mediatek/mt7961/WIFI_RAM_CODE_MT7961_1.bin
Files: mediatek/mt7922/WIFI_MT7922_patch_mcu_1_1_hdr.bin
Files: mediatek/mt7922/WIFI_RAM_CODE_MT7922_1.bin
Files: mediatek/mt7925/WIFI_MT7925_patch_mcu_1_1_hdr.bin
Files: mediatek/mt7925/WIFI_RAM_CODE_MT7925_1_1.bin

Licence: Redistributable. See LICENCE.mediatek for details.
```

The accompanying `LICENCE.mediatek` grants the right to **redistribute the
firmware in its unmodified binary form**, with the copyright and permission
notice reproduced. When a blob is staged into m3OS it must be the byte-identical
upstream file, and the `linux-firmware` source path + the exact commit hash it
was taken from must be recorded in the per-chip table below.

### Blobs the project intends to ship (when staged)

| Chip | Staged path | Upstream files | linux-firmware source + commit |
|---|---|---|---|
| MT7921 (connac2) | `kernel/initrd/lib/firmware/mt7961/` | `WIFI_MT7961_patch_mcu_1_2_hdr.bin`, `WIFI_RAM_CODE_MT7961_1.bin` | `mediatek/mt7961/` @ `<commit>` *(record on staging)* |
| MT7922 (connac2) | `kernel/initrd/lib/firmware/mt7922/` | `WIFI_MT7922_patch_mcu_1_1_hdr.bin`, `WIFI_RAM_CODE_MT7922_1.bin` | `mediatek/mt7922/` @ `<commit>` *(record on staging)* |
| MT7925 (connac3) | `kernel/initrd/lib/firmware/mt7925/` | `WIFI_MT7925_patch_mcu_1_1_hdr.bin`, `WIFI_RAM_CODE_MT7925_1_1.bin` | `mediatek/mt7925/` @ `<commit>` *(record on staging)* |

> MT7921's blob set is named `mt7961` upstream — the marketing name (MT7921) and
> the firmware family name (mt7961) differ; the driver's `select_firmware_set`
> maps the PCI device id to the `mt7961`/`mt7922`/`mt7925` stem.

### Compliance notes

- Each blob is shipped **unmodified**; the staging step (`stage_wifi_firmware`
  in `xtask`) copies the operator-supplied bytes verbatim and never rewrites
  them. No source code is required (Redistributable, not GPL).
- The blobs are **not committed** to this repository. They are staged by the
  operator from `linux-firmware` (or `/usr/lib/firmware/mediatek/` on a Linux
  host) into `kernel/initrd/lib/firmware/<chip>/`. The build succeeds whether or
  not they are present.

## Firmware-delivery decision (Task A.8 / F.3)

The mt792x driver uses the **`include_bytes!`-in-driver-crate seam** that the
`r8169` / `r8125` drivers established (`fw::firmware_blob()` returning
`Option<&'static [u8]>`), **not** a `generated_initrd_asset!` / ext2 initrd
asset.

Rationale and trade-off:

- **`include_bytes!`-in-driver (chosen).** Matches the existing NIC-driver
  convention, keeps the firmware lifetime `'static` and the load path
  syscall-free (there is **no** `request_firmware` / `sys_device_firmware*`
  syscall in the device-host ABI), and keeps the blob's presence a compile-time
  fact the driver can branch on. **Cost:** the blob (hundreds of KB) is linked
  into the driver ELF, enlarging it.
- **Initrd asset (rejected for 1.0).** Would keep the driver ELF small and let
  the firmware live on the data disk, but adds a runtime file-read + path
  resolution the other ring-3 NIC drivers do not use, and a second staging
  mechanism. If the driver-ELF size becomes a problem once a real blob is
  linked, revisiting this is the documented escape hatch.

Because no blob is committed today, `firmware_blob()` currently returns `None`
and neither cost is yet incurred; the seam is in place for an operator to wire
the `include_bytes!` once a blob is staged and this license review is satisfied
for the specific files shipped.
