//! AC'97 controller backend — Phase 57 Track D.2.
//!
//! Track D.1 lands a scaffold that names every public symbol Tracks
//! D.2..D.5 consume. The real register-poking + DMA programming code
//! lands in D.2 (this file again, behind the same `MmioOps` seam the
//! e1000 driver established in Phase 55b).

#![allow(dead_code)] // D.2/D.3/D.4 consume every symbol; see module docs.

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
// AudioBackend — the trait every audio device-class backend implements
// ---------------------------------------------------------------------------

/// Phase 57 Track D.2 device-backend trait.
///
/// Splitting the trait from the concrete implementation lets a later
/// phase add a second backend (e.g., HDA after AC'97) by adding a
/// file rather than editing callers. The Phase 57 single-format
/// constraint (S16Le / Stereo / 48000 Hz) is enforced by every impl
/// returning `AudioError::InvalidFormat` for any other shape; the
/// shape-validation test harness lives in the parent module.
pub trait AudioBackend {
    /// Initialise the controller — reset, configure, leave it ready
    /// to accept an `open_stream`.
    fn init(&mut self) -> Result<(), AudioError>;

    /// Open a stream of the requested PCM shape. Returns the
    /// stream id on success; rejects unsupported formats with
    /// `AudioError::InvalidFormat`. Phase 57 single-format constraint
    /// holds here — only `S16Le` / `Stereo` / `Hz48000` is accepted.
    fn open_stream(
        &mut self,
        format: PcmFormat,
        layout: ChannelLayout,
        rate: SampleRate,
    ) -> Result<u32, AudioError>;

    /// Append `bytes` to the open stream's PCM ring. Returns the
    /// number of bytes accepted (always `bytes.len()` on success).
    fn submit_frames(&mut self, stream_id: u32, bytes: &[u8]) -> Result<usize, AudioError>;

    /// Block until every submitted frame has been consumed by the
    /// device. The io loop calls this in response to an IRQ wake;
    /// the function itself returns immediately after recording the
    /// drain request — the io loop polls `handle_irq` to observe
    /// completion.
    fn drain(&mut self, stream_id: u32) -> Result<(), AudioError>;

    /// Halt the stream (write `CR=0`), reset its BDL, and release the
    /// slot for the next opener.
    fn close_stream(&mut self, stream_id: u32) -> Result<(), AudioError>;

    /// Decode the next IRQ. Reads the per-stream status register,
    /// advances ring tails, and returns a typed [`IrqEvent`]. Called
    /// once per `RecvResult::Notification`; the io loop uses the
    /// result to fan out to the stream registry and the stats verb.
    fn handle_irq(&mut self) -> Result<IrqEvent, AudioError>;
}

// ---------------------------------------------------------------------------
// AC'97 register layout — single source of truth
// ---------------------------------------------------------------------------

/// AC'97 Native Audio Mixer (NAM, BAR0) register offsets used by the
/// Phase 57 driver. Each constant matches the chosen-target memo
/// (`docs/appendix/phase-57-audio-target-choice.md`).
pub mod nam {
    /// `RESET` — 16-bit, write any value to issue a cold codec reset.
    pub const RESET: usize = 0x00;
    /// `MASTER_VOLUME` — 16-bit, 5-bit per channel + mute.
    pub const MASTER_VOLUME: usize = 0x02;
    /// `PCM_OUT_VOLUME` — 16-bit, output stream volume + mute.
    pub const PCM_OUT_VOLUME: usize = 0x18;
    /// `PCM_FRONT_DAC_RATE` — 16-bit, sample-rate select. Phase 57
    /// programs `48000`.
    pub const PCM_FRONT_DAC_RATE: usize = 0x2C;
    /// `EXT_AUDIO_ID` — 16-bit, optional codec capabilities.
    pub const EXT_AUDIO_ID: usize = 0x28;
    /// `EXT_AUDIO_STATUS_CTRL` — 16-bit, variable-rate-audio enable.
    /// Bit 0 (`VRA`) must be set before `PCM_FRONT_DAC_RATE` is
    /// honored on real ICH silicon.
    pub const EXT_AUDIO_STATUS_CTRL: usize = 0x2A;
}

