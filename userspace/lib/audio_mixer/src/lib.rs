//! `audio_mixer` — Phase 63a Track A: pure-logic mixer.
//!
//! A fixed-channel (`<= 32`) software mixer that consumes
//! unsigned-8-bit PCM source samples (DMX-style, where `128` is
//! silence), resamples each active channel to 48 kHz via 16.16
//! fixed-point linear interpolation, applies per-channel left/right
//! volume (`0..=127`), accumulates into an `i32`, and clamps to
//! `i16::MIN..=i16::MAX` on store. Output frames are stereo S16LE
//! (4 bytes per frame).
//!
//! The mixer is `#![no_std]`, allocation-free in [`Mixer::step`], and
//! has no IPC or WAD knowledge — Single Responsibility. The C-ABI
//! surface in [`ffi`] lets the doomgeneric platform layer
//! (`m3os_sound.c`, `m3os_music.c`) reuse the same engine without
//! re-implementing the resampler in C.
//!
//! ## Sample format
//!
//! Source samples are unsigned 8-bit PCM with `128` representing
//! silence (DMX format). Internally we convert to a 16-bit signed
//! domain via `(sample as i16 - 128) << 8` so the resampler and
//! per-channel volume operate on i16-scaled values. The accumulator
//! is `i32` to absorb overdriven sums; the final stage clamps to
//! `i16::MIN..=i16::MAX`.
//!
//! ## Volume scaling
//!
//! Per-channel `left_vol` / `right_vol` are `0..=127`. The mixer
//! divides by `128` via an arithmetic right-shift of `7` (no division
//! on the hot path).

// Pure-logic mixer: `#![no_std]` outside of host tests. The
// staticlib build of the DOOM audio path links this crate as an
// rlib transitively from `audio_client_ffi`; the panic_handler /
// global_allocator live there so they're defined exactly once at
// the staticlib root.
#![cfg_attr(not(test), no_std)]

pub mod ffi;

/// Maximum number of channels the mixer supports. The DOOM platform
/// claims `0..15` for SFX (matching `MAX_CHANNELS = 16` in the engine)
/// and `16..31` for music-synth voices. A future system-mixer service
/// can reuse the same crate with a different split.
pub const MAX_CHANNELS: usize = 32;

/// Output sample rate. The audio_server accepts a fixed `48_000` for
/// the AC'97 Phase 63 backend; the mixer hard-codes the same value so
/// the resampler increment is a compile-time constant for the common
/// case.
pub const OUTPUT_RATE_HZ: u32 = 48_000;

/// Bytes per stereo S16LE frame.
pub const BYTES_PER_FRAME: usize = 4;

/// Per-channel mix state. Public for tests and FFI introspection;
/// production callers never construct one directly — they go through
/// [`Mixer::set_channel`].
#[derive(Clone, Copy, Debug)]
pub struct ChannelState {
    samples_ptr: *const u8,
    samples_len: usize,
    /// 16.16 fixed-point cursor into the source samples.
    cursor: u64,
    /// 16.16 fixed-point increment per output frame.
    inc: u32,
    left_vol: u8,
    right_vol: u8,
    active: bool,
}

unsafe impl Send for ChannelState {}
unsafe impl Sync for ChannelState {}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            samples_ptr: core::ptr::null(),
            samples_len: 0,
            cursor: 0,
            inc: 0,
            left_vol: 0,
            right_vol: 0,
            active: false,
        }
    }
}

impl ChannelState {
    /// `true` if this channel is currently playing a sample.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// `true` once the channel has consumed every sample (cursor
    /// walked past `samples_len`). Mirrors the internal disable that
    /// `step` performs on end-of-sample.
    pub fn is_finished(&self) -> bool {
        !self.active
    }
}

/// Fixed-channel software mixer. Constructed via [`Mixer::new`];
/// drives [`Mixer::set_channel`], [`Mixer::clear_channel`], and
/// [`Mixer::step`].
pub struct Mixer {
    channels: [ChannelState; MAX_CHANNELS],
    channel_count: usize,
}

impl Mixer {
    /// Construct a mixer with `channel_count` active slots. Panics if
    /// `channel_count > MAX_CHANNELS`.
    pub fn new(channel_count: usize) -> Self {
        assert!(
            channel_count <= MAX_CHANNELS,
            "audio_mixer: channel_count exceeds MAX_CHANNELS"
        );
        Self {
            channels: [ChannelState::default(); MAX_CHANNELS],
            channel_count,
        }
    }

    /// Active channel count. Set at construction time.
    pub fn channel_count(&self) -> usize {
        self.channel_count
    }

    /// Read-only access to a channel slot. Intended for tests and
    /// FFI inspection.
    pub fn channel(&self, idx: usize) -> Option<&ChannelState> {
        self.channels.get(idx)
    }

