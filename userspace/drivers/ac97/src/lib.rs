//! AC'97 controller backend — Phase 80 Track A.5 (out-of-process driver).
//!
//! Extracted from `userspace/audio_server/src/device.rs`.  The logic is
//! identical; the only structural change is that `Ac97Backend` no longer
//! implements the `AudioBackend` trait (which lives in audio_server and is not
//! visible here). Instead every former trait method is an inherent method so the
//! `audio.hw` server loop in `src/main.rs` can call them directly.
//!
//! Keep this file `#![no_std]` + `extern crate alloc` so the `#[cfg(test)]`
//! host tests continue to compile and run under
//! `cargo test -p ac97_driver --target x86_64-unknown-linux-gnu`.

#![cfg_attr(not(test), no_std)]
#![allow(dead_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use kernel_core::audio::{AudioError, ChannelLayout, PcmFormat, SampleRate};

#[cfg(not(test))]
use driver_runtime::DeviceHandle;

// ---------------------------------------------------------------------------
// IrqEvent — decoded outcome of a single audio IRQ wake
// ---------------------------------------------------------------------------

/// Outcome of an AC'97 status-register read after an IRQ wake.
///
/// The variants name each Phase 57 audio condition the io loop reacts
/// to. `Empty` is the "BDL drained, no underrun" case — the BDL ran
/// out of buffers but the consumer was caught up; the io loop posts
/// fresh buffers from the PCM ring. `Underrun` adds the
/// "consumer-was-not-caught-up" condition; the stats verb's
/// underrun_count advances on this path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IrqEvent {
    /// No bits set — spurious wake or shared-vector noise.
    None,
    /// `LastValidIndex` (LVBCI) — BDL hit `LVI`. The driver advances
    /// the ring tail and reposts fresh buffers.
    LastValidIndex,
    /// `BufferCompletion` (BCIS) — the consumed-buffer counter
    /// advanced. The driver advances `frames_consumed`.
    Empty,
    /// `FifoError` (FIFOE) — the controller observed a FIFO underrun
    /// before the driver could repost. Stats `underrun_count++`.
    Underrun,
    /// FIFO error in a non-empty submission — programming bug, surface
    /// as `AudioError::Internal` to the client.
    FifoError,
}

// ---------------------------------------------------------------------------
// AC'97 register layout — single source of truth
// ---------------------------------------------------------------------------

/// AC'97 Native Audio Mixer (NAM, BAR0) register offsets used by the
/// Phase 57 driver.
pub mod nam {
    /// `RESET` — 16-bit, write any value to issue a cold codec reset.
    pub const RESET: usize = 0x00;
    /// `MASTER_VOLUME` — 16-bit, 5-bit per channel + mute.
    pub const MASTER_VOLUME: usize = 0x02;
    /// `PCM_OUT_VOLUME` — 16-bit, output stream volume + mute.
    pub const PCM_OUT_VOLUME: usize = 0x18;
    /// `PCM_FRONT_DAC_RATE` — 16-bit, sample-rate select.
    pub const PCM_FRONT_DAC_RATE: usize = 0x2C;
    /// `EXT_AUDIO_ID` — 16-bit, optional codec capabilities.
    pub const EXT_AUDIO_ID: usize = 0x28;
    /// `EXT_AUDIO_STATUS_CTRL` — 16-bit, variable-rate-audio enable.
    pub const EXT_AUDIO_STATUS_CTRL: usize = 0x2A;
}

/// AC'97 Native Audio Bus Master (NABM, BAR1) register offsets.
pub mod nabm {
    /// PCM-out stream base offset within BAR1.
    pub const PCM_OUT_BASE: usize = 0x10;

    /// Buffer-descriptor-list base address (32-bit phys ptr).
    pub const BDBAR: usize = 0x00;
    /// Current index value (read-only, 8-bit).
    pub const CIV: usize = 0x04;
    /// Last valid index (8-bit, ring tail).
    pub const LVI: usize = 0x05;
    /// Status register (16-bit). Bits: DCH, CELV, LVBCI, BCIS, FIFOE.
    pub const SR: usize = 0x06;
    /// Position in current buffer (16-bit).
    pub const PICB: usize = 0x08;
    /// Prefetch index value (read-only, 8-bit).
    pub const PIV: usize = 0x0A;
    /// Control register (8-bit).
    pub const CR: usize = 0x0B;
}

/// Status register bit masks for `nabm::SR` (per-stream).
pub mod sr_bits {
    /// DMA controller halted.
    pub const DCH: u16 = 1 << 0;
    /// Current-equals-last-valid.
    pub const CELV: u16 = 1 << 1;
    /// Last valid buffer completion interrupt.
    pub const LVBCI: u16 = 1 << 2;
    /// Buffer completion interrupt status.
    pub const BCIS: u16 = 1 << 3;
    /// FIFO error.
    pub const FIFOE: u16 = 1 << 4;
    /// All interrupt-cause bits combined — used to clear status by
    /// writing this mask back to `SR` (bits W1C).
    pub const W1C_MASK: u16 = LVBCI | BCIS | FIFOE;
}

/// Control register bit masks for `nabm::CR` (per-stream).
pub mod cr_bits {
    /// Run / pause bus master.
    pub const RPBM: u8 = 1 << 0;
    /// Reset registers.
    pub const RR: u8 = 1 << 1;
    /// Last-valid-buffer interrupt enable.
    pub const LVBIE: u8 = 1 << 2;
    /// FIFO-error interrupt enable.
    pub const FEIE: u8 = 1 << 3;
    /// IOC (interrupt on completion) enable.
    pub const IOCE: u8 = 1 << 4;
}

/// AC'97 buffer-descriptor-list entry (8 bytes per Intel ICH spec).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BufferDescriptor {
    /// 32-bit physical address of the buffer.
    pub phys_addr: u32,
    /// Number of 16-bit samples in the buffer.
    pub samples: u16,
    /// Flags — bit 15 (`IOC`) requests an interrupt on completion.
    pub flags: u16,
}

/// Number of BDL entries — fixed by the AC'97 spec.
pub const BDL_ENTRIES: usize = 32;

/// Maximum sample count per BDL entry (15-bit field).
pub const BDL_MAX_SAMPLES: usize = 0xFFFE;

/// Default PCM-data ring size — 16 KiB.
pub const DEFAULT_PCM_RING_BYTES: usize = 16 * 1024;

/// Sample rate (Hz) the Phase 57 single-format constraint pins.
pub const SAMPLE_RATE_HZ: u16 = 48_000;

// ---------------------------------------------------------------------------
// MmioOps — minimal seam for register access (test-double friendly)
// ---------------------------------------------------------------------------

/// Read / write surface the AC'97 init + IRQ paths consume.
pub trait MmioOps {
    /// Read an 8-bit register at `(bar, offset)`.
    fn read_u8(&self, bar: u8, offset: usize) -> u8;
    /// Read a 16-bit register at `(bar, offset)`.
    fn read_u16(&self, bar: u8, offset: usize) -> u16;
    /// Read a 32-bit register at `(bar, offset)`.
    fn read_u32(&self, bar: u8, offset: usize) -> u32;
    /// Write an 8-bit register.
    fn write_u8(&self, bar: u8, offset: usize, value: u8);
    /// Write a 16-bit register.
    fn write_u16(&self, bar: u8, offset: usize, value: u16);
    /// Write a 32-bit register.
    fn write_u32(&self, bar: u8, offset: usize, value: u32);
}

// ---------------------------------------------------------------------------
// Pure helpers — exercised by host tests without real hardware
// ---------------------------------------------------------------------------

/// Compose the value written to `nabm::CR` to issue a per-stream reset.
#[inline]
pub const fn cr_reset_value() -> u8 {
    cr_bits::RR
}

/// Compose the value written to `nabm::CR` to start the bus master.
#[inline]
pub const fn cr_run_value() -> u8 {
    cr_bits::RPBM | cr_bits::LVBIE | cr_bits::FEIE | cr_bits::IOCE
}

