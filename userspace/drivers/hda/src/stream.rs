//! HDA output stream engine — Phase 80b Track C.2.
//!
//! One output stream descriptor (block `0x80 + n*0x20`) driving a cyclic PCM
//! DMA buffer described by a BDL. Configure cycles `SDnCTL.SRST`, programs the
//! BDL IOVA / `SDnCBL` / `SDnLVI` / `SDnFMT`, writes the 4-bit stream tag into
//! `SDnCTL[23:20]`, then sets `SDnCTL.RUN` **last** so the DMA engine starts.
//! `SDnLPIB` is polled for the consumed position (the DMA-position-buffer is
//! deferred — Redox does the same).

use crate::controller::HdaController;
use alloc::vec::Vec;
use driver_runtime::{DeviceHandle, DmaBuffer, Mmio};
use kernel_core::hda::{self, fmt};

/// Cyclic PCM DMA buffer size (bytes). Holds ≥ one `SubmitFrames` window.
pub const PCM_BUF_BYTES: usize = 64 * 1024;
/// Per-BDL-entry chunk size (128-byte aligned, as `build_bdl` requires).
const CHUNK: usize = 4096;
/// Number of BDL entries covering the cyclic buffer.
const N_BDL: usize = PCM_BUF_BYTES / CHUNK;
const POLL_BUDGET: u32 = 200_000;

/// The output stream: cyclic PCM buffer + BDL + position tracking.
pub struct OutputStream {
    pcm: DmaBuffer<[u8; PCM_BUF_BYTES]>,
    bdl: DmaBuffer<[fmt::BdlEntry; N_BDL]>,
    index: usize,
    tag: u8,
    write_cursor: usize,
    last_lpib: u32,
    total_consumed: u64,
    total_submitted: u64,
}