    /// Seed channel `idx` with a sample slice, source rate, and pan
    /// volumes.
    ///
    /// # Safety
    ///
    /// `samples` must remain valid for reads of `samples_len` bytes
    /// for as long as the channel is active. Production callers
    /// either cache the decoded SFX permanently for the lifetime of
    /// the process (`m3os_sound.c`) or supply a `'static` waveform
    /// buffer (`m3os_music.c`). The `unsafe` marker exists because
    /// the channel state stores the raw pointer across many `step`
    /// calls and the mixer cannot enforce the lifetime itself.
    pub unsafe fn set_channel(
        &mut self,
        idx: usize,
        samples: &[u8],
        source_rate_hz: u32,
        left_vol: u8,
        right_vol: u8,
    ) {
        if idx >= self.channel_count {
            return;
        }
        if samples.is_empty() || source_rate_hz == 0 {
            self.channels[idx] = ChannelState::default();
            return;
        }
        let lv = left_vol.min(127);
        let rv = right_vol.min(127);
        let inc = (((source_rate_hz as u64) << 16) / OUTPUT_RATE_HZ as u64) as u32;
        self.channels[idx] = ChannelState {
            samples_ptr: samples.as_ptr(),
            samples_len: samples.len(),
            cursor: 0,
            inc,
            left_vol: lv,
            right_vol: rv,
            active: true,
        };
    }

    /// Zero channel `idx`. Used by `S_StopSound` and at music
    /// note-off.
    pub fn clear_channel(&mut self, idx: usize) {
        if idx >= self.channel_count {
            return;
        }
        self.channels[idx] = ChannelState::default();
    }

