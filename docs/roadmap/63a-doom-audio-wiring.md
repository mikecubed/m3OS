# Phase 63a - DOOM Audio Wiring

**Status:** Planned
**Source Ref:** phase-63a
**Depends on:** Phase 47 (DOOM) ✅, Phase 57 (Audio and Local Session) ✅, Phase 63 (Audio Stack Implementation) ✅
**Builds on:** Phase 63 made `audio_server` emit real PCM end-to-end and Phase 57 already exposed the `audio_client` library; this phase plugs the DOOM port into that stack by adding two new userspace crates (`audio_client_ffi` for the C-ABI seam, `audio_mixer` for the pure-logic mix engine), three new doomgeneric platform-layer translation units (`m3os_sound.c`, `m3os_dmx.c`, `m3os_music.c`), a one-file patches overlay that registers them with the engine's module list, an xtask build-wiring flip from `-UFEATURE_SOUND` to `-DFEATURE_SOUND`, and a `doom-audio-smoke` gate paralleling Phase 63's `audio-smoke` shape. The kernel does not change; everything in 63a lives in userspace and xtask.
**Primary Components:** `userspace/lib/audio_mixer` (new), `userspace/lib/audio_client_ffi` (new), `userspace/doom/m3os_sound.c` (new), `userspace/doom/m3os_dmx.c` (new), `userspace/doom/m3os_music.c` (new), `userspace/doom/patches/i_sound.c` (new), `xtask/src/main.rs::build_doom`, `xtask/src/main.rs::cmd_doom_audio_smoke` (new)

## Milestone Goal

A user running `cargo xtask run-gui` can launch `/bin/doom` from the `term` prompt and hear DOOM's SFX (menu cursor, gunshots, doors, monster sounds) and a Tier 2a square/triangle synth rendition of the title music play through the host audio device. `cargo xtask doom-audio-smoke` boots headless, scripts DOOM into Episode 1 Map 1 via `-warp 1 1`, fires the player's pistol with one `Ctrl` keystroke to produce a `DSPPISTOL` SFX submission, asserts `frames_consumed > 0` via the shutdown-time `M3OS_DOOM:audio_summary` line, and verifies the QEMU-recorded WAV is non-silent. The Phase 63 single-client `EBUSY` policy is preserved: if `audio_server` is busy when DOOM starts, DOOM logs `doom.audio.unavailable` once at INFO and runs silently; the BEL is silently dropped while DOOM holds the stream and re-arms on DOOM exit. The kernel patch-bumps to `0.63.1`.

## Why This Phase Exists

Phase 47 landed DOOM as a framebuffer + keyboard application and explicitly deferred sound to Phase 57. Phase 57 landed `audio_server` and `audio_client` with full host-test coverage but a stub `Ac97Backend`. Phase 63 replaced that stub with real PCM emission, closing the kernel/driver/server stack — but the DOOM platform layer (`userspace/doom/dg_m3os.c`) still has zero audio wiring (`grep -E 'audio|sound|sfx|i_sound' dg_m3os.c` returns nothing), and `build_doom` in `xtask/src/main.rs` still passes `-UFEATURE_SOUND` so the upstream `i_sound.c` dispatcher compiles to no-ops.

63a is the last consumer-side gap. Without it, the BEL is the only audible payload on the system and the audio path has only one consumer archetype (single-tone `audio-demo`). DOOM is a materially different consumer — game-loop cadence, 16-channel mix, real WAD-embedded sample data at native rates — and exercises ABI assumptions that a single sine wave never touches. 63a also makes the mix-engine surface a reusable crate (`audio_mixer`), which prepares the ground for the future system mixer service the Phase 63 doom-audio-wiring memo names as Tier 4.

The phase deliberately stays in userspace + xtask. The kernel ABI does not change. The Phase 63 driver, the `audio_server` registry, and the `audio_client` library are all reused unchanged.

## Learning Goals