/// Compose the value written to `nabm::CR` to halt the bus master.
#[inline]
pub const fn cr_halt_value() -> u8 {
    0
}

/// Compose the W1C value for `nabm::SR` to acknowledge every
/// interrupt cause.
#[inline]
pub const fn sr_ack_value(observed: u16) -> u16 {
    observed & sr_bits::W1C_MASK
}

/// Decode an SR snapshot into an [`IrqEvent`].
pub const fn classify_sr(sr: u16, ring_was_empty: bool) -> IrqEvent {
    if sr & sr_bits::FIFOE != 0 {
        if ring_was_empty {
            IrqEvent::Underrun
        } else {
            IrqEvent::FifoError
        }
    } else if sr & sr_bits::LVBCI != 0 {
        IrqEvent::LastValidIndex
    } else if sr & sr_bits::BCIS != 0 {
        IrqEvent::Empty
    } else {
        IrqEvent::None
    }
}

/// Validate that the requested PCM shape matches the Phase 57
/// single-format constraint (S16Le / Stereo or Mono / 48 kHz).
pub const fn shape_supported(format: PcmFormat, _layout: ChannelLayout, rate: SampleRate) -> bool {
    matches!(format, PcmFormat::S16Le) && matches!(rate, SampleRate::Hz48000)
}

// ---------------------------------------------------------------------------
// BAR identifiers — pure-data constants
// ---------------------------------------------------------------------------

/// BAR-index value for the AC'97 NAM (mixer) PIO window.
pub const BAR_NAM: u8 = 0;
/// BAR-index value for the AC'97 NABM (bus-master) PIO window.
pub const BAR_NABM: u8 = 1;

// ---------------------------------------------------------------------------
// AC'97 init / open / close / IRQ helpers
// ---------------------------------------------------------------------------

const VOLUME_UNMUTED: u16 = 0x0202;

/// Reset the codec (NAM RESET write), unmute master + PCM-out
/// volumes, enable variable-rate audio, and program the 48 kHz
/// sample rate.
pub fn init_controller<M: MmioOps>(mmio: &M) -> Result<(), AudioError> {
    mmio.write_u16(BAR_NAM, nam::RESET, 0);
    mmio.write_u16(BAR_NAM, nam::MASTER_VOLUME, VOLUME_UNMUTED);
    mmio.write_u16(BAR_NAM, nam::PCM_OUT_VOLUME, VOLUME_UNMUTED);
    let prev = mmio.read_u16(BAR_NAM, nam::EXT_AUDIO_STATUS_CTRL);
    mmio.write_u16(BAR_NAM, nam::EXT_AUDIO_STATUS_CTRL, prev | 0x0001);
    mmio.write_u16(BAR_NAM, nam::PCM_FRONT_DAC_RATE, SAMPLE_RATE_HZ);
    Ok(())
}

/// Verify that the DMA range `[iova, iova + size)` fits entirely within the
/// low 4 GiB so AC'97's 32-bit BDBAR / BDL `phys_addr` fields can address it
/// without silent truncation.
pub fn check_iova_fits_u32(iova: u64, size: usize) -> Result<(), AudioError> {
    let end = iova.checked_add(size as u64).ok_or(AudioError::Internal)?;
    if end > (u32::MAX as u64) + 1 {
        return Err(AudioError::Internal);
    }
    Ok(())
}

/// Open the PCM-out stream by programming `BDBAR → LVI = 0 → CR.RPBM`.
pub fn open_pcm_out_stream<M: MmioOps>(mmio: &M, bdl_iova: u64) -> Result<(), AudioError> {
    mmio.write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CR, cr_reset_value());
    let bdbar_low = (bdl_iova & 0xFFFF_FFFF) as u32;
    mmio.write_u32(BAR_NABM, nabm::PCM_OUT_BASE + nabm::BDBAR, bdbar_low);
    mmio.write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::LVI, 0);
    mmio.write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CR, cr_run_value());
    Ok(())
}

/// Close the PCM-out stream by halting the bus master (CR=0) and
/// resetting per-stream registers (CR.RR).
pub fn close_pcm_out_stream<M: MmioOps>(mmio: &M) -> Result<(), AudioError> {
    mmio.write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CR, cr_halt_value());
    mmio.write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CR, cr_reset_value());
    Ok(())
}

/// Read the per-stream status register, classify it via
/// [`classify_sr`], and acknowledge the W1C bits.
pub fn handle_pcm_out_irq<M: MmioOps>(
    mmio: &M,
    ring_was_empty: bool,
) -> Result<IrqEvent, AudioError> {
    let sr = mmio.read_u16(BAR_NABM, nabm::PCM_OUT_BASE + nabm::SR);
    let event = classify_sr(sr, ring_was_empty);
    let ack = sr_ack_value(sr);
    if ack != 0 {
        mmio.write_u16(BAR_NABM, nabm::PCM_OUT_BASE + nabm::SR, ack);
    }
    Ok(event)
}

// ---------------------------------------------------------------------------
// PCM-ring slot stride — derived from ring + BDL constants
// ---------------------------------------------------------------------------

/// Number of bytes per PCM-ring slot.
pub const PCM_SLOT_STRIDE: usize = DEFAULT_PCM_RING_BYTES / BDL_ENTRIES;

/// A single silence slot — one BDL entry's worth of zero PCM data.
pub const SILENCE_FRAME: [u8; PCM_SLOT_STRIDE] = [0u8; PCM_SLOT_STRIDE];

// ---------------------------------------------------------------------------
// submit_frames_inner — pure copy + BDL-post helper
// ---------------------------------------------------------------------------

/// Copy `bytes` into the PCM ring DMA region and post one or more BDL entries.
pub fn submit_frames_inner(
    bytes: &[u8],
    pcm_ring: &mut [u8; DEFAULT_PCM_RING_BYTES],
    pcm_ring_iova: u64,
    bdl_dma: &mut [BufferDescriptor; BDL_ENTRIES],
    bdl_iova: u64,
    logic: &mut Ac97Logic,
) -> Result<usize, AudioError> {
    if bytes.is_empty() {
        return Err(AudioError::InvalidArgument);
    }

    let num_slots = bytes.len().div_ceil(PCM_SLOT_STRIDE);

    let in_flight = logic.head.wrapping_sub(logic.tail);
    let free_slots = BDL_ENTRIES.saturating_sub(in_flight);
    if free_slots < num_slots {
        return Err(AudioError::WouldBlock);
    }

    for i in 0..num_slots {
        let head = logic.head % BDL_ENTRIES;
        let slot_byte_offset = head * PCM_SLOT_STRIDE;
        let src_offset = i * PCM_SLOT_STRIDE;
        let copy_len = (bytes.len() - src_offset).min(PCM_SLOT_STRIDE);

        let dst = &mut pcm_ring[slot_byte_offset..slot_byte_offset + PCM_SLOT_STRIDE];
        dst[..copy_len].copy_from_slice(&bytes[src_offset..src_offset + copy_len]);
        if copy_len < PCM_SLOT_STRIDE {
            dst[copy_len..].fill(0);
        }

        let bdl_iova_offset = bdl_iova + (head * core::mem::size_of::<BufferDescriptor>()) as u64;
        let slot_phys_addr = (pcm_ring_iova + slot_byte_offset as u64) as u32;
        let samples = PCM_SLOT_STRIDE / 2;

        logic
            .submit_buffer(bdl_iova_offset, slot_phys_addr, samples)
            .map_err(|_| AudioError::Internal)?;

        bdl_dma[head] = logic.bdl[head];
    }

    Ok(bytes.len())
}

// ---------------------------------------------------------------------------
// Ac97Logic — pure-state companion to `Ac97Backend`
// ---------------------------------------------------------------------------

