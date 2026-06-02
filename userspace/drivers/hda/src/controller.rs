//! HDA host-controller bring-up — Phase 80b Track B.1 / B.2.
//!
//! Reset (`GCTL.CRST`) → STATESTS codec-ready poll → `GCAP` decode → CORB/RIRB
//! bring-up. Issuing a verb before the codec-ready poll returns garbage (the
//! #1 first-driver pitfall), so the STATESTS poll gates everything.

use crate::corb::CorbRirb;
use alloc::vec::Vec;
use driver_runtime::{DeviceCapKey, DeviceHandle, Mmio, pci_config_read, pci_config_write};
use kernel_core::device_host::pci_pm;
use kernel_core::hda::{self, amd, irq, regs};

const POLL_BUDGET: u32 = 2_000_000;

/// Log `label` followed by `val` as `0x........` + newline (no_std, no alloc).
fn dbg_hex(label: &str, val: u32) {
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, label);
    let mut buf = [b'0', b'x', 0, 0, 0, 0, 0, 0, 0, 0];
    for i in 0..8 {
        let nib = ((val >> ((7 - i) * 4)) & 0xF) as u8;
        buf[2 + i] = if nib < 10 {
            b'0' + nib
        } else {
            b'a' + (nib - 10)
        };
    }
    if let Ok(s) = core::str::from_utf8(&buf) {
        syscall_lib::write_str(syscall_lib::STDOUT_FILENO, s);
    }
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "\n");
}

/// The brought-up HDA controller: BAR0 MMIO window, the CORB/RIRB rings, the
/// decoded `GCAP`, the chosen output stream-descriptor index, and the codec
/// addresses reported by STATESTS.
pub struct HdaController {
    pub mmio: Mmio<u8>,
    pub rings: CorbRirb,
    pub gcap: regs::GcapInfo,
    pub output_stream_index: usize,
    pub codecs: Vec<u8>,
}

impl HdaController {
    /// Bring the controller from cold reset to "rings running, codecs known".
    ///
    /// `key` is the controller's BDF (for config-space programming) and
    /// `vendor` its PCI vendor ID (to gate the AMD snoop quirk). Both come from
    /// the caller's pre-claim probe.
    pub fn bring_up(
        device: &DeviceHandle,
        key: DeviceCapKey,
        vendor: u16,
        mmio: Mmio<u8>,
    ) -> Result<Self, &'static str> {
        // Vendor config-space programming the generic register path can't do:
        // force PCI power state D0 (a VFIO host may have left the controller —
        // and its internal codec block — in D3, which keeps the codec out of
        // STATESTS), and enable the AMD/ATI snoop bit for coherent DMA. Mirrors
        // Linux `snd_hda_intel` `azx_init_pci`, run before the link reset.
        Self::power_up_and_quirk(key, vendor);

        // A real codec — especially after a VFIO/FLR reset that took it away
        // from a prior OS owner (Linux `snd_hda_intel`) — may need several full
        // reset cycles + a long enumeration window before it reports in
        // STATESTS. Mirror Linux `azx_reset`'s retry loop; QEMU's emulated
        // codec reports on the first try.
        let mut statests = 0u16;
        for _ in 0..2 {
            Self::reset(&mmio)?;
            statests = Self::wait_codecs(&mmio);
            if statests != 0 {
                break;
            }
        }
        if statests == 0 {
            dbg_hex(
                "hda_driver: GCAP=",
                u32::from(mmio.read_reg::<u16>(hda::REG_GCAP)),
            );
            dbg_hex("hda_driver: STATESTS=", u32::from(statests));
            dbg_hex("hda_driver: GCTL=", mmio.read_reg::<u32>(hda::REG_GCTL));
            return Err("no codecs reported in STATESTS");
        }
        let codecs: Vec<u8> = regs::codecs_from_statests(statests).collect();
        if codecs.is_empty() {
            return Err("no codecs reported in STATESTS");
        }

        let gcap = regs::decode_gcap(mmio.read_reg::<u16>(hda::REG_GCAP));
        let output_stream_index = regs::output_stream_descriptor_index(&gcap);
        if !regs::output_index_valid(&gcap, output_stream_index) {
            return Err("controller reports no output stream");
        }