/// AC'97 Native Audio Bus Master (NABM, BAR1) register offsets used by
/// the Phase 57 driver. The PCM-out stream's per-stream block lives at
/// offset `0x10` from BAR1; each per-stream register is the offset
/// declared here PLUS that base.
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
    /// Control register (8-bit). Bits: RPBM (run/pause), RR (reset),
    /// LVBIE, FEIE, IOCE.
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
///
/// Each entry references one DMA-mapped audio buffer. Hardware reads
/// `phys_addr`, sends `samples` 16-bit samples to the codec, and
/// raises an interrupt according to `flags`.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BufferDescriptor {
    /// 32-bit physical address of the buffer (low 32 bits — AC'97 is
    /// a 32-bit-IOVA device).
    pub phys_addr: u32,
    /// Number of 16-bit samples in the buffer.
    pub samples: u16,
    /// Flags — bit 15 (`IOC`) requests an interrupt on completion;
    /// bit 14 (`BUP`) signals "buffer underrun" should fire on this
    /// descriptor.
    pub flags: u16,
}

/// Number of BDL entries — fixed by the AC'97 spec.
pub const BDL_ENTRIES: usize = 32;

/// Maximum sample count per BDL entry (15-bit field).
pub const BDL_MAX_SAMPLES: usize = 0xFFFE;

/// Default PCM-data ring size — 16 KiB. Within the 4 KiB ≤ N ≤ 64 KiB
/// bound from the chosen-target memo.
pub const DEFAULT_PCM_RING_BYTES: usize = 16 * 1024;

/// Sample rate (Hz) the Phase 57 single-format constraint pins.
pub const SAMPLE_RATE_HZ: u16 = 48_000;

// ---------------------------------------------------------------------------
// MmioOps — minimal seam for register access (test-double friendly)
// ---------------------------------------------------------------------------

/// Read / write surface the AC'97 init + IRQ paths consume. The
/// production backend implements this against PIO ports (AC'97's BARs
/// are I/O-space in real ICH and in QEMU's `-device AC97` emulation);
/// the host-side `FakeMmio` in the test module records every access
/// so register-write ordering is asserted without real hardware.
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

/// Compose the value written to `nabm::CR` to issue a per-stream reset
/// from a previously-running state.
#[inline]
pub const fn cr_reset_value() -> u8 {
    cr_bits::RR
}

/// Compose the value written to `nabm::CR` to start the bus master with
/// every interrupt cause enabled.
#[inline]
pub const fn cr_run_value() -> u8 {
    cr_bits::RPBM | cr_bits::LVBIE | cr_bits::FEIE | cr_bits::IOCE
}

/// Compose the value written to `nabm::CR` to halt the bus master and
/// silence interrupts.
#[inline]
pub const fn cr_halt_value() -> u8 {
    0
}

/// Compose the W1C value for `nabm::SR` to acknowledge every
/// interrupt cause. AC'97's SR bits are write-1-to-clear; writing the
/// observed bits back clears them.
#[inline]
pub const fn sr_ack_value(observed: u16) -> u16 {
    observed & sr_bits::W1C_MASK
}