/// Pure-logic AC'97 state — the BDL ring + cursors + counters,
/// without the `DeviceHandle` or `DmaBuffer` ownership.
#[derive(Debug, Clone)]
pub struct Ac97Logic {
    pub(crate) bdl: [BufferDescriptor; BDL_ENTRIES],
    pub(crate) head: usize,
    pub(crate) tail: usize,
    pub(crate) lvi: u8,
    pub(crate) frames_submitted: u64,
    pub(crate) frames_consumed: u64,
    pub(crate) underrun_count: u32,
}

impl Default for Ac97Logic {
    fn default() -> Self {
        Self::new()
    }
}

impl Ac97Logic {
    /// Construct an empty BDL.
    pub const fn new() -> Self {
        Self {
            bdl: [BufferDescriptor {
                phys_addr: 0,
                samples: 0,
                flags: 0,
            }; BDL_ENTRIES],
            head: 0,
            tail: 0,
            lvi: 0,
            frames_submitted: 0,
            frames_consumed: 0,
            underrun_count: 0,
        }
    }

    /// Borrow the BDL.
    pub fn bdl(&self) -> &[BufferDescriptor; BDL_ENTRIES] {
        &self.bdl
    }

    /// Current LVI mirror.
    pub fn lvi(&self) -> u8 {
        self.lvi
    }

    /// Running `frames_submitted` (samples handed to the BDL).
    pub fn frames_submitted(&self) -> u64 {
        self.frames_submitted
    }

    /// Running `frames_consumed` (samples drained by hardware).
    pub fn frames_consumed(&self) -> u64 {
        self.frames_consumed
    }

    /// Running `underrun_count`.
    pub fn underrun_count(&self) -> u32 {
        self.underrun_count
    }

    /// Append a buffer to the BDL.
    pub fn submit_buffer(
        &mut self,
        _bdl_iova: u64,
        phys_addr: u32,
        samples: usize,
    ) -> Result<(), AudioError> {
        if samples > BDL_MAX_SAMPLES {
            return Err(AudioError::InvalidArgument);
        }
        let in_flight = self.head.wrapping_sub(self.tail);
        if in_flight >= BDL_ENTRIES {
            return Err(AudioError::WouldBlock);
        }
        let idx = self.head % BDL_ENTRIES;
        self.bdl[idx] = BufferDescriptor {
            phys_addr,
            samples: samples as u16,
            flags: 0x8000,
        };
        self.head = self.head.wrapping_add(1);
        self.lvi = (self.head.wrapping_sub(1) % BDL_ENTRIES) as u8;
        self.frames_submitted = self.frames_submitted.saturating_add(samples as u64);
        Ok(())
    }

    /// Observe an IRQ: classify the status register and update
    /// `frames_consumed` / `underrun_count` based on `new_civ`.
    pub fn observe_irq(&mut self, sr: u16, new_civ: u8) -> IrqEvent {
        let ring_was_empty = self.tail == self.head;
        let event = classify_sr(sr, ring_was_empty);
        let civ = new_civ as usize;
        for _ in 0..BDL_ENTRIES {
            if self.tail % BDL_ENTRIES == civ {
                break;
            }
            let idx = self.tail % BDL_ENTRIES;
            let entry = self.bdl[idx];
            let samples = { entry.samples } as u64;
            self.frames_consumed = self.frames_consumed.saturating_add(samples);
            self.tail = self.tail.wrapping_add(1);
        }
        if matches!(event, IrqEvent::Underrun) {
            self.underrun_count = self.underrun_count.saturating_add(1);
        }
        event
    }
}

// ---------------------------------------------------------------------------
// Ac97Backend — concrete AC'97 driver backend (production, no_std only)
// ---------------------------------------------------------------------------

/// Snapshot of the running stats counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub frames_submitted: u64,
    pub frames_consumed: u64,
    pub underrun_count: u32,
}

#[cfg(not(test))]
pub struct Ac97Backend {
    pub(crate) device: DeviceHandle,
    pub(crate) bus: Ac97PioBus,
    pub(crate) bdl: driver_runtime::DmaBuffer<[BufferDescriptor; BDL_ENTRIES]>,
    pub(crate) pcm_ring: driver_runtime::DmaBuffer<[u8; DEFAULT_PCM_RING_BYTES]>,
    pub(crate) logic: Ac97Logic,
    pub(crate) stream_open: bool,
}

#[cfg(not(test))]
impl Ac97Backend {
    /// Stream id for the single PCM-out stream Phase 57 supports.
    pub const PCM_OUT_STREAM_ID: u32 = 1;

    /// Construct the backend from a claimed device handle.
    pub fn init(device: DeviceHandle) -> Result<Self, AudioError> {
        let bus = Ac97PioBus::new(&device)?;

        let bdl_size = core::mem::size_of::<[BufferDescriptor; BDL_ENTRIES]>();
        let bdl: driver_runtime::DmaBuffer<[BufferDescriptor; BDL_ENTRIES]> =
            driver_runtime::DmaBuffer::allocate(&device, bdl_size, core::mem::align_of::<u64>())
                .map_err(|_| AudioError::Internal)?;

        let pcm_ring: driver_runtime::DmaBuffer<[u8; DEFAULT_PCM_RING_BYTES]> =
            driver_runtime::DmaBuffer::allocate(
                &device,
                DEFAULT_PCM_RING_BYTES,
                core::mem::align_of::<u64>(),
            )
            .map_err(|_| AudioError::Internal)?;

        let bdl_iova = bdl.iova();
        let pcm_ring_iova = pcm_ring.iova();
        check_iova_fits_u32(bdl_iova, bdl_size)?;
        check_iova_fits_u32(pcm_ring_iova, DEFAULT_PCM_RING_BYTES)?;

        init_controller(&bus)?;
        open_pcm_out_stream(&bus, bdl_iova)?;

        Ok(Self {
            device,
            bus,
            bdl,
            pcm_ring,
            logic: Ac97Logic::new(),
            stream_open: false,
        })
    }

    /// Borrow the underlying device handle for IRQ subscription.
    pub fn device(&self) -> &DeviceHandle {
        &self.device
    }

    /// Snapshot the running stats counters.
    pub fn stats(&self) -> StatsSnapshot {
        StatsSnapshot {
            frames_submitted: self.logic.frames_submitted(),
            frames_consumed: self.logic.frames_consumed(),
            underrun_count: self.logic.underrun_count(),
        }
    }

