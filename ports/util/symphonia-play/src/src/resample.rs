//! Channel mapping + linear resampling to the `audio_server` contract:
//! interleaved S16LE, stereo, 48 kHz (the ONLY format/layout/rate the
//! Phase 57 protocol carries — `kernel-core/src/audio/format.rs`).
//!
//! Linear interpolation is deliberately simple (this is a teaching-OS
//! player, not a mastering chain): for 44.1→48 kHz upsampling its
//! artifacts sit far below the AC'97 output path's own noise floor.

const TARGET_RATE: u32 = 48_000;

/// Convert interleaved `samples` (`channels`-wide, `src_rate`) into
/// interleaved stereo 48 kHz.
pub fn to_stereo_48k(samples: &[i16], channels: usize, src_rate: u32) -> Vec<i16> {
    let stereo = to_stereo(samples, channels);
    if src_rate == TARGET_RATE {
        return stereo;
    }
    resample_stereo(&stereo, src_rate)
}

/// Mono duplicates; stereo passes through; >2 channels keep the first
/// pair (front L/R in every layout symphonia emits).
fn to_stereo(samples: &[i16], channels: usize) -> Vec<i16> {
    match channels {
        0 => Vec::new(),
        1 => {
            let mut out = Vec::with_capacity(samples.len() * 2);
            for &s in samples {
                out.push(s);
                out.push(s);
            }
            out
        }
        2 => samples.to_vec(),
        n => {
            let frames = samples.len() / n;
            let mut out = Vec::with_capacity(frames * 2);
            for f in 0..frames {
                out.push(samples[f * n]);
                out.push(samples[f * n + 1]);
            }
            out
        }
    }
}

/// Linear-interpolate interleaved stereo from `src_rate` to 48 kHz.
fn resample_stereo(stereo: &[i16], src_rate: u32) -> Vec<i16> {
    let frames_in = stereo.len() / 2;
    if frames_in < 2 || src_rate == 0 {
        return stereo.to_vec();
    }
    let frames_out = ((frames_in as u64) * TARGET_RATE as u64 / src_rate as u64) as usize;
    let mut out = Vec::with_capacity(frames_out * 2);
    // 32.32 fixed-point source position per output frame.
    let step: u64 = ((src_rate as u64) << 32) / TARGET_RATE as u64;
    let mut pos: u64 = 0;
    for _ in 0..frames_out {
        let idx = (pos >> 32) as usize;
        let frac = (pos & 0xFFFF_FFFF) as i64;
        let idx1 = (idx + 1).min(frames_in - 1);
        for ch in 0..2 {
            let a = stereo[idx * 2 + ch] as i64;
            let b = stereo[idx1 * 2 + ch] as i64;
            let v = a + ((b - a) * frac >> 32);
            out.push(v as i16);
        }
        pos += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_duplicates_to_stereo() {
        assert_eq!(to_stereo(&[1, -2, 3], 1), vec![1, 1, -2, -2, 3, 3]);
    }

    #[test]
    fn stereo_passthrough_at_48k() {
        let s = vec![10, -10, 20, -20];
        assert_eq!(to_stereo_48k(&s, 2, 48_000), s);
    }

    #[test]
    fn five_one_keeps_front_pair() {
        // One 6-channel frame: FL FR FC LFE RL RR.
        assert_eq!(to_stereo(&[1, 2, 3, 4, 5, 6], 6), vec![1, 2]);
    }

    #[test]
    fn resample_44100_to_48000_frame_count_and_range() {
        // 44100 input frames (1 s) → ~48000 output frames.
        let frames_in = 44_100usize;
        let mut stereo = Vec::with_capacity(frames_in * 2);
        for i in 0..frames_in {
            // A ramp keeps interpolation between neighbors in-range.
            let v = ((i % 2000) as i32 - 1000) as i16;
            stereo.push(v);
            stereo.push(-v);
        }
        let out = to_stereo_48k(&stereo, 2, 44_100);
        let frames_out = out.len() / 2;
        assert_eq!(frames_out, 48_000);
        // Linear interpolation never exceeds the neighbor extrema.
        assert!(out.iter().all(|&s| (-1000..=1000).contains(&(s as i32))));
    }

    #[test]
    fn resample_identity_when_already_48k() {
        let s: Vec<i16> = (0..96).map(|i| i as i16).collect();
        assert_eq!(resample_stereo(&s, TARGET_RATE), s);
    }
}
