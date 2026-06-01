# HDA + Realtek empirical capture (Phase 80 Track F.1)

**Status:** Pending hardware execution. The Phase 80 HDA driver + Realtek
amp-enable code is complete and passes `hda-smoke` against QEMU's generic
`intel-hda`/`hda-duplex` codec. The remaining acceptance — audible output
through the dev laptop's **internal speaker via its Realtek codec** — is an
operator action that cannot run in QEMU/CI (QEMU's codec has no EAPD/GPIO amp
gating). Run `scripts/hda-vfio-validate.md` on the dev laptop and fill in the
sections below from the captured serial/register state.

> This is the audio analog of `docs/research/` captures for the Phase 79
> Realtek NIC: QEMU only emulates a generic part, so real Realtek behaviour
> (external-amp EAPD/GPIO, pin-default config, multi-codec selection) is
> recorded here once observed on silicon.

## Target hardware — CONFIRMED on the dev host (read-only, 2026-06-01)

The build host **is** the dev laptop. Confirmed non-destructively via `lspci` /
`/sys/kernel/iommu_groups` / `/proc/asound` (no sudo, no driver unbind):

- **HDA controller:** AMD `1022:15e3` (class `0x040300`) at BDF **`0000:10:00.6`**,
  MMIO `0xf6c80000`, host IRQ 112. DeviceName (SMBIOS): **Realtek ALC1220**.
- **Codec:** **Realtek ALC1220** — one of the Track-E target codecs
  (ALC888/892/1220). Confirms `kernel_core::hda::realtek` (EAPD/GPIO amp-enable,
  pin-default selection) targets the right silicon.
- **Subsystem:** MSI (Micro-Star) `1462:ed76`.
- **IOMMU group:** **31 — ISOLATED** (only `10:00.6` in the group), so VFIO
  passthrough of the HDA controller is safe and pulls in no GPU/display device.
- **Multi-codec reality:** the GPU's audio function `10:00.1` (ATI Rembrandt
  Radeon HD Audio, `1002:1640`, codec "ATI R6xx HDMI") is a *separate*
  controller — exactly the analog-vs-HDMI split `kernel_core::hda::widget::
  select_codec` is written to handle (prefer the analog ALC1220 over HDMI).

> The driver's design assumptions all match this silicon: `hda_pci_match`
> accepts `1022:15e3` (in `HDA_DEVICE_IDS`), `select_codec` prefers the analog
> ALC1220, and `realtek::*` drives the ALC1220 amp-enable. The remaining steps
> below require **root (sudo)** to bind vfio-pci + run QEMU, and a **human to
> listen** for "audible through the internal speaker" — both inherently
> operator actions (this session has no passwordless sudo and cannot hear).

## Turnkey operator run (this exact host)

Run each line with the `! ` prefix in a Claude Code prompt (executes in-session
so the serial lands here), or in a root shell. **Host audio drops** while the
ALC1220 controller is bound to vfio-pci; step 5 restores it.

```bash
# 1. Bind the isolated HDA controller to vfio-pci (drops host audio):
sudo modprobe vfio-pci
echo 0000:10:00.6 | sudo tee /sys/bus/pci/devices/0000:10:00.6/driver/unbind
echo 1022 15e3   | sudo tee /sys/bus/pci/drivers/vfio-pci/new_id
lspci -nnks 10:00.6     # expect: Kernel driver in use: vfio-pci

# 2. Build + boot m3OS with the real controller passed through, serial to a file.
#    (Temporarily swap `-device intel-hda -device hda-duplex` in hda_smoke_qemu_args
#    for `-device vfio-pci,host=10:00.6`, or run `cargo xtask image` and launch
#    QEMU directly with the standard UEFI/OVMF + data-disk args + that -device +
#    `-serial file:/tmp/hda-hw.log`.)
M3OS_SMOKE_SERIAL_DUMP=/tmp/hda-hw.log sudo -E cargo xtask hda-smoke   # after the vfio swap

# 3. Grep the serial for the bring-up (closes F.1 #1 / #3 programmatically):
grep -iE "hda_driver|stream IRQ|codecs ready|VENDOR" /tmp/hda-hw.log

# 4. LISTEN — audible, non-silent through the internal speaker (F.1 #2, human-only).

# 5. Restore host audio:
echo 0000:10:00.6 | sudo tee /sys/bus/pci/drivers/vfio-pci/unbind
echo 0000:10:00.6 | sudo tee /sys/bus/pci/drivers/snd_hda_intel/bind
```

## To capture from the run (fill in)

## To capture (fill in after the VFIO run)

### Controller
- `GCAP` raw = `0x____` → OSS=__ ISS=__ BSS=__ 64OK=__
- `STATESTS` = `0x____` → codec address(es): ____
- Reset + CORB/RIRB RUN-enable read-back: `CORBCTL.CORBRUN`=__ `RIRBCTL.RIRBDMAEN`=__

### Codec widget graph
- Vendor:device (`GET_PARAMETER VENDOR_ID`) = `0x____:0x____`
- AFG NID = `0x__`; widget NID range = `0x__..0x__`
- Enumerated widgets (NID → type): ____
- Output pins + `GET_CONFIG_DEFAULT` words:
  - Internal speaker pin NID `0x__` cfg=`0x________` (default_device=Speaker, port=fixed)
  - HP pin NID `0x__` cfg=`0x________`
  - Rear line-out NID `0x__` cfg=`0x________`
- Selected pin→DAC path: ____

### The amp-enable question (the load-bearing Realtek datum)
- Did `SET_EAPD_BTLENABLE 0x70C` (payload `0x02`) alone yield audible output? **[ ] yes / [ ] no**
- If no, did the GPIO-EAPD fallback (`SET_GPIO_{DIRECTION,MASK,DATA}`) help? mask used = `0x__`
- If still silent, which vendor COEF write was required? `SET_COEF_INDEX 0x__` / `SET_PROC_COEF 0x____`
  (this is the only datum that would seed any future board-specific COEF table;
  m3OS ships zero quirk tables by default.)

### Result
- `audio-smoke` over VFIO: internal speaker audible, non-silent? **[ ] yes / [ ] no**
- Any kernel/driver bug uncovered (cf. Phase 79's ECAM/BAR/IRQ fixes): ____ (commit ref ____)

## Notes

- The driver selects the analog codec over an HDMI/DP-only codec via
  `kernel_core::hda::widget::select_codec`; on a multi-codec board record which
  codec address was chosen.
- Live HDA interrupt delivery: the driver arms `INTCTL` + handles `SDnSTS.BCIS`
  but uses `SDnLPIB` polling as the authoritative completion path. Note here
  whether the BCIS IRQ is observed on real hardware (it may behave differently
  from QEMU's `intel-hda`, where it was not delivered to the ring-3 driver).
