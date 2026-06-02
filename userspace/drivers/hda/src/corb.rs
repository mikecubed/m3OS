//! CORB/RIRB verb DMA rings + immediate-command fallback — Phase 80b Track B.3.
//!
//! HDA replaces AC'97's register-poke model with a pair of DMA rings: CORB
//! (host→codec, 32-bit verbs) and RIRB (codec→host, 64-bit responses). Each
//! ring is a `DmaBuffer` whose **IOVA** (not a host-physical address — the
//! Redox `ihdad` difference) is programmed into the controller's base
//! registers. Both DMA engines must be explicitly RUN-enabled
//! (`CORBCTL.CORBRUN`, `RIRBCTL.RIRBDMAEN`) *after* sizing + pointer reset or
//! no verb transfers. A single-verb immediate-command path (ICOI/IRII/ICS) is
//! the reliability fallback Redox branches on per-emulator.

use driver_runtime::{DeviceHandle, DmaBuffer, Mmio};
use kernel_core::hda::{self, verb};

/// Ring entry count (256-entry configuration).
pub const RING_ENTRIES: usize = hda::RING_ENTRIES_256;
/// 128-byte alignment for the CORB/RIRB bases (`CORBLBASE` low 7 bits reserved).
const RING_ALIGN: usize = 128;
/// Bounded poll budget for response/handshake waits.
const POLL_BUDGET: u32 = 200_000;

/// CORB/RIRB ring pair + the immediate-command fallback.
pub struct CorbRirb {
    corb: DmaBuffer<[u32; RING_ENTRIES]>,
    rirb: DmaBuffer<[u64; RING_ENTRIES]>,
    corb_wp: u16,
    rirb_rp: u16,
}