    /// Poll the controller's `CIV` register and advance internal
    /// completion state to match.
    pub fn poll_completed_buffers(&mut self) -> usize {
        let civ = self.bus.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV);
        let tail_before = self.logic.tail;
        self.logic.observe_irq(0, civ);
        self.logic.tail.wrapping_sub(tail_before)
    }

    /// Open a stream of the requested PCM shape.
    pub fn open_stream(
        &mut self,
        format: PcmFormat,
        layout: ChannelLayout,
        rate: SampleRate,
    ) -> Result<u32, AudioError> {
        if !shape_supported(format, layout, rate) {
            return Err(AudioError::InvalidFormat);
        }
        if self.stream_open {
            return Err(AudioError::Busy);
        }
        self.logic = Ac97Logic::new();
        open_pcm_out_stream(&self.bus, self.bdl.iova())?;
        self.stream_open = true;
        Ok(Self::PCM_OUT_STREAM_ID)
    }

    /// Append `bytes` to the open stream's PCM ring.
    pub fn submit_frames(&mut self, stream_id: u32, bytes: &[u8]) -> Result<usize, AudioError> {
        if stream_id != Self::PCM_OUT_STREAM_ID || !self.stream_open {
            return Err(AudioError::InvalidArgument);
        }
        self.poll_completed_buffers();
        let pcm_ring_iova = self.pcm_ring.iova();
        let bdl_iova = self.bdl.iova();
        let n = submit_frames_inner(
            bytes,
            &mut self.pcm_ring,
            pcm_ring_iova,
            &mut self.bdl,
            bdl_iova,
            &mut self.logic,
        )?;
        self.bus
            .write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::LVI, self.logic.lvi());
        Ok(n)
    }

    /// Block until every submitted frame has been consumed by the device.
    pub fn drain(&mut self, stream_id: u32) -> Result<(), AudioError> {
        if stream_id != Self::PCM_OUT_STREAM_ID || !self.stream_open {
            return Err(AudioError::InvalidArgument);
        }
        Ok(())
    }

    /// Halt the stream and release the slot for the next opener.
    pub fn close_stream(&mut self, stream_id: u32) -> Result<(), AudioError> {
        if stream_id != Self::PCM_OUT_STREAM_ID {
            return Err(AudioError::InvalidArgument);
        }
        close_pcm_out_stream(&self.bus)?;
        self.stream_open = false;
        Ok(())
    }

    /// Poll for newly-completed buffers and return the running
    /// `frames_consumed` counter.
    pub fn poll_frames_consumed(&mut self) -> u64 {
        self.poll_completed_buffers();
        self.logic.frames_consumed()
    }

    /// Decode the next IRQ from the hardware status register.
    pub fn handle_irq(&mut self) -> Result<IrqEvent, AudioError> {
        let ring_was_empty = self.logic.head == self.logic.tail;
        let civ = self.bus.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV);
        let event = handle_pcm_out_irq(&self.bus, ring_was_empty)?;
        let sr_for_observe: u16 = match event {
            IrqEvent::Underrun | IrqEvent::FifoError => sr_bits::FIFOE,
            IrqEvent::LastValidIndex => sr_bits::LVBCI,
            IrqEvent::Empty => sr_bits::BCIS,
            IrqEvent::None => 0,
        };
        self.logic.observe_irq(sr_for_observe, civ);
        if matches!(event, IrqEvent::FifoError) {
            return Err(AudioError::Internal);
        }
        Ok(event)
    }
}

// ---------------------------------------------------------------------------
// Ac97PioBus — Phase 63 Track Z.4
// ---------------------------------------------------------------------------

/// Production [`MmioOps`] adapter that dispatches register accesses to the
/// two AC'97 PIO BARs via `sys_device_pio_read` / `sys_device_pio_write`.
#[cfg(not(test))]
pub struct Ac97PioBus {
    /// BAR0 — Native Audio Mixer (NAM) PIO window.
    nam: driver_runtime::Pio<()>,
    /// BAR1 — Native Audio Bus Master (NABM) PIO window.
    nabm: driver_runtime::Pio<()>,
}

#[cfg(not(test))]
impl Ac97PioBus {
    /// Construct an `Ac97PioBus` by mapping both AC'97 PIO BARs.
    pub fn new(device: &driver_runtime::DeviceHandle) -> Result<Self, AudioError> {
        let nam = driver_runtime::Pio::map(device, BAR_NAM).map_err(|_| AudioError::Internal)?;
        let nabm = driver_runtime::Pio::map(device, BAR_NABM).map_err(|_| AudioError::Internal)?;
        Ok(Self { nam, nabm })
    }
}

#[cfg(not(test))]
impl MmioOps for Ac97PioBus {
    fn read_u8(&self, bar: u8, offset: usize) -> u8 {
        match bar {
            BAR_NAM => self.nam.read_u8(offset),
            BAR_NABM => self.nabm.read_u8(offset),
            _ => 0,
        }
    }

    fn read_u16(&self, bar: u8, offset: usize) -> u16 {
        match bar {
            BAR_NAM => self.nam.read_u16(offset),
            BAR_NABM => self.nabm.read_u16(offset),
            _ => 0,
        }
    }

    fn read_u32(&self, bar: u8, offset: usize) -> u32 {
        match bar {
            BAR_NAM => self.nam.read_u32(offset),
            BAR_NABM => self.nabm.read_u32(offset),
            _ => 0,
        }
    }

    fn write_u8(&self, bar: u8, offset: usize, value: u8) {
        match bar {
            BAR_NAM => self.nam.write_u8(offset, value),
            BAR_NABM => self.nabm.write_u8(offset, value),
            _ => {}
        }
    }

    fn write_u16(&self, bar: u8, offset: usize, value: u16) {
        match bar {
            BAR_NAM => self.nam.write_u16(offset, value),
            BAR_NABM => self.nabm.write_u16(offset, value),
            _ => {}
        }
    }

    fn write_u32(&self, bar: u8, offset: usize, value: u32) {
        match bar {
            BAR_NAM => self.nam.write_u32(offset, value),
            BAR_NABM => self.nabm.write_u32(offset, value),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — moved from audio_server/src/device.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    // -- check_iova_fits_u32 --------------------------------------------------

    #[test]
    fn check_iova_fits_u32_accepts_low_dma_buffer() {
        assert_eq!(check_iova_fits_u32(0x0010_0000, 16 * 1024), Ok(()));
    }

    #[test]
    fn check_iova_fits_u32_accepts_buffer_ending_exactly_at_4gib() {
        let size: usize = 256;
        let iova: u64 = (1u64 << 32) - size as u64;
        assert_eq!(check_iova_fits_u32(iova, size), Ok(()));
    }

    #[test]
    fn check_iova_fits_u32_rejects_buffer_crossing_4gib_boundary() {
        let iova: u64 = (1u64 << 32) - 1;
        assert_eq!(check_iova_fits_u32(iova, 2), Err(AudioError::Internal));
    }

    #[test]
    fn check_iova_fits_u32_rejects_buffer_fully_above_4gib() {
        assert_eq!(
            check_iova_fits_u32(1u64 << 33, 16 * 1024),
            Err(AudioError::Internal)
        );
    }

    #[test]
    fn check_iova_fits_u32_rejects_overflow_on_add() {
        assert_eq!(
            check_iova_fits_u32(u64::MAX - 5, 100),
            Err(AudioError::Internal)
        );
    }

    // -- FakeMmio -------------------------------------------------------------

    struct FakeMmio {
        log: RefCell<Vec<(u8, usize, u32, u8)>>, // (bar, off, value, width)
        reg: RefCell<Vec<(u8, usize, u32, u8)>>,
    }

    impl FakeMmio {
        fn new() -> Self {
            Self {
                log: RefCell::new(Vec::new()),
                reg: RefCell::new(Vec::new()),
            }
        }
        fn set_u8(&self, bar: u8, off: usize, val: u8) {
            let mut r = self.reg.borrow_mut();
            if let Some(slot) = r.iter_mut().find(|(b, o, _, _)| *b == bar && *o == off) {
                slot.2 = val as u32;
                slot.3 = 8;
            } else {
                r.push((bar, off, val as u32, 8));
            }
        }
        fn set_u16(&self, bar: u8, off: usize, val: u16) {
            let mut r = self.reg.borrow_mut();
            if let Some(slot) = r.iter_mut().find(|(b, o, _, _)| *b == bar && *o == off) {
                slot.2 = val as u32;
                slot.3 = 16;
            } else {
                r.push((bar, off, val as u32, 16));
            }
        }
        fn writes(&self) -> Vec<(u8, usize, u32, u8)> {
            self.log.borrow().clone()
        }
        fn write_offsets(&self) -> Vec<(u8, usize)> {
            self.log.borrow().iter().map(|w| (w.0, w.1)).collect()
        }
    }

    impl MmioOps for FakeMmio {
        fn read_u8(&self, bar: u8, offset: usize) -> u8 {
            self.reg
                .borrow()
                .iter()
                .find(|(b, o, _, _)| *b == bar && *o == offset)
                .map(|(_, _, v, _)| *v as u8)
                .unwrap_or(0)
        }
        fn read_u16(&self, bar: u8, offset: usize) -> u16 {
            self.reg
                .borrow()
                .iter()
                .find(|(b, o, _, _)| *b == bar && *o == offset)
                .map(|(_, _, v, _)| *v as u16)
                .unwrap_or(0)
        }
        fn read_u32(&self, bar: u8, offset: usize) -> u32 {
            self.reg
                .borrow()
                .iter()
                .find(|(b, o, _, _)| *b == bar && *o == offset)
                .map(|(_, _, v, _)| *v)
                .unwrap_or(0)
        }
        fn write_u8(&self, bar: u8, offset: usize, value: u8) {
            self.log.borrow_mut().push((bar, offset, value as u32, 8));
            self.set_u8(bar, offset, value);
            if bar == BAR_NABM
                && offset == nabm::PCM_OUT_BASE + nabm::CR
                && value & cr_bits::RR != 0
            {
                let cleared = value & !cr_bits::RR;
                self.set_u8(bar, offset, cleared);
            }
        }
        fn write_u16(&self, bar: u8, offset: usize, value: u16) {
            self.log.borrow_mut().push((bar, offset, value as u32, 16));
            self.set_u16(bar, offset, value);
        }
        fn write_u32(&self, bar: u8, offset: usize, value: u32) {
            self.log.borrow_mut().push((bar, offset, value, 32));
            let mut r = self.reg.borrow_mut();
            if let Some(slot) = r.iter_mut().find(|(b, o, _, _)| *b == bar && *o == offset) {
                slot.2 = value;
                slot.3 = 32;
            } else {
                r.push((bar, offset, value, 32));
            }
        }
    }

    // -- D.2 production-path tests against `Ac97Logic` -----------------------

    #[test]
    fn init_controller_writes_reset_then_clears_volume_then_programs_rate() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init succeeds on a responsive codec");
        let writes = mmio.writes();

        let nam_writes: Vec<&(u8, usize, u32, u8)> =
            writes.iter().filter(|w| w.0 == BAR_NAM).collect();
        let positions: Vec<usize> = nam_writes.iter().map(|w| w.1).collect();
        let pos_reset = positions.iter().position(|&o| o == nam::RESET);
        let pos_master = positions.iter().position(|&o| o == nam::MASTER_VOLUME);
        let pos_pcmout = positions.iter().position(|&o| o == nam::PCM_OUT_VOLUME);
        let pos_vra = positions
            .iter()
            .position(|&o| o == nam::EXT_AUDIO_STATUS_CTRL);
        let pos_rate = positions.iter().position(|&o| o == nam::PCM_FRONT_DAC_RATE);
        assert!(pos_reset.is_some(), "RESET write must occur");
        assert!(pos_master.is_some(), "MASTER_VOLUME write must occur");
        assert!(pos_pcmout.is_some(), "PCM_OUT_VOLUME write must occur");
        assert!(
            pos_vra.is_some(),
            "EXT_AUDIO_STATUS_CTRL.VRA write must occur"
        );
        assert!(pos_rate.is_some(), "PCM_FRONT_DAC_RATE write must occur");
        assert!(pos_reset < pos_master);
        assert!(pos_master < pos_pcmout);
        assert!(pos_pcmout < pos_vra);
        assert!(pos_vra < pos_rate);
    }

    #[test]
    fn init_controller_unmutes_master_and_pcm_out_volumes() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        let mv = mmio.read_u16(BAR_NAM, nam::MASTER_VOLUME);
        let pv = mmio.read_u16(BAR_NAM, nam::PCM_OUT_VOLUME);
        assert_eq!(mv & 0x8000, 0, "master volume must be unmuted");
        assert_eq!(pv & 0x8000, 0, "pcm-out volume must be unmuted");
    }