- See how a sound-module function-table boundary in a legacy C engine (`sound_module_t`, `music_module_t`) is a textbook example of dependency injection — the same engine swaps between SDL, ALSA, and m3OS back-ends with zero engine-code changes.
- Understand how a Rust library can be reused across a C↔Rust language boundary via a thin staticlib + generated header, keeping the protocol definition single-sourced rather than hand-copied into every consumer.
- Learn how separating decode (DMX header parse), mix (channels → 48 kHz stereo S16LE), and transport (`audio_client_ffi`) into three orthogonal modules makes each independently unit-testable and lets a future system mixer service consume the mix engine without touching DOOM.
- See why an "in-loop submit" model is the right starting point on a young OS — the engine's frame cadence, the BDL cushion, and Phase 63's underrun-zero-fill together make a producer thread unnecessary for a 35 Hz game loop.
- Understand the design tension between "DOOM-faithful music" (SoundFont synth, large sample bank, licensing) and "DOOM-audible music" (square/triangle synth, no extra assets), and why a teaching OS picks the latter first.

## Feature Scope

### Reusable mix engine — `audio_mixer` crate (Track A)

A new `userspace/lib/audio_mixer/` crate exposes a pure-logic, `#![no_std]` mixer that any audio consumer can drive. Public Rust surface: a `Mixer` struct with a parameterized channel count, a `ChannelState` describing one active sound, and a `step` method that consumes a slice of mix-frames-to-produce and emits stereo S16LE bytes. Resampling is 16.16-fixed-point linear interpolation. Volume is a `u8` 0..=127 scale matching DOOM's per-channel left/right pan. The crate also exposes a C-ABI surface (`audio_mixer_*` functions over an opaque `Mixer*` handle) for the doomgeneric C consumer. Pure-Rust host tests live in the crate alongside the implementation; `cargo test -p audio_mixer` covers exact-output assertions, channel-isolation, clamp-at-bounds, and resampler precision.

### C-ABI veneer — `audio_client_ffi` crate (Track B)

A new `userspace/lib/audio_client_ffi/` crate wraps `audio_client::AudioClient` in a C-callable surface: `audio_ffi_connect`, `audio_ffi_open`, `audio_ffi_submit`, `audio_ffi_drain`, `audio_ffi_get_stats`, `audio_ffi_close`, plus an opaque handle type. The crate ships a hand-written `audio_client.h` C header (committed to the repo for IDE friendliness; verified against the Rust signatures by a build-script post-check). Error codes round-trip as `int`, with stable constants matching `AudioClientError` discriminants. The crate is consumed by DOOM via a static link in `build_doom` and is available to any future C consumer without rework.

### DMX decoder — `m3os_dmx.c` (Track C)

A pure C module parses the WAD-embedded DMX header (12-byte header: 16-bit format tag, 16-bit sample rate, 32-bit sample count, 16-bit padding × 2; payload follows as unsigned 8-bit PCM). The decoder validates the header, bounds the sample window against the lump length, and returns a `(rate_hz, samples_u8, len)` triple. DOOM's `S_StartSound` caches the result per SFX lump; `m3os_dmx_decode` is called once per lump and never on the hot path.

### Platform sound module — `m3os_sound.c` (Track D)

Implements the `sound_module_t` function table:

- `Init`: connects to `audio_server` via `audio_ffi_connect`, calls `audio_ffi_open` with the fixed 48 kHz / S16LE / stereo format. On `EBUSY`, logs `doom.audio.unavailable` once at INFO, sets a module-level `audio_disabled` flag, and binds `StartSound` / `UpdateSoundParams` to no-op stubs for the rest of the process. On success, creates a `Mixer` via `audio_mixer_new(32)` — SFX claims channels `0..15` (DOOM's `MAX_CHANNELS = 16`), the Tier 2a music synth claims `16..31`.
- `Shutdown`: drains and closes the stream, frees the mixer.
- `StartSound`: looks up the cached decode, claims a free mixer channel, seeds it with the sample pointer, source rate, volume, and pan.
- `UpdateSoundParams` / `StopSound` / `SoundIsPlaying`: thin wrappers over `Mixer` channel operations.
- `Update`: invoked once per tic; calls `audio_mixer_step` to produce one tic's worth of output (≈ 11 BDL slots × 512 bytes), then submits via `audio_ffi_submit`. On `WouldBlock`, drops this tic's submission and lets the BDL drain — Phase 63's underrun-zero-fill handles the recovery if the ring empties.
- `GetSfxLumpNum`, `CacheSounds`: unchanged from the upstream defaults — these are WAD-relative lookups that don't touch the audio path.

The transport is injected via a `m3os_audio_submitter_t` function-table struct held in the module-level state — production wires it to `audio_client_ffi`; the host unit tests wire it to a `FakeSubmitter` recording every call. The mixer is held behind the same DI seam so the test suite never needs `audio_server`.

### Tier 2a music synth — `m3os_music.c` (Track E)

Implements the `music_module_t` function table over a tick-driven MUS parser plus a 16-voice square/triangle synth that writes into the same `audio_mixer` accumulator (music voices are extra channels, not a parallel mix path — Single Responsibility for the mixer crate). The synth uses fixed-point phase accumulators per voice, with the MUS instrument number mapping to a tiny waveform table (square + triangle, no envelope shaping in Tier 2a). `SetMusicVolume`, `PauseMusic`, `ResumeMusic`, `RegisterSong`, `UnRegisterSong`, `PlaySong`, `StopSong`, `MusicIsPlaying` all route through the synth state machine; `Poll` is invoked once per tic from `m3os_sound::Update` (single submit path, single mixer).

MIDI fallthrough — the small minority of WADs that ship MIDI directly — reuses the MUS event dispatcher with a thin format converter on `RegisterSong`. SoundFont-driven synth (Tier 2b) is deferred to a separate phase.

### Engine registration overlay — `patches/i_sound.c` (Track F)

A patches-overlay file (same mechanism Phase 47 uses for `i_input.c`) replaces upstream's module-registration list with one that includes `m3os_sound_module` and `m3os_music_module`. The overlay is copied over the upstream source by `build_doom` after `git checkout`. The mechanism is unchanged from Phase 47; only one new file is added.

### xtask build wiring (Track G)

Four changes to `build_doom` plus one new platform-layer marker:

1. Flip `-UFEATURE_SOUND` to `-DFEATURE_SOUND` so upstream `i_sound.c` and `s_sound.c` compile in.
2. Add `m3os_sound.c`, `m3os_dmx.c`, `m3os_music.c` to the source list.
3. Link the `audio_client_ffi` and `audio_mixer` Rust staticlibs into the final musl-gcc invocation, with `-I` pointing at the committed C headers.
4. In `dg_m3os.c`, add a one-shot `M3OS_DOOM:title_ready` serial print on the first `DG_DrawFrame` invocation so the smoke harness has a deterministic "DOOM is past init" signal. Engine-side audio module init flow stays unchanged — upstream's `I_InitSound` iterates the registered `sound_modules` array Track F installs.

### Audio smoke gate (Track H)

A new `cmd_doom_audio_smoke` function adds a `cargo xtask doom-audio-smoke` subcommand mirroring `cmd_audio_smoke`'s WAV-backed shape: boot headless with the WAV-recording AC'97 audiodev, wait for `term`, send `/bin/doom -warp 1 1\n` (the `-warp` flag skips the title-screen menu entirely and drops the player into Episode 1 Map 1 with a pistol in hand), wait for the `M3OS_DOOM:title_ready` marker, send a single `Ctrl` keystroke to fire the pistol (producing one `DSPPISTOL` SFX submission), then issue the DOOM in-game quit sequence (`Esc` → `Q` → `Y`) which triggers `m3os_sound_module.Shutdown`. Shutdown prints `M3OS_DOOM:audio_summary frames_submitted=<N> frames_consumed=<M> underruns=<K>`; the harness asserts `frames_consumed > 0` from that line and ≥ 5 % non-silent samples from the recorded WAV.

The gate is *not* wired into `cmd_check` (which stays QEMU-free) — instead it joins `audio-smoke`, `bell-smoke`, `smoke-test`, and `regression` as a pre-push hook gate. This mirrors Phase 63's `audio-smoke` placement.

### Stream-leak resilience (Track I)

If DOOM crashes or is SIGKILLed mid-frame, a relaunch within one second must still acquire the stream. The behavior is mostly governed by `audio_server`'s socket-disconnect → stream-close path, which Phase 57's tests already cover — Track I is verification only, not new code: a host-side test in `audio_client_ffi` exercises the connect/open/abort/reconnect/open cycle, and a `doom-audio-smoke` post-step launches DOOM twice back-to-back and asserts the second launch never logs `doom.audio.unavailable code=ebusy`.

### Kernel patch bump + doc wiring (Track J)

`kernel/Cargo.toml` moves from `0.63.0` to `0.63.1`. No kernel source changes. The bump lets 63a release independently from the next kernel-touching phase. The roadmap README and `AGENTS.md` project overview are updated, the `docs/appendix/doom-audio-wiring.md` memo is flipped to "Implemented in Phase 63a", and a new learning doc captures the manual audible-on-host smoke checklist.

## Important Components and How They Work

### `userspace/lib/audio_mixer/src/lib.rs`

Owns `Mixer`, `ChannelState`, and the pure mix loop. `Mixer::step(out: &mut [u8], frames: usize)` walks the (up to 32) channels — SFX in `0..15`, music voices in `16..31` — for each active channel computes `(source_rate << 16) / 48000` per output frame, advances a 16.16 cursor into the sample slice, looks up two adjacent samples and linearly interpolates, multiplies by per-channel left/right volume into a 32-bit accumulator, then clamps the accumulator to `i16::MIN..=i16::MAX` on store. No allocation in `step`. The C-ABI surface (`audio_mixer_new`, `audio_mixer_set_channel`, `audio_mixer_clear_channel`, `audio_mixer_step`, `audio_mixer_drop`) takes an opaque `*mut Mixer` and returns stable `int` codes.

### `userspace/lib/audio_client_ffi/src/lib.rs`

Wraps `audio_client::AudioClient` in `#[no_mangle] pub extern "C"` shims. Owns a `Mutex<Option<AudioClient<SyscallSocket>>>` behind an opaque handle to keep the C side from having to think about thread safety. Error mapping is a flat table over the cartesian product of `AudioClientError` × the inner `AudioError` payload of `Server(_)`: `Server(AudioError::Busy)` → `AUDIO_FFI_ERR_BUSY`, `Server(AudioError::WouldBlock)` → `AUDIO_FFI_ERR_WOULD_BLOCK`, `Server(AudioError::FormatMismatch)` → `AUDIO_FFI_ERR_FORMAT`, `Io(_)` → `AUDIO_FFI_ERR_IO`, `Protocol(_)` → `AUDIO_FFI_ERR_PROTOCOL`, `AlreadyOpen` / `NotOpen` / `UnexpectedReply` → their own constants. The full table is published in `audio_client.h`. The build script reads the header, regex-matches every `#define AUDIO_FFI_* <int>` line, and `assert!`s the value matches the corresponding `pub const` in `src/lib.rs` — mismatch is a hard build error so the C and Rust tables cannot silently drift.

### `userspace/doom/m3os_dmx.c`

Pure-C decoder. One function: `int m3os_dmx_decode(const uint8_t *lump, size_t lump_len, m3os_dmx_decoded *out)`. Validates the format tag (must be 3 for DMX), bounds the sample count against the lump length, and returns 0 on success or `-1` on malformed input. No allocation; `out->samples` points into the caller's lump buffer (zero-copy decode).

### `userspace/doom/m3os_sound.c`

Holds the `sound_module_t` table, the `m3os_audio_submitter_t` injection point, and the module-level state (`mixer`, `submitter`, `audio_disabled`, per-channel cache). Every callable in the function table is one of:

- thin wrapper that delegates to the mixer or the submitter (`StartSound`, `UpdateSoundParams`, `StopSound`, `SoundIsPlaying`);
- no-op stub when `audio_disabled` is set (so a `Busy` outcome at init silently degrades to "no audio" without per-call branches in the hot path);
- `Init` / `Shutdown` lifecycle hooks.

The submitter and mixer fields are populated in `Init`; tests construct the module state with a `FakeSubmitter` and a real `Mixer`, exercising the full table without `audio_server`.

### `userspace/doom/m3os_music.c`

Holds the `music_module_t` table, the MUS event queue, the per-voice synth state, and a thin MIDI → MUS converter for `RegisterSong`. Voices are extra mixer channels (channel indices 16..32 of a 32-channel `Mixer` instance) so the audio path through `audio_mixer_step` is identical for SFX and music. `Poll` advances the MUS tick scheduler and dispatches NoteOn / NoteOff / PitchBend / Controller events to the synth voices.

### `userspace/doom/patches/i_sound.c`

Overlay file. Replaces the upstream module-registration list with one that names `m3os_sound_module` and `m3os_music_module`. Same patch-overlay mechanism Phase 47 uses (the patches dir is copied over `target/doomgeneric-src/doomgeneric/` after `git checkout` in `build_doom`).

### `xtask/src/main.rs::build_doom`

Three small additions: drop `-UFEATURE_SOUND`, add the three new `.c` files to `c_files`, append two `-L<rust-target>/libaudio_client_ffi.a` / `-L<rust-target>/libaudio_mixer.a` arguments before the final `-o`. The musl cross-compiler is unchanged.

### `xtask/src/main.rs::cmd_doom_audio_smoke`

New top-level command. Boots QEMU with the existing `wav,id=snd0,path=…` audiodev (same shape `audio-smoke` uses), waits for `term` to register, sends `/bin/doom -warp 1 1\n` (the `-warp` flag drops the player directly into Episode 1 Map 1 with a pistol in hand — no title-screen menu navigation), waits for the `M3OS_DOOM:title_ready` marker (Track G.4), sends one `Ctrl` keystroke to fire the pistol (a single `DSPPISTOL` SFX submission), sends the DOOM in-game quit sequence (`Esc` → `Q` → `Y`) so `m3os_sound_module.Shutdown` prints the `M3OS_DOOM:audio_summary` line, parses the WAV file, and asserts both `frames_consumed > 0` (from the summary line) and ≥ 5 % non-silent samples (from the WAV). Three distinct failure modes per Phase 63's pattern.

## How This Builds on Earlier Phases

- Reuses Phase 63's PCM emission path unchanged. No kernel work in 63a.
- Reuses `audio_client::AudioClient` unchanged — `audio_client_ffi` wraps it, doesn't replace it.
- Reuses Phase 57's single-client / single-stream `EBUSY` policy. DOOM's `Init` simply maps `EBUSY` to silent-fallback.
- Reuses Phase 57's `Bell` + `AudioClientBellSink` unchanged — the BEL silently drops while DOOM holds the stream because that's already the behavior when a second `audio_client` connects.
- Reuses Phase 47's patches-overlay mechanism (`userspace/doom/patches/` copied over upstream after `git checkout`). One new file in that directory.
- Reuses Phase 63's `audio-smoke` shape for `doom-audio-smoke`: same WAV backend, same non-silent assertion, same serial-console scripting harness.

## Implementation Outline

TDD-first across the board — the mixer math, the DMX decoder, and the FFI veneer all live in pure-logic modules that are unit-testable before any DOOM-side wiring. Then the integration tracks (D, E, F, G) plug the pure modules into the engine and the smoke harness.

1. `userspace/lib/audio_mixer` — design `Mixer` and `ChannelState`, write the Rust tests for `step`, implement, expose C ABI.
2. `userspace/lib/audio_client_ffi` — design the C surface, write the host-side tests against a `FakeAudioClient`, implement the shims, write `audio_client.h`, wire the build-script post-check.
3. `userspace/doom/m3os_dmx.c` — write a small host-side C test runner that links the file standalone, assert on synthetic WAD-DMX inputs.
4. `userspace/doom/m3os_sound.c` — implement against `FakeSubmitter` and the real `Mixer`, write tests for the SFX state machine without `audio_server`.
5. `userspace/doom/m3os_music.c` — implement the MUS parser and the synth, host-tested the same way.
6. `userspace/doom/patches/i_sound.c` — register the two modules; smallest file in the phase.
7. `xtask/src/main.rs::build_doom` — flip `FEATURE_SOUND`, add the new C files, link the Rust staticlibs.
8. `xtask/src/main.rs::cmd_doom_audio_smoke` — new subcommand mirroring `cmd_audio_smoke`.
9. Stream-leak resilience verification: dual-launch test in `doom-audio-smoke`.
10. Bump `kernel/Cargo.toml` to `0.63.1`; update `AGENTS.md` and `docs/roadmap/README.md`; flip `docs/appendix/doom-audio-wiring.md` to "Implemented in Phase 63a" with a closure pointer to this doc.

## Acceptance Criteria

- `cargo xtask run-gui` + manual DOOM launch: title-screen menu cursor SFX is audibly heard; gunshot, door, and pickup SFX are audibly heard during gameplay; a square/triangle rendition of the title music plays.
- `cargo xtask doom-audio-smoke` passes with the `frames_consumed > 0` summary-line assertion plus the ≥ 5 % non-silent WAV check; the same gate fails when run against a scratch revert of Track D's mixer-submit wiring (confirmed by reverting and re-running).
- A second consecutive `/bin/doom` launch within one second of the first crash/exit acquires the stream cleanly (no `EBUSY` from a leaked socket).
- When `term`'s BEL fires while DOOM is holding the stream, the BEL is silently dropped — no crash, no log spam, and the BEL re-arms audibly on DOOM exit (verified by a follow-up BEL test post-DOOM in `doom-audio-smoke`).
- `audio_disabled` fallback verified: a second instance of `/bin/doom` started while the first is running prints `doom.audio.unavailable` once at INFO, runs without crashing, and exits cleanly.
- `cargo test -p audio_mixer` and `cargo test -p audio_client_ffi` pass; the mixer host tests assert exact output samples for at least: single-channel mute, single-channel full-volume, two-channel left-pan + right-pan, clamp-at-bounds for an overdriven mix, and resampler precision at 11025 → 48000.
- `kernel/Cargo.toml` is at `0.63.1`; `AGENTS.md` and `docs/roadmap/README.md` reflect the bump; `docs/appendix/doom-audio-wiring.md` carries the Phase 63a closure note.

## Companion Task List

- [Phase 63a Task List](./tasks/63a-doom-audio-wiring-tasks.md)

## How Real OS Implementations Differ

- Linux PulseAudio / PipeWire mix many DOOM-style clients concurrently; m3OS's single-client `EBUSY` policy means BEL and DOOM cannot both play at once. The Phase 63 memo's Tier 4 system-mixer service is the m3OS path to closing that gap.
- Modern DOOM ports (Chocolate, Crispy, GZDoom) use SDL_mixer (or its successor) for SRC and channel management; m3OS rolls a minimal in-process mixer because SDL is not on the platform. The shape is the same — channel state + per-channel resampler + accumulator + clamp — the dependency surface is smaller.
- DOOM-faithful music uses FluidSynth + a SoundFont (~ 1–10 MB sample bank). m3OS Tier 2a uses a square/triangle synth (no extra assets, no licensing surface). Tier 2b (SoundFont) is a separate deferred phase.
- Production audio servers run the producer on a dedicated thread to insulate the audio device from game-loop hitches. 63a uses in-loop submit because m3OS userspace threading in C is unproven and Phase 63's underrun-zero-fill makes a missed tic survivable. The thread is the right shape long-term, once Phase 76 dynamic linker / pthreads land.

## Deferred Until Later

- Tier 2b SoundFont synth (proposed `63b-doom-music-soundfont`).
- Tier 4 system mixer service that lets BEL and DOOM coexist (no roadmap entry yet; will land alongside whichever phase first needs concurrent audio).
- Distance-attenuation curve A/B match against Chocolate DOOM reference (memo's Tier 3 §12).
- Crossfade on `S_ChangeMusic` (memo's Tier 3 §13).
- `audio_server` restart resync (memo's Tier 3 §14) — defer until Phase 64's session-manager lifecycle work formalizes service restart budgets.
- Producer thread for the audio submit loop — defer until Phase 76 lands a stable userspace threading story.
- Dynamic BDL sizing and latency reporting to clients.
- Per-client volume control (DOOM uses one global gain via `snd_SfxVolume` / `snd_MusicVolume`).
