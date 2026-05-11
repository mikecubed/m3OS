# DOOM Audio Wiring — Design Notes for a Future Track

**Status:** Proposed (no implementation track scheduled yet)
**Source Ref:** post-phase-63
**Cross-links:**
- Phase 47 DOOM port — [`docs/47-doom.md`](../47-doom.md)
- Phase 57 audio + local session — [`docs/57-audio-and-local-session.md`](../57-audio-and-local-session.md)
- Phase 57 audio ABI — [`docs/appendix/phase-57-audio-abi.md`](./phase-57-audio-abi.md)
- Phase 57 audio target choice — [`docs/appendix/phase-57-audio-target-choice.md`](./phase-57-audio-target-choice.md)
- Phase 63 audio stack implementation — [`docs/roadmap/63-audio-stack-implementation.md`](../roadmap/63-audio-stack-implementation.md)

---

## Why This Doc Exists

The Phase 47 DOOM port (`userspace/doom/dg_m3os.c`) implements doomgeneric's framebuffer
and keyboard hooks only. It has no audio wiring at all: a `grep` of `dg_m3os.c` for
`audio|sound|sfx|i_sound` returns nothing, and the Phase 47 doc explicitly defers
"audio output … including DOOM sound/music support" to Phase 57. As of Phase 63 the
underlying capability finally exists end-to-end (`audio_server` emits real PCM through
AC'97 with `frames_consumed > 0` and a verified non-silent WAV recording from the
`audio-smoke` gate), but nothing routes DOOM's sound output into it yet.

This doc captures the wiring shape and the recommended feature set for a future
"DOOM audio" track, so when the work is scheduled the implementer does not have to
re-discover the constraints. It is not a binding plan — the scope is a design memo,
not a roadmap entry.

## The Engine's Sound Module Surface

doomgeneric carries the unmodified Chocolate DOOM `i_sound.c` dispatcher plus the
engine's mixer-callback model. The platform layer (m3OS, in our case) provides two
function tables:

| Table | Header | Purpose |
|---|---|---|
| `sound_module_t` | `i_sound.h` | Sound-effect playback: `Init`, `Shutdown`, `GetSfxLumpNum`, `Update`, `UpdateSoundParams`, `StartSound`, `StopSound`, `SoundIsPlaying`, `CacheSounds`. The engine calls these from the main tic loop |
| `music_module_t` | `i_sound.h` | MUS / MIDI music: `Init`, `Shutdown`, `SetMusicVolume`, `PauseMusic`, `ResumeMusic`, `RegisterSong`, `UnRegisterSong`, `PlaySong`, `StopSong`, `MusicIsPlaying`, `Poll` |

`i_sound.c` looks up which module to bind by walking a registered-modules list and
picking the first that succeeds at `Init`. Chocolate DOOM ships SDL-backed
implementations (`i_sdlsound.c`, `i_sdlmusic.c`); doomgeneric strips those out and
leaves `i_sound.c` calling out to whatever the platform layer registers, defaulting
to silent stubs when nothing is registered.

The integration shape on m3OS is therefore:

- Add two new translation units under `userspace/doom/` — say `m3os_sound.c` and
  `m3os_music.c` — that define `m3os_sound_module` and `m3os_music_module` of the
  declared types.