impl CorbRirb {
    /// Allocate the CORB (1 KiB) and RIRB (2 KiB) DMA rings in the driver's
    /// own IOMMU domain.
    pub fn new(device: &DeviceHandle) -> Result<Self, &'static str> {
        let corb = DmaBuffer::<[u32; RING_ENTRIES]>::allocate(device, RING_ENTRIES * 4, RING_ALIGN)
            .map_err(|_| "CORB DMA alloc failed")?;
        let rirb = DmaBuffer::<[u64; RING_ENTRIES]>::allocate(device, RING_ENTRIES * 8, RING_ALIGN)
            .map_err(|_| "RIRB DMA alloc failed")?;
        Ok(Self {
            corb,
            rirb,
            corb_wp: 0,
            rirb_rp: 0,
        })
    }

    /// Program ring bases (IOVA) + sizes + pointer reset, then RUN-enable the
    /// CORB/RIRB DMA engines **last**. Returns `Err` if the RUN bits do not
    /// read back set.
    pub fn program(&mut self, mmio: &Mmio<u8>) -> Result<(), &'static str> {
        // Stop both engines before reprogramming.
        mmio.write_reg::<u8>(hda::REG_CORBCTL, 0);
        mmio.write_reg::<u8>(hda::REG_RIRBCTL, 0);

        // CORB base IOVA + size (256 entries) + write-pointer zero.
        let corb_iova = self.corb.iova();
        mmio.write_reg::<u32>(hda::REG_CORBLBASE, (corb_iova & 0xFFFF_FFFF) as u32);
        mmio.write_reg::<u32>(hda::REG_CORBUBASE, (corb_iova >> 32) as u32);
        mmio.write_reg::<u8>(hda::REG_CORBSIZE, hda::RING_SIZE_256);
        mmio.write_reg::<u16>(hda::REG_CORBWP, 0);
        self.corb_wp = 0;

        // CORBRP reset handshake: set CORBRPRST → read-1 → clear → read-0. A
        // controller that never acknowledges the reset would leave the ring in
        // an unknown state, so fail bring-up here (matching the CORBRUN/RIRBDMAEN
        // readback checks below) rather than continuing silently.
        mmio.write_reg::<u16>(hda::REG_CORBRP, hda::CORBRP_RST);
        if !self.poll(|| verb::corbrp_reset_asserted(mmio.read_reg::<u16>(hda::REG_CORBRP))) {
            return Err("CORBRP reset did not assert");
        }
        mmio.write_reg::<u16>(hda::REG_CORBRP, 0);
        if !self.poll(|| verb::corbrp_reset_cleared(mmio.read_reg::<u16>(hda::REG_CORBRP))) {
            return Err("CORBRP reset did not clear");
        }

        // RIRB base IOVA + size + write-pointer reset.
        let rirb_iova = self.rirb.iova();
        mmio.write_reg::<u32>(hda::REG_RIRBLBASE, (rirb_iova & 0xFFFF_FFFF) as u32);
        mmio.write_reg::<u32>(hda::REG_RIRBUBASE, (rirb_iova >> 32) as u32);
        mmio.write_reg::<u8>(hda::REG_RIRBSIZE, hda::RING_SIZE_256);
        mmio.write_reg::<u16>(hda::REG_RIRBWP, hda::RIRBWP_RST);
        self.rirb_rp = 0;
        // Interrupt after every response (we poll, but keep the engine sane).
        mmio.write_reg::<u16>(hda::REG_RINTCNT, 1);

        // RUN-enable LAST — without these no verb ever transfers.
        mmio.write_reg::<u8>(hda::REG_CORBCTL, hda::CORBCTL_RUN);
        mmio.write_reg::<u8>(hda::REG_RIRBCTL, hda::RIRBCTL_DMAEN);

        if mmio.read_reg::<u8>(hda::REG_CORBCTL) & hda::CORBCTL_RUN == 0 {
            return Err("CORBRUN did not set");
        }
        if mmio.read_reg::<u8>(hda::REG_RIRBCTL) & hda::RIRBCTL_DMAEN == 0 {
            return Err("RIRBDMAEN did not set");
        }
        Ok(())
    }

    /// Send a 12-bit-verb command and return the codec response (low 32 bits),
    /// falling back to the immediate-command interface if the ring path stalls.
    pub fn command(
        &mut self,
        mmio: &Mmio<u8>,
        codec: u8,
        nid: u8,
        verb12: u32,
        payload: u8,
    ) -> Option<u32> {
        let dword = verb::encode_verb12(codec, nid, verb12, payload);
        self.exchange(mmio, dword)
    }

    /// Send a 4-bit-verb command (`SET_STREAM_FORMAT` / `SET_AMP_GAIN_MUTE`,
    /// 16-bit payload) and return the response.
    pub fn command4(
        &mut self,
        mmio: &Mmio<u8>,
        codec: u8,
        nid: u8,
        verb4: u32,
        payload: u16,
    ) -> Option<u32> {
        let dword = verb::encode_verb4(codec, nid, verb4, payload);
        self.exchange(mmio, dword)
    }

    /// Send a pre-encoded command dword (e.g. a Realtek verb sequence built in
    /// `kernel_core::hda::realtek`) and return the response.
    pub fn command_raw(&mut self, mmio: &Mmio<u8>, dword: u32) -> Option<u32> {
        self.exchange(mmio, dword)
    }

    fn exchange(&mut self, mmio: &Mmio<u8>, dword: u32) -> Option<u32> {
        self.send_dword(mmio, dword);
        match self.get_response(mmio) {
            Some(r) => Some(r),
            None => self.immediate_command(mmio, dword),
        }
    }

    fn send_dword(&mut self, mmio: &Mmio<u8>, dword: u32) {
        let next = verb::corb_next_wp(self.corb_wp);
        // `next < RING_ENTRIES` (corb_next_wp wraps mod 256); write through the
        // DmaBuffer's safe DerefMut.
        self.corb[next as usize] = dword;
        mmio.write_reg::<u16>(hda::REG_CORBWP, next);
        self.corb_wp = next;
    }

    fn get_response(&mut self, mmio: &Mmio<u8>) -> Option<u32> {
        for _ in 0..POLL_BUDGET {
            let wp = mmio.read_reg::<u16>(hda::REG_RIRBWP) & 0xFF;
            if wp != self.rirb_rp {
                let next = ((self.rirb_rp as usize) + 1) % RING_ENTRIES;
                // `next < RING_ENTRIES`; read through the DmaBuffer's safe Deref.
                let entry = self.rirb[next];
                self.rirb_rp = next as u16;
                return Some((entry & 0xFFFF_FFFF) as u32);
            }
        }
        None
    }

    /// Immediate-command interface: clear any stale `ICS.IRV`, write `ICOI`, set
    /// `ICS.ICB`, poll `ICS.IRV`, read `IRII`, then clear `IRV` again. The
    /// reliable single-verb fallback.
    fn immediate_command(&self, mmio: &Mmio<u8>, dword: u32) -> Option<u32> {
        // Wait for any in-flight immediate command to finish (ICB self-clears).
        for _ in 0..POLL_BUDGET {
            if mmio.read_reg::<u16>(hda::REG_ICS) & hda::ICS_ICB == 0 {
                break;
            }
        }
        // `IRV` is write-1-to-clear and is NOT cleared by setting `ICB`.
        // Acknowledge any stale Immediate-Result-Valid left by a prior command
        // before issuing this one — otherwise the `IRV` poll below sees the old
        // bit still set and returns the PREVIOUS response from `IRII` without
        // waiting for this command to complete. (Linux `azx_single_send_cmd`
        // clears `IRV` right before issuing for exactly this reason.)
        mmio.write_reg::<u16>(hda::REG_ICS, hda::ICS_IRV);
        mmio.write_reg::<u32>(hda::REG_ICOI, dword);
        mmio.write_reg::<u16>(hda::REG_ICS, hda::ICS_ICB);
        for _ in 0..POLL_BUDGET {
            if mmio.read_reg::<u16>(hda::REG_ICS) & hda::ICS_IRV != 0 {
                let resp = mmio.read_reg::<u32>(hda::REG_IRII);
                // Clear `IRV` (W1C) so the next immediate command starts clean.
                mmio.write_reg::<u16>(hda::REG_ICS, hda::ICS_IRV);
                return Some(resp);
            }
        }
        None
    }

    /// Spin up to `POLL_BUDGET` reads waiting for `ready`; returns whether it
    /// became ready before the budget was exhausted, so callers can fail a
    /// stuck handshake instead of proceeding on an unacknowledged reset.
    fn poll(&self, mut ready: impl FnMut() -> bool) -> bool {
        for _ in 0..POLL_BUDGET {
            if ready() {
                return true;
            }
        }
        false
    }
}
