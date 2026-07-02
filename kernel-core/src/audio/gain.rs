//! Phase 105 Track D.2 — system master-volume gain over interleaved S16LE
//! PCM.
//!
//! `audio_server` forwards already-mixed PCM from each client to the
//! backend driver; the per-client [`audio_mixer::Mixer`] gain runs inside
//! the client, so a *system* master volume (the settings-panel slider) has
//! to apply at the server, on the frames it forwards. This is that pure,
//! host-tested primitive: it scales S16LE samples in place by a Q15
//! multiplier. Keeping it here (kernel-core, which `audio_server` already
//! depends on for the protocol) lets the gain math be host-tested without a
//! live audio backend.
//!
//! The Q15 convention matches `audio_mixer::MASTER_GAIN_UNITY_Q15`
//! (`0x8000` = unity); the two crates carry the constant independently
//! because `audio_mixer` is a dependency-free leaf lib.

/// Q15 master gain that leaves the PCM unchanged (`1.0`). `0` mutes;
/// intermediate values attenuate linearly. Requests above unity are
/// clamped by [`apply_master_gain_s16le`], so the master stage only ever
/// attenuates and cannot introduce new clipping.
pub const MASTER_GAIN_UNITY_Q15: u16 = 0x8000;

/// Scale interleaved little-endian S16 samples in `buf` by `q15_gain`
/// in place.
///
/// `q15_gain` is clamped to [`MASTER_GAIN_UNITY_Q15`] first (no
/// amplification). Unity is a no-op fast path — the common case where the
/// user has not attenuated pays nothing. Each 16-bit sample is scaled as
/// `(s * gain) >> 15` and clamped back into `i16` range. A trailing odd
/// byte (a truncated sample at the end of `buf`) is left untouched rather
/// than half-scaled.
pub fn apply_master_gain_s16le(buf: &mut [u8], q15_gain: u16) {
    let gain = q15_gain.min(MASTER_GAIN_UNITY_Q15) as i32;
    if gain == MASTER_GAIN_UNITY_Q15 as i32 {
        return; // unity — nothing to do
    }
    for sample in buf.chunks_exact_mut(2) {
        let s = i16::from_le_bytes([sample[0], sample[1]]) as i32;
        let scaled = (s * gain) >> 15;
        let out = scaled.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let bytes = out.to_le_bytes();
        sample[0] = bytes[0];
        sample[1] = bytes[1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s16le(samples: &[i16]) -> alloc::vec::Vec<u8> {
        let mut v = alloc::vec::Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    fn decode(buf: &[u8]) -> alloc::vec::Vec<i16> {
        buf.chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[test]
    fn unity_is_a_noop() {
        let mut buf = s16le(&[0, 100, -100, i16::MAX, i16::MIN]);
        let orig = buf.clone();
        apply_master_gain_s16le(&mut buf, MASTER_GAIN_UNITY_Q15);
        assert_eq!(buf, orig);
    }

    #[test]
    fn zero_gain_mutes() {
        let mut buf = s16le(&[1234, -4321, i16::MAX, i16::MIN]);
        apply_master_gain_s16le(&mut buf, 0);
        assert_eq!(decode(&buf), [0, 0, 0, 0]);
    }

    #[test]
    fn half_gain_halves() {
        // 0x4000 = 0.5; (s * 0x4000) >> 15 == s / 2 (toward -inf for the
        // arithmetic shift, but the values here are exact halves).
        let mut buf = s16le(&[0, 200, -200, 1000]);
        apply_master_gain_s16le(&mut buf, 0x4000);
        assert_eq!(decode(&buf), [0, 100, -100, 500]);
    }

    #[test]
    fn above_unity_is_clamped_to_unity() {
        let mut buf = s16le(&[0, 100, -100, i16::MAX]);
        let orig = buf.clone();
        apply_master_gain_s16le(&mut buf, 0xFFFF);
        assert_eq!(buf, orig, "gain > unity must be clamped to a no-op");
    }

    #[test]
    fn odd_trailing_byte_untouched() {
        // 5 bytes = 2 whole samples + 1 dangling byte.
        let mut buf = alloc::vec![0x10u8, 0x00, 0x20, 0x00, 0x7F];
        apply_master_gain_s16le(&mut buf, 0); // mute
        // First two samples zeroed; the trailing 0x7F is preserved.
        assert_eq!(buf, alloc::vec![0u8, 0, 0, 0, 0x7F]);
    }
}