/// Decode an SR snapshot into an [`IrqEvent`].
///
/// Priority: `FIFOE` > `LVBCI` > `BCIS` > else `None`. The order
/// reflects severity: a FIFO error indicates a programming bug and
/// must surface first; LVBCI says the BDL has wrapped and the driver
/// must repost; BCIS says the consumed counter advanced; everything
/// else is no-op.
pub const fn classify_sr(sr: u16, ring_was_empty: bool) -> IrqEvent {
    if sr & sr_bits::FIFOE != 0 {
        // FIFO underrun. If the producer ring was empty, the underrun
        // is the consumer-side event the stats verb counts. Otherwise
        // it's a hard programming bug.
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
///
/// The chosen-target memo accepts both `Mono` and `Stereo` channel
/// layouts; rate is fixed at 48 kHz; format must be S16Le.
pub const fn shape_supported(format: PcmFormat, _layout: ChannelLayout, rate: SampleRate) -> bool {
    matches!(format, PcmFormat::S16Le) && matches!(rate, SampleRate::Hz48000)
}

// ---------------------------------------------------------------------------
// BAR identifiers — pure-data constants
// ---------------------------------------------------------------------------

/// Conventional BAR-index value the [`MmioOps`] seam uses to address
/// the AC'97 NAM (mixer) PIO window.  Real BAR0 of the device.
pub const BAR_NAM: u8 = 0;
/// BAR-index value for the AC'97 NABM (bus-master) PIO window.  Real
/// BAR1 of the device.
pub const BAR_NABM: u8 = 1;

// ---------------------------------------------------------------------------
// AC'97 init / open / close / IRQ — declarations land in D.2-green.
// ---------------------------------------------------------------------------

/// Reset the codec, unmute volumes, set rate.  Implementation lands
/// in D.2-green.
pub fn init_controller<M: MmioOps>(_mmio: &M) -> Result<(), AudioError> {
    Err(AudioError::Internal)
}

/// Open the PCM-out stream.  Implementation lands in D.2-green.
pub fn open_pcm_out_stream<M: MmioOps>(_mmio: &M, _bdl_iova: u64) -> Result<(), AudioError> {
    Err(AudioError::Internal)
}

/// Close the PCM-out stream.  Implementation lands in D.2-green.
pub fn close_pcm_out_stream<M: MmioOps>(_mmio: &M) -> Result<(), AudioError> {
    Err(AudioError::Internal)
}

/// Read SR, classify, ack.  Implementation lands in D.2-green.
pub fn handle_pcm_out_irq<M: MmioOps>(
    _mmio: &M,
    _ring_was_empty: bool,
) -> Result<IrqEvent, AudioError> {
    Err(AudioError::Internal)
}

// ---------------------------------------------------------------------------
// Ac97Logic — declaration; implementation lands in D.2-green.
// ---------------------------------------------------------------------------

/// Pure-logic AC'97 state — declaration only at D.2-red; the
/// behavior the tests pin lands in D.2-green.
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

    pub fn bdl(&self) -> &[BufferDescriptor; BDL_ENTRIES] {
        &self.bdl
    }
    pub fn lvi(&self) -> u8 {
        self.lvi
    }
    pub fn frames_consumed(&self) -> u64 {
        self.frames_consumed
    }
    pub fn underrun_count(&self) -> u32 {
        self.underrun_count
    }

    /// Submit a buffer to the BDL.  D.2-red stub returns
    /// `AudioError::Internal`; the real implementation lands next
    /// commit.
    pub fn submit_buffer(
        &mut self,
        _bdl_iova: u64,
        _phys_addr: u32,
        _samples: usize,
    ) -> Result<(), AudioError> {
        Err(AudioError::Internal)
    }

    /// Observe an IRQ.  D.2-red stub returns `IrqEvent::None`.
    pub fn observe_irq(&mut self, _sr: u16, _new_civ: u8) -> IrqEvent {
        IrqEvent::None
    }
}

// ---------------------------------------------------------------------------
// Ac97Backend — concrete implementation of `AudioBackend`
// ---------------------------------------------------------------------------

/// Concrete AC'97 backend. Constructed via [`Ac97Backend::init`] from a
/// claimed `DeviceHandle`; subsequent calls follow the
/// [`AudioBackend`] trait.
///
/// The backend owns:
///
/// - The claimed `DeviceHandle` (so the IRQ subscription path can read
///   its cap).
/// - The BDL DMA buffer (`DmaBuffer<[BufferDescriptor; BDL_ENTRIES]>`).
/// - The PCM-data DMA ring (`DmaBuffer<[u8; DEFAULT_PCM_RING_BYTES]>`).
/// - Producer / consumer cursors mirroring the AC'97 LVI / CIV registers.
/// - Per-stream stats counters consumed by the `Stats` control event.
///
/// This struct is `pub` so the io loop and the stream registry can
/// borrow it through the trait. Internal state is `pub(crate)` so
/// host tests in the same crate can poke at it without exposing the
/// fields to outside consumers.
#[cfg(not(test))]
pub struct Ac97Backend {
    pub(crate) device: DeviceHandle,
    pub(crate) initialised: bool,
    pub(crate) stream_open: bool,
    pub(crate) frames_submitted: u64,
    pub(crate) frames_consumed: u64,
    pub(crate) underrun_count: u32,
}

#[cfg(not(test))]
impl Ac97Backend {
    /// Stream id for the single PCM-out stream Phase 57 supports.
    pub const PCM_OUT_STREAM_ID: u32 = 1;

    /// Construct the backend from a claimed device handle. Performs
    /// reset → status read → DMA programming.
    ///
    /// Track D.1 stub: the real bring-up path lands in D.2; this stub
    /// records the device handle so the io loop scaffold can compile.
    pub fn init(device: DeviceHandle) -> Result<Self, AudioError> {
        // D.2 will read RESET, poll status, allocate BDL + ring, set
        // master volume, set sample rate. Phase 57 D.1 records the
        // handle and reports a clean "ready" state.
        Ok(Self {
            device,
            initialised: true,
            stream_open: false,
            frames_submitted: 0,
            frames_consumed: 0,
            underrun_count: 0,
        })
    }

    /// Borrow the underlying device handle for IRQ subscription.
    pub fn device(&self) -> &DeviceHandle {
        &self.device
    }

    /// Snapshot the running stats counters.
    pub fn stats(&self) -> StatsSnapshot {
        StatsSnapshot {
            frames_submitted: self.frames_submitted,
            frames_consumed: self.frames_consumed,
            underrun_count: self.underrun_count,
        }
    }
}

#[cfg(not(test))]
impl AudioBackend for Ac97Backend {
    fn init(&mut self) -> Result<(), AudioError> {
        // D.2: real reset path. D.1 stub: confirm we never re-init.
        if self.initialised {
            return Ok(());
        }
        self.initialised = true;
        Ok(())
    }

    fn open_stream(
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
        self.stream_open = true;
        Ok(Self::PCM_OUT_STREAM_ID)
    }

    fn submit_frames(&mut self, stream_id: u32, bytes: &[u8]) -> Result<usize, AudioError> {
        if stream_id != Self::PCM_OUT_STREAM_ID || !self.stream_open {
            return Err(AudioError::InvalidArgument);
        }
        // D.2: write into the PCM ring + advance LVI. D.1 stub:
        // accept the bytes for accounting only.
        self.frames_submitted = self.frames_submitted.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }

    fn drain(&mut self, stream_id: u32) -> Result<(), AudioError> {
        if stream_id != Self::PCM_OUT_STREAM_ID || !self.stream_open {
            return Err(AudioError::InvalidArgument);
        }
        Ok(())
    }

    fn close_stream(&mut self, stream_id: u32) -> Result<(), AudioError> {
        if stream_id != Self::PCM_OUT_STREAM_ID {
            return Err(AudioError::InvalidArgument);
        }
        self.stream_open = false;
        Ok(())
    }

    fn handle_irq(&mut self) -> Result<IrqEvent, AudioError> {
        // D.2: read SR, classify, ack. D.1 stub: report "no event".
        Ok(IrqEvent::None)
    }
}

/// Snapshot of the running stats counters returned by
/// [`Ac97Backend::stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub frames_submitted: u64,
    pub frames_consumed: u64,
    pub underrun_count: u32,
}

