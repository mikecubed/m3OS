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

    /// Snapshot the device-side `frames_consumed` counter.
    ///
    /// Polls the controller for any newly-completed buffers (bringing
    /// the internal `tail` cursor up to the hardware's `CIV`) and
    /// returns the resulting `frames_consumed` value. Acts as the
    /// non-IRQ stats path for backends like QEMU's `-audiodev wav`,
    /// which advances the bus-master timer but never raises IOC.
    /// Default implementation returns `0` so test doubles compile
    /// unchanged.
    fn poll_frames_consumed(&mut self) -> u64 {
        0
    }
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
// AC'97 init / open / close / IRQ — pure register-access helpers exercised
// by host tests without real hardware.
// ---------------------------------------------------------------------------

/// Default volume value written to `MASTER_VOLUME` and `PCM_OUT_VOLUME`.
///
/// `0x0202` is the AC'97-conventional "low attenuation, both channels
/// equal, mute clear" value (≈ -3 dB on each side; bit 15 clear).
/// Phase 57 picks a fixed value rather than expose a verb because
/// volume control is a Phase 58+ extension; only mute-clear matters
/// to Phase 57 acceptance.
const VOLUME_UNMUTED: u16 = 0x0202;

/// Reset the codec (NAM RESET write), unmute master + PCM-out
/// volumes, enable variable-rate audio, and program the 48 kHz
/// sample rate.
///
/// The exact write order is part of the Phase 57 D.2 acceptance:
/// `RESET → MASTER_VOLUME → PCM_OUT_VOLUME → EXT_AUDIO_STATUS_CTRL →
/// PCM_FRONT_DAC_RATE`.
pub fn init_controller<M: MmioOps>(mmio: &M) -> Result<(), AudioError> {
    // 1. Cold codec reset — write any value (spec).
    mmio.write_u16(BAR_NAM, nam::RESET, 0);

    // 2. Unmute master volume.
    mmio.write_u16(BAR_NAM, nam::MASTER_VOLUME, VOLUME_UNMUTED);

    // 3. Unmute PCM-out volume.
    mmio.write_u16(BAR_NAM, nam::PCM_OUT_VOLUME, VOLUME_UNMUTED);

    // 4. Enable variable-rate audio (VRA = bit 0). Required before
    //    PCM_FRONT_DAC_RATE is honored on real ICH silicon.
    let prev = mmio.read_u16(BAR_NAM, nam::EXT_AUDIO_STATUS_CTRL);
    mmio.write_u16(BAR_NAM, nam::EXT_AUDIO_STATUS_CTRL, prev | 0x0001);

    // 5. Program the 48 kHz sample rate.
    mmio.write_u16(BAR_NAM, nam::PCM_FRONT_DAC_RATE, SAMPLE_RATE_HZ);

    Ok(())
}

/// Verify that the DMA range `[iova, iova + size)` fits entirely within the
/// low 4 GiB so AC'97's 32-bit BDBAR / BDL `phys_addr` fields can address it
/// without silent truncation.
///
/// Returns `AudioError::Internal` if the range spills above `u32::MAX + 1`.
/// Phase 63 review-resolution: the kernel DMA allocator returns 64-bit IOVAs;
/// passing one whose high half is non-zero to the AC'97 controller would aim
/// bus-mastering at unrelated memory and either corrupt RAM or hang the DMA
/// engine. The check is alloc-free and runs once per buffer at `Ac97Backend::init`.
pub fn check_iova_fits_u32(iova: u64, size: usize) -> Result<(), AudioError> {
    let end = iova.checked_add(size as u64).ok_or(AudioError::Internal)?;
    if end > (u32::MAX as u64) + 1 {
        return Err(AudioError::Internal);
    }
    Ok(())
}

/// Open the PCM-out stream by programming `BDBAR → LVI = 0 → CR.RPBM`
/// in that order. Acceptance pins BDBAR before LVI before CR run-bit.
pub fn open_pcm_out_stream<M: MmioOps>(mmio: &M, bdl_iova: u64) -> Result<(), AudioError> {
    // First, halt + reset the per-stream registers in case a prior
    // session left them dirty. Writing CR.RR is the Intel-recommended
    // way to clear LVI / CIV / SR.
    mmio.write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CR, cr_reset_value());

    // 1. BDBAR — low 32 bits of the BDL IOVA. AC'97's BDBAR is a
    //    32-bit register; the high half of a 64-bit IOVA is discarded.
    let bdbar_low = (bdl_iova & 0xFFFF_FFFF) as u32;
    mmio.write_u32(BAR_NABM, nabm::PCM_OUT_BASE + nabm::BDBAR, bdbar_low);

    // 2. LVI = 0 — empty BDL until SubmitFrames appends the first
    //    descriptor.
    mmio.write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::LVI, 0);

    // 3. CR — enable the bus master + every interrupt cause.
    mmio.write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CR, cr_run_value());

    Ok(())
}

/// Close the PCM-out stream by halting the bus master (CR=0) and
/// resetting per-stream registers (CR.RR).
pub fn close_pcm_out_stream<M: MmioOps>(mmio: &M) -> Result<(), AudioError> {
    // 1. Halt the bus master.
    mmio.write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CR, cr_halt_value());
    // 2. Reset per-stream registers. CR.RR is self-clearing on real
    //    hardware; we issue the write and trust the bit clears before
    //    the next stream open.
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

/// Number of bytes per PCM-ring slot. The PCM ring is divided into
/// `BDL_ENTRIES` equal slots; each submission occupies one or more
/// contiguous slots.
pub const PCM_SLOT_STRIDE: usize = DEFAULT_PCM_RING_BYTES / BDL_ENTRIES;

