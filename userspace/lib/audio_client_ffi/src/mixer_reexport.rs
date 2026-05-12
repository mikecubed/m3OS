//! Force the `audio_mixer_*` C-ABI symbols to be preserved in the
//! `audio_client_ffi` staticlib.
//!
//! When `audio_mixer` is a transitive rlib dependency, Rust's
//! dead-code analyzer drops symbols that aren't referenced from the
//! staticlib root's Rust code. The C-ABI consumers (`m3os_sound.c`,
//! `m3os_music.c`) call `audio_mixer_*` directly from C, so the
//! Rust-side analyzer never sees them as live.
//!
//! `#[used]` on a static of function pointers keeps the references
//! alive through the analyzer; the linker then pulls each function
//! body into the .a so the C-side `extern "C"` calls resolve.

use core::ffi::c_int;

/// Force the linker to keep every public `audio_mixer_*` C symbol.
/// Each function pointer here transitively pulls in the function
/// body during dead-code elimination so the staticlib contains the
/// full C-ABI surface.
#[used]
static AUDIO_MIXER_KEEPALIVE: AudioMixerSymbols = AudioMixerSymbols {
    new: audio_mixer::ffi::audio_mixer_new,
    drop: audio_mixer::ffi::audio_mixer_drop,
    set_channel: audio_mixer::ffi::audio_mixer_set_channel,
    set_channel_loop: audio_mixer::ffi::audio_mixer_set_channel_loop,
    clear_channel: audio_mixer::ffi::audio_mixer_clear_channel,
    release_channel: audio_mixer::ffi::audio_mixer_release_channel,
    step: audio_mixer::ffi::audio_mixer_step,
};

#[allow(dead_code)]
struct AudioMixerSymbols {
    new: extern "C" fn(usize) -> *mut audio_mixer::Mixer,
    drop: unsafe extern "C" fn(*mut audio_mixer::Mixer),
    set_channel: unsafe extern "C" fn(
        *mut audio_mixer::Mixer,
        usize,
        *const u8,
        usize,
        u32,
        u8,
        u8,
    ) -> c_int,
    set_channel_loop: unsafe extern "C" fn(
        *mut audio_mixer::Mixer,
        usize,
        *const u8,
        usize,
        u32,
        u8,
        u8,
    ) -> c_int,
    clear_channel: unsafe extern "C" fn(*mut audio_mixer::Mixer, usize) -> c_int,
    release_channel: unsafe extern "C" fn(*mut audio_mixer::Mixer, usize, u16) -> c_int,
    step: unsafe extern "C" fn(*mut audio_mixer::Mixer, *mut u8, usize, usize) -> isize,
}
