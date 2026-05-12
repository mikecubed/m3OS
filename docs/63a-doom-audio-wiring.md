# DOOM Audio Wiring (Phase 63a)

**Aligned Roadmap Phase:** Phase 63a
**Status:** Complete
**Source Ref:** phase-63a
**Supersedes Legacy Doc:** [`docs/appendix/doom-audio-wiring.md`](./appendix/doom-audio-wiring.md) (the proposal memo is retained as a historical design pointer).

## Overview

Phase 63a closes the consumer-side gap that Phase 47 (DOOM) deferred to Phase 57 and that Phase 57 deferred to Phase 63: with the audio path now able to emit real PCM end-to-end (Phase 63), DOOM finally gains audible SFX and Tier 2a synth music through `audio_server`. Two new userspace crates appear (`audio_mixer` — a pure-logic 32-channel software mixer with a stable C-ABI surface; `audio_client_ffi` — a thin C-ABI veneer over `audio_client::AudioClient`), three new doomgeneric platform-layer files land (`m3os_sound.c`, `m3os_music.c`, `m3os_dmx.c`), the engine's `i_sound.c` registration list gets a one-file patches overlay, and `cargo xtask doom-audio-smoke` joins the audio-smoke / bell-smoke pair as a deterministic CI gate that boots DOOM through `fb-takeover`, verifies non-zero `frames_consumed` across two consecutive runs, and re-arms the BEL post-DOOM.

Kernel source does not change in 63a. The kernel patch-bumps from `0.63.0` → `0.63.1` so the phase can release independently from the next kernel-touching phase. Every host-side test added in 63a runs under `cargo xtask check` without booting QEMU — 12 `audio_mixer` Rust tests, 1 no-alloc integration test, 7 `audio_client_ffi` Rust tests, 6 DMX C tests, 10 sound-module C tests, 7 music-synth C tests.

## What This Doc Covers

- The shape of the new `audio_mixer` crate (channel state, 16.16 fixed-point resampler, clamp, per-frame accumulator).
- The C-ABI veneer pattern — how `audio_client_ffi` keeps `audio_client` single-sourced for the protocol byte format while still letting C consumers drive it.
- The two compile-time drift checks (`audio_mixer.h` / `audio_client.h` ↔ `pub const`) and the `doom_c_test_step` xtask helper that runs the per-module C tests on every commit.
- Why Tier 2a music uses MUS channel-id parity for waveform selection rather than tracking `Controller(0)=patch` events.
- The `m3os_audio_submitter_t` DI seam and how the host tests replace `audio_client_ffi` with a recording fake.
- The `/tmp/doom-autoquit-tics` seam that lets the smoke gate trigger a clean engine shutdown without injecting PS/2 scancodes.
- Why the staticlib link goes through one combined `libaudio_client_ffi.a` (with `audio_mixer` rolled in via `#[used]`) rather than two side-by-side `.a` files — duplicate `#[panic_handler]` / `#[global_allocator]` symbols.

## Key Files

| File | Role |
|---|---|
| `userspace/lib/audio_mixer/src/lib.rs` | `Mixer`, `ChannelState`, 16.16-fixed-point resampler, per-frame `i32` accumulator, `i16` clamp |
| `userspace/lib/audio_mixer/src/ffi.rs` | `audio_mixer_*` C-ABI surface over an opaque `Mixer *` handle |
| `userspace/lib/audio_mixer/include/audio_mixer.h` | Hand-shipped C header, drift-verified by `build.rs` |
| `userspace/lib/audio_mixer/tests/no_alloc.rs` | `#[global_allocator]` tripwire test asserting no allocation in `Mixer::step` |
| `userspace/lib/audio_client_ffi/src/lib.rs` | `audio_ffi_*` C-ABI shims, flat-table error mapping, `AudioFfiHandle` |
| `userspace/lib/audio_client_ffi/src/staticlib_runtime.rs` | musl-only `#[panic_handler]` + libc-malloc `#[global_allocator]` |
| `userspace/lib/audio_client_ffi/src/mixer_reexport.rs` | `#[used]` keepalive that prevents the `audio_mixer_*` C symbols from being dead-code-eliminated |
| `userspace/lib/audio_client_ffi/include/audio_client.h` | C header for the FFI shims; `AUDIO_FFI_ERR_*` constants drift-verified by `build.rs` |
| `userspace/doom/m3os_dmx.c` / `m3os_dmx.h` | DMX SFX lump decoder (zero-copy `(rate, samples, len)` triple) |
| `userspace/doom/m3os_sound.c` / `m3os_sound.h` | `sound_module_t` over the mixer + DI submitter seam; `EBUSY` silent-fallback |
| `userspace/doom/m3os_music.c` / `m3os_music.h` | Tier 2a square/triangle synth + MUS event scheduler |
| `userspace/doom/patches/i_sound.c` | Engine registration overlay — `sound_modules[]` + `InitMusicModule()` point at our modules |
| `userspace/doom/dg_m3os.c` | `M3OS_DOOM:title_ready` marker + `/tmp/doom-autoquit-tics` autoquit seam |
| `userspace/doom/tests/test_m3os_*.c` | Host-side C unit tests (6 + 10 + 7) run by `doom_c_test_step` |
| `xtask/src/main.rs::build_doom` | Staticlib build + `-DFEATURE_SOUND` + extended cache fingerprint |
| `xtask/src/main.rs::cmd_doom_audio_smoke` | 25-step serial script: boot → DOOM → audio_summary → relaunch → bell-test |
| `.githooks/pre-push` | `M3OS_DOOM_AUDIO_REGRESSION=1` env-var gate |