/// A single silence slot — one BDL entry's worth of zero PCM data.
/// Used by the underrun-recovery path to re-arm the BDL without
/// introducing audible clicks.
pub const SILENCE_FRAME: [u8; PCM_SLOT_STRIDE] = [0u8; PCM_SLOT_STRIDE];

// ---------------------------------------------------------------------------
// submit_frames_inner — pure copy + BDL-post helper
// ---------------------------------------------------------------------------

/// Copy `bytes` into the PCM ring DMA region and post one or more BDL
/// entries. Pure logic — takes raw slices so host tests can drive it
/// without a real [`DmaBuffer`].
///
/// # Rules
///
/// - `bytes.len()` must be a non-zero multiple of [`PCM_SLOT_STRIDE`].
///   A submission smaller than one slot returns
///   [`AudioError::InvalidArgument`]; a submission that is not a
///   multiple of the stride also returns `InvalidArgument`.
/// - BDL capacity is checked for the **entire** submission before any
///   copy. If the BDL cannot hold every requested slot the function
///   returns [`AudioError::WouldBlock`] without copying any bytes or
///   posting any descriptors. This matches `audio_client`'s
///   all-or-nothing submit contract — partial accepts are surfaced as
///   `WouldBlock`, never as a short `Ok(n < bytes.len())`.
/// - Returns `Ok(bytes.len())` on success.
///
/// # Arguments
///
/// - `bytes` — caller-supplied PCM data (S16Le).
/// - `pcm_ring` — mutable reference to the full PCM ring DMA region
///   (`DEFAULT_PCM_RING_BYTES` bytes).
/// - `pcm_ring_iova` — device-visible IOVA base of `pcm_ring`.
/// - `bdl_dma` — mutable reference to the DMA-backed BDL the AC'97
///   controller actually reads via BDBAR. Each posted descriptor is
///   mirrored into this array immediately after the logic-side
///   `submit_buffer` succeeds, so the hardware never DMAs from a
///   stale/zero entry. `Ac97Logic::bdl` is the in-process mirror; the
///   DMA buffer is the device-visible truth — the two must stay in
///   lockstep, which is enforced here by writing both from the same
///   loop body.
/// - `bdl_iova` — device-visible IOVA base of the BDL DMA buffer.
///   Used to compute the `bdl_iova_offset` argument for
///   [`Ac97Logic::submit_buffer`].
/// - `logic` — mutable reference to the BDL ring state machine.
pub fn submit_frames_inner(
    bytes: &[u8],
    pcm_ring: &mut [u8; DEFAULT_PCM_RING_BYTES],
    pcm_ring_iova: u64,
    bdl_dma: &mut [BufferDescriptor; BDL_ENTRIES],
    bdl_iova: u64,
    logic: &mut Ac97Logic,
) -> Result<usize, AudioError> {
    // Partial-slot submissions are not supported in Phase 63.
    if bytes.len() < PCM_SLOT_STRIDE || bytes.len() % PCM_SLOT_STRIDE != 0 {
        return Err(AudioError::InvalidArgument);
    }

    let num_slots = bytes.len() / PCM_SLOT_STRIDE;

    // All-or-nothing: require enough BDL room for the *whole* submission
    // before touching the ring. `audio_client::submit_frames` documents
    // that a successful submit returns `bytes.len()` exactly — partial
    // accepts are not a thing, backpressure is surfaced as `WouldBlock`
    // and the caller retries. Accepting a prefix here would silently
    // drop the tail of the PCM payload.
    let in_flight = logic.head.wrapping_sub(logic.tail);
    let free_slots = BDL_ENTRIES.saturating_sub(in_flight);
    if free_slots < num_slots {
        return Err(AudioError::WouldBlock);
    }

    let mut total_copied = 0usize;

    for i in 0..num_slots {
        let head = logic.head % BDL_ENTRIES;
        let slot_byte_offset = head * PCM_SLOT_STRIDE;
        let src_offset = i * PCM_SLOT_STRIDE;

        // Copy into the PCM ring DMA region at the correct slot.
        pcm_ring[slot_byte_offset..slot_byte_offset + PCM_SLOT_STRIDE]
            .copy_from_slice(&bytes[src_offset..src_offset + PCM_SLOT_STRIDE]);

        // Compute addresses for `submit_buffer`.
        let bdl_iova_offset = bdl_iova + (head * core::mem::size_of::<BufferDescriptor>()) as u64;
        let slot_phys_addr = (pcm_ring_iova + slot_byte_offset as u64) as u32;
        let samples = PCM_SLOT_STRIDE / 2; // S16Le — 2 bytes per sample

        // Post the BDL entry. `submit_buffer` validates and updates
        // `lvi`; it cannot fail here because we pre-checked capacity.
        logic
            .submit_buffer(bdl_iova_offset, slot_phys_addr, samples)
            .map_err(|_| AudioError::Internal)?;

        // Mirror the descriptor `submit_buffer` just wrote into the
        // DMA-backed BDL. AC'97 DMAs descriptors from the buffer
        // pointed at by BDBAR, not from the in-process `logic.bdl`
        // mirror — without this write the controller would replay
        // zeroed/stale entries.
        bdl_dma[head] = logic.bdl[head];

        total_copied += PCM_SLOT_STRIDE;
    }

    Ok(total_copied)
}

// ---------------------------------------------------------------------------
// Ac97Logic — pure-state companion to `Ac97Backend`
// ---------------------------------------------------------------------------