impl OutputStream {
    /// Allocate the cyclic PCM buffer + BDL in the driver's IOMMU domain.
    pub fn new(device: &DeviceHandle, index: usize, tag: u8) -> Result<Self, &'static str> {
        let pcm = DmaBuffer::<[u8; PCM_BUF_BYTES]>::allocate(device, PCM_BUF_BYTES, 128)
            .map_err(|_| "PCM DMA alloc failed")?;
        let bdl_bytes = N_BDL * core::mem::size_of::<fmt::BdlEntry>();
        let bdl = DmaBuffer::<[fmt::BdlEntry; N_BDL]>::allocate(device, bdl_bytes, 128)
            .map_err(|_| "BDL DMA alloc failed")?;
        Ok(Self {
            pcm,
            bdl,
            index,
            tag,
            write_cursor: 0,
            last_lpib: 0,
            total_consumed: 0,
            total_submitted: 0,
        })
    }

    fn sd(&self, reg: usize) -> usize {
        hda::stream_desc_offset(self.index) + reg
    }

    /// Program the stream descriptor: SRST cycle → BDL/CBL/LVI/FMT → tag + RUN.
    pub fn configure(&mut self, mmio: &Mmio<u8>, sdnfmt: u16) -> Result<(), &'static str> {
        // Build the BDL over the whole cyclic buffer and copy it into the
        // BDL DMA region the controller reads via SDnBDPL/U.
        let entries: Vec<fmt::BdlEntry> =
            fmt::build_bdl(self.pcm.iova(), PCM_BUF_BYTES as u32, CHUNK as u32);
        // `entries.len() == N_BDL` (PCM_BUF_BYTES / CHUNK); write through the
        // DmaBuffer's safe DerefMut into the device-visible BDL region.
        {
            let dst = &mut self.bdl[..];
            for (i, e) in entries.iter().enumerate() {
                dst[i] = *e;
            }
        }
        let cbl = fmt::bdl_cbl(&entries);
        let lvi = fmt::bdl_lvi(&entries);

        let ctl = self.sd(hda::SD_CTL);
        // SRST reset: set → read-1 → clear → read-0.
        mmio.write_reg::<u32>(ctl, mmio.read_reg::<u32>(ctl) | hda::SDCTL_SRST);
        for _ in 0..POLL_BUDGET {
            if mmio.read_reg::<u32>(ctl) & hda::SDCTL_SRST != 0 {
                break;
            }
        }
        mmio.write_reg::<u32>(ctl, mmio.read_reg::<u32>(ctl) & !hda::SDCTL_SRST);
        for _ in 0..POLL_BUDGET {
            if mmio.read_reg::<u32>(ctl) & hda::SDCTL_SRST == 0 {
                break;
            }
        }

        let bdl_iova = self.bdl.iova();
        mmio.write_reg::<u32>(self.sd(hda::SD_BDPL), (bdl_iova & 0xFFFF_FFFF) as u32);
        mmio.write_reg::<u32>(self.sd(hda::SD_BDPU), (bdl_iova >> 32) as u32);
        mmio.write_reg::<u32>(self.sd(hda::SD_CBL), cbl);
        mmio.write_reg::<u16>(self.sd(hda::SD_LVI), lvi);
        mmio.write_reg::<u16>(self.sd(hda::SD_FMT), sdnfmt);

        // Stream tag into SDnCTL[23:20], then RUN (+ IOCE) last.
        let tagged =
            ((self.tag as u32) << hda::SDCTL_STREAM_TAG_SHIFT) | hda::SDCTL_RUN | hda::SDCTL_IOCE;
        mmio.write_reg::<u32>(ctl, tagged);
        if mmio.read_reg::<u32>(ctl) & hda::SDCTL_RUN == 0 {
            return Err("SDnCTL.RUN did not set");
        }

        self.write_cursor = 0;
        self.last_lpib = 0;
        self.total_consumed = 0;
        self.total_submitted = 0;
        Ok(())
    }

    /// Read `SDnLPIB` and fold the delta (handling wrap at `SDnCBL`) into the
    /// running consumed-bytes counter.
    pub fn poll_consumed(&mut self, mmio: &Mmio<u8>) -> u64 {
        let lpib = mmio.read_reg::<u32>(self.sd(hda::SD_LPIB));
        let cbl = PCM_BUF_BYTES as u32;
        let lpib = lpib % cbl;
        let delta = if lpib >= self.last_lpib {
            lpib - self.last_lpib
        } else {
            cbl - self.last_lpib + lpib
        };
        self.total_consumed = self.total_consumed.wrapping_add(u64::from(delta));
        self.last_lpib = lpib;
        self.total_consumed
    }

    /// Copy `pcm` into the cyclic buffer ahead of the DMA read position.
    /// Returns `false` (WouldBlock) when the unplayed backlog would overflow
    /// the buffer — preserving the all-or-nothing submit contract; `true` when
    /// the whole submission was accepted.
    pub fn submit(&mut self, mmio: &Mmio<u8>, pcm: &[u8]) -> bool {
        self.poll_consumed(mmio);
        if pcm.len() > PCM_BUF_BYTES {
            return false;
        }
        let in_flight = self.total_submitted.saturating_sub(self.total_consumed) as usize;
        if in_flight + pcm.len() > PCM_BUF_BYTES {
            return false;
        }
        let mut off = self.write_cursor % PCM_BUF_BYTES;
        {
            let buf = &mut self.pcm[..];
            for &b in pcm {
                buf[off] = b;
                off = (off + 1) % PCM_BUF_BYTES;
            }
        }
        self.write_cursor = off;
        self.total_submitted = self.total_submitted.wrapping_add(pcm.len() as u64);
        true
    }

    /// Halt the stream DMA engine (clear `SDnCTL.RUN`).
    pub fn stop(&self, mmio: &Mmio<u8>) {
        let ctl = self.sd(hda::SD_CTL);
        mmio.write_reg::<u32>(ctl, mmio.read_reg::<u32>(ctl) & !hda::SDCTL_RUN);
    }
}

/// Open + configure a fresh output stream against `ctrl`'s controller: pick the
/// codec output path, then bring the stream descriptor to RUN. Returns the
/// stream + the configured path.
pub fn open_output(
    device: &DeviceHandle,
    ctrl: &mut HdaController,
    tag: u8,
) -> Result<(OutputStream, crate::codec::OutputPath), &'static str> {
    let sdnfmt = fmt::encode_sdnfmt(48_000, 16, 2);
    // Configure the codec converter/amps/pin for this format + tag first.
    let path = crate::codec::configure_output(ctrl, sdnfmt, tag)?;
    // Then bring up the controller-side stream descriptor.
    let mut stream = OutputStream::new(device, ctrl.output_stream_index, tag)?;
    stream.configure(&ctrl.mmio, sdnfmt)?;
    Ok((stream, path))
}