## Manual Smoke Checklist

The automated `doom-audio-smoke` gate boots headless with a WAV backend. For the audible-on-host verification:

1. `cargo xtask run-gui` — boot the OS with the PulseAudio backend (or whichever real audio backend the host has).
2. At the m3OS login, sign in as `root` (default password — see Phase 27 doc).
3. `/bin/fb-takeover /bin/doom -iwad /usr/share/doom/doom1.wad`
4. Confirm the **title-screen menu cursor SFX** is audibly heard as the menu cursor moves up/down (use arrow keys).
5. Start a new game; confirm the **in-game gunshot SFX** (`DSPPISTOL`) is audible when firing.
6. Confirm the **title screen's Tier 2a square/triangle synth music** is audible (intentionally crude — not the full Bobby Prince track).
7. Quit DOOM via `Esc` → `Q` → `Y` (or wait for the autoquit if `/tmp/doom-autoquit-tics` is set).
8. After DOOM exits, run `/bin/bell-test`; confirm the BEL chime is audible and `BELL_TEST:PASS:consumed=<N>` prints with `N > 0`.
9. Optionally: relaunch `/bin/fb-takeover /bin/doom -iwad ...` within a few seconds to confirm DOOM's audio path acquires the stream cleanly the second time around (no `doom.audio.unavailable code=ebusy` line on the serial console).

If any step is silent or produces the EBUSY fallback line, file an issue with the serial-console transcript attached.

## Deferred Until Later

A detailed inventory of every deferred item — with concrete effort estimates and dependency notes for the next person picking one up — lives in **[`docs/appendix/doom-audio-deferred-work.md`](./appendix/doom-audio-deferred-work.md)**. Highlights:

- **Tier 2b SoundFont synth** (proposed `63b-doom-music-soundfont`): the proper "DOOM-faithful music" path. ~8 h focused work, deserves a real phase.
- **Tier 4 system mixer service** so BEL + DOOM can coexist concurrently: 1–2 days, its own phase, the `audio_mixer` crate is already named generically so the future service can consume it.
- **Producer thread for the audio submit loop**: blocked on Phase 76 (dynamic linker / pthreads).
- **MIDI fallthrough in `RegisterSong`** — landed correctly only after Tier 2b's preset state exists.
- **Loose follow-ups** (cross-fade between voice-reclaim notes, MUS NoteOn velocity continuation, `UpdateSoundParams` mid-note volume changes, `audio_server` restart resync, distance-attenuation A/B match, crossfade on `S_ChangeMusic`, bandlimited synthesis, sub-tick audio sub-stepping, dynamic BDL sizing, per-client volume) — each sized at 15 min – 2 h.

The Tier-2a-plus drum-synth (squarewave/triangle drums via channel 15 in MUS) is not in the deferred list because it landed alongside this doc.

Out-of-band concerns (not audio-specific): QEMU-monitor `sendkey` infrastructure so the smoke gate can drive DOOM via real scancodes instead of the `/tmp/doom-autoquit-tics` seam — useful for testing DOOM input handling, not strictly necessary for the audio path.

## Cross-Links

- Memo this implementation phase ratifies — [`docs/appendix/doom-audio-wiring.md`](./appendix/doom-audio-wiring.md)
- Phase 47 DOOM port — [`docs/47-doom.md`](./47-doom.md)
- Phase 57 audio + local session — [`docs/57-audio-and-local-session.md`](./57-audio-and-local-session.md)
- Phase 63 audio stack implementation — [`docs/63-audio-stack-implementation.md`](./63-audio-stack-implementation.md)
- Phase 63a design doc — [`docs/roadmap/63a-doom-audio-wiring.md`](./roadmap/63a-doom-audio-wiring.md)
- Phase 63a task list — [`docs/roadmap/tasks/63a-doom-audio-wiring-tasks.md`](./roadmap/tasks/63a-doom-audio-wiring-tasks.md)