// ---------------------------------------------------------------------------
// Tests — Track D.2 host coverage
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    // -- FakeMmio ----------------------------------------------------------
    //
    // Mirror of `userspace/drivers/e1000/src/init.rs::FakeMmio` adapted
    // for AC'97's two-BAR + 8/16/32-bit register access pattern.  Every
    // write is recorded so the register-ordering tests can assert the
    // reset → BDBAR → LVI → CR sequence.

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
            // Self-clearing behavior for CR.RR — the per-stream reset
            // bit clears immediately on real hardware once the reset
            // completes; the fake mirrors that so `reset_stream` can
            // converge without spinning.
            if bar == BAR_NABM && offset == nabm::PCM_OUT_BASE + nabm::CR && value & cr_bits::RR != 0 {
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

    // -- D.2 production-path tests against `Ac97Logic` ---------------------

    /// Acceptance bullet: reset → status reads → DMA buffer programming.
    /// `init_controller` must perform exactly those steps in that order.
    #[test]
    fn init_controller_writes_reset_then_clears_volume_then_programs_rate() {
        let mmio = FakeMmio::new();
        // Pretend the codec reports "ready" after reset.  The
        // EXT_AUDIO_STATUS_CTRL register's VRA bit is required before
        // the rate register is honored.
        init_controller(&mmio).expect("init succeeds on a responsive codec");
        let writes = mmio.writes();

        // Required sequence: NAM RESET → MASTER_VOLUME → PCM_OUT_VOLUME
        // → EXT_AUDIO_STATUS_CTRL (set VRA) → PCM_FRONT_DAC_RATE.
        let nam_writes: Vec<&(u8, usize, u32, u8)> =
            writes.iter().filter(|w| w.0 == BAR_NAM).collect();
        let positions: Vec<usize> = nam_writes.iter().map(|w| w.1).collect();
        let pos_reset = positions.iter().position(|&o| o == nam::RESET);
        let pos_master = positions.iter().position(|&o| o == nam::MASTER_VOLUME);
        let pos_pcmout = positions.iter().position(|&o| o == nam::PCM_OUT_VOLUME);
        let pos_vra = positions.iter().position(|&o| o == nam::EXT_AUDIO_STATUS_CTRL);
        let pos_rate = positions.iter().position(|&o| o == nam::PCM_FRONT_DAC_RATE);
        assert!(pos_reset.is_some(), "RESET write must occur");
        assert!(pos_master.is_some(), "MASTER_VOLUME write must occur");
        assert!(pos_pcmout.is_some(), "PCM_OUT_VOLUME write must occur");
        assert!(pos_vra.is_some(), "EXT_AUDIO_STATUS_CTRL.VRA write must occur");
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
        // Both volume registers must be written with mute clear and a
        // non-mute attenuation value.  Bit 15 = mute (per AC'97 spec);
        // 0x0000 means full volume on the inverted-attenuation scale.
        let mv = mmio.read_u16(BAR_NAM, nam::MASTER_VOLUME);
        let pv = mmio.read_u16(BAR_NAM, nam::PCM_OUT_VOLUME);
        assert_eq!(mv & 0x8000, 0, "master volume must be unmuted");
        assert_eq!(pv & 0x8000, 0, "pcm-out volume must be unmuted");
    }

    #[test]
    fn init_controller_programs_48khz_sample_rate() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        // AC'97 spec: PCM_FRONT_DAC_RATE is a 16-bit register holding
        // the requested rate in Hz directly (e.g. 0xBB80 == 48000).
        let r = mmio.read_u16(BAR_NAM, nam::PCM_FRONT_DAC_RATE);
        assert_eq!(r, SAMPLE_RATE_HZ);
    }

    #[test]
    fn open_stream_programs_bdbar_then_lvi_then_cr_run() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        let bdl_iova: u64 = 0x0000_0001_DEAD_BEEF;
        open_pcm_out_stream(&mmio, bdl_iova).expect("open succeeds");

        // Acceptance: BDBAR before LVI before CR run-bit.
        let nabm_offsets: Vec<usize> = mmio
            .writes()
            .iter()
            .filter(|w| w.0 == BAR_NABM)
            .map(|w| w.1)
            .collect();
        let pos_bdbar = nabm_offsets
            .iter()
            .position(|&o| o == nabm::PCM_OUT_BASE + nabm::BDBAR);
        let pos_lvi = nabm_offsets
            .iter()
            .position(|&o| o == nabm::PCM_OUT_BASE + nabm::LVI);
        let pos_cr = nabm_offsets
            .iter()
            .position(|&o| o == nabm::PCM_OUT_BASE + nabm::CR);
        assert!(pos_bdbar.is_some(), "BDBAR write required");
        assert!(pos_lvi.is_some(), "LVI write required");
        assert!(pos_cr.is_some(), "CR write required");
        assert!(pos_bdbar < pos_lvi, "BDBAR must precede LVI");
        assert!(pos_lvi < pos_cr, "LVI must precede CR");
    }

    #[test]
    fn open_stream_writes_bdbar_with_low32_of_iova() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        // AC'97 BDBAR is a 32-bit register; the high half of a 64-bit
        // IOVA must be discarded by the driver (AC'97 cannot DMA above
        // 4 GiB on classic ICH).  Phase 55a's identity-fallback IOVAs
        // live in low memory so this is enforced upstream too.
        let bdl_iova: u64 = 0x0000_0000_C0FF_EE00;
        open_pcm_out_stream(&mmio, bdl_iova).expect("open");
        let bdbar = mmio.read_u32(BAR_NABM, nabm::PCM_OUT_BASE + nabm::BDBAR);
        assert_eq!(bdbar, 0xC0FF_EE00);
    }

    #[test]
    fn open_stream_initial_lvi_is_zero() {
        // Phase 57 D.2 acceptance: LVI starts at 0 (the BDL is empty
        // until SubmitFrames has appended a buffer); the CR.RPBM bit
        // is enabled afterward so the controller idles waiting for
        // submissions.
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
        // Acceptance: close must include a CR.RR write so the stream
        // returns to a clean state for the next opener.  The fake
        // self-clears RR on the same write, so we observe it in the
        // write log rather than the reg map.
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
        assert!(!rr_writes.is_empty(), "close must issue at least one CR.RR write");
    }

    #[test]
    fn handle_irq_reads_sr_then_acks_observed_bits() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        open_pcm_out_stream(&mmio, 0x1000).expect("open");
        // Pretend hardware raised LVBCI + BCIS.
        mmio.set_u16(
            BAR_NABM,
            nabm::PCM_OUT_BASE + nabm::SR,
            sr_bits::LVBCI | sr_bits::BCIS,
        );
        let event = handle_pcm_out_irq(&mmio, /*ring_was_empty=*/ false).expect("irq");
        // LVBCI takes priority — see classify_sr.
        assert_eq!(event, IrqEvent::LastValidIndex);
        // The driver must clear the observed W1C bits.
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
        // BDL has 32 entries each up to 64 KiB; submit one buffer and
        // assert LVI advanced to 0 (first slot) and the BDL entry
        // describes the submission.
        let bdl_iova = 0x2_0000;
        logic
            .submit_buffer(bdl_iova, 0xAAAA_AAAA, 1024)
            .expect("submit");
        // BufferDescriptor is `repr(C, packed)`, so field accesses
        // require a copy through a local first.
        let entry0 = logic.bdl()[0];
        assert_eq!({ entry0.phys_addr }, 0xAAAA_AAAA);
        assert_eq!({ entry0.samples }, 1024);
        assert_eq!(logic.lvi(), 0);

        // Submit a second buffer; LVI must move to 1.
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
        // BDL_MAX_SAMPLES is 0xFFFE; 0x10000 must be rejected.
        let err = logic
            .submit_buffer(0x1000, 0, 0x10000)
            .expect_err("oversize must be rejected");
        assert_eq!(err, AudioError::InvalidArgument);
    }

    #[test]
    fn submit_buffer_returns_busy_when_bdl_is_full() {
        let mut logic = Ac97Logic::new();
        // Fill every BDL slot.
        for _ in 0..BDL_ENTRIES {
            logic.submit_buffer(0x1000, 0xCAFE_F00D, 64).expect("submit");
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
        // Hardware advanced CIV from 0 → 1 and signalled BCIS.
        let event = logic.observe_irq(sr_bits::BCIS, /*new_civ=*/ 1);
        assert_eq!(event, IrqEvent::Empty);
        assert_eq!(logic.frames_consumed(), 64);

        // Hardware fired FIFOE while the producer ring was empty.
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
        // DCH and CELV are *not* W1C — they reflect device state.
        // Ack must not write them back even if observed.
        let observed = sr_bits::DCH | sr_bits::CELV | sr_bits::BCIS;
        assert_eq!(sr_ack_value(observed), sr_bits::BCIS);
    }
}
