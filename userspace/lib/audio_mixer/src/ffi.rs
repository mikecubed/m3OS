//! C-ABI surface for [`crate::Mixer`].
//!
//! All symbols here use `#[no_mangle] pub extern "C"`. Errors are
//! returned as the negative `AUDIO_MIXER_ERR_*` constants below;
//! success is `0` for the verbs that return an `int`, or the count
//! of bytes written for [`audio_mixer_step`] (which returns `isize`).
//!
//! Memory: [`audio_mixer_new`] returns a heap-allocated `Mixer` via
//! `Box::into_raw`. The caller owns the pointer and must release it
//! with [`audio_mixer_drop`].

use core::ffi::c_int;

use crate::Mixer;

/// Sentinel — success.
pub const AUDIO_MIXER_OK: c_int = 0;
/// Invalid argument (e.g. channel_count > 32, null pointer where not
/// allowed).
pub const AUDIO_MIXER_ERR_INVAL: c_int = -1;
/// Sample buffer pointer was null or len was zero.
pub const AUDIO_MIXER_ERR_EMPTY: c_int = -2;
/// Output buffer was smaller than `frames * 4` bytes.
pub const AUDIO_MIXER_ERR_OUTPUT_TOO_SMALL: c_int = -3;
/// Null Mixer handle.
pub const AUDIO_MIXER_ERR_NULL_HANDLE: c_int = -4;

/// Allocate a new `Mixer` with `channel_count` slots. Returns a
/// raw pointer the caller owns, or `NULL` if `channel_count` is
/// invalid (must be `<= MAX_CHANNELS`).
///
/// # Safety
///
/// The returned pointer must be released with [`audio_mixer_drop`].
#[unsafe(no_mangle)]
pub extern "C" fn audio_mixer_new(channel_count: usize) -> *mut Mixer {
    if channel_count > crate::MAX_CHANNELS {
        return core::ptr::null_mut();
    }
    // Heap allocation is required so a C caller can store the
    // pointer across calls. The mixer itself does not allocate after
    // construction.
    alloc_box(Mixer::new(channel_count))
}

/// Release a `Mixer` allocated with [`audio_mixer_new`].
///
/// # Safety
///
/// `mixer` must be a pointer previously returned by
/// [`audio_mixer_new`] and not previously freed. After this call
/// the pointer is invalid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_mixer_drop(mixer: *mut Mixer) {
    if mixer.is_null() {
        return;
    }
    // SAFETY: caller upholds that `mixer` came from
    // `audio_mixer_new` and is unfreed.
    unsafe {
        drop_box(mixer);
    }
}

/// Seed channel `idx` with a sample buffer, source rate, and pan
/// volumes (`0..=127`). Returns `0` on success or a negative error
/// code.
///
/// # Safety
///
/// - `mixer` must be a valid pointer from [`audio_mixer_new`].
/// - `samples` must be readable for `len` bytes for as long as the
///   channel is active. The mixer stores the raw pointer; freeing
///   the source buffer before the channel is cleared is undefined
///   behavior.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_mixer_set_channel(
    mixer: *mut Mixer,
    idx: usize,
    samples: *const u8,
    len: usize,
    source_rate_hz: u32,
    left_vol: u8,
    right_vol: u8,
) -> c_int {
    if mixer.is_null() {
        return AUDIO_MIXER_ERR_NULL_HANDLE;
    }
    if samples.is_null() || len == 0 {
        return AUDIO_MIXER_ERR_EMPTY;
    }
    if source_rate_hz == 0 {
        return AUDIO_MIXER_ERR_INVAL;
    }
    // SAFETY: caller upholds that `mixer` is valid.
    let m = unsafe { &mut *mixer };
    if idx >= m.channel_count() {
        return AUDIO_MIXER_ERR_INVAL;
    }
    // SAFETY: caller upholds the slice contract.
    let slice = unsafe { core::slice::from_raw_parts(samples, len) };
    // SAFETY: re-entering the safe API; the slice contract was just
    // documented for the caller.
    unsafe {
        m.set_channel(idx, slice, source_rate_hz, left_vol, right_vol);
    }
    AUDIO_MIXER_OK
}

/// Seed channel `idx` with a sample buffer that loops indefinitely
/// (the cursor wraps modulo `len`). Used by music voices so a
/// one-period waveform sustains until `audio_mixer_clear_channel`
/// silences it. Returns `0` on success or a negative error code.
///
/// # Safety
///
/// Same contract as [`audio_mixer_set_channel`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_mixer_set_channel_loop(
    mixer: *mut Mixer,
    idx: usize,
    samples: *const u8,
    len: usize,
    source_rate_hz: u32,
    left_vol: u8,
    right_vol: u8,
) -> c_int {
    if mixer.is_null() {
        return AUDIO_MIXER_ERR_NULL_HANDLE;
    }
    if samples.is_null() || len == 0 {
        return AUDIO_MIXER_ERR_EMPTY;
    }
    if source_rate_hz == 0 {
        return AUDIO_MIXER_ERR_INVAL;
    }
    // SAFETY: caller upholds that `mixer` is valid.
    let m = unsafe { &mut *mixer };
    if idx >= m.channel_count() {
        return AUDIO_MIXER_ERR_INVAL;
    }
    // SAFETY: caller upholds the slice contract.
    let slice = unsafe { core::slice::from_raw_parts(samples, len) };
    // SAFETY: re-entering the safe API; the slice contract was just
    // documented for the caller.
    unsafe {
        m.set_channel_loop(idx, slice, source_rate_hz, left_vol, right_vol);
    }
    AUDIO_MIXER_OK
}