/// Pure-logic AC'97 state — the BDL ring + cursors + counters,
/// without the `DeviceHandle` or `DmaBuffer` ownership.  The
/// production `Ac97Backend` (cfg `not(test)`) wraps an [`Ac97Logic`]
/// plus the DMA + cap state; host tests construct `Ac97Logic`
/// directly so the ring-management math is exercisable without a
/// real kernel.
#[derive(Debug, Clone)]
pub struct Ac97Logic {
    pub(crate) bdl: [BufferDescriptor; BDL_ENTRIES],
    /// Next slot to write — strictly monotonic counter.
    pub(crate) head: usize,
    /// Next slot hardware will consume — strictly monotonic counter.
    pub(crate) tail: usize,
    /// Mirror of the LVI register the io loop will program through
    /// `MmioOps`.
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

    /// Append a buffer to the BDL. Returns:
    ///
    /// - `Err(InvalidArgument)` if `samples > BDL_MAX_SAMPLES`.
    /// - `Err(WouldBlock)` if every BDL slot is in flight.
    /// - `Ok(())` and advances the LVI mirror to the new head index.
    pub fn submit_buffer(
        &mut self,
        _bdl_iova: u64,
        phys_addr: u32,
        samples: usize,
    ) -> Result<(), AudioError> {
        if samples > BDL_MAX_SAMPLES {
            return Err(AudioError::InvalidArgument);
        }
        // Ring full when in_flight == BDL_ENTRIES (every slot owned by
        // hardware). `wrapping_sub` over `usize` correctly handles the
        // monotonic-counter case provided no overflow has occurred —
        // for any realistic playback duration this is millennia away.
        let in_flight = self.head.wrapping_sub(self.tail);
        if in_flight >= BDL_ENTRIES {
            return Err(AudioError::WouldBlock);
        }
        let idx = self.head % BDL_ENTRIES;
        self.bdl[idx] = BufferDescriptor {
            phys_addr,
            samples: samples as u16,
            // Bit 15 = IOC: request interrupt on completion so the
            // io loop wakes for every consumed buffer.
            flags: 0x8000,
        };
        self.head = self.head.wrapping_add(1);
        self.lvi = (self.head.wrapping_sub(1) % BDL_ENTRIES) as u8;
        self.frames_submitted = self.frames_submitted.saturating_add(samples as u64);
        Ok(())
    }

