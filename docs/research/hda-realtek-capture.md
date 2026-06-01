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

## Target hardware

- **HDA controller:** AMD `0x1022:0x15e3` (class `0x040300`). _(Confirm BDF +
  IOMMU group via `scripts/hda-vfio-validate.md` step 0.)_
- **Codec:** Realtek ALC___ (fill in: ALC892 / ALC1220 / …), subsystem id ____.

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
