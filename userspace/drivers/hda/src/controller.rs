//! HDA host-controller bring-up — Phase 80b Track B.1 / B.2.
//!
//! Reset (`GCTL.CRST`) → STATESTS codec-ready poll → `GCAP` decode → CORB/RIRB
//! bring-up. Issuing a verb before the codec-ready poll returns garbage (the
//! #1 first-driver pitfall), so the STATESTS poll gates everything.

use crate::corb::CorbRirb;
use alloc::vec::Vec;
use driver_runtime::{DeviceHandle, Mmio};
use kernel_core::hda::{self, irq, regs};

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
    pub fn bring_up(device: &DeviceHandle, mmio: Mmio<u8>) -> Result<Self, &'static str> {
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

    /// `GCTL.CRST` reset: clear stale STATESTS → clear CRST → poll read-0 → set
    /// CRST → poll read-1 → wait for codec self-enumeration.
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
