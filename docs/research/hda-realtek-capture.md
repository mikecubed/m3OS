# HDA + Realtek empirical capture (Phase 80 Track F.1)

**Status:** AMD-codec-enablement follow-up **landed** (2026-06-01); awaiting an
operator VFIO re-run to confirm whether the ALC1220 now enumerates. The Phase 80
HDA driver + Realtek amp-enable code passes `hda-smoke` against QEMU's generic
`intel-hda`/`hda-duplex` codec. After the first VFIO run found the ALC1220 not
enumerating in `STATESTS` on the real AMD controller, web-traced Linux
`snd_hda_intel` source (see "Corrected diagnosis" below) and landed the
ring-3-side bring-up the driver was missing: a `sys_device_config_write` syscall,
a PCI **D0** power-up, an in-reset **PLL-settle delay**, and the AMD/ATI snoop
write. The remaining acceptance — audible output through the dev laptop's
**internal speaker via its Realtek codec** — still needs an operator VFIO re-run
(QEMU's codec has no EAPD/GPIO amp gating, and the codec-enumeration outcome can
only be observed on the real AMD silicon). Re-run `scripts/hda-vfio-validate.md`
(or the turnkey block below) and fill in the result sections.

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

**Diagnosis (initial, since refined — see "Corrected diagnosis" below):** the AMD
"Family 17h/19h HD Audio Controller" (`1022:15e3`) is fully accessible and resets
correctly, but the ALC1220 did not assert SDI presence via the standard HDA
`GCTL.CRST` sequence as the driver then issued it. This run hypothesised the
missing piece was an AMD snoop/config-space write; tracing Linux source
afterward corrected that to a **reset-timing + codec-power** problem (the snoop
write is DMA-coherency only). The `sys_device_config_write` syscall this run
flagged as absent **has since been added**, alongside the Linux-correct reset
timing and a D0 power-up — see "Follow-up landed" below.

**Consequently:** F.1 #2 (operator-audible output) cannot be reached on this
controller until the codec enumerates — it is blocked behind codec enumeration,
not behind the driver's QEMU-validated stream path (which is complete and
produces non-silent audio against the generic codec). The follow-up below targets
exactly that enumeration gap and needs an operator re-run to confirm.

### Corrected diagnosis (2026-06-01, traced from Linux `snd_hda_intel` source)

The first VFIO run hypothesised the fix was an **AMD snoop config write**. Tracing
the Linux kernel source (v6.6 `sound/pci/hda/hda_intel.c` + `sound/hda/
hdac_controller.c`) corrected this:

- **The ATI/AMD snoop write at config `0x42` governs DMA cache coherency only —
  it is NOT what makes a codec appear in `STATESTS`.** A codec enumerates without
  it (playback would just be garbled). So snoop alone was never the enumeration
  fix.
- **The real enumeration gates are reset timing + codec power.** Linux's
  `snd_hdac_bus_reset_link` (1) holds CRST asserted **≥100 µs (it uses 500–1000 µs)
  for the codec PLL to settle**, then (2) waits **≥540 µs (uses 1000–1200 µs)
  after deasserting CRST before reading `STATESTS`**. The m3OS driver had the
  post-CRST window (2 ms + a 4 s poll) but **deasserted reset immediately** with
  no in-reset PLL-settle delay — the most likely real-silicon gap. Reading
  `STATESTS` too early is the classic cause of `0x0000` on hardware while QEMU
  (instant codec) passes.
- **Codec presence does not require CORB/RIRB or interrupts** — Linux reads
  `STATESTS` before either is started, ruling out "verb ring must be live first".
- **`1022:15e3` carries `AZX_DCAPS_PM_RUNTIME`.** Under VFIO of just the HDA
  function, the host's runtime-PM may have left the controller (and its internal
  codec block) in **D3**, and the platform ACPI power-resources / `_PS0` that gate
  the codec link clock are not replayed in the guest. The Linux-recommended
  mitigation is to **force/keep the function at D0** before bring-up.

### Follow-up landed (2026-06-01) — ring-3 bring-up the driver was missing

Implemented in this branch (all QEMU-`hda-smoke`-green, no regression; the AMD
paths are no-ops on QEMU's Intel device):

1. **`sys_device_config_write` device-host syscall** (`0x1129`, mirror of
   `sys_device_config_read`) — gated on the caller **owning the BDF claim** (a
   write mutates device state, unlike the pre-claim read probe). Kernel
   `kernel/src/syscall/device_host.rs::sys_device_config_write` +
   `pci_config_write_u8`; dispatch in `arch/x86_64/syscall/mod.rs`; userspace
   `driver_runtime::pci_config_write`. Validation + numbering host-tested in
   `kernel_core::device_host::{config_write, syscalls}`.
2. **PCI D0 power-up before reset** — `hda_driver` walks the PM capability
   (`kernel_core::device_host::pci_pm::find_capability`) and clears the PMCSR
   power-state field to D0 if set (host-tested PMCSR decode/force). Defensive
   against the VFIO runtime-PM-in-D3 case.
3. **In-reset PLL-settle delay** — `controller.rs::reset` now holds CRST asserted
   ~600 µs before deasserting, matching Linux `snd_hdac_bus_enter_link_reset`.
   This is the highest-confidence enumeration fix for real silicon.
4. **AMD/ATI snoop write** (`kernel_core::hda::amd::ati_snoop_rmw`, config `0x42`
   `&= ~0x07 |= 0x02`, AMD-gated by vendor `0x1022`) — for coherent DMA once a
   codec is up, applied via the new config-write syscall.

### Remaining risk if the re-run still shows `STATESTS = 0`

If correct reset timing + D0 still does not surface the codec, the codec block is
power/clock-gated at the **SoC level under VFIO** (ACPI power resources not
replayed in the guest) — a platform/passthrough limitation, not a driver bug.
Linux exposes no single config-space "enable codec link clock" bit for `15e3`; it
relies on the standard CRST cycle + correct power state + patient retry. Possible
operator-side mitigations: bind host `snd_hda_intel` once to power the codec up,
then hand the function to VFIO **without** a power cycle; or validate on bare
metal rather than under passthrough.

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
