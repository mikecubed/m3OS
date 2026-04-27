//! Phase 57 audio pure-logic surfaces.
//!
//! The Phase 57 audio subsystem is built on a single-source-of-truth
//! `kernel-core` module. `audio_server` (the ring-3 driver), every audio
//! client library (`userspace/lib/audio_client`), the `audio-demo`
//! reference, `term`'s bell, and any future media client all consume the
//! definitions in this module — a workspace-wide grep for any audio
//! symbol declared here must return exactly one definition site.
//!
//! The chosen first audio target is the Intel 82801AA AC'97 controller
//! (`0x8086:0x2415`); see `docs/appendix/phase-57-audio-target-choice.md`.
//! That choice constrains `format::PcmFormat` (`S16Le`),
//! `format::SampleRate` (`Hz48000`), and `format::ChannelLayout`
//! (`Mono` / `Stereo`) — no speculative variants are added.
//!
//! [`format`] declares the PCM-format value types.
//! [`ring`] declares the single-producer / single-consumer ring-buffer
//! state model that the audio_server io loop uses to track DMA progress.

pub mod format;
pub mod ring;

pub use format::{ChannelLayout, PcmFormat, SampleRate, frame_size_bytes};
pub use ring::{AudioRingState, AudioSink, RingError};