/// Zero channel `idx`.
///
/// # Safety
///
/// `mixer` must be a valid pointer from [`audio_mixer_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_mixer_clear_channel(mixer: *mut Mixer, idx: usize) -> c_int {
    if mixer.is_null() {
        return AUDIO_MIXER_ERR_NULL_HANDLE;
    }
    // SAFETY: caller upholds validity.
    let m = unsafe { &mut *mixer };
    if idx >= m.channel_count() {
        return AUDIO_MIXER_ERR_INVAL;
    }
    m.clear_channel(idx);
    AUDIO_MIXER_OK
}

/// Mix `frames` stereo S16LE frames into `out` (capacity
/// `byte_capacity`). Returns the number of bytes written, or a
/// negative error code.
///
/// # Safety
///
/// - `mixer` must be a valid pointer from [`audio_mixer_new`].
/// - `out` must be writable for `byte_capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_mixer_step(
    mixer: *mut Mixer,
    out: *mut u8,
    byte_capacity: usize,
    frames: usize,
) -> isize {
    if mixer.is_null() {
        return AUDIO_MIXER_ERR_NULL_HANDLE as isize;
    }
    if out.is_null() {
        return AUDIO_MIXER_ERR_EMPTY as isize;
    }
    let needed = frames.saturating_mul(crate::BYTES_PER_FRAME);
    if byte_capacity < needed {
        return AUDIO_MIXER_ERR_OUTPUT_TOO_SMALL as isize;
    }
    // SAFETY: caller upholds validity / writability.
    let m = unsafe { &mut *mixer };
    let slice = unsafe { core::slice::from_raw_parts_mut(out, byte_capacity) };
    let written = m.step(slice, frames);
    written as isize
}

// Internal helpers — the alloc dependency is isolated to the FFI
// surface so the pure-Rust mixer in `lib.rs` stays allocation-free
// in `step`. The C-ABI heap-allocates the `Mixer` so a C caller can
// hold the pointer across calls.
extern crate alloc;

fn alloc_box(mixer: Mixer) -> *mut Mixer {
    let boxed = alloc::boxed::Box::new(mixer);
    alloc::boxed::Box::into_raw(boxed)
}

unsafe fn drop_box(ptr: *mut Mixer) {
    // SAFETY: caller upholds that `ptr` was produced by
    // `Box::into_raw` and not previously freed.
    let _ = unsafe { alloc::boxed::Box::from_raw(ptr) };
}

// ---------------------------------------------------------------------------
// FFI host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_round_trip_matches_rust_api() {
        let mixer = audio_mixer_new(4);
        assert!(!mixer.is_null());
        let samples: alloc::vec::Vec<u8> = alloc::vec![192u8, 192, 192, 192];
        let rc = unsafe {
            audio_mixer_set_channel(mixer, 0, samples.as_ptr(), samples.len(), 48_000, 127, 127)
        };
        assert_eq!(rc, AUDIO_MIXER_OK);

        let mut out = [0u8; 4 * crate::BYTES_PER_FRAME];
        let n = unsafe { audio_mixer_step(mixer, out.as_mut_ptr(), out.len(), 4) };
        assert_eq!(n, 16);
        // Sample 192 → signed +16384 → with vol 127 → 16256.
        for i in 0..4 {
            let off = i * crate::BYTES_PER_FRAME;
            let l = i16::from_le_bytes([out[off], out[off + 1]]);
            assert_eq!(l, 16256);
        }

        let rc = unsafe { audio_mixer_clear_channel(mixer, 0) };
        assert_eq!(rc, AUDIO_MIXER_OK);

        unsafe { audio_mixer_drop(mixer) };
    }

    #[test]
    fn ffi_rejects_oversized_channel_count() {
        let m = audio_mixer_new(crate::MAX_CHANNELS + 1);
        assert!(m.is_null());
    }

    #[test]
    fn ffi_rejects_null_handle() {
        let rc = unsafe { audio_mixer_clear_channel(core::ptr::null_mut(), 0) };
        assert_eq!(rc, AUDIO_MIXER_ERR_NULL_HANDLE);
        let n = unsafe { audio_mixer_step(core::ptr::null_mut(), core::ptr::null_mut(), 0, 0) };
        assert_eq!(n as c_int, AUDIO_MIXER_ERR_NULL_HANDLE);
    }

    #[test]
    fn ffi_rejects_undersized_output() {
        let mixer = audio_mixer_new(1);
        let mut tiny = [0u8; 2];
        let n = unsafe { audio_mixer_step(mixer, tiny.as_mut_ptr(), tiny.len(), 16) };
        assert_eq!(n as c_int, AUDIO_MIXER_ERR_OUTPUT_TOO_SMALL);
        unsafe { audio_mixer_drop(mixer) };
    }

    #[test]
    fn ffi_rejects_empty_sample_buffer() {
        let mixer = audio_mixer_new(1);
        let rc =
            unsafe { audio_mixer_set_channel(mixer, 0, core::ptr::null(), 0, 48_000, 127, 127) };
        assert_eq!(rc, AUDIO_MIXER_ERR_EMPTY);
        unsafe { audio_mixer_drop(mixer) };
    }
}