        let mut rings = CorbRirb::new(device)?;
        rings.program(&mmio)?;

        Ok(Self {
            mmio,
            rings,
            gcap,
            output_stream_index,
            codecs,
        })
    }

    /// Force the controller to PCI power state **D0** and apply the AMD/ATI
    /// snoop quirk via the new claim-gated config-space write syscall, before
    /// the link reset. Best-effort: each config-space access is independently
    /// fault-tolerant — a failure is logged-by-omission and bring-up proceeds,
    /// since on QEMU (already D0, non-AMD) every step is a no-op anyway.
    fn power_up_and_quirk(key: DeviceCapKey, vendor: u16) {
        // 1. Ensure D0. Under VFIO the host's runtime-PM may have suspended the
        //    function; resetting a D3 controller leaves its codec dark. Walk to
        //    the PM capability and clear the PMCSR power-state field if set.
        if let Ok(status) = pci_config_read(key, pci_pm::PCI_STATUS_REG, 2) {
            let read_u8 = |off: u8| {
                pci_config_read(key, u16::from(off), 1)
                    .ok()
                    .map(|v| v as u8)
            };
            if let Some(pm_cap) =
                pci_pm::find_capability(status as u16, read_u8, pci_pm::PCI_CAP_ID_PM)
            {
                let pmcsr_off = pci_pm::pmcsr_offset(pm_cap);
                if let Ok(pmcsr) = pci_config_read(key, pmcsr_off, 2) {
                    let state = pci_pm::pm_power_state(pmcsr as u16);
                    if state != 0 {
                        let d0 = pci_pm::pmcsr_force_d0(pmcsr as u16);
                        let _ = pci_config_write(key, pmcsr_off, 2, u32::from(d0));
                        // PCI PM spec: ≤10 ms recovery for a D3hot→D0 transition.
                        let _ = syscall_lib::nanosleep_for(0, 10_000_000);
                        dbg_hex(
                            "hda_driver: forced PCI power state D0, was D",
                            u32::from(state),
                        );
                    }
                }
            }
        }

        // 2. AMD/ATI snoop enable (coherent DMA). NOT an enumeration fix — the
        //    codec still appears without it; this prevents garbled playback once
        //    it does. Skipped for non-AMD controllers (QEMU intel-hda).
        if amd::is_amd_controller(vendor) {
            if let Ok(cur) = pci_config_read(key, u16::from(amd::ATI_SNOOP_REG), 1) {
                let patched = amd::ati_snoop_rmw(cur as u8);
                let _ = pci_config_write(key, u16::from(amd::ATI_SNOOP_REG), 1, u32::from(patched));
                dbg_hex("hda_driver: AMD snoop (cfg 0x42) <- ", u32::from(patched));
            }
        }
    }

    /// `GCTL.CRST` reset: clear stale STATESTS → clear CRST → poll read-0 →
    /// in-reset PLL-settle delay → set CRST → poll read-1 → wait for codec
    /// self-enumeration.
    fn reset(mmio: &Mmio<u8>) -> Result<(), &'static str> {
        // Clear any stale STATESTS bits before reset so a fresh codec wake is
        // detectable afterward (Redox `ihdad` does this; a real codec may not
        // re-set an already-set bit). STATESTS is write-1-to-clear.
        mmio.write_reg::<u16>(hda::REG_STATESTS, 0x7FFF);

        let gctl = mmio.read_reg::<u32>(hda::REG_GCTL);
        mmio.write_reg::<u32>(hda::REG_GCTL, gctl & !hda::GCTL_CRST);
        for _ in 0..POLL_BUDGET {
            if regs::crst_deasserted(mmio.read_reg::<u32>(hda::REG_GCTL)) {
                break;
            }
        }
        // HDA spec Rev 0.9 §5.5.1: hold reset asserted ≥100 µs so the codec PLL
        // settles before bringing the link back up. Linux `snd_hdac_bus_enter_
        // link_reset` waits 500–1000 µs here; the driver previously deasserted
        // immediately, which on real silicon can leave the codec un-clocked and
        // absent from STATESTS even though QEMU's instant codec still reports.
        let _ = syscall_lib::nanosleep_for(0, 600_000); // 600 µs
        mmio.write_reg::<u32>(hda::REG_GCTL, hda::GCTL_CRST);
        let mut up = false;
        for _ in 0..POLL_BUDGET {
            if regs::reset_ready(mmio.read_reg::<u32>(hda::REG_GCTL)) {
                up = true;
                break;
            }
        }
        if !up {
            return Err("GCTL.CRST reset timeout");
        }
        // HDA spec §3.3.7: codecs need ≥521 µs (25 frames) after CRST is
        // deasserted before STATESTS reflects their presence. A poll-only loop
        // can race ahead on a fast MMIO path (and real silicon needs the full
        // window), so wait explicitly before polling STATESTS.
        let _ = syscall_lib::nanosleep_for(0, 2_000_000); // 2 ms, generous
        Ok(())
    }

    /// Poll `STATESTS` over a real wall-clock window (1 ms between reads) until
    /// at least one codec reports in; returns the codec mask, or `0` if none
    /// appeared within ~4 s. The caller retries the full reset on `0`. (QEMU's
    /// emulated codec reports in <1 ms; real silicon needs the wall-clock wait.)
    fn wait_codecs(mmio: &Mmio<u8>) -> u16 {
        const STATESTS_POLL_MS: u32 = 4000;
        for _ in 0..STATESTS_POLL_MS {
            let s = mmio.read_reg::<u16>(hda::REG_STATESTS) & 0x7FFF;
            if s != 0 {
                return s;
            }
            let _ = syscall_lib::nanosleep_for(0, 1_000_000); // 1 ms
        }
        0
    }

    /// Issue a 12-bit-verb command to a codec node, returning the response.
    pub fn command(&mut self, codec: u8, nid: u8, verb12: u32, payload: u8) -> Option<u32> {
        self.rings.command(&self.mmio, codec, nid, verb12, payload)
    }

    /// Issue a 4-bit-verb command (`SET_STREAM_FORMAT`/`SET_AMP_GAIN_MUTE`).
    pub fn command4(&mut self, codec: u8, nid: u8, verb4: u32, payload: u16) -> Option<u32> {
        self.rings.command4(&self.mmio, codec, nid, verb4, payload)
    }

    /// `GET_PARAMETER(param)` convenience.
    pub fn get_parameter(&mut self, codec: u8, nid: u8, param: u32) -> Option<u32> {
        self.command(codec, nid, hda::VERB_GET_PARAMETER, param as u8)
    }

    /// Issue a pre-encoded command dword (Realtek verb sequences are built as
    /// full command dwords in `kernel_core::hda::realtek`).
    pub fn raw_command(&mut self, dword: u32) -> Option<u32> {
        self.rings.command_raw(&self.mmio, dword)
    }

    /// Arm controller interrupts (C.3): global interrupt enable + the output
    /// stream's per-stream interrupt-enable bit in `INTCTL`. The stream's
    /// `SDnCTL.IOCE` (set at configure) + the BDL IOC flags then fire a
    /// `BCIS` interrupt on each completed buffer.
    pub fn arm_interrupts(&self) {
        let intctl = hda::INTCTL_GIE | (1u32 << self.output_stream_index);
        self.mmio.write_reg::<u32>(hda::REG_INTCTL, intctl);
    }

    /// Service an interrupt: decode `INTSTS`, and if our output stream fired,
    /// clear its `SDnSTS.BCIS` (write-1-to-clear) so the interrupt does not
    /// re-assert forever. Returns `true` if the output stream's IRQ was
    /// handled. Uses the host-tested [`irq`] decode.
    pub fn handle_irq(&self) -> bool {
        let intsts = self.mmio.read_reg::<u32>(hda::REG_INTSTS);
        if irq::stream_fired(intsts, self.output_stream_index) {
            let sts = hda::stream_desc_offset(self.output_stream_index) + hda::SD_STS;
            self.mmio.write_reg::<u8>(sts, irq::bcis_clear_value());
            true
        } else {
            false
        }
    }
}