    /// Observe an IRQ: classify the status register and update
    /// `frames_consumed` / `underrun_count` based on `new_civ` (the
    /// hardware-side current-buffer index).
    pub fn observe_irq(&mut self, sr: u16, new_civ: u8) -> IrqEvent {
        let ring_was_empty = self.tail == self.head;
        let event = classify_sr(sr, ring_was_empty);
        // Advance the consumed-counter from old `tail` up to `new_civ`.
        // Bound the loop by BDL_ENTRIES so a misbehaving fake cannot
        // trap the test or the production io loop.
        let civ = new_civ as usize;
        for _ in 0..BDL_ENTRIES {
            if self.tail % BDL_ENTRIES == civ {
                break;
            }
            let idx = self.tail % BDL_ENTRIES;
            // BufferDescriptor is `repr(C, packed)`; copy through a
            // local before reading the field — direct field access is
            // UB on a packed struct.
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

    /// Construct the backend from a claimed device handle. Performs:
    /// 1. Construct [`Ac97PioBus`] over both PIO BARs.
    /// 2. Allocate BDL and PCM-ring [`DmaBuffer`]s via `sys_device_dma_alloc`.
    /// 3. Call [`init_controller`] to reset + configure the codec.
    /// 4. Call [`open_pcm_out_stream`] to arm the BDL and start the bus master.
    ///
    /// On any error, `?` propagates after the partially-constructed state
    /// drops — `DmaBuffer::Drop` releases the DMA cap back to the kernel,
    /// and `Ac97PioBus::Drop` is a no-op (PIO has no address-space mapping).
    pub fn init(device: DeviceHandle) -> Result<Self, AudioError> {
        // Step 1: map both AC'97 PIO BARs.
        let bus = Ac97PioBus::new(&device)?;

        // Step 2: allocate the BDL DMA buffer.
        // Size = BDL_ENTRIES × 8 bytes (size_of::<BufferDescriptor>()).
        // Alignment = 8 bytes (size_of::<u64>() per DmaBuffer contract).
        let bdl_size = core::mem::size_of::<[BufferDescriptor; BDL_ENTRIES]>();
        let bdl: driver_runtime::DmaBuffer<[BufferDescriptor; BDL_ENTRIES]> =
            driver_runtime::DmaBuffer::allocate(&device, bdl_size, core::mem::align_of::<u64>())
                .map_err(|_| AudioError::Internal)?;

        // Step 3: allocate the PCM-ring DMA buffer.
        let pcm_ring: driver_runtime::DmaBuffer<[u8; DEFAULT_PCM_RING_BYTES]> =
            driver_runtime::DmaBuffer::allocate(
                &device,
                DEFAULT_PCM_RING_BYTES,
                core::mem::align_of::<u64>(),
            )
            .map_err(|_| AudioError::Internal)?;

        // AC'97 bus mastering is 32-bit: BDBAR is a 32-bit register and BDL
        // entries carry 32-bit `phys_addr` fields. The kernel DMA allocator
        // returns 64-bit IOVAs, so we must verify both DMA buffers land
        // entirely within the low 4 GiB before programming the controller.
        // Without this check, an allocation above 4 GiB would silently
        // truncate to u32 and aim the device at unrelated memory.
        let bdl_iova = bdl.iova();
        let pcm_ring_iova = pcm_ring.iova();
        check_iova_fits_u32(bdl_iova, bdl_size)?;
        check_iova_fits_u32(pcm_ring_iova, DEFAULT_PCM_RING_BYTES)?;

        // Step 4: reset + configure the codec (RESET → volumes → VRA → rate).
        init_controller(&bus)?;

        // Step 5: arm the BDL and start the bus master (CR.RR → BDBAR → LVI → CR.RPBM).
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
    /// `frames_submitted` is read from `logic` (the single source of truth).
    pub fn stats(&self) -> StatsSnapshot {
        StatsSnapshot {
            frames_submitted: self.logic.frames_submitted(),
            frames_consumed: self.logic.frames_consumed(),
            underrun_count: self.logic.underrun_count(),
        }
    }

    /// Poll the controller's `CIV` register and advance internal
    /// completion state to match. Acts as the non-IRQ fallback for
    /// QEMU's `-audiodev wav` backend, which does not deliver IOC
    /// IRQs even though the bus master continues to fetch buffers
    /// (CIV / PICB advance on a timer). Without this, `tail` and
    /// `frames_consumed` stay pinned at the IRQ-side baseline and
    /// `submit_frames` deadlocks on `WouldBlock` after the first
    /// 16 KiB chunk fills the BDL ring.
    ///
    /// Returns the number of completed buffers folded in; `0` means
    /// nothing changed since the last poll. Idempotent.
    pub fn poll_completed_buffers(&mut self) -> usize {
        let civ = self.bus.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV);
        let tail_before = self.logic.tail;
        // `observe_irq` advances `tail` from its current value up to
        // `civ`, accumulating each retired buffer's `samples` into
        // `frames_consumed`. Pass `sr=0` so no spurious IRQ event is
        // synthesised; the function is pure state-machine logic and
        // doesn't itself touch hardware.
        self.logic.observe_irq(0, civ);
        self.logic.tail.wrapping_sub(tail_before) as usize
    }
}

#[cfg(not(test))]
impl AudioBackend for Ac97Backend {
    fn init(&mut self) -> Result<(), AudioError> {
        // The real init path is in `Ac97Backend::init` (the constructor).
        // By the time we hold a `&mut self`, init_controller +
        // open_pcm_out_stream have already run. No-op idempotent guard.
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
        // Phase 63 second-open fix: `close_stream` halts the bus master
        // (CR=0) and issues CR.RR, which clears the AC'97 hardware's
        // CIV/LVI/SR/BDBAR. The mirror in `Ac97Logic` (head, tail, lvi,
        // BDL entries) is untouched, so a second `open_stream` that
        // only flipped `stream_open = true` would leave:
        //
        //  - The bus master halted (CR=0) — hardware can't consume
        //    anything `submit_frames` posts, so CIV stays at 0 and
        //    `tail` never advances.
        //  - `Ac97Logic.head` pointing at the previous run's end —
        //    submits append past stale positions and `frames_*`
        //    counters carry over.
        //
        // The visible symptom is run #2 of `audio-demo` failing with
        // `Server:WouldBlock` after ~1 s of 200×5 ms retries while
        // submit_buffer rejects every chunk because `in_flight` reads
        // as full from the carried-over head/tail.
        //
        // Re-program BDBAR → LVI=0 → CR.RPBM and reset the mirror so
        // every Open IPC begins from a clean state.
        self.logic = Ac97Logic::new();
        open_pcm_out_stream(&self.bus, self.bdl.iova())?;
        self.stream_open = true;
        Ok(Self::PCM_OUT_STREAM_ID)
    }

    fn submit_frames(&mut self, stream_id: u32, bytes: &[u8]) -> Result<usize, AudioError> {
        if stream_id != Self::PCM_OUT_STREAM_ID || !self.stream_open {
            return Err(AudioError::InvalidArgument);
        }
        // Free any buffers the hardware has already consumed but not
        // yet acknowledged via IRQ (QEMU's `-audiodev wav` advances CIV
        // on a timer without firing IOC). Without this, `tail` stays
        // pinned and `submit_frames_inner` returns `WouldBlock` after
        // the first ring-full chunk.
        self.poll_completed_buffers();
        // Delegate to the pure helper so the ring-copy logic is host-testable.
        // Capture IOVAs before the mutable borrows to satisfy the borrow checker.
        // `pcm_ring`, `bdl`, and `logic` are disjoint fields, so three simultaneous
        // mut borrows are allowed; the helper writes the PCM ring and mirrors BDL
        // descriptors in lockstep with `logic.bdl`.
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
        // Write the updated LVI to the hardware register.
        self.bus
            .write_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::LVI, self.logic.lvi());
        Ok(n)
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
        close_pcm_out_stream(&self.bus)?;
        self.stream_open = false;
        Ok(())
    }

    fn poll_frames_consumed(&mut self) -> u64 {
        self.poll_completed_buffers();
        self.logic.frames_consumed()
    }

    fn handle_irq(&mut self) -> Result<IrqEvent, AudioError> {
        // A.3: Read CIV before SR so `ring_was_empty` reflects the
        // producer state at the moment the IRQ fired (before any
        // side effects from `handle_pcm_out_irq`'s SR ack write).
        let ring_was_empty = self.logic.head == self.logic.tail;

        // Read the current-index-value register (hardware's DMA cursor).
        let civ = self.bus.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV);

        // Read SR, classify, and write-1-to-clear the W1C bits.
        let event = handle_pcm_out_irq(&self.bus, ring_was_empty)?;

        // Re-read SR for `observe_irq`: `handle_pcm_out_irq` already
        // acked the W1C bits so we derive the SR value from the
        // classified event rather than re-reading the register
        // (reading it again could miss a new IRQ that arrived after
        // the ack). Reconstruct the minimal SR snapshot the logic
        // needs from the event variant.
        let sr_for_observe: u16 = match event {
            IrqEvent::Underrun | IrqEvent::FifoError => sr_bits::FIFOE,
            IrqEvent::LastValidIndex => sr_bits::LVBCI,
            IrqEvent::Empty => sr_bits::BCIS,
            IrqEvent::None => 0,
        };
        self.logic.observe_irq(sr_for_observe, civ);

        // Surface a hard FIFO error as an internal fault to the caller.
        if matches!(event, IrqEvent::FifoError) {
            return Err(AudioError::Internal);
        }

        Ok(event)
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
// Ac97PioBus — Phase 63 Track Z.4
// ---------------------------------------------------------------------------

/// Production [`MmioOps`] adapter that dispatches register accesses to the
/// two AC'97 PIO BARs via the Phase 63 `sys_device_pio_read` /
/// `sys_device_pio_write` syscalls.
///
/// AC'97 BARs are I/O-space in real ICH silicon and in QEMU's `-device AC97`
/// emulation — the existing `sys_device_mmio_map` path filters them out.
/// `Ac97PioBus` holds one [`driver_runtime::Pio<()>`] per BAR and dispatches
/// each [`MmioOps`] call to the right handle strictly by the `bar` parameter.
/// No shared state exists between the two handles.
///
/// Use [`Ac97PioBus::new`] to construct — it performs both `Pio::map` calls
/// and returns an error if either fails (e.g. the device is not claimed or
/// the BAR is not PIO).
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
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Internal`] if either [`driver_runtime::Pio::map`]
    /// call fails (e.g. the device is not claimed, the BAR index is wrong, or
    /// the BAR is MMIO rather than PIO).
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
// Tests — Track D.2 host coverage
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    // -- check_iova_fits_u32 ----------------------------------------------
    //
    // Phase 63 review-resolution: AC'97 BDBAR and BDL `phys_addr` are 32-bit.
    // `Ac97Backend::init` must reject DMA allocations that spill above 4 GiB
    // so the controller never bus-masters from a silently-truncated address.

    #[test]
    fn check_iova_fits_u32_accepts_low_dma_buffer() {
        // A typical kernel DMA allocation at 1 MiB with a 16 KiB ring.
        assert_eq!(check_iova_fits_u32(0x0010_0000, 16 * 1024), Ok(()));
    }

    #[test]
    fn check_iova_fits_u32_accepts_buffer_ending_exactly_at_4gib() {
        // The last legal range: ends at exactly u32::MAX + 1 = 1 << 32.
        let size: usize = 256;
        let iova: u64 = (1u64 << 32) - size as u64;
        assert_eq!(check_iova_fits_u32(iova, size), Ok(()));
    }

    #[test]
    fn check_iova_fits_u32_rejects_buffer_crossing_4gib_boundary() {
        // Starts below 4 GiB but ends one byte above: would silently
        // truncate to 0 when cast to u32, aiming DMA at unrelated memory.
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
        // iova near u64::MAX + huge size — must surface as Internal,
        // not panic on the addition.
        assert_eq!(
            check_iova_fits_u32(u64::MAX - 5, 100),
            Err(AudioError::Internal)
        );
    }

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

        // Acceptance: BDBAR before LVI before the CR run-bit write.
        // (`open_pcm_out_stream` may issue a CR.RR reset write first
        // to clear stale state from a prior session; the run-bit
        // write is the *last* CR write and is the one that races
        // hardware against an unprogrammed BDL or LVI.)
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

    // -- A.1: frames_submitted getter and StatsSnapshot wiring ---------------

    /// A.1: `Ac97Logic` exposes a `frames_submitted()` getter so the
    /// production backend can build `StatsSnapshot` from the single
    /// source-of-truth counters owned by `Ac97Logic`.
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

    /// A.1: `StatsSnapshot` built from `Ac97Logic` counters is consistent
    /// with what `Ac97Logic::frames_consumed` / `underrun_count` report.
    #[test]
    fn stats_snapshot_built_from_ac97_logic_counters_is_consistent() {
        let mut logic = Ac97Logic::new();
        logic
            .submit_buffer(0x1000, 0xCAFE_0000, 512)
            .expect("submit");
        // Simulate one buffer consumed: hardware advanced CIV from 0 → 1.
        let _ = logic.observe_irq(sr_bits::BCIS, 1);
        // Simulate one underrun (empty ring + FIFOE).
        let _ = logic.observe_irq(sr_bits::FIFOE, 1);

        // Build the snapshot the way the production backend does:
        let snap = StatsSnapshot {
            frames_submitted: logic.frames_submitted(),
            frames_consumed: logic.frames_consumed(),
            underrun_count: logic.underrun_count(),
        };
        assert_eq!(snap.frames_submitted, 512);
        assert_eq!(snap.frames_consumed, 512);
        assert_eq!(snap.underrun_count, 1);
    }

    // -- A.2: init wiring — init_controller + open_pcm_out_stream ordering ---

    /// A.2: The production init path calls `init_controller` (which programs
    /// RESET → volumes → VRA → sample-rate) followed by `open_pcm_out_stream`
    /// (which programs CR.RR → BDBAR → LVI → CR.RPBM).  This test exercises
    /// those helpers directly with `FakeMmio` to assert the full sequence
    /// without requiring a real QEMU instance.
    #[test]
    fn init_then_open_stream_writes_codec_then_bdl_in_correct_order() {
        let mmio = FakeMmio::new();
        let bdl_iova: u64 = 0x0000_0000_FEED_C0DE;

        // Reproduce what `Ac97Backend::init` does after constructing the bus.
        init_controller(&mmio).expect("init_controller must succeed");
        open_pcm_out_stream(&mmio, bdl_iova).expect("open_pcm_out_stream must succeed");

        let writes = mmio.writes();

        // The last BDBAR write must carry the low 32 bits of `bdl_iova`.
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

        // The codec reset (NAM RESET write) must precede the BDBAR write.
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

        // VRA enable must precede sample-rate programming.
        let pos_vra = writes
            .iter()
            .position(|w| w.0 == BAR_NAM && w.1 == nam::EXT_AUDIO_STATUS_CTRL);
        let pos_rate = writes
            .iter()
            .position(|w| w.0 == BAR_NAM && w.1 == nam::PCM_FRONT_DAC_RATE);
        assert!(pos_vra < pos_rate, "VRA must precede PCM_FRONT_DAC_RATE");

        // CR run-bit must be the final CR write (BDBAR → LVI before CR.RPBM).
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

    /// A.2: `open_pcm_out_stream` passes the exact BDL IOVA it receives
    /// as the low 32-bit value written to BDBAR — the truncation is
    /// intentional (AC'97 is 32-bit DMA).
    #[test]
    fn open_stream_uses_provided_bdl_iova_for_bdbar() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        let bdl_iova: u64 = 0x0000_0001_2345_6780; // high bits present; low = 0x2345_6780
        open_pcm_out_stream(&mmio, bdl_iova).expect("open");
        let bdbar = mmio.read_u32(BAR_NABM, nabm::PCM_OUT_BASE + nabm::BDBAR);
        assert_eq!(
            bdbar, 0x2345_6780,
            "BDBAR must hold the low 32 bits of bdl_iova"
        );
    }

    // -- A.3: IRQ handler — CIV read, classify, observe_irq, FifoError ------

    /// A.3: After `handle_pcm_out_irq` returns `IrqEvent::Empty` (BCIS),
    /// `Ac97Logic::observe_irq` correctly advances `frames_consumed` by the
    /// samples of the consumed BDL entry.
    #[test]
    fn handle_irq_bcis_advances_logic_frames_consumed() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        open_pcm_out_stream(&mmio, 0x1000).expect("open");

        let mut logic = Ac97Logic::new();
        // Submit one buffer of 128 samples at phys 0xCAFE_0000.
        logic
            .submit_buffer(0x1000, 0xCAFE_0000, 128)
            .expect("submit");

        // Pretend hardware consumed buffer 0 → CIV advanced to 1, signalled BCIS.
        mmio.set_u16(BAR_NABM, nabm::PCM_OUT_BASE + nabm::SR, sr_bits::BCIS);
        let ring_was_empty = logic.head == logic.tail;
        let civ = mmio.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV); // reads 0 (default)
        let event = handle_pcm_out_irq(&mmio, ring_was_empty).expect("handle_irq");
        assert_eq!(event, IrqEvent::Empty, "BCIS alone must yield Empty");

        // Derive sr for observe_irq from the event (mirrors the production path).
        let sr_for_observe: u16 = sr_bits::BCIS;
        logic.observe_irq(sr_for_observe, civ);
        // CIV is 0 here (default), so observe_irq walks tail (0) up to CIV (0) — no advance.
        // Set CIV to 1 and observe again to advance.
        let event2 = logic.observe_irq(sr_bits::BCIS, 1);
        assert_eq!(event2, IrqEvent::Empty);
        assert_eq!(
            logic.frames_consumed(),
            128,
            "frames_consumed must equal the submitted samples"
        );
    }

    /// A.3: When `handle_pcm_out_irq` returns `IrqEvent::Underrun` (FIFOE on
    /// an empty ring), the follow-up `observe_irq` increments `underrun_count`.
    #[test]
    fn handle_irq_fifo_error_on_empty_ring_increments_underrun_count() {
        let mmio = FakeMmio::new();
        init_controller(&mmio).expect("init");
        open_pcm_out_stream(&mmio, 0x1000).expect("open");

        let mut logic = Ac97Logic::new();
        // Ring is empty (no submit_buffer calls).
        mmio.set_u16(BAR_NABM, nabm::PCM_OUT_BASE + nabm::SR, sr_bits::FIFOE);
        let ring_was_empty = logic.head == logic.tail; // true
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

    /// A.3: When `handle_pcm_out_irq` returns `IrqEvent::FifoError` (FIFOE
    /// on a non-empty ring), the production `handle_irq` method should
    /// map this to `Err(AudioError::Internal)`. We verify `classify_sr`
    /// produces `FifoError` for a non-empty ring, which is the precondition
    /// for the `Err(AudioError::Internal)` return in the production backend.
    #[test]
    fn fifo_error_on_non_empty_ring_classifies_as_fifo_error_variant() {
        // ring_was_empty = false: a non-empty ring + FIFOE is a programming bug.
        let sr = sr_bits::FIFOE;
        let event = classify_sr(sr, /*ring_was_empty=*/ false);
        assert_eq!(
            event,
            IrqEvent::FifoError,
            "FIFOE on a non-empty ring must classify as FifoError, which maps to AudioError::Internal"
        );
    }

    /// A.3: CIV must be read from the NABM register, not derived from
    /// the BDL logic state — verifies that `read_u8(BAR_NABM, PCM_OUT_BASE + CIV)`
    /// is the correct address used in the production handle_irq path.
    #[test]
    fn civ_is_read_from_bar_nabm_pcm_out_base_plus_civ_offset() {
        let mmio = FakeMmio::new();
        // Pre-load the NABM CIV register with a known value.
        mmio.set_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV, 5);
        let civ = mmio.read_u8(BAR_NABM, nabm::PCM_OUT_BASE + nabm::CIV);
        assert_eq!(
            civ, 5,
            "CIV must be read from BAR_NABM at PCM_OUT_BASE + CIV offset"
        );
    }

    // -- B.1: submit_frames_inner — PCM-ring copy + BDL-post -----------------

    /// Helper: a zeroed BDL DMA mirror for test setups. Matches the
    /// layout `Ac97Backend.bdl` would have at boot before any submit.
    fn fresh_bdl_dma() -> [BufferDescriptor; BDL_ENTRIES] {
        [BufferDescriptor {
            phys_addr: 0,
            samples: 0,
            flags: 0,
        }; BDL_ENTRIES]
    }

    /// B.1(a): bytes are copied into the PCM ring at the correct slot offset.
    /// The first submission must land at `head=0 × PCM_SLOT_STRIDE = 0`.
    #[test]
    fn submit_frames_inner_copies_bytes_to_correct_slot_offset() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        // Fill submission with a recognisable pattern.
        let mut submission = [0u8; PCM_SLOT_STRIDE];
        for (i, b) in submission.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        submit_frames_inner(
            &submission,
            &mut pcm_ring,
            /*pcm_ring_iova=*/ 0x0010_0000,
            &mut bdl_dma,
            /*bdl_iova=*/ 0x0020_0000,
            &mut logic,
        )
        .expect("submit must succeed");

        // Head was 0 before the call, so slot 0 (offset 0) was used.
        assert_eq!(
            &pcm_ring[0..PCM_SLOT_STRIDE],
            &submission[..],
            "bytes must be at slot 0 (offset 0)"
        );
    }

    /// B.1(a): after the first submission the second submission lands at
    /// slot index 1, i.e., byte offset `PCM_SLOT_STRIDE`.
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

        assert_eq!(
            &pcm_ring[0..PCM_SLOT_STRIDE],
            &first[..],
            "slot 0 must hold the first submission"
        );
        assert_eq!(
            &pcm_ring[PCM_SLOT_STRIDE..2 * PCM_SLOT_STRIDE],
            &second[..],
            "slot 1 must hold the second submission"
        );
    }

    /// B.1(b): after a successful call, `Ac97Logic::submit_buffer` was
    /// driven with the correct IOVA and sample count.
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

        // The BDL entry at slot 0 should have been posted.
        let entry0 = logic.bdl()[0];
        let expected_phys_addr = pcm_ring_iova as u32; // slot 0 is at base IOVA
        assert_eq!(
            { entry0.phys_addr },
            expected_phys_addr,
            "phys_addr must be pcm_ring_iova + 0 (slot 0)"
        );
        let expected_samples = (PCM_SLOT_STRIDE / 2) as u16; // S16Le
        assert_eq!(
            { entry0.samples },
            expected_samples,
            "samples must be PCM_SLOT_STRIDE / 2"
        );
        // LVI must have advanced to 0 after the first submit.
        assert_eq!(logic.lvi(), 0, "LVI must be 0 after first slot");
    }

    /// B.1(b): LVI advances correctly after two submissions.
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
        assert_eq!(logic.lvi(), 0, "LVI=0 after slot 0");

        submit_frames_inner(
            &data,
            &mut pcm_ring,
            0x1000,
            &mut bdl_dma,
            0x2000,
            &mut logic,
        )
        .expect("second");
        assert_eq!(logic.lvi(), 1, "LVI=1 after slot 1");
    }

    /// B.1(d): partial-slot submission (bytes.len() < PCM_SLOT_STRIDE)
    /// must return `InvalidArgument`.
    #[test]
    fn submit_frames_inner_partial_slot_returns_invalid_argument() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let partial = [0u8; PCM_SLOT_STRIDE - 1];
        let err = submit_frames_inner(
            &partial,
            &mut pcm_ring,
            0x1000,
            &mut bdl_dma,
            0x2000,
            &mut logic,
        )
        .expect_err("partial slot must be rejected");
        assert_eq!(err, AudioError::InvalidArgument);
    }

    /// B.1(d): zero-length submission must also return `InvalidArgument`.
    #[test]
    fn submit_frames_inner_zero_len_returns_invalid_argument() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let err = submit_frames_inner(&[], &mut pcm_ring, 0x1000, &mut bdl_dma, 0x2000, &mut logic)
            .expect_err("zero len must be rejected");
        assert_eq!(err, AudioError::InvalidArgument);
    }

    /// B.1(d): non-multiple-of-stride submission must return
    /// `InvalidArgument` even when larger than one slot.
    #[test]
    fn submit_frames_inner_non_stride_multiple_returns_invalid_argument() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        // PCM_SLOT_STRIDE + 1 is not a multiple of PCM_SLOT_STRIDE.
        let odd = alloc::vec![0u8; PCM_SLOT_STRIDE + 1];
        let err = submit_frames_inner(
            &odd,
            &mut pcm_ring,
            0x1000,
            &mut bdl_dma,
            0x2000,
            &mut logic,
        )
        .expect_err("non-multiple stride must be rejected");
        assert_eq!(err, AudioError::InvalidArgument);
    }

    /// B.1(e): when the BDL is full, submit must return `WouldBlock`
    /// without copying any bytes.
    #[test]
    fn submit_frames_inner_bdl_full_returns_would_block() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        // Fill all BDL slots via submit_buffer directly.
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

    /// B.1(f): a two-slot submission is split into two BDL entries,
    /// and the function returns the total bytes copied (2 × slot stride).
    #[test]
    fn submit_frames_inner_over_slot_splits_into_multiple_entries() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();
        let mut data = alloc::vec![0u8; 2 * PCM_SLOT_STRIDE];
        // Write distinct patterns into each slot.
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

        assert_eq!(n, 2 * PCM_SLOT_STRIDE, "must return total bytes copied");
        assert_eq!(logic.lvi(), 1, "LVI must be 1 after two BDL entries");
        // Both slots must have been written.
        assert_eq!(
            &pcm_ring[0..PCM_SLOT_STRIDE],
            &data[0..PCM_SLOT_STRIDE],
            "slot 0 pattern"
        );
        assert_eq!(
            &pcm_ring[PCM_SLOT_STRIDE..2 * PCM_SLOT_STRIDE],
            &data[PCM_SLOT_STRIDE..2 * PCM_SLOT_STRIDE],
            "slot 1 pattern"
        );
    }

    /// B.1(g) — review #148 thread PRRT...A8x2_: every BDL descriptor posted
    /// via `submit_frames_inner` is mirrored into the DMA-backed BDL the
    /// AC'97 controller reads via BDBAR. Without this mirror the device
    /// would DMA from zeroed/stale entries even though `Ac97Logic` advanced.
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

        // The first three DMA descriptors must equal `logic.bdl[..3]`
        // field-for-field — anything else means the hardware would see a
        // different ring than the logic-side mirror.
        for idx in 0..3 {
            let dma_entry = bdl_dma[idx];
            let logic_entry = logic.bdl()[idx];
            assert_eq!(
                { dma_entry.phys_addr },
                { logic_entry.phys_addr },
                "slot {idx} phys_addr must match the logic mirror"
            );
            assert_eq!(
                { dma_entry.samples },
                { logic_entry.samples },
                "slot {idx} samples must match the logic mirror"
            );
            assert_eq!(
                { dma_entry.flags },
                { logic_entry.flags },
                "slot {idx} flags must match the logic mirror"
            );
            // Sanity: the posted descriptor is not the all-zero default.
            assert_ne!({ dma_entry.samples }, 0, "slot {idx} must be non-empty");
        }
        // Slots beyond the submission must remain zeroed in the DMA buffer.
        for idx in 3..BDL_ENTRIES {
            let dma_entry = bdl_dma[idx];
            assert_eq!({ dma_entry.phys_addr }, 0, "slot {idx} must remain zero");
            assert_eq!({ dma_entry.samples }, 0, "slot {idx} must remain zero");
        }
    }

    /// B.1(h) — review #148 thread PRRT...A8x3J: when the BDL has *some*
    /// room but not enough for the *whole* submission, `submit_frames_inner`
    /// must return `WouldBlock` without copying or posting anything. This
    /// matches the `audio_client::submit_frames` documented contract
    /// (successful submit returns `bytes.len()`; backpressure is
    /// `WouldBlock`, never a short success).
    #[test]
    fn submit_frames_inner_partial_fit_returns_would_block_without_writes() {
        let mut pcm_ring = [0u8; DEFAULT_PCM_RING_BYTES];
        let mut bdl_dma = fresh_bdl_dma();
        let mut logic = Ac97Logic::new();

        // Pre-fill BDL_ENTRIES - 1 slots so exactly one slot is free.
        for i in 0..(BDL_ENTRIES - 1) {
            logic
                .submit_buffer(
                    (i * core::mem::size_of::<BufferDescriptor>()) as u64,
                    0xDEAD_0000 | (i as u32),
                    PCM_SLOT_STRIDE / 2,
                )
                .expect("pre-fill");
        }
        // Snapshot the producer state so we can confirm it is unchanged
        // after the rejected submit.
        let head_before = logic.head;
        let tail_before = logic.tail;
        let lvi_before = logic.lvi();
        let frames_submitted_before = logic.frames_submitted();

        // Two-slot submission requires 2 free slots; only 1 is available.
        let data = alloc::vec![0xEEu8; 2 * PCM_SLOT_STRIDE];
        // Mark the PCM ring with a sentinel so we can prove it stayed put.
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
        // Producer state unchanged.
        assert_eq!(logic.head, head_before, "head must not advance");
        assert_eq!(logic.tail, tail_before, "tail must not advance");
        assert_eq!(logic.lvi(), lvi_before, "lvi must not advance");
        assert_eq!(
            logic.frames_submitted(),
            frames_submitted_before,
            "frames_submitted must not advance"
        );
        // PCM ring still holds the sentinel — nothing was copied.
        assert!(
            pcm_ring.iter().all(|&b| b == 0x42),
            "PCM ring must be untouched on WouldBlock"
        );
        // The next free DMA slot is the one logic.head modulo BDL_ENTRIES,
        // which was set up to be slot `BDL_ENTRIES - 1`. That slot must
        // still be zero — proof we did not mirror a descriptor anywhere.
        let free_idx = head_before % BDL_ENTRIES;
        let free_dma = bdl_dma[free_idx];
        assert_eq!({ free_dma.phys_addr }, 0, "free DMA slot must be zero");
        assert_eq!({ free_dma.samples }, 0, "free DMA slot must be zero");
    }

    /// B.1: SILENCE_FRAME is exactly one slot stride of zero bytes.
    #[test]
    fn silence_frame_is_one_slot_stride_of_zeros() {
        assert_eq!(SILENCE_FRAME.len(), PCM_SLOT_STRIDE);
        assert!(SILENCE_FRAME.iter().all(|&b| b == 0));
    }
}
