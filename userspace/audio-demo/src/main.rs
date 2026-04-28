//! `audio-demo` — Phase 57 Track E.2 audio reference client.
//!
//! On run, the demo opens a stream against `audio_server`, submits a
//! one-second 440 Hz sine wave (16-bit signed LE, stereo, 48 kHz),
//! drains, closes, and exits 0. On any [`audio_client::AudioClientError`]
//! the demo logs a structured line containing the variant name and
//! exits non-zero. The demo doubles as the audio smoke harness for
//! Track H.1.
//!
//! ## Why this binary is *not* a daemon
//!
//! The four-step new-binary convention covers the four wiring sites:
//! workspace `members`, xtask `bins`, kernel ramdisk `BIN_ENTRIES`,
//! and (only for daemons) `etc/services.d/<name>.conf` +
//! `KNOWN_CONFIGS` in `userspace/init/src/main.rs`.
//!
//! `audio-demo` is a one-shot — it opens, plays, closes, exits — so
//! the service-config step is intentionally skipped. The demo runs
//! either by manual invocation from the shell (`/bin/audio-demo`) or
//! by H.1 driving it as a smoke client. Adding a daemon manifest
//! here would invite the service supervisor to relaunch it on every
//! exit, which is the wrong semantics for a one-shot.
//!
//! ## Test-tone generation
//!
//! Output: a 1-second 440 Hz sine wave, 16-bit signed little-endian,
//! stereo, 48 kHz. The tone is generated entirely in fixed-point
//! integer arithmetic — the kernel target (`x86_64-unknown-none`)
//! disables SSE, so floating-point sin / cos would either pull in
//! soft-float library helpers or risk a #UD trap; an integer LUT is
//! both cheaper and cleaner.
//!
//! Algorithm:
//!
//! 1. A 256-entry quarter-sine table holds `sin(x)` for `x` in
//!    `[0, π/2]`, computed at startup via the 7th-order Taylor
//!    series `x - x³/6 + x⁵/120 - x⁷/5040`.  The result is scaled
//!    to `i16` so the table values are directly comparable to the
//!    target PCM range.
//! 2. A 32-bit phase accumulator advances by
//!    `(TONE_FREQ_HZ / SAMPLE_RATE_HZ) * 2^32` every output frame.
//!    The top two phase bits select the quadrant; the next bits
//!    index the LUT (with the lower bits of the index dropped — no
//!    interpolation).  A 256-entry quarter-sine table backs a
//!    1024-entry full-circle table, more than enough quality for a
//!    tone test.
//! 3. The amplitude is scaled to `i16::MAX * AMPLITUDE` so the
//!    waveform is well below clip and easy to spot on a 'scope.
//! 4. The stream is mono-per-channel: both channels carry the same
//!    sample.  Adapting to mono / panned tones is a follow-on
//!    exercise; the protocol carries the layout, so the server's
//!    backend remixes correctly.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use audio_client::{AudioClient, AudioClientError};
use kernel_core::audio::{ChannelLayout, MAX_SUBMIT_BYTES, PcmFormat, ProtocolError, SampleRate};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "audio-demo: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "audio-demo: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

// ---------------------------------------------------------------------------
// Tone parameters — recorded as named constants per the E.2 acceptance
// bullet so a reader can regenerate the buffer without reading binary
// blobs.
// ---------------------------------------------------------------------------

/// Tone frequency in hertz (concert A; standard reference pitch).
const TONE_FREQ_HZ: u32 = 440;
/// Sample rate in hertz. Locked to 48 kHz by the AC'97 backend.
const SAMPLE_RATE_HZ: u32 = 48_000;
/// Tone duration in seconds.
const DURATION_S: u32 = 1;
/// Peak amplitude as a fraction of i16::MAX, in 1/65536ths. 0.3 ≈
/// 19661 → tone sits well below clip and is easy to spot on a 'scope.
const AMPLITUDE_NUM: i32 = 19_661;
const AMPLITUDE_DEN: i32 = 65_536;
/// Quarter-sine LUT length. 256 entries × 4 quadrants = 1024-step
/// effective phase resolution.
const LUT_LEN: usize = 256;
/// Bytes per stereo frame: 2 channels × 2 bytes (S16Le).
const STEREO_FRAME_BYTES: usize = 4;

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:BEGIN\n");

    let lut = build_quarter_sine_lut();
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:lut-ready\n");

    let mut client =
        match AudioClient::open(PcmFormat::S16Le, ChannelLayout::Stereo, SampleRate::Hz48000) {
            Ok(c) => c,
            Err(err) => {
                log_error("open", err);
                return 2;
            }
        };
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:opened\n");

    if let Err(err) = submit_tone(&mut client, &lut) {
        log_error("submit", err);
        return 3;
    }
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:submitted\n");

    if let Err(err) = client.drain() {
        log_error("drain", err);
        return 4;
    }
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:drained\n");

    if let Err(err) = client.close() {
        log_error("close", err);
        return 5;
    }
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:closed\n");
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:PASS\n");
    0
}

