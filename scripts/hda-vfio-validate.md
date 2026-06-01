# HDA + Realtek real-hardware validation via VFIO passthrough (operator runbook)

Phase 80 Track F.1's acceptance — "on the dev laptop, the `hda_driver` completes
bring-up (claims `0x1022:0x15e3`, selects the analog Realtek codec, enumerates
it, selects the internal-speaker path, powers/unmutes the path, enables EAPD)
and `audio-smoke` produces operator-audible, non-silent output through the
internal speaker" — needs the driver to run against physical HDA silicon. This
is the **only remaining Phase 80 step that cannot run in QEMU/CI**: QEMU's
`intel-hda` exposes a generic codec, so Realtek-specific EAPD/GPIO/COEF
amp-enable, real pin-default config, and multi-codec selection can only be
proven on hardware.

> **This is an operator action, not an automated step.** It requires `root`,
> claims the machine's HDA controller (so **host audio drops** for the
> duration), and is hardware-specific to the dev laptop's AMD HDA controller
> (`0x1022:0x15e3`) + its onboard Realtek codec. A Claude Code session can run
> each step interactively by prefixing it with `!` (e.g. `! sudo ...`). The
> driver + Realtek code (`userspace/drivers/hda/`, `kernel-core/src/hda/`,
> `realtek.rs`) is complete and passes `hda-smoke` against QEMU's generic
> codec; this runbook is the hardware-only confirmation and exists so it is
> reproducible and authorized.

## 0. Pre-flight (read-only, safe)

```bash
# Identify the HDA controller + confirm vendor/device + IOMMU group.
lspci -nnk | grep -i 'audio\|hda'                 # expect 1022:15e3 (AMD HDA), driver snd_hda_intel
hda=$(lspci -Dn | awk '/0403:/{print $1; exit}')  # class 0403 = HD Audio
echo "HDA at $hda"
lspci -nnks "$hda"
readlink -f /sys/bus/pci/devices/$hda/iommu_group  # all devices in the group must be passed together
dmesg | grep -i 'snd_hda\|realtek\|ALC'            # note the Realtek codec id (e.g. ALC892/ALC1220)
```

Record: the controller BDF + `1022:15e3`, the IOMMU group members, and the
Realtek codec model + subsystem id (for the capture doc).

## 1. Bind the controller to vfio-pci

```bash
sudo modprobe vfio-pci
# Unbind from snd_hda_intel and bind to vfio-pci (drops host audio):
echo "$hda" | sudo tee /sys/bus/pci/devices/$hda/driver/unbind
vd=$(lspci -ns "$hda" | awk '{print $3}' | tr ':' ' ')   # "1022 15e3"
echo $vd | sudo tee /sys/bus/pci/drivers/vfio-pci/new_id
lspci -nnks "$hda"                                       # expect: Kernel driver in use: vfio-pci
```

## 2. Launch m3OS with the HDA controller passed through

Build the image, then add VFIO passthrough to the `hda-smoke`-style QEMU args
(replace the emulated `-device intel-hda`):

```bash
cargo xtask image
sudo qemu-system-x86_64 \
  ... (the standard m3OS UEFI/OVMF + data-disk args from xtask) ... \
  -device vfio-pci,host=${hda#0000:} \
  -serial stdio
```

(Easiest: temporarily edit `hda_smoke_qemu_args` to emit `-device
vfio-pci,host=<bdf>` instead of `-device intel-hda -device hda-duplex`, then run
`sudo M3OS_HDA_REGRESSION=1 cargo xtask hda-smoke --display` and listen.)

## 3. Confirm bring-up on the serial console

Expect, in order:
- `hda_driver: spawned`
- `hda_driver: controller up, codecs ready` (STATESTS reported the Realtek codec)
- `HDA_SMOKE:server:READY`
- `audio_server` connects to `audio.hw`; `audio-demo` / the greeter chime plays.

Then **listen**: the internal speaker must produce audible, non-silent output.
On a real Realtek codec the EAPD/GPIO amp-enable in `realtek.rs` is what makes
the difference between "stream runs (SDnLPIB advances) but silent" and audible
sound.

## 4. Capture empirical register/codec state

While the guest is up (or via a one-shot enumeration build), record into
`docs/research/hda-realtek-capture.md`:
- `GCAP` (OSS/ISS/BSS), the codec address(es) from `STATESTS`.
- The enumerated widget list + the selected pin→DAC path.
- The Realtek codec vendor/device + the pin-default config words.
- Whether EAPD alone sufficed or the GPIO-EAPD fallback / a COEF write was
  needed for audible output (this is the data that would seed any future
  board-specific COEF handling).

## 5. Restore host audio

```bash
echo "$hda" | sudo tee /sys/bus/pci/drivers/vfio-pci/unbind
echo "$hda" | sudo tee /sys/bus/pci/drivers/snd_hda_intel/bind
# or simply: sudo modprobe -r vfio-pci && sudo alsactl restore
```

## Why this is hardware-only

QEMU's `intel-hda` + `hda-duplex` is a *generic* codec with no EAPD/GPIO amp
gating and a fixed pin config, so the `hda-smoke` CI gate proves the controller
+ generic widget-graph + stream engine but **cannot** exercise the Realtek
amp-enable path. Only physical Realtek silicon (or VFIO passthrough of it) can.
This mirrors the Phase 79 Realtek NIC tracks (`scripts/r8125-vfio-validate.md`).
