//! Audio error → negative errno mapping — Phase 57 B.5.
//!
//! Tests-only commit. The implementation lands in the next commit; these
//! tests are committed first to satisfy the red-before-green TDD rule.
//!
//! `audio_error_to_neg_errno` is the single workspace site that
//! translates [`AudioError`] values into negative POSIX-style errno
//! integers. Every kernel-side or userspace-side path that surfaces a
//! POSIX errno on the audio surface must call into this helper — a
//! workspace-wide grep for `AudioError ->` arrows or `EBUSY` /
//! `EAGAIN` in audio-adjacent files must confirm a single call site
//! per variant. This mirrors the Phase 55c `net_error_to_neg_errno`
//! discipline.
//!
//! Mapping table (per Phase 57 task list, B.5 acceptance):
//!
//! | Variant            | Errno (negative) |
//! |--------------------|------------------|
//! | `Busy`             | `-EBUSY` (-16)   |
//! | `WouldBlock`       | `-EAGAIN` (-11)  |
//! | `NoDevice`         | `-ENODEV` (-19)  |
//! | `BrokenPipe`       | `-EPIPE` (-32)   |
//! | `InvalidFormat`    | `-EINVAL` (-22)  |
//! | `InvalidArgument`  | `-EINVAL` (-22)  |
//! | `Internal`         | `-EIO` (-5)      |

use crate::audio::AudioError;

/// `-EBUSY` — resource is busy (single-client policy collision).
pub const NEG_EBUSY: i64 = -16;
/// `-EAGAIN` — caller should retry later (transient ring-full / restart).
pub const NEG_EAGAIN: i64 = -11;
/// `-ENODEV` — device claim has not completed.
pub const NEG_ENODEV: i64 = -19;
/// `-EPIPE` — client stream has been disconnected.
pub const NEG_EPIPE: i64 = -32;
/// `-EINVAL` — protocol argument invalid (format / layout / payload size).
pub const NEG_EINVAL: i64 = -22;
/// `-EIO` — hard I/O error (DMA fault, register sequence violation).
pub const NEG_EIO: i64 = -5;

/// Map an [`AudioError`] to a negative POSIX errno.
///
/// Total mapping over the closed [`AudioError`] surface; the match is
/// exhaustive so adding a new `AudioError` variant requires updating
/// this function (the compiler enforces the obligation).
///
/// This is the **single** workspace site that performs this
/// translation — `audio_server`, every audio client, and any future
/// kernel facade share the same mapping. A workspace-wide grep for
/// `audio_error_to_neg_errno` returns exactly one definition site.
pub const fn audio_error_to_neg_errno(error: AudioError) -> i64 {
    match error {
        AudioError::Busy => NEG_EBUSY,
        AudioError::WouldBlock => NEG_EAGAIN,
        AudioError::NoDevice => NEG_ENODEV,
        AudioError::BrokenPipe => NEG_EPIPE,
        AudioError::InvalidFormat => NEG_EINVAL,
        AudioError::InvalidArgument => NEG_EINVAL,
        AudioError::Internal => NEG_EIO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioError;

    #[test]
    fn busy_maps_to_ebusy() {
        assert_eq!(audio_error_to_neg_errno(AudioError::Busy), -16);
    }

    #[test]
    fn would_block_maps_to_eagain() {
        assert_eq!(audio_error_to_neg_errno(AudioError::WouldBlock), -11);
    }

    #[test]
    fn no_device_maps_to_enodev() {
        assert_eq!(audio_error_to_neg_errno(AudioError::NoDevice), -19);
    }

    #[test]
    fn broken_pipe_maps_to_epipe() {
        assert_eq!(audio_error_to_neg_errno(AudioError::BrokenPipe), -32);
    }

    #[test]
    fn invalid_format_maps_to_einval() {
        assert_eq!(audio_error_to_neg_errno(AudioError::InvalidFormat), -22);
    }

    #[test]
    fn invalid_argument_maps_to_einval() {
        assert_eq!(audio_error_to_neg_errno(AudioError::InvalidArgument), -22);
    }

    #[test]
    fn internal_maps_to_eio() {
        assert_eq!(audio_error_to_neg_errno(AudioError::Internal), -5);
    }

    /// Total-mapping check. Every variant must produce a non-zero
    /// negative integer; this is also the place that catches the
    /// "added a new AudioError variant but forgot to map it" mistake
    /// — the exhaustive match in audio_error_to_neg_errno would fail
    /// to compile, and adding a new arm here forces the mapping
    /// decision to be conscious.
    #[test]
    fn every_variant_maps_to_a_negative_errno() {
        for err in [
            AudioError::Busy,
            AudioError::WouldBlock,
            AudioError::NoDevice,
            AudioError::BrokenPipe,
            AudioError::InvalidFormat,
            AudioError::InvalidArgument,
            AudioError::Internal,
        ] {
            let n = audio_error_to_neg_errno(err);
            assert!(n < 0, "{err:?} mapped to non-negative {n}");
        }
    }
}