// ---------------------------------------------------------------------------
// Tone submission — chunks the 1-second buffer into MAX_SUBMIT_BYTES
// pieces (rounded down to a stereo-frame boundary) and submits each.
// ---------------------------------------------------------------------------

fn submit_tone(
    client: &mut AudioClient<audio_client::SyscallSocket>,
    lut: &[i16; LUT_LEN],
) -> Result<(), AudioClientError> {
    // Phase increment per sample in 32-bit fixed-point. Rounded to
    // the nearest u32 — the rounding error is below 1/2^32 of a Hz,
    // far below human hearing.
    let phase_step: u32 = (((TONE_FREQ_HZ as u64) << 32) / SAMPLE_RATE_HZ as u64) as u32;
    let mut phase: u32 = 0;

    let total_frames = (SAMPLE_RATE_HZ * DURATION_S) as usize;
    // Round chunk size down to a stereo-frame boundary so we never
    // split a frame across submits.
    let max_chunk_frames = MAX_SUBMIT_BYTES / STEREO_FRAME_BYTES;
    let mut frames_remaining = total_frames;

    // One stack-allocated scratch buffer, reused across chunks. Sized
    // to MAX_SUBMIT_BYTES so the largest chunk fits.
    let mut chunk = [0u8; MAX_SUBMIT_BYTES];

    while frames_remaining > 0 {
        let frames_this_chunk = core::cmp::min(frames_remaining, max_chunk_frames);
        let chunk_bytes = frames_this_chunk * STEREO_FRAME_BYTES;

        for f in 0..frames_this_chunk {
            let sample = sample_at(phase, lut);
            phase = phase.wrapping_add(phase_step);
            let bytes = sample.to_le_bytes();
            // Stereo: write the same sample to both channels.
            let off = f * STEREO_FRAME_BYTES;
            chunk[off] = bytes[0];
            chunk[off + 1] = bytes[1];
            chunk[off + 2] = bytes[0];
            chunk[off + 3] = bytes[1];
        }

        let written = client.submit_frames(&chunk[..chunk_bytes])?;
        if written != chunk_bytes {
            // Partial accept is not part of the Phase 57 contract;
            // surface as a `Protocol(Truncated)` so the operator
            // sees a typed reason in the log.
            return Err(AudioClientError::Protocol(ProtocolError::Truncated));
        }
        frames_remaining -= frames_this_chunk;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Sine generation — quarter-sine LUT lookup with quadrant fold
// ---------------------------------------------------------------------------

/// Compute `sin(2π × phase / 2^32)` to 16-bit precision using the
/// quarter-sine LUT.  The top two phase bits select the quadrant; the
/// remaining bits index the LUT.
///
/// Quadrant layout (CCW from 0):
///
/// - 00 (`[0, π/2]`):    `+lut[i]`
/// - 01 (`[π/2, π]`):    `+lut[LUT_LEN-1-i]`
/// - 10 (`[π, 3π/2]`):   `-lut[i]`
/// - 11 (`[3π/2, 2π]`):  `-lut[LUT_LEN-1-i]`
fn sample_at(phase: u32, lut: &[i16; LUT_LEN]) -> i16 {
    let quadrant = (phase >> 30) & 0b11;
    // Bits below the quadrant select the in-quadrant position. We
    // have 30 phase bits for the quadrant; LUT_LEN = 256 needs 8 of
    // them, so shift by (30 - 8) = 22.
    let index = ((phase >> 22) & (LUT_LEN as u32 - 1)) as usize;
    let folded = match quadrant {
        0 => lut[index],
        1 => lut[LUT_LEN - 1 - index],
        2 => lut[index].wrapping_neg(),
        _ => lut[LUT_LEN - 1 - index].wrapping_neg(),
    };
    // Scale by the amplitude: folded × AMPLITUDE_NUM / AMPLITUDE_DEN.
    // i16 × i32 fits in i64 with room to spare.
    let scaled = (folded as i32 * AMPLITUDE_NUM) / AMPLITUDE_DEN;
    // Saturate to i16 — the math above never overflows because
    // AMPLITUDE_NUM < AMPLITUDE_DEN, but the saturation is cheap and
    // makes the contract explicit.
    if scaled > i16::MAX as i32 {
        i16::MAX
    } else if scaled < i16::MIN as i32 {
        i16::MIN
    } else {
        scaled as i16
    }
}

/// Build the 256-entry quarter-sine LUT using a 7th-order Taylor
/// series in fixed-point Q15.16. The argument range is `[0, π/2)`,
/// the result range is `[0, +1.0]` mapped to `[0, i16::MAX]`.
///
/// Using a Taylor series keeps the build host-independent: no `libm`
/// dependency, no `f32` reliance, identical bit-for-bit output across
/// every host that builds the kernel.
fn build_quarter_sine_lut() -> [i16; LUT_LEN] {
    // Q16.16 fixed-point: `1.0` is `1 << 16`. We compute
    //   sin(x) ≈ x - x³/6 + x⁵/120 - x⁷/5040
    // for x ∈ [0, π/2). The 7th-order term keeps peak error below
    // 0.0002 over the quarter — far better than i16 quantisation.
    const Q: i64 = 1 << 16;
    // π/2 in Q16.16 ≈ 1.5707963 × 65536 = 102943.
    const HALF_PI_Q: i64 = 102_944;
    let mut lut = [0i16; LUT_LEN];
    for (i, slot) in lut.iter_mut().enumerate() {
        // x = (i / LUT_LEN) × π/2, in Q16.16.
        let x: i64 = HALF_PI_Q * i as i64 / LUT_LEN as i64;
        let x2 = (x * x) / Q;
        let x3 = (x2 * x) / Q;
        let x5 = (x3 * x2) / Q;
        let x7 = (x5 * x2) / Q;
        // Coefficients in Q16.16: 1/6 ≈ 10923, 1/120 ≈ 546, 1/5040 ≈ 13.
        let term3 = (x3 * 10923) / Q;
        let term5 = (x5 * 546) / Q;
        let term7 = (x7 * 13) / Q;
        let sin_q = x - term3 + term5 - term7;
        // Rescale Q16.16 result (in [0, 1.0)) to [0, i16::MAX].
        let scaled = (sin_q * i16::MAX as i64) / Q;
        *slot = if scaled > i16::MAX as i64 {
            i16::MAX
        } else if scaled < 0 {
            0
        } else {
            scaled as i16
        };
    }
    lut
}

// ---------------------------------------------------------------------------
// Error logging — structured single-line for the E.2 acceptance bullet
// ---------------------------------------------------------------------------

fn log_error(stage: &str, err: AudioClientError) {
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:FAIL stage=");
    syscall_lib::write_str(STDOUT_FILENO, stage);
    syscall_lib::write_str(STDOUT_FILENO, " variant=");
    syscall_lib::write_str(STDOUT_FILENO, error_label(err));
    syscall_lib::write_str(STDOUT_FILENO, "\n");
}

fn error_label(err: AudioClientError) -> &'static str {
    match err {
        AudioClientError::Io(_) => "Io",
        AudioClientError::Protocol(_) => "Protocol",
        AudioClientError::Server(_) => "Server",
        AudioClientError::AlreadyOpen => "AlreadyOpen",
        AudioClientError::NotOpen => "NotOpen",
        AudioClientError::UnexpectedReply => "UnexpectedReply",
        // `AudioClientError` is `#[non_exhaustive]`. New variants
        // surface as a labelled-but-generic line so the demo's exit
        // path stays well-formed even after an ABI bump.
        _ => "Unknown",
    }
}
