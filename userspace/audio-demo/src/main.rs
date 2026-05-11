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

use audio_client::{AudioClient, AudioClientError, AudioStats};
use kernel_core::audio::{AudioError, ChannelLayout, PcmFormat, ProtocolError, SampleRate};
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

    // D.2: query the server for consumption stats before closing the stream.
    // The sentinel line `AUDIO_DEMO:stats consumed=<N> underruns=<M>` is
    // parsed by the audio-smoke harness to assert frames_consumed > 0.
    let stats = match client.get_stats() {
        Ok(s) => s,
        Err(err) => {
            log_error("get_stats", err);
            return 6;
        }
    };

    if let Err(err) = client.close() {
        log_error("close", err);
        return 5;
    }
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:closed\n");
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:PASS\n");
    // Emit the stats sentinel AFTER PASS so the smoke harness's
    // post-PASS `WaitLineNotMatching` step still finds it in the
    // serial buffer — the PASS-step drain otherwise consumes
    // everything up to and including PASS.
    log_stats(stats);
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
    // Cap the per-call chunk size to roughly half the AC'97 backend's
    // BDL ring (16 KiB / 2 = 8 KiB) so a fresh submit always fits even
    // when the controller is mid-playing a previous chunk. A full
    // 16 KiB submit would require all 32 ring slots to be free, but
    // `observe_irq` leaves the slot at the current CIV in-flight,
    // which produces a permanent 1-slot deficit and a `WouldBlock`
    // loop for the smoke harness's second submit. Round down to a
    // stereo-frame boundary so we never split a frame across submits.
    const SUBMIT_CHUNK_BYTES: usize = 8 * 1024;
    let max_chunk_frames = SUBMIT_CHUNK_BYTES / STEREO_FRAME_BYTES;
    let mut frames_remaining = total_frames;

    // One stack-allocated scratch buffer, reused across chunks. Sized
    // to the per-call cap so the largest chunk fits without overflowing
    // the user stack.
    let mut chunk = [0u8; SUBMIT_CHUNK_BYTES];

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

        // Retry on `WouldBlock` (server-side AC'97 BDL ring full). The
        // ring drains as the controller consumes buffers; sleeping ~5 ms
        // between attempts gives QEMU's bus-master timer time to make
        // progress without burning a tight CPU loop. Cap retries so a
        // genuinely stalled controller surfaces as an error instead of
        // wedging the demo.
        let written;
        let mut retries = 0usize;
        loop {
            match client.submit_frames(&chunk[..chunk_bytes]) {
                Ok(n) => {
                    written = n;
                    break;
                }
                Err(AudioClientError::Server(AudioError::WouldBlock)) => {
                    if retries >= 200 {
                        return Err(AudioClientError::Server(AudioError::WouldBlock));
                    }
                    retries += 1;
                    syscall_lib::nanosleep_for(0, 5_000_000);
                }
                Err(other) => return Err(other),
            }
        }
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
// Stats logging — D.2 sentinel parsed by the audio-smoke harness
// ---------------------------------------------------------------------------

/// Print the `AUDIO_DEMO:stats consumed=<N> underruns=<M>` sentinel.
///
/// The line shape is locked for Track E / WaitLineNotMatching compatibility:
/// `consumed=` reads `AudioStats::frames_consumed`; `underruns=` reads
/// `AudioStats::underrun_count`. The sentinel label words are intentionally
/// shorter than the wire field names so the output stays human-readable.
///
/// Uses a minimal integer-to-string helper to stay `no_std` / alloc-free.
fn log_stats(stats: AudioStats) {
    syscall_lib::write_str(STDOUT_FILENO, "AUDIO_DEMO:stats consumed=");
    write_u64(stats.frames_consumed);
    syscall_lib::write_str(STDOUT_FILENO, " underruns=");
    write_u32(stats.underrun_count);
    syscall_lib::write_str(STDOUT_FILENO, "\n");
}

