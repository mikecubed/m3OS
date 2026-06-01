//! HDA host-controller bring-up — Phase 80b Track B.1 / B.2.
//!
//! Reset (`GCTL.CRST`) → STATESTS codec-ready poll → `GCAP` decode → CORB/RIRB
//! bring-up. Issuing a verb before the codec-ready poll returns garbage (the
//! #1 first-driver pitfall), so the STATESTS poll gates everything.

use crate::corb::CorbRirb;
use alloc::vec::Vec;
use driver_runtime::{DeviceHandle, Mmio};
use kernel_core::hda::{self, regs};

const POLL_BUDGET: u32 = 2_000_000;

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
        Self::reset(&mmio)?;
        let statests = Self::wait_codecs(&mmio)?;
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

    /// `GCTL.CRST` reset: clear → poll read-0 → set → poll read-1.
    fn reset(mmio: &Mmio<u8>) -> Result<(), &'static str> {
        let gctl = mmio.read_reg::<u32>(hda::REG_GCTL);
        mmio.write_reg::<u32>(hda::REG_GCTL, gctl & !hda::GCTL_CRST);
        for _ in 0..POLL_BUDGET {
            if regs::crst_deasserted(mmio.read_reg::<u32>(hda::REG_GCTL)) {
                break;
            }
        }
        mmio.write_reg::<u32>(hda::REG_GCTL, hda::GCTL_CRST);
        for _ in 0..POLL_BUDGET {
            if regs::reset_ready(mmio.read_reg::<u32>(hda::REG_GCTL)) {
                return Ok(());
            }
        }
        Err("GCTL.CRST reset timeout")
    }

    /// Poll `STATESTS` until at least one codec reports in (bounded bailout).
    fn wait_codecs(mmio: &Mmio<u8>) -> Result<u16, &'static str> {
        for _ in 0..POLL_BUDGET {
            let s = mmio.read_reg::<u16>(hda::REG_STATESTS) & 0x7FFF;
            if s != 0 {
                return Ok(s);
            }
        }
        Err("STATESTS codec-ready timeout")
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
}