- Register both from `dg_m3os.c::DG_Init` (or from `userspace/doom/patches/i_sound.c`,
  if upstream's registration list is too restrictive to extend cleanly from outside).
- Implement the module bodies against the Phase 57 `audio_client` API.

DOOM's engine handles 3D-positional / distance / pitch math itself and hands the
platform 16-bit signed mono samples at the SFX source rate, plus per-channel
left/right volume bytes. The platform layer is responsible for resample + mix + pan,
not spatialization.

## audio_server Constraints That Shape the Design

Three Phase 57 / Phase 63 constraints fix the shape of the platform sound module
before the implementer writes a line of code. They are recorded here so the implementer
can build against the real ABI rather than against the most flexible imaginable one.

### 1. Single-client PCM-out stream

`audio_server` accepts exactly one PCM-out stream at a time. A second `try_open`
returns `AudioError::Busy` immediately (`userspace/audio_server/src/stream.rs`'s
`second_try_open_returns_busy` test pins this). Implications for DOOM:

- While DOOM holds the stream, **no other userspace process can play audio** — the
  terminal BEL, future system sounds, login-success chimes, all of them are blocked
  until DOOM closes its stream.
- DOOM must do its own mixing of SFX channels (DOOM's engine assumes the platform
  layer mixes up to 16 simultaneous SFX channels into one output stream).
- A later "system mixer" service (out of scope for this track) would resolve the
  contention by sitting between every client and `audio_server`; this doc deliberately
  does not require it.

The single-client constraint is therefore a **policy decision** for the DOOM track,
not an obstacle to fix. The recommended policy: DOOM grabs the stream at `DG_Init`,
holds it for the lifetime of the process, and releases it on clean shutdown. If
`audio_server` returns `Busy` (something else already has the stream), the DOOM
sound module falls back to the silent-stub behavior — gameplay still runs, just
without audio.

### 2. Fixed PCM format

`audio_server`'s `open(format, layout, rate)` validates against a one-tuple set in
`kernel-core/src/audio/format.rs::shape_supported`:

- `PcmFormat::S16Le` (16-bit signed little-endian);
- `ChannelLayout::Stereo` (2 channels);
- `SampleRate::Hz48000` (48 kHz, AC'97 VRA disabled).

Any other shape returns `AudioError::InvalidFormat`. So the m3OS sound module always
opens stereo S16LE @ 48 kHz, regardless of what the SFX source rate is, and the
**sample-rate conversion happens in the sound module**, not in `audio_server`.

### 3. Submit cap and BDL ring geometry

- `audio_client::submit_frames` rejects any payload larger than `MAX_SUBMIT_BYTES`
  (`userspace/lib/audio_client/src/protocol.rs` defines it; current value 64 KiB)
  with `Protocol(PayloadTooLarge)` before the IPC call.
- `submit_frames` is **all-or-nothing**: on success it returns `bytes.len()` exactly;
  on backpressure it returns `Server(WouldBlock)` and copies nothing. Phase 63 round-3
  review (commit `4ef2b74`) fixed a server-side partial-accept bug that violated this
  contract; the documented behavior is now also the actual behavior.
- The AC'97 BDL has `BDL_ENTRIES = 32` slots; each slot is `PCM_SLOT_STRIDE = 16 KiB
  / 32 = 512` bytes (= 128 stereo S16LE frames ≈ 2.67 ms of audio @ 48 kHz). The
  whole ring therefore holds ~85 ms of stereo audio when fully populated.

The sound module must therefore:

- chunk every submit into a multiple of `PCM_SLOT_STRIDE` (512 bytes) — partial-slot
  submissions return `InvalidArgument`;
- treat `WouldBlock` as a retry-later signal, not a fatal error;
- size its prefill / target-queue depth such that the BDL has enough lead time for
  DOOM's 35 Hz tic loop (≈ 28.6 ms per tic) without overshooting the 85 ms ring.

The recommended target is **2 BDL slots of lead time** (≈ 5.3 ms) at steady state,
which is well under the 28.6 ms-per-tic budget and well above the AC'97 emulation's
typical PulseAudio host-side jitter. Phase 57's `audio-demo` already exercises this
exact shape, so the timing is known good.

## Recommended Feature Set

The list below is graded; an implementer can ship the "Tier 1" set as a minimum
viable DOOM-audio track and defer the rest without leaving the audio path in a broken
state.

### Tier 1 — Minimum viable SFX

1. **`m3os_sound_module` skeleton** implementing the `sound_module_t` function table.
2. **Audio stream lifecycle**: open the PCM-out stream in `Init`, close on
   `Shutdown`. On `Busy`, log a one-line `doom.audio.unavailable` and switch the
   module's `StartSound` body to a no-op for the rest of the process.
3. **DMX → S16LE mono decode**: parse the WAD-embedded DMX header (12 bytes:
   format tag, sample rate, length, padding), bound the sample window, return a
   `(rate_hz, samples_s8_or_u8, len)` triple. The engine caches the result per
   SFX lump.
4. **Per-channel mixer** (16 channels matching DOOM's `MAX_CHANNELS`): each active
   channel carries `(sample_ptr, sample_len, position, source_rate, left_vol,
   right_vol)`. Use a 16.16 fixed-point cursor for resampling at
   `(source_rate << 16) / 48000` per output frame.
5. **48 kHz stereo output mix**: accumulate into 32-bit signed scratch buffer with
   `left_acc += sample * left_vol`, `right_acc += sample * right_vol`; clamp to
   `i16::MIN..=i16::MAX` on store. Volume is `0..=127`; left/right pan is already
   computed by DOOM and handed in via `UpdateSoundParams`.
6. **Producer thread / poll integration**: either a dedicated thread that calls
   `submit_frames` in a `WouldBlock`-aware loop, or a hook in DOOM's main tic loop
   that submits one BDL slot per tic. The thread approach is cleaner for jitter; the
   in-loop approach is simpler to debug.
7. **Sample-rate conversion**: nearest-neighbor or linear interpolation from
   `source_rate` (typically 11025 Hz or 22050 Hz for DMX) to 48000 Hz. Nearest-neighbor
   is acceptable for SFX; linear is preferred for music if Tier 2 lands.
8. **`AudioStats` health check**: poll `get_stats()` every N tics; if
   `underrun_count` is climbing the mixer is starving the device — increase prefill
   depth or reduce per-tic work.

### Tier 2 — Music

9. **`m3os_music_module` with MUS support**: parse DOOM's MUS format (a compact
   MIDI-like sequence), maintain a tick-driven scheduler, route events to a tiny
   in-process synth. Two synth choices:
   - **Square / triangle wave synth (Tier 2a)**: 16-voice basic waveform synth keyed
     by the MUS instrument number. Sufficient for "there is music," not faithful to
     the original. Lowest implementation cost.
   - **SoundFont-driven synth (Tier 2b)**: bundle a small SoundFont (≤ 1 MB) on the
     ext2 image; implement the SF2 sample-playback path. Closer to the canonical
     DOOM music experience but materially more code.
10. **MIDI fallthrough**: a small minority of WADs ship MIDI directly; reuse the same
    synth pipeline.
11. **Music gain control**: separate volume curve from SFX, controllable via the
    engine's existing `snd_MusicVolume` cvar plumbing.

### Tier 3 — Polish

12. **Distance attenuation curve match**: DOOM applies its own attenuation, but the
    final-mix headroom can be tuned per-channel to match the Chocolate DOOM reference
    more closely. A scoped A/B test against a recorded Chocolate DOOM run is the
    right validation harness.
13. **Crossfade on `S_ChangeMusic`**: avoid the abrupt cut when a level change
    swaps tracks.
14. **Resync on `audio_server` restart**: if the supervised driver restarts (Phase
    55b restart policy), the cached `AudioClient` socket goes dead. Catch the
    `BrokenPipe` / `EAGAIN` on submit, re-connect, re-open, refill BDL, resume play.
    Phase 57's `term::bell::AudioClientBellSink` already implements this pattern —
    reuse the shape.
15. **Latency calibration**: expose the target-queue-depth knob as a runtime tunable
    so a future fb-takeover-tiers-style perf doc can record the chosen value.
16. **Stuck-sample release**: DOOM's `StopSound` can run mid-channel; ensure the
    mixer immediately zeroes the channel's `sample_len` to avoid a single tic of
    leftover audio.

### Tier 4 — Deferred multi-client (out of scope; future track)

17. **System mixer service**: a small daemon that accepts N upstream `audio_client`
    connections, mixes them, and forwards a single stream to `audio_server`. Resolves
    the BEL-while-DOOM-runs contention. This is its own roadmap phase, not part of
    the DOOM audio track. The DOOM module should not assume it exists; if the system
    mixer is later introduced, the DOOM module's behavior does not change (it still
    opens `audio.cmd` exactly as it does today, the mixer just routes it).

## How DOOM's 35 Hz Tic Loop Maps to the Audio Ring

DOOM's main loop runs at 35 ticrate Hz → one tic every 28.57 ms. AC'97 at stereo
S16LE / 48 kHz = 192,000 bytes/sec. One tic of audio is therefore ≈ 5485.7 bytes,
which rounds to **11 BDL slots per tic** at 512 bytes/slot. The whole ring (32 slots)
holds ≈ 2.91 tics' worth of audio.

This produces a natural cadence: submit ~11 slots' worth per tic, keep at most ~22
slots in flight, leave ~10 slots of slack so the producer can absorb one slow tic
without the BDL going empty. The slack also means a single missed tic does not
cause an audible underrun — `underrun_count` should stay zero across a normal play
session.

If `underrun_count` ever does climb during play, the diagnosis order is:

1. `audio_server` stats: `frames_submitted - frames_consumed` near zero means the
   ring is starving;
2. m3OS tic-loop slowness (the Phase 47 framebuffer blit and WAD I/O are the usual
   suspects);
3. mixer per-tic cost (16 channels × ~5500 frames = ~88k mix ops per tic — well
   within budget on a single core but worth profiling).

## What Lives Where (Proposed File Layout)

| File | Purpose |
|---|---|
| `userspace/doom/m3os_sound.c` | `m3os_sound_module` body — open/close stream, mixer state, `StartSound` / `StopSound` / `Update` / `UpdateSoundParams` / `SoundIsPlaying` / `GetSfxLumpNum` / `CacheSounds` |
| `userspace/doom/m3os_mixer.c` | Pure mixer — 16-channel S16LE accumulator, 16.16-fixed-point resampler, S32→S16 clamp |
| `userspace/doom/m3os_dmx.c` | DMX SFX decoder — header parse, sample-window bounds check, returns `(rate_hz, samples, len)` |
| `userspace/doom/m3os_music.c` | `m3os_music_module` body — only landed in Tier 2 |
| `userspace/doom/patches/i_sound.c` | Persistent local patch registering `m3os_sound_module` (and `m3os_music_module` once Tier 2 lands) on the engine's module list — same patch-overlay mechanism Phase 47 uses for `i_input.c` |
| `userspace/doom/dg_m3os.c` | Call `m3os_sound_init()` from `DG_Init` (the m3OS-side hook), tear down from `DG_Shutdown` if/when doomgeneric adds one |

## Validation Plan

The work is testable along the same axes Phase 63 used for the audio path:

1. **Host unit tests** on the mixer math (no engine, no `audio_client`): drive a
   `m3os_mixer_step` C function with synthetic input, assert exact output samples.
   These run under the existing C-test scaffold already used by the Phase 47 build.
2. **Smoke gate** under `cargo xtask smoke-test`: a new `doom-audio-smoke` step that
   boots DOOM headlessly, drives a scripted SFX trigger (e.g., the title-screen
   menu cursor sound), records the QEMU PulseAudio sink to a WAV via
   `-audiodev wav,id=snd0,path=…`, and asserts the resulting recording is non-silent
   over a documented window — mirroring the `audio-smoke` shape from Phase 63.
3. **Manual GUI run** via `cargo xtask run-gui` for sustained gameplay-jitter
   listening — host PulseAudio sink.

## What Phase 63 Already Did (and Why That Matters Here)

The Phase 63 round-3 review specifically fixed two bugs that would have produced
silent or corrupted audio under any DOOM workload:

- **BDL DMA-mirror gap (PRRT…A8x2_, fixed in `4ef2b74`)**: `submit_frames_inner`
  was updating only the in-process `Ac97Logic` BDL mirror; the DMA-backed BDL that
  the controller reads via BDBAR was left at zero. Before the fix the device would
  have DMA'd from zeroed descriptors no matter what the producer submitted. DOOM
  would have produced complete silence even with a perfectly correct sound module.
- **Partial-accept vs `WouldBlock` (PRRT…A8x3J, fixed in the same commit)**:
  `submit_frames_inner` returned `Ok(short_n)` when the BDL had partial room. The
  documented `audio_client` contract is all-or-nothing; the previous behavior would
  have silently dropped the tail of any submission larger than the free-slot count.
  DOOM's per-tic ~5.5 KiB submission is large enough to hit this regularly during
  any sustained-loudness sequence (e.g., the second-level rocket fight).

A DOOM audio track that begins after Phase 63 inherits a working PCM path. A track
that had begun before Phase 63 would have been blocked on both bugs.

## Open Questions Left for the Implementing Track

These are intentionally not decided here; the track that lands the work should
record decisions in a follow-up appendix doc.

1. **Resampler quality**: nearest-neighbor vs linear vs windowed-sinc. Tier 1 should
   start with nearest-neighbor and measure perceived quality before paying for
   linear.
2. **Music synth**: Tier 2a (square/triangle) vs Tier 2b (SoundFont). The SoundFont
   choice depends on whether a small permissively-licensed SF2 file fits the ext2
   image budget.
3. **Producer thread vs in-loop submit**: the trade-off is jitter resilience vs
   debug simplicity. The Phase 63 `audio-demo` did in-loop; a real game with a
   software blit hot path may need the thread.
4. **Stream restart on `audio_server` crash**: the term-bell pattern is the
   reference; whether DOOM should re-attempt aggressively or fall through to
   silent depends on whether the team values robustness or determinism more during
   debugging.

The answers to those questions are the kind of decisions that fit cleanly into a
Phase task doc once the track is scheduled.