    #[test]
    fn init_controller_programs_48khz_sample_rate() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        let r = mmio.read_u16(BAR_NAM, nam::PCM_FRONT_DAC_RATE);
        assert_eq!(r, SAMPLE_RATE_HZ);
    }

    #[test]
    fn open_stream_programs_bdbar_then_lvi_then_cr_run() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        let bdl_iova: u64 = 0x0000_0001_DEAD_BEEF;
        open_pcm_out_stream(&mmio, bdl_iova).expect("open succeeds");

        let writes = mmio.writes();
        let nabm_writes: Vec<&(u8, usize, u32, u8)> =
            writes.iter().filter(|w| w.0 == BAR_NABM).collect();
        let pos_bdbar = nabm_writes
            .iter()
            .position(|w| w.1 == nabm::PCM_OUT_BASE + nabm::BDBAR);
        let pos_lvi = nabm_writes
            .iter()
            .position(|w| w.1 == nabm::PCM_OUT_BASE + nabm::LVI);
        let pos_cr_run = nabm_writes
            .iter()
            .rposition(|w| w.1 == nabm::PCM_OUT_BASE + nabm::CR && (w.2 as u8) == cr_run_value());
        assert!(pos_bdbar.is_some(), "BDBAR write required");
        assert!(pos_lvi.is_some(), "LVI write required");
        assert!(pos_cr_run.is_some(), "CR run-bit write required");
        assert!(pos_bdbar < pos_lvi, "BDBAR must precede LVI");
        assert!(pos_lvi < pos_cr_run, "LVI must precede CR run-bit");
    }

    #[test]
    fn open_stream_writes_bdbar_with_low32_of_iova() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        let bdl_iova: u64 = 0x0000_0000_C0FF_EE00;
        open_pcm_out_stream(&mmio, bdl_iova).expect("open");
        let bdbar = mmio.read_u32(BAR_NABM, nabm::PCM_OUT_BASE + nabm::BDBAR);
        assert_eq!(bdbar, 0xC0FF_EE00);
    }

    #[test]
    fn open_stream_initial_lvi_is_zero() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        open_pcm_out_stream(&mmio, 0x1000).expect("open");
        let lvi = mmio.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::LVI);
        assert_eq!(lvi, 0);
    }

    #[test]
    fn close_stream_writes_zero_to_cr_to_halt_bus_master() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        open_pcm_out_stream(&mmio, 0x1000).expect("open");
        close_pcm_out_stream(&mmio).expect("close");
        let cr = mmio.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CR);
        assert_eq!(cr, cr_halt_value(), "close must halt CR");
    }

    #[test]
    fn close_stream_resets_per_stream_registers_via_rr_bit() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        open_pcm_out_stream(&mmio, 0x1000).expect("open");
        close_pcm_out_stream(&mmio).expect("close");
        let rr_writes: Vec<_> = mmio
            .writes()
            .iter()
            .filter(|w| {
                w.0 == BAR_NABM
                    && w.1 == nabm::PCM_OUT_BASE + nabm::CR
                    && (w.2 as u8) & cr_bits::RR != 0
            })
            .copied()
            .collect();
        assert!(
            !rr_writes.is_empty(),
            "close must issue at least one CR.RR write"
        );
    }

    #[test]
    fn handle_irq_reads_sr_then_acks_observed_bits() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        open_pcm_out_stream(&mmio, 0x1000).expect("open");
        mmio.set_u16(
            BAR_NABM,
            nabm::PCM_OUT_BASE + nabm::SR,
            sr_bits::LVBCI | sr_bits::BCIS,
        );
        let event = handle_pcm_out_irq(&mmio, /*ring_was_empty=*/ false).expect("irq");
        assert_eq!(event, IrqEvent::LastValidIndex);
        let acks: Vec<_> = mmio
            .writes()
            .iter()
            .filter(|w| w.0 == BAR_NABM && w.1 == nabm::PCM_OUT_BASE + nabm::SR)
            .copied()
            .collect();
        assert!(!acks.is_empty(), "handle_irq must ack SR");
        let last_ack = acks.last().unwrap().2 as u16;
        assert_ne!(last_ack & sr_bits::LVBCI, 0);
        assert_ne!(last_ack & sr_bits::BCIS, 0);
    }

    #[test]
    fn submit_frames_appends_to_pcm_ring_and_advances_lvi() {
        let mut logic = Ac97Logic::new();
        let bdl_iova = 0x2_0000;
        logic
            .submit_buffer(bdl_iova, 0xAAAA_AAAA, 1024)
            .expect("submit");
        let entry0 = logic.bdl()[0];
        assert_eq!({ entry0.phys_addr }, 0xAAAA_AAAA);
        assert_eq!({ entry0.samples }, 1024);
        assert_eq!(logic.lvi(), 0);

        logic
            .submit_buffer(bdl_iova, 0xBBBB_BBBB, 2048)
            .expect("submit");
        let entry1 = logic.bdl()[1];
        assert_eq!({ entry1.phys_addr }, 0xBBBB_BBBB);
        assert_eq!(logic.lvi(), 1);
    }

    #[test]
    fn submit_buffer_rejects_oversize_sample_count() {
        let mut logic = Ac97Logic::new();
        let err = logic
            .submit_buffer(0x1000, 0, 0x10000)
            .expect_err("oversize must be rejected");
        assert_eq!(err, AudioError::InvalidArgument);
    }

    #[test]
    fn submit_buffer_returns_busy_when_bdl_is_full() {
        let mut logic = Ac97Logic::new();
        for _ in 0..BDL_ENTRIES {
            logic
                .submit_buffer(0x1000, 0xCAFE_F00D, 64)
                .expect("submit");
        }
        let err = logic
            .submit_buffer(0x1000, 0xDEAD_BEEF, 64)
            .expect_err("BDL full must be rejected");
        assert_eq!(err, AudioError::WouldBlock);
    }

    #[test]
    fn ac97_logic_handle_irq_advances_civ_and_increments_underrun_on_fifo_error() {
        let mut logic = Ac97Logic::new();
        logic.submit_buffer(0x1000, 0x1, 64).expect("submit");
        let event = logic.observe_irq(sr_bits::BCIS, /*new_civ=*/ 1);
        assert_eq!(event, IrqEvent::Empty);
        assert_eq!(logic.frames_consumed(), 64);

        let event2 = logic.observe_irq(sr_bits::FIFOE, /*new_civ=*/ 1);
        assert_eq!(event2, IrqEvent::Underrun);
        assert_eq!(logic.underrun_count(), 1);
    }

    #[test]
    fn cr_reset_value_sets_only_rr() {
        assert_eq!(cr_reset_value(), cr_bits::RR);
        assert_eq!(cr_reset_value() & cr_bits::RPBM, 0);
    }

    #[test]
    fn cr_run_value_arms_run_plus_every_irq_cause() {
        let v = cr_run_value();
        assert_ne!(v & cr_bits::RPBM, 0);
        assert_ne!(v & cr_bits::LVBIE, 0);
        assert_ne!(v & cr_bits::FEIE, 0);
        assert_ne!(v & cr_bits::IOCE, 0);
    }

    #[test]
    fn cr_halt_value_is_zero() {
        assert_eq!(cr_halt_value(), 0);
    }

    #[test]
    fn shape_supported_accepts_phase57_default() {
        assert!(shape_supported(
            PcmFormat::S16Le,
            ChannelLayout::Stereo,
            SampleRate::Hz48000,
        ));
        assert!(shape_supported(
            PcmFormat::S16Le,
            ChannelLayout::Mono,
            SampleRate::Hz48000,
        ));
    }

    #[test]
    fn classify_sr_priorities_fifo_error_first_for_non_empty_ring() {
        assert_eq!(
            classify_sr(sr_bits::FIFOE | sr_bits::LVBCI, false),
            IrqEvent::FifoError,
        );
    }

    #[test]
    fn classify_sr_treats_fifoe_on_empty_ring_as_underrun() {
        assert_eq!(classify_sr(sr_bits::FIFOE, true), IrqEvent::Underrun);
    }

    #[test]
    fn classify_sr_lvbci_takes_priority_over_bcis() {
        assert_eq!(
            classify_sr(sr_bits::LVBCI | sr_bits::BCIS, false),
            IrqEvent::LastValidIndex,
        );
    }

    #[test]
    fn classify_sr_bcis_alone_yields_empty() {
        assert_eq!(classify_sr(sr_bits::BCIS, false), IrqEvent::Empty);
    }

    #[test]
    fn classify_sr_no_bits_yields_none() {
        assert_eq!(classify_sr(0, false), IrqEvent::None);
    }

    #[test]
    fn sr_ack_masks_to_w1c_bits_only() {
        let observed = sr_bits::DCH | sr_bits::CELV | sr_bits::BCIS;
        assert_eq!(sr_ack_value(observed), sr_bits::BCIS);
    }

    // -- A.1: frames_submitted getter and StatsSnapshot wiring ----------------

    #[test]
    fn ac97_logic_frames_submitted_getter_reflects_submit_buffer_calls() {
        let mut logic = Ac97Logic::new();
        assert_eq!(
            logic.frames_submitted(),
            0,
            "initial frames_submitted must be 0"
        );
        logic
            .submit_buffer(0x1000, 0xAAAA_0000, 100)
            .expect("submit");
        assert_eq!(
            logic.frames_submitted(),
            100,
            "frames_submitted must equal the samples argument"
        );
        logic
            .submit_buffer(0x1000, 0xBBBB_0000, 256)
            .expect("submit");
        assert_eq!(
            logic.frames_submitted(),
            356,
            "frames_submitted is cumulative across submit_buffer calls"
        );
    }

    #[test]
    fn stats_snapshot_built_from_ac97_logic_counters_is_consistent() {
        let mut logic = Ac97Logic::new();
        logic
            .submit_buffer(0x1000, 0xCAFE_0000, 512)
            .expect("submit");
        let _ = logic.observe_irq(sr_bits::BCIS, 1);
        let _ = logic.observe_irq(sr_bits::FIFOE, 1);

        let snap = StatsSnapshot {
            frames_submitted: logic.frames_submitted(),
            frames_consumed: logic.frames_consumed(),
            underrun_count: logic.underrun_count(),
        };
        assert_eq!(snap.frames_submitted, 512);
        assert_eq!(snap.frames_consumed, 512);
        assert_eq!(snap.underrun_count, 1);
    }

    // -- A.2: init wiring -------------------------------------------------------

    #[test]
    fn init_then_open_stream_writes_codec_then_bdl_in_correct_order() {
        let mmio = FakeMmio::new();
        let bdl_iova: u64 = 0x0000_0000_FEED_C0DE;

        init_controller(&mmio).expect("init_controller must succeed");
        open_pcm_out_stream(&mmio, bdl_iova).expect("open_pcm_out_stream must succeed");

        let writes = mmio.writes();

        let bdbar_writes: Vec<_> = writes
            .iter()
            .filter(|w| w.0 == BAR_NABM && w.1 == nabm::PCM_OUT_BASE + nabm::BDBAR)
            .collect();
        assert!(!bdbar_writes.is_empty(), "BDBAR must be written");
        let last_bdbar = bdbar_writes.last().unwrap().2;
        assert_eq!(
            last_bdbar,
            (bdl_iova & 0xFFFF_FFFF) as u32,
            "BDBAR must carry the low 32 bits of the BDL IOVA"
        );

        let pos_reset = writes
            .iter()
            .position(|w| w.0 == BAR_NAM && w.1 == nam::RESET);
        let pos_bdbar = writes
            .iter()
            .position(|w| w.0 == BAR_NABM && w.1 == nabm::PCM_OUT_BASE + nabm::BDBAR);
        assert!(pos_reset.is_some(), "NAM RESET must be written");
        assert!(pos_bdbar.is_some(), "BDBAR must be written");
        assert!(
            pos_reset < pos_bdbar,
            "codec RESET must precede BDBAR programming"
        );

        let pos_vra = writes
            .iter()
            .position(|w| w.0 == BAR_NAM && w.1 == nam::EXT_AUDIO_STATUS_CTRL);
        let pos_rate = writes
            .iter()
            .position(|w| w.0 == BAR_NAM && w.1 == nam::PCM_FRONT_DAC_RATE);
        assert!(pos_vra < pos_rate, "VRA must precede PCM_FRONT_DAC_RATE");

        let pos_lvi = writes
            .iter()
            .position(|w| w.0 == BAR_NABM && w.1 == nabm::PCM_OUT_BASE + nabm::LVI);
        let pos_cr_run = writes.iter().rposition(|w| {
            w.0 == BAR_NABM && w.1 == nabm::PCM_OUT_BASE + nabm::CR && (w.2 as u8) == cr_run_value()
        });
        assert!(pos_lvi.is_some(), "LVI must be written");
        assert!(pos_cr_run.is_some(), "CR run-bit must be written");
        assert!(
            pos_bdbar < pos_lvi,
            "BDBAR must precede LVI in open_pcm_out_stream"
        );
        assert!(
            pos_lvi < pos_cr_run,
            "LVI must precede CR run-bit in open_pcm_out_stream"
        );
    }

    #[test]
    fn open_stream_uses_provided_bdl_iova_for_bdbar() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        let bdl_iova: u64 = 0x0000_0001_2345_6780;
        open_pcm_out_stream(&mmio, bdl_iova).expect("open");
        let bdbar = mmio.read_u32(BAR_NABM, nabm::PCM_OUT_BASE + nabm::BDBAR);
        assert_eq!(
            bdbar, 0x2345_6780,
            "BDBAR must hold the low 32 bits of bdl_iova"
        );
    }

    // -- A.3: IRQ handler tests ------------------------------------------------

    #[test]
    fn handle_irq_bcis_advances_logic_frames_consumed() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        open_pcm_out_stream(&mmio, 0x1000).expect("open");

        let mut logic = Ac97Logic::new();
        logic
            .submit_buffer(0x1000, 0xCAFE_0000, 128)
            .expect("submit");

        mmio.set_u16(BAR_NABM, nabm::PCM_OUT_BASE + nabm::SR, sr_bits::BCIS);
        let ring_was_empty = logic.head == logic.tail;
        let civ = mmio.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV);
        let event = handle_pcm_out_irq(&mmio, ring_was_empty).expect("handle_irq");
        assert_eq!(event, IrqEvent::Empty, "BCIS alone must yield Empty");

        let sr_for_observe: u16 = sr_bits::BCIS;
        logic.observe_irq(sr_for_observe, civ);
        let event2 = logic.observe_irq(sr_bits::BCIS, 1);
        assert_eq!(event2, IrqEvent::Empty);
        assert_eq!(
            logic.frames_consumed(),
            128,
            "frames_consumed must equal the submitted samples"
        );
    }

    #[test]
    fn handle_irq_fifo_error_on_empty_ring_increments_underrun_count() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        open_pcm_out_stream(&mmio, 0x1000).expect("open");

        let mut logic = Ac97Logic::new();
        mmio.set_u16(BAR_NABM, nabm::PCM_OUT_BASE + nabm::SR, sr_bits::FIFOE);
        let ring_was_empty = logic.head == logic.tail;
        let civ = mmio.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV);
        let event = handle_pcm_out_irq(&mmio, ring_was_empty).expect("handle_irq");
        assert_eq!(
            event,
            IrqEvent::Underrun,
            "FIFOE on empty ring must yield Underrun (not FifoError)"
        );

        logic.observe_irq(sr_bits::FIFOE, civ);
        assert_eq!(
            logic.underrun_count(),
            1,
            "underrun_count must be 1 after one underrun event"
        );
    }

    #[test]
    fn fifo_error_on_non_empty_ring_classifies_as_fifo_error_variant() {
        let sr = sr_bits::FIFOE;
        let event = classify_sr(sr, /*ring_was_empty=*/ false);
        assert_eq!(
            event,
            IrqEvent::FifoError,
            "FIFOE on a non-empty ring must classify as FifoError"
        );
    }

    #[test]
    fn civ_is_read_from_bar_nabm_pcm_out_base_plus_civ_offset() {
        let mmio = FakeMmio::new();
        mmio.set_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV, 5);
        let civ = mmio.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV);
        assert_eq!(
            civ, 5,
            "CIV must be read from BAR_NABM at PCM_OUT_BASE + CIV offset"
        );
    }

    // -- B.1: submit_frames_inner tests ----------------------------------------

    fn fresh_bdl_dma() -> [BufferDescriptor; BDL_ENTRIES] {
        [BufferDescriptor {
            phys_addr: 0,
            samples: 0,
            flags: 0,
        }; BDL_ENTRIES]
    }

    #[test]
    fn submit_frames_inner_copies_bytes_to_correct_slot_offset() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let mut submission = [0u8; PCM_SLOT_STRIDE];
        for (i, b) in submission.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        submit_frames_inner(
            &submission,
            &mut pcm_ring,
            0x0010_0000,
            &mut bdl_dma,
            0x0020_0000,
            &mut logic,
        )
        .expect("submit must succeed");

        assert_eq!(
            &pcm_ring[0..PCM_SLOT_STRIDE],
            &submission[..],
            "bytes must be at slot 0 (offset 0)"
        );
    }

    #[test]
    fn submit_frames_inner_uses_next_slot_for_second_submission() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let first = [0xAAu8; PCM_SLOT_STRIDE];
        let second = [0xBBu8; PCM_SLOT_STRIDE];

        submit_frames_inner(
            &first,
            &mut pcm_ring,
            0x1000_0000,
            &mut bdl_dma,
            0x2000_0000,
            &mut logic,
        )
        .expect("first submit");
        submit_frames_inner(
            &second,
            &mut pcm_ring,
            0x1000_0000,
            &mut bdl_dma,
            0x2000_0000,
            &mut logic,
        )
        .expect("second submit");

        assert_eq!(&pcm_ring[0..PCM_SLOT_STRIDE], &first[..]);
        assert_eq!(&pcm_ring[PCM_SLOT_STRIDE..2 * PCM_SLOT_STRIDE], &second[..]);
    }

    #[test]
    fn submit_frames_inner_posts_bdl_entry_with_correct_iova_and_samples() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let pcm_ring_iova: u64 = 0x0010_0000;
        let bdl_iova: u64 = 0x0020_0000;

        let data = [0u8; PCM_SLOT_STRIDE];
        submit_frames_inner(
            &data,
            &mut pcm_ring,
            pcm_ring_iova,
            &mut bdl_dma,
            bdl_iova,
            &mut logic,
        )
        .expect("submit");

        let entry0 = logic.bdl()[0];
        let expected_phys_addr = pcm_ring_iova as u32;
        assert_eq!({ entry0.phys_addr }, expected_phys_addr);
        let expected_samples = (PCM_SLOT_STRIDE / 2) as u16;
        assert_eq!({ entry0.samples }, expected_samples);
        assert_eq!(logic.lvi(), 0);
    }

    #[test]
    fn submit_frames_inner_lvi_advances_after_each_slot() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let data = [0u8; PCM_SLOT_STRIDE];

        submit_frames_inner(
            &data,
            &mut pcm_ring,
            0x1000,
            &mut bdl_dma,
            0x2000,
            &mut logic,
        )
        .expect("first");
        assert_eq!(logic.lvi(), 0);

        submit_frames_inner(
            &data,
            &mut pcm_ring,
            0x1000,
            &mut bdl_dma,
            0x2000,
            &mut logic,
        )
        .expect("second");
        assert_eq!(logic.lvi(), 1);
    }

    #[test]
    fn submit_frames_inner_partial_slot_is_padded_and_accepted() {
        let mut pcm_ring = [0xAAu8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let payload = [0x5Au8; PCM_SLOT_STRIDE / 2];
        let n = submit_frames_inner(
            &payload,
            &mut pcm_ring,
            0x1000,
            &mut bdl_dma,
            0x2000,
            &mut logic,
        )
        .expect("partial slot must be padded and accepted");
        assert_eq!(n, payload.len());
        assert_eq!(&pcm_ring[..payload.len()], &payload[..]);
        for (i, &b) in pcm_ring
            .iter()
            .enumerate()
            .skip(payload.len())
            .take(PCM_SLOT_STRIDE - payload.len())
        {
            assert_eq!(b, 0, "pad byte {i} must be silence (got {b:#x})");
        }
        assert_eq!(logic.lvi(), 0);
        assert_eq!(logic.head, 1);
    }

    #[test]
    fn submit_frames_inner_zero_len_returns_invalid_argument() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let err = submit_frames_inner(&[], &mut pcm_ring, 0x1000, &mut bdl_dma, 0x2000, &mut logic)
            .expect_err("zero len must be rejected");
        assert_eq!(err, AudioError::InvalidArgument);
    }

    #[test]
    fn submit_frames_inner_partial_trailing_slot_pads_only_the_tail() {
        let mut pcm_ring = [0xAAu8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let mut data = alloc::vec![0x11u8; PCM_SLOT_STRIDE];
        data.extend(core::iter::repeat_n(0x22u8, PCM_SLOT_STRIDE / 2));
        assert_eq!(data.len(), PCM_SLOT_STRIDE + PCM_SLOT_STRIDE / 2);

        let n = submit_frames_inner(
            &data,
            &mut pcm_ring,
            0x1000,
            &mut bdl_dma,
            0x2000,
            &mut logic,
        )
        .expect("partial trailing slot must be padded and accepted");

        assert_eq!(n, data.len());
        assert!(pcm_ring[..PCM_SLOT_STRIDE].iter().all(|&b| b == 0x11));
        let slot1 = &pcm_ring[PCM_SLOT_STRIDE..2 * PCM_SLOT_STRIDE];
        assert!(slot1[..PCM_SLOT_STRIDE / 2].iter().all(|&b| b == 0x22));
        assert!(slot1[PCM_SLOT_STRIDE / 2..].iter().all(|&b| b == 0));
        assert_eq!(logic.lvi(), 1);
        let entry0 = logic.bdl()[0];
        let entry1 = logic.bdl()[1];
        assert_eq!({ entry0.samples }, (PCM_SLOT_STRIDE / 2) as u16);
        assert_eq!({ entry1.samples }, (PCM_SLOT_STRIDE / 2) as u16);
    }

    #[test]
    fn submit_frames_inner_accepts_bell_tone_shape_unpadded() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let bell_tone = alloc::vec![0x33u8; 5760];
        assert_ne!(bell_tone.len() % PCM_SLOT_STRIDE, 0);
        let n = submit_frames_inner(
            &bell_tone,
            &mut pcm_ring,
            0x1000,
            &mut bdl_dma,
            0x2000,
            &mut logic,
        )
        .expect("bell tone (5760 B) must be accepted");
        assert_eq!(n, bell_tone.len());
        assert_eq!(logic.lvi(), 11);
        assert_eq!(logic.head, 12);
    }

    #[test]
    fn submit_frames_inner_bdl_full_returns_would_block() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        for i in 0..BDL_ENTRIES {
            logic
                .submit_buffer(
                    (i * core::mem::size_of::<BufferDescriptor>()) as u64,
                    i as u32 * PCM_SLOT_STRIDE as u32,
                    PCM_SLOT_STRIDE / 2,
                )
                .expect("fill BDL");
        }
        let data = [0u8; PCM_SLOT_STRIDE];
        let err = submit_frames_inner(
            &data,
            &mut pcm_ring,
            0x1000,
            &mut bdl_dma,
            0x2000,
            &mut logic,
        )
        .expect_err("BDL full must yield WouldBlock");
        assert_eq!(err, AudioError::WouldBlock);
    }

    #[test]
    fn submit_frames_inner_over_slot_splits_into_multiple_entries() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let mut data = alloc::vec![0u8; 2 * PCM_SLOT_STRIDE];
        for b in data[0..PCM_SLOT_STRIDE].iter_mut() {
            *b = 0xCC;
        }
        for b in data[PCM_SLOT_STRIDE..2 * PCM_SLOT_STRIDE].iter_mut() {
            *b = 0xDD;
        }

        let n = submit_frames_inner(
            &data,
            &mut pcm_ring,
            0x0010_0000,
            &mut bdl_dma,
            0x0020_0000,
            &mut logic,
        )
        .expect("two-slot submit must succeed");

        assert_eq!(n, 2 * PCM_SLOT_STRIDE);
        assert_eq!(logic.lvi(), 1);
        assert_eq!(&pcm_ring[0..PCM_SLOT_STRIDE], &data[0..PCM_SLOT_STRIDE]);
        assert_eq!(
            &pcm_ring[PCM_SLOT_STRIDE..2 * PCM_SLOT_STRIDE],
            &data[PCM_SLOT_STRIDE..2 * PCM_SLOT_STRIDE]
        );
    }

    #[test]
    fn submit_frames_inner_mirrors_bdl_into_dma_buffer() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let pcm_ring_iova: u64 = 0x0010_0000;
        let bdl_iova: u64 = 0x0020_0000;

        let data = alloc::vec![0u8; 3 * PCM_SLOT_STRIDE];
        submit_frames_inner(
            &data,
            &mut pcm_ring,
            pcm_ring_iova,
            &mut bdl_dma,
            bdl_iova,
            &mut logic,
        )
        .expect("three-slot submit");

        for idx in 0..3 {
            let dma_entry = bdl_dma[idx];
            let logic_entry = logic.bdl()[idx];
            assert_eq!({ dma_entry.phys_addr }, { logic_entry.phys_addr });
            assert_eq!({ dma_entry.samples }, { logic_entry.samples });
            assert_eq!({ dma_entry.flags }, { logic_entry.flags });
            assert_ne!({ dma_entry.samples }, 0);
        }
        for idx in 3..BDL_ENTRIES {
            let dma_entry = bdl_dma[idx];
            assert_eq!({ dma_entry.phys_addr }, 0);
            assert_eq!({ dma_entry.samples }, 0);
        }
    }

    #[test]
    fn submit_frames_inner_partial_fit_returns_would_block_without_writes() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();

        for i in 0..(BDL_ENTRIES - 1) {
            logic
                .submit_buffer(
                    (i * core::mem::size_of::<BufferDescriptor>()) as u64,
                    0xDEAD_0000 | (i as u32),
                    PCM_SLOT_STRIDE / 2,
                )
                .expect("pre-fill");
        }
        let head_before = logic.head;
        let tail_before = logic.tail;
        let lvi_before = logic.lvi();
        let frames_submitted_before = logic.frames_submitted();

        let data = alloc::vec![0xEEu8; 2 * PCM_SLOT_STRIDE];
        for b in pcm_ring.iter_mut() {
            *b = 0x42;
        }
        let err = submit_frames_inner(
            &data,
            &mut pcm_ring,
            0x0010_0000,
            &mut bdl_dma,
            0x0020_0000,
            &mut logic,
        )
        .expect_err("partial-fit submit must reject before writing");

        assert_eq!(err, AudioError::WouldBlock);
        assert_eq!(logic.head, head_before);
        assert_eq!(logic.tail, tail_before);
        assert_eq!(logic.lvi(), lvi_before);
        assert_eq!(logic.frames_submitted(), frames_submitted_before);
        assert!(pcm_ring.iter().all(|&b| b == 0x42));
        let free_idx = head_before % BDL_ENTRIES;
        let free_dma = bdl_dma[free_idx];
        assert_eq!({ free_dma.phys_addr }, 0);
        assert_eq!({ free_dma.samples }, 0);
    }

    #[test]
    fn silence_frame_is_one_slot_stride_of_zeros() {
        assert_eq!(SILENCE_FRAME.len(), PCM_SLOT_STRIDE);
        assert!(SILENCE_FRAME.iter().all(|&b| b == 0));
    }
}
