//! `symphonia-play` — Phase 105 Track E terminal audio player.
//!
//! Decodes audio files (FLAC / WAV / OGG-Vorbis / MP3, per the enabled
//! symphonia features), converts to the one format `audio_server`
//! accepts (S16LE, 48 kHz, stereo — see `kernel-core/src/audio/format.rs`),
//! and submits the PCM over the m3OS audio IPC.
//!
//! ## Why this is a musl-`std` binary that speaks m3OS IPC
//!
//! symphonia requires `std`, so this is a Phase 94-style cargo port
//! (`x86_64-unknown-linux-musl`, static ET_EXEC). The kernel dispatches
//! Linux-numbered and m3OS-native syscalls from ONE flat table with no
//! personality gate (`kernel/src/arch/x86_64/syscall/mod.rs`), and the
//! m3OS syscall convention is byte-identical to Linux x86_64
//! (`rax` + `rdi/rsi/rdx/r10/r8/r9`, `syscall`), so the three IPC
//! syscalls the audio path needs are invoked here via raw `asm!` — see
//! [`m3ipc`]. The wire protocol is a small re-expression of
//! `kernel-core/src/audio/protocol.rs` + `userspace/lib/audio_client`
//! (those are `x86_64-unknown-none` workspace crates a musl crate cannot
//! link); constants carry provenance comments back to the originals.
//!
//! ## Serial sentinels (the `symphonia-smoke` oracle)
//!
//! - `SYMPHONIA_PLAY:decoded file=<f> codec=<c> rate=<hz> ch=<n> frames=<n>`
//!   (decode-only mode, or printed before playback starts)
//! - `SYMPHONIA_PLAY:ok file=<f> frames48k=<n>` — playback drained.
//! - `SYMPHONIA_PLAY:error file=<f> reason=<r>` — any failure.

use std::path::Path;

mod m3ipc;
mod player;
mod resample;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// A fully decoded stream: interleaved i16 at the SOURCE rate/channels.
struct Decoded {
    samples: Vec<i16>,
    rate: u32,
    channels: usize,
    codec: String,
}

fn decode_file(path: &Path) -> Result<Decoded, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "no decodable track".to_string())?;
    let track_id = track.id;
    let codec = format!("{:?}", track.codec_params.codec);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("decoder: {e}"))?;

    let mut samples: Vec<i16> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<i16>> = None;
    let mut rate = 0u32;
    let mut channels = 0usize;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // End of stream: symphonia signals it as an IO error.
            Err(SymError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("next_packet: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                if sample_buf.is_none() {
                    let spec = *audio_buf.spec();
                    rate = spec.rate;
                    channels = spec.channels.count();
                    sample_buf =
                        Some(SampleBuffer::<i16>::new(audio_buf.capacity() as u64, spec));
                }
                let sb = sample_buf.as_mut().unwrap();
                sb.copy_interleaved_ref(audio_buf);
                samples.extend_from_slice(sb.samples());
            }
            // A malformed packet is skippable; a real error is not.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode: {e}")),
        }
    }

    if samples.is_empty() || rate == 0 || channels == 0 {
        return Err("decoded zero frames".to_string());
    }
    Ok(Decoded {
        samples,
        rate,
        channels,
        codec,
    })
}

fn file_label(path: &Path) -> &str {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<file>")
}

fn run(paths: &[String], decode_only: bool) -> i32 {
    let mut failures = 0;
    for p in paths {
        let path = Path::new(p);
        let label = file_label(path);
        let decoded = match decode_file(path) {
            Ok(d) => d,
            Err(e) => {
                println!("SYMPHONIA_PLAY:error file={label} reason=decode:{e}");
                failures += 1;
                continue;
            }
        };
        let src_frames = decoded.samples.len() / decoded.channels;
        println!(
            "SYMPHONIA_PLAY:decoded file={} codec={} rate={} ch={} frames={}",
            label, decoded.codec, decoded.rate, decoded.channels, src_frames
        );
        if decode_only {
            continue;
        }

        // audio_server accepts exactly S16LE / 48 kHz / mono|stereo — we
        // always submit stereo.
        let stereo = resample::to_stereo_48k(&decoded.samples, decoded.channels, decoded.rate);
        let frames48k = stereo.len() / 2;
        match player::play_s16le_stereo_48k(&stereo) {
            Ok(()) => println!("SYMPHONIA_PLAY:ok file={label} frames48k={frames48k}"),
            Err(e) => {
                println!("SYMPHONIA_PLAY:error file={label} reason=play:{e}");
                failures += 1;
            }
        }
    }
    if failures == 0 { 0 } else { 1 }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let decode_only = args.iter().any(|a| a == "--decode-only");
    let paths: Vec<String> = args.into_iter().filter(|a| a != "--decode-only").collect();
    if paths.is_empty() {
        println!("usage: symphonia-play [--decode-only] <file>...");
        std::process::exit(2);
    }
    std::process::exit(run(&paths, decode_only));
}
