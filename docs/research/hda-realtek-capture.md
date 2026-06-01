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
echo 1022 15e3    | sudo tee /sys/bus/pci/drivers/vfio-pci/new_id
lspci -nnks 10:00.6     # expect: Kernel driver in use: vfio-pci

# 2. Boot m3OS with the real controller passed through (turnkey: the
#    M3OS_HDA_VFIO_BDF env var makes hda-smoke pass the BDF through instead of
#    the emulated intel-hda codec and skip the WAV check). Run as root so QEMU
#    can open /dev/vfio. --display lets you also see it; drop it for headless.
sudo -E M3OS_HDA_VFIO_BDF=10:00.6 M3OS_SMOKE_SERIAL_DUMP=/tmp/hda-hw.log \
    cargo xtask hda-smoke --display

# 3. Grep the serial for the real-hardware bring-up (closes F.1 #1 / #3):
grep -iE "hda_driver|stream IRQ|codecs ready|HDA_SMOKE" /tmp/hda-hw.log

# 4. LISTEN — audible, non-silent through the internal speaker (F.1 #2, human-only).

# 5. Restore host audio:
echo 0000:10:00.6 | sudo tee /sys/bus/pci/drivers/vfio-pci/unbind
echo 0000:10:00.6 | sudo tee /sys/bus/pci/drivers/snd_hda_intel/bind
```

## Real-hardware run results (2026-06-01, this host, via VFIO)

Ran `M3OS_HDA_VFIO_BDF=10:00.6 cargo xtask hda-smoke` against the physical AMD
controller (vfio-pci bound). **Findings — three real issues surfaced (F.1 #3),
two fixed, one open:**

### ✅ Fixed: PCI-slot collision with the nvme sentinel
The passed-through controller first landed at guest `00:04.0`, which is
`nvme_driver`'s **sentinel BDF** — nvme claimed the HDA (wrong device), failed,
and its restart-churn starved `hda_driver` ("device claim failed"). **Fix:** the
VFIO mode pins the device to slot `0x8` (clear of all sentinels e1000=3/nvme=4/
ac97=5/xhci=6); `hda_driver` then claims it cleanly (`device_host.claim pid=NN
bdf=0000:00:08.0`).

### ✅ Fixed/hardened: reset + codec-ready wait
Added STATESTS-clear-before-reset (Redox-style), a post-CRST codec-enumeration
delay, a reset retry, and a 4 s wall-clock STATESTS poll (QEMU's codec reports
in <1 ms; real silicon needs the window). No regression to the QEMU gate.

### ⚠️ OPEN (AMD vendor quirk): the ALC1220 does not enumerate in STATESTS
After the controller is fully up the codec link does not wake:
- `GCAP = 0x4401` → OSS=4, ISS=4, BSS=0, 64OK=1 — **valid; MMIO reads/writes work.**
- `GCTL = 0x00000001` → CRST set — **controller is out of reset.**
- `STATESTS = 0x0000` — **no codec on any SDI line**, even after 2 reset cycles ×
  4 s polling (~8 s total) with the STATESTS-clear + delay.

**Diagnosis:** the AMD "Family 17h/19h HD Audio Controller" (`1022:15e3`) is
fully accessible and resets correctly, but the ALC1220 does not assert SDI
presence via the standard HDA `GCTL.CRST` sequence. Real drivers (Linux
`snd_hda_intel`) bring AMD codecs up with vendor-specific handling not modelled
by QEMU's `intel-hda` — e.g. AMD snoop/coherency config in PCI config space, and
link/clock enablement beyond `CRST`. m3OS has no PCI-config-**write** syscall yet
(only `sys_device_config_read`, Phase 79) and ships zero vendor quirks, so this
AMD link bring-up is genuine follow-up work (the accretive, datasheet-driven part
Track F represents).

**Consequently:** F.1 #2 (operator-audible output) cannot be reached on this
controller until the codec enumerates — it is blocked behind this open item, not
behind the driver's QEMU-validated stream path (which is complete and produces
non-silent audio against the generic codec).

### What a future AMD-codec-enablement pass needs
- A `sys_device_config_write` device-host syscall (mirror of `sys_device_config_read`).
- AMD snoop/coherency + link-clock config in `hda_driver` (keyed off the AMD
  controller IDs already in `HDA_DEVICE_IDS`), validated by re-running this VFIO
  runbook until `STATESTS != 0`, then capturing the widget graph below.

### Widget graph (capture once STATESTS != 0)
- Vendor:device (`GET_PARAMETER VENDOR_ID`) = `0x10ec:0x____` (expect ALC1220)
- Output pins + `GET_CONFIG_DEFAULT` words; selected pin→DAC path: ____

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
