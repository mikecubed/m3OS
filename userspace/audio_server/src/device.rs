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
// Tests — Track D.2 host coverage (lands red in next commit)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // The D.2 register-ordering tests + FakeMmio scaffold land in the
    // next commit. Track D.1 commits a single placeholder so this
    // module's `#[cfg(test)]` block compiles green for the scaffold.

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