/// Write a `u64` decimal integer to stdout without heap allocation.
fn write_u64(mut n: u64) {
    // Build digits in reverse, then write forward.
    let mut buf = [0u8; 20]; // 2^64 fits in 20 decimal digits
    let mut len = 0;
    if n == 0 {
        syscall_lib::write_str(STDOUT_FILENO, "0");
        return;
    }
    while n > 0 {
        buf[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    buf[..len].reverse();
    // Safety: all bytes are ASCII digits.
    if let Ok(s) = core::str::from_utf8(&buf[..len]) {
        syscall_lib::write_str(STDOUT_FILENO, s);
    }
}

/// Write a `u32` decimal integer to stdout without heap allocation.
fn write_u32(n: u32) {
    write_u64(n as u64);
}

// ---------------------------------------------------------------------------
// Error logging — structured single-line for the E.2 acceptance bullet
// ---------------------------------------------------------------------------

fn log_error(stage: &str, err: AudioClientError) {
    // Assemble the FAIL line in a stack buffer and emit it in one write
    // so the smoke harness — which fires on the `AUDIO_DEMO:FAIL stage=`
    // prefix — captures the variant suffix instead of a torn line.
    let mut buf = [0u8; 192];
    let mut len = 0;
    let mut errno_buf = [0u8; 16];
    let errno_slice = format_errno(&err, &mut errno_buf);
    let parts: [&[u8]; 7] = [
        b"AUDIO_DEMO:FAIL stage=",
        stage.as_bytes(),
        b" variant=",
        error_label(err).as_bytes(),
        b" errno=",
        errno_slice,
        b"\n",
    ];
    for part in parts {
        let take = part.len().min(buf.len() - len);
        buf[len..len + take].copy_from_slice(&part[..take]);
        len += take;
        if len == buf.len() {
            break;
        }
    }
    let _ = syscall_lib::write(STDOUT_FILENO, &buf[..len]);
}

/// Format the errno tag for the FAIL line. Only `Io(i32)` carries an
/// errno today; every other variant emits `-` so the smoke harness
/// always sees a stable `errno=<value>` field.
fn format_errno<'a>(err: &AudioClientError, buf: &'a mut [u8; 16]) -> &'a [u8] {
    let value = match err {
        AudioClientError::Io(code) => *code,
        _ => return &b"-"[..],
    };
    let mut n = value.unsigned_abs() as u64;
    let mut tmp = [0u8; 16];
    let mut idx = tmp.len();
    if n == 0 {
        idx -= 1;
        tmp[idx] = b'0';
    } else {
        while n > 0 {
            idx -= 1;
            tmp[idx] = b'0' + (n % 10) as u8;
            n /= 10;
        }
    }
    let digits = &tmp[idx..];
    let mut out_len = 0;
    if value < 0 {
        buf[out_len] = b'-';
        out_len += 1;
    }
    let take = digits.len().min(buf.len() - out_len);
    buf[out_len..out_len + take].copy_from_slice(&digits[..take]);
    out_len += take;
    &buf[..out_len]
}

fn error_label(err: AudioClientError) -> &'static str {
    match err {
        AudioClientError::Io(_) => "Io",
        AudioClientError::Protocol(_) => "Protocol",
        // Inline the AudioError discriminant so the smoke harness can
        // distinguish `WouldBlock` (ring-pressure retry exhaustion)
        // from `Internal` (DMA fault) etc. without a separate field.
        AudioClientError::Server(inner) => match inner {
            AudioError::Busy => "Server:Busy",
            AudioError::WouldBlock => "Server:WouldBlock",
            AudioError::NoDevice => "Server:NoDevice",
            AudioError::BrokenPipe => "Server:BrokenPipe",
            AudioError::InvalidFormat => "Server:InvalidFormat",
            AudioError::InvalidArgument => "Server:InvalidArgument",
            AudioError::Internal => "Server:Internal",
            _ => "Server:Unknown",
        },
        AudioClientError::AlreadyOpen => "AlreadyOpen",
        AudioClientError::NotOpen => "NotOpen",
        AudioClientError::UnexpectedReply => "UnexpectedReply",
        // `AudioClientError` is `#[non_exhaustive]`. New variants
        // surface as a labelled-but-generic line so the demo's exit
        // path stays well-formed even after an ABI bump.
        _ => "Unknown",
    }
}
