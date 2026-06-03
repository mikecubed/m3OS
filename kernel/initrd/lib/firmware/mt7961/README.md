# MediaTek mt792x Wi-Fi firmware staging

Phase 81 (Task A.8 / F.3). This directory is where the **operator** stages the
license-cleared MediaTek mt792x Wi-Fi firmware blobs. **The blob bytes are NOT
committed to this repository** — they are MediaTek "Redistributable" firmware
whose redistribution terms are recorded in
[`docs/legal/firmware-licenses.md`](../../../../../docs/legal/firmware-licenses.md).

Expected blobs (from the upstream `linux-firmware` `mediatek/` tree):

- **MT7921** (`mt7961/`): `WIFI_MT7961_patch_mcu_1_2_hdr.bin`, `WIFI_RAM_CODE_MT7961_1.bin`
- **MT7922** (`mt7922/`): `WIFI_MT7922_patch_mcu_1_1_hdr.bin`, `WIFI_RAM_CODE_MT7922_1.bin`
- **MT7925** (`mt7925/`): `mt7925/WIFI_*` (if the connac3 part is targeted)

## Behaviour when absent (the default in this repo)

`cargo xtask` runs a firmware-staging step (`stage_wifi_firmware`) that reports
which blobs it found. When none are present it prints a **skip-with-reason** and
the build still succeeds: the ring-3 `mt792x` driver's `firmware_blob()` returns
`None`, emits `MT792X_FW:absent:…` at boot, and degrades gracefully (no panic, no
build break). Wi-Fi is simply disabled until a blob is supplied.

The firmware-delivery decision (`include_bytes!` in the driver crate vs. an
initrd asset) is recorded in `docs/legal/firmware-licenses.md` (Task F.3).