    /// Mix `frames` stereo S16LE frames into `out`. Returns the
    /// number of bytes written, or `0` if `out` is too small.
    ///
    /// No allocation: the mix loop walks the fixed channel array and
    /// writes interleaved little-endian samples in place.
    pub fn step(&mut self, out: &mut [u8], frames: usize) -> usize {
        let needed = frames * BYTES_PER_FRAME;
        if out.len() < needed {
            return 0;
        }
        // Zero the output region first so the per-channel loop can
        // accumulate via `+=` without an explicit silence path.
        for byte in &mut out[..needed] {
            *byte = 0;
        }
        for frame_i in 0..frames {
            let mut acc_l: i32 = 0;
            let mut acc_r: i32 = 0;
            for ch in &mut self.channels[..self.channel_count] {
                if !ch.active {
                    continue;
                }
                let cursor_int = (ch.cursor >> 16) as usize;
                if cursor_int >= ch.samples_len {
                    ch.active = false;
                    continue;
                }
                // SAFETY: `set_channel` requires the slice to outlive
                // the active period; `cursor_int < samples_len` was
                // just bounds-checked.
                let s0 = (unsafe { *ch.samples_ptr.add(cursor_int) }) as i32;
                let s1 = if cursor_int + 1 < ch.samples_len {
                    (unsafe { *ch.samples_ptr.add(cursor_int + 1) }) as i32
                } else {
                    128
                };
                // DMX-style unsigned 8-bit → signed 16-bit.
                let s0_full = (s0 - 128) << 8;
                let s1_full = (s1 - 128) << 8;
                let frac = (ch.cursor & 0xFFFF) as i32;
                let interp = (s0_full * (0x10000 - frac) + s1_full * frac) >> 16;
                let l = (interp * ch.left_vol as i32) >> 7;
                let r = (interp * ch.right_vol as i32) >> 7;
                acc_l = acc_l.saturating_add(l);
                acc_r = acc_r.saturating_add(r);
                ch.cursor = ch.cursor.wrapping_add(ch.inc as u64);
            }
            let l = acc_l.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let r = acc_r.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let off = frame_i * BYTES_PER_FRAME;
            let lu = l as u16;
            let ru = r as u16;
            out[off] = lu as u8;
            out[off + 1] = (lu >> 8) as u8;
            out[off + 2] = ru as u8;
            out[off + 3] = (ru >> 8) as u8;
        }
        needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic unsigned-8 sample helpers (128 = silence).
    fn const_sample(value: u8, len: usize) -> alloc::vec::Vec<u8> {
        alloc::vec![value; len]
    }

    fn ramp_sample(len: usize) -> alloc::vec::Vec<u8> {
        (0..len).map(|i| (i & 0xFF) as u8).collect()
    }

    extern crate alloc;

    #[test]
    fn single_channel_mute() {
        let mut mixer = Mixer::new(1);
        let samples = const_sample(255, 64);
        unsafe {
            mixer.set_channel(0, &samples, 48_000, 0, 0);
        }
        let mut out = [0xAAu8; 64 * BYTES_PER_FRAME];
        let n = mixer.step(&mut out, 64);
        assert_eq!(n, 64 * BYTES_PER_FRAME);
        for &b in &out[..n] {
            assert_eq!(b, 0, "muted channel should produce all-zero output");
        }
    }

    #[test]
    fn single_channel_full_volume() {
        // 4-sample synthetic input at 48 kHz, full volume left+right.
        // Samples: [128, 192, 192, 128] → signed (post -128, <<8):
        // [0, +16384, +16384, 0]. With volume 127, scaling is
        // ((interp * 127) >> 7) which is `interp * 127 / 128`, i.e.
        // 16384 * 127 / 128 = 16256. Stereo S16LE pairs per frame.
        let mut mixer = Mixer::new(1);
        let samples = alloc::vec![128u8, 192, 192, 128];
        unsafe {
            mixer.set_channel(0, &samples, 48_000, 127, 127);
        }
        let mut out = [0u8; 4 * BYTES_PER_FRAME];
        let n = mixer.step(&mut out, 4);
        assert_eq!(n, 16);

        let expected_signed: [i16; 4] = [0, 16256, 16256, 0];
        for (i, &v) in expected_signed.iter().enumerate() {
            let off = i * BYTES_PER_FRAME;
            let l = i16::from_le_bytes([out[off], out[off + 1]]);
            let r = i16::from_le_bytes([out[off + 2], out[off + 3]]);
            assert_eq!(l, v, "left frame {} mismatch (got {}, want {})", i, l, v);
            assert_eq!(r, v, "right frame {} mismatch (got {}, want {})", i, r, v);
        }
    }

    #[test]
    fn two_channel_pan() {
        // Channel 0: hard left (left_vol=127, right_vol=0)
        // Channel 1: hard right (left_vol=0, right_vol=127)
        let mut mixer = Mixer::new(2);
        let s0 = alloc::vec![192u8; 4]; // +16384 signed
        let s1 = alloc::vec![64u8; 4]; // -16384 signed
        unsafe {
            mixer.set_channel(0, &s0, 48_000, 127, 0);
            mixer.set_channel(1, &s1, 48_000, 0, 127);
        }
        let mut out = [0u8; 4 * BYTES_PER_FRAME];
        let n = mixer.step(&mut out, 4);
        assert_eq!(n, 16);
        for i in 0..4 {
            let off = i * BYTES_PER_FRAME;
            let l = i16::from_le_bytes([out[off], out[off + 1]]);
            let r = i16::from_le_bytes([out[off + 2], out[off + 3]]);
            // Left output is purely channel 0 (positive).
            assert!(l > 0, "frame {}: left should be positive, got {}", i, l);
            // Right output is purely channel 1 (negative).
            assert!(r < 0, "frame {}: right should be negative, got {}", i, r);
        }
    }

    #[test]
    fn clamp_at_bounds() {
        // 8 channels all at max +127 signed (sample value 255) with
        // full vol 127 → each yields ~16256 → sum ≈ 130 048, well
        // above i16::MAX. The clamp must hold the output at i16::MAX
        // rather than wrap.
        let mut mixer = Mixer::new(8);
        let samples = const_sample(255, 16);
        for i in 0..8 {
            unsafe {
                mixer.set_channel(i, &samples, 48_000, 127, 127);
            }
        }
        let mut out = [0u8; 4 * BYTES_PER_FRAME];
        let n = mixer.step(&mut out, 4);
        assert_eq!(n, 16);
        for i in 0..4 {
            let off = i * BYTES_PER_FRAME;
            let l = i16::from_le_bytes([out[off], out[off + 1]]);
            let r = i16::from_le_bytes([out[off + 2], out[off + 3]]);
            assert_eq!(l, i16::MAX, "frame {}: left should clamp to i16::MAX", i);
            assert_eq!(r, i16::MAX, "frame {}: right should clamp to i16::MAX", i);
        }
    }

    #[test]
    fn resampler_11025_to_48000() {
        // A monotonically-non-decreasing ramp at 11025 Hz must remain
        // monotonically non-decreasing after resampling to 48 kHz, up
        // to the point where the ramp wraps (value 255 → 0 in the u8
        // domain). Only check the first quarter of the buffer (before
        // any wrap).
        let mut mixer = Mixer::new(1);
        let samples = ramp_sample(64);
        unsafe {
            mixer.set_channel(0, &samples, 11_025, 127, 127);
        }
        let mut out = [0u8; 32 * BYTES_PER_FRAME];
        let n = mixer.step(&mut out, 32);
        assert_eq!(n, 128);
        let mut prev = i16::MIN;
        for i in 0..16 {
            let off = i * BYTES_PER_FRAME;
            let l = i16::from_le_bytes([out[off], out[off + 1]]);
            assert!(
                l >= prev,
                "frame {}: resampled output should be non-decreasing (got {}, prev {})",
                i,
                l,
                prev
            );
            prev = l;
        }
    }

    #[test]
    fn clear_channel_silences_output() {
        let mut mixer = Mixer::new(1);
        let samples = const_sample(255, 16);
        unsafe {
            mixer.set_channel(0, &samples, 48_000, 127, 127);
        }
        mixer.clear_channel(0);
        let mut out = [0xFFu8; 4 * BYTES_PER_FRAME];
        let n = mixer.step(&mut out, 4);
        assert_eq!(n, 16);
        for &b in &out[..n] {
            assert_eq!(b, 0);
        }
    }

    #[test]
    fn step_returns_zero_on_undersized_buffer() {
        let mut mixer = Mixer::new(1);
        let mut out = [0u8; 4];
        let n = mixer.step(&mut out, 32);
        assert_eq!(n, 0);
    }
}
