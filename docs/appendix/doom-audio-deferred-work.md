# DOOM Audio — Deferred Work Inventory

**Status:** Scheduling reference (not a phase doc)
**Source Ref:** post-phase-63a
**Cross-links:**
- Original design memo — [`docs/appendix/doom-audio-wiring.md`](./doom-audio-wiring.md)
- Phase 63a learning doc — [`docs/63a-doom-audio-wiring.md`](../63a-doom-audio-wiring.md)
- Phase 63a design — [`docs/roadmap/63a-doom-audio-wiring.md`](../roadmap/63a-doom-audio-wiring.md)

---

## Purpose

Phase 63a wired DOOM SFX + a Tier 2a square/triangle music synth through `audio_server`. Several follow-ups were intentionally deferred — some sized for a dedicated phase, others small enough to be loose follow-ups. This doc inventories every deferred item with a concrete effort estimate and dependency notes so the next person picking one up doesn't have to re-derive scope from the design doc.

Each item lists:

- **Why deferred** — the trade-off Phase 63a made.
- **Effort** — honest engineering hours (not padded calendar days).
- **Depends on** — explicit dependencies, if any.
- **Suggested grouping** — which future phase is the right home, or "loose follow-up" if it can land any time.

---

## Suggested phase: 63b — SoundFont Music Synth (Tier 2b)

The headline deferred item. Replaces the Tier 2a square/triangle synth with a sample-based SoundFont (SF2) renderer. Closer to canonical DOOM music; subsumes the drum-synth path because drums are just SF2 presets in bank 128.

### 63b-A. SF2 RIFF parser

- **Why deferred:** Phase 63a's Tier 2a path is "synth without external assets". SF2 adds a parser, an asset to bundle, and a richer voice model.
- **Effort:** 1–2 h. RIFF chunk walking with bounds checks; the trickier `pgen`/`igen` chunks are uniform `{u16 oper, u16 amount}` records.
- **Depends on:** nothing in 63a.

### 63b-B. Preset → instrument → sample lookup pipeline

- **Why deferred:** Only meaningful with the parser in place.
- **Effort:** 1 h. Three nested table lookups plus a velocity / note-range match.
- **Depends on:** 63b-A.

### 63b-C. Mixer support for i16 samples + sub-range loop

- **Why deferred:** SF2 samples are 16-bit signed PCM with loop start/end points inside the sample buffer; Phase 63a's mixer only supports u8 DMX-format and whole-buffer looping.
- **Effort:** 1 h. Add a `sample_format` enum and `loop_start` / `loop_end` fields to `ChannelState`; one conditional in the per-frame `s0`/`s1` read.
- **Depends on:** nothing — could land standalone as a mixer improvement.
- **Test impact:** existing mixer tests stay green (u8 format remains default); add one i16-format test.

### 63b-D. Basic ADSR envelope in the mixer

- **Why deferred:** Tier 2a's fixed release-fade is enough for clean note endings; SF2 instruments specify per-preset attack / hold / decay / sustain-level / release that the rendered audio should reflect.
- **Effort:** 1 h. Extend the existing `fade_out_*` fields into a four-stage envelope; the release stage is what already exists.
- **Depends on:** nothing in 63a.

### 63b-E. MUS program → SF2 preset mapping (incl. drum bank 128)

- **Why deferred:** Tier 2a ignores MUS instrument numbers; SF2 needs them to pick the right preset.
- **Effort:** 30 min. 128-entry static GM-to-preset table; channel-15 maps to bank 128 (drum kit).
- **Depends on:** 63b-B.

### 63b-F. SoundFont asset bundling in xtask

- **Why deferred:** No SF2 → nothing to bundle.
- **Effort:** 30 min. Mirror the `doom1.wad` fetch/cache/embed pattern.
- **Depends on:** nothing technical; **does depend on a licensing decision** — pick TimGM6mb (≈6 MB, GPL-compatible) or trim a font to a DOOM-relevant subset (~20 melodic + drum kit, target ≤1 MB).

### 63b-G. Host tests for parser + lookup + envelope math

- **Effort:** 1–2 h. Same shape as the existing mixer / music tests.
- **Depends on:** 63b-A, B, D.

### 63b-H. Smoke gate + iteration on real DOOM music

- **Why deferred:** The integration-side work where unknowns live. Sample-pitch math when a sample's "natural" note differs from the playing note is the most likely source of bugs.
- **Effort:** 1–2 h.
- **Depends on:** all of the above.

**Total 63b effort:** 6–10 h focused work, probably 8. Deserves a real phase doc + task list because it ships a new asset, a parser, and a substantively richer audio path.

---

## Suggested phase: Tier 4 system mixer service

### Tier-4-A. System-wide mixer service that lets BEL + DOOM coexist

- **What:** Today the single-client `EBUSY` policy means BEL is silently dropped while DOOM holds the stream. A mixer service would sit between consumers and `audio_server` so multiple clients can submit independently.
- **Why deferred:** Phase 57's single-client policy is intentional first-step. The `audio_mixer` crate is already named generically so the service can consume it.
- **Effort:** **Substantially larger** than the other items here — call it 1–2 days of focused work. Needs a new daemon with client handling, fair-share mixing policy, restart story.
- **Depends on:** nothing in 63a; could land before or after 63b.
- **Suggested grouping:** its own phase, probably in the 70s-80s range alongside other system-service work.

---

## Loose follow-ups (any-time, small)

These don't deserve a dedicated phase. Any of them can be landed as a small PR when convenient.

### LF-1. Cross-fade between voice-reclaim notes

- **What:** Phase 63a uses cursor-preservation + voice lockout to suppress most reclaim clicks, but the volume-scaling step at the moment of replacement still produces a tiny click. A proper cross-fade (old voice fades out while new voice fades in across the same channel) eliminates it entirely.
- **Effort:** 1 h. Add a `fade_in_remaining` companion to `fade_out_remaining` in the mixer; have `set_channel_with` start a fade-in when the channel was active at re-seed time.
- **Depends on:** nothing.

### LF-2. Honour MUS NoteOn velocity continuation

- **What:** When MUS PlayNote omits the velocity byte, the spec says "use the previous velocity for this channel". 63a defaults to 127 instead. Music subtly loses dynamics.
- **Effort:** 15 min. Per-channel `last_velocity[16]` array in `m3os_mus_state`; PlayNote without velocity reads from it.
- **Depends on:** nothing.

### LF-3. UpdateSoundParams: respect mid-note vol/pan changes

- **What:** Engine calls this when a sound's source moves relative to the player; 63a's implementation is a no-op (relies on `S_UpdateSounds` re-invoking `StartSound`). Distance attenuation is therefore choppy.
- **Effort:** 30 min. Add a mixer API to update only the `left_vol` / `right_vol` of an active channel without resetting the cursor; wire from `UpdateSoundParams`.
- **Depends on:** nothing.

### LF-4. MUS PitchBend + Controller routing

- **What:** 63a parses these events but does nothing with them. PitchBend would change voice frequency; Controllers 0/7/10 set patch / volume / pan.
- **Effort:** 1 h once 63b-B lands (you need preset/voice state to apply them). Currently moot.
- **Depends on:** 63b-B (preset state) for full functionality. A subset (channel-volume controller 7 → master_vol per channel) is doable today in ~30 min.

### LF-5. `audio_server` restart resync in `audio_client_ffi`

- **What:** If the supervised audio_server restarts, the cached `AudioClient` socket goes dead and submits return `BrokenPipe`. 63a logs once and runs silent for the rest of the process. Term's `AudioClientBellSink` already implements re-connect-and-resume; 63a's FFI doesn't.
- **Effort:** 45 min. Add a re-connect path to `audio_ffi_submit` that triggers on `BrokenPipe`, re-opens, then retries. Reuse `term::bell::AudioClientBellSink`'s logic as a template.
- **Depends on:** nothing — independent of 63b.

### LF-6. Producer thread for the submit loop

- **What:** 63a uses in-loop submit (from `m3os_sound::Update` each tic). A dedicated thread would insulate audio from game-loop hitches. Today an `Update` that takes >29 ms causes BDL contention (zero-fill kicks in).
- **Why deferred:** m3OS userspace threading in C is unproven — Phase 47 doesn't use threads in DOOM.
- **Effort:** Once Phase 76 (dynamic linker / pthreads) lands, ~2 h: a thread that pulls from a ring buffer the mixer fills, with `audio_ffi_submit` rate-limited to consume the device's pace.
- **Depends on:** Phase 76.

### LF-7. Distance-attenuation curve A/B match against Chocolate DOOM

- **What:** Final-mix headroom and per-channel pan curves can be tuned to match Chocolate DOOM's reference output.
- **Effort:** 1–2 h, mostly listening + measurement (capture a Chocolate DOOM run, render an m3OS run, A/B-compare with a difference plot).
- **Depends on:** nothing.

### LF-8. Crossfade on `S_ChangeMusic`

- **What:** Avoid the abrupt cut when a level change swaps music tracks. Today the old track is stopped, the new one is registered + played — listener hears a small gap or click.
- **Effort:** 30 min. Trigger a release-fade on each active music voice when `m3os_mm_stop_song` is called; have the new song's first NoteOn start with a short fade-in.
- **Depends on:** LF-1 (which lands the fade-in mechanism).

### LF-9. Bandlimited synthesis to suppress high-pitch aliasing

- **What:** Tier 2a's 128-sample-per-period buffer aliases above ~1.5 kHz (the linear interpolator under-samples the smooth-triangle's curvature). Audible as a slight metallic / buzzy quality on high melodic notes.
- **Why deferred:** Tier 2b SoundFont voices are bandlimited by virtue of being real recordings; fixing Tier 2a aliasing duplicates effort the SF2 path will obviate.
- **Effort:** 2 h if pursued — pre-compute multiple lower-detail waveform buffers (mip-mapping) and have the synth pick one based on note frequency.
- **Depends on:** nothing technical; skip if 63b is the next phase.

### LF-9b. Smooth the drum-synth noise envelope

- **What:** Tier 2a drums (cymbals, hats, snare) are filtered white noise × linear decay envelope. WAV analysis of `doom-audio-smoke` output shows the "clicks" the listener reports during music are mostly the noise-content sample-by-sample transitions (jump amplitudes of 4000–7000 i16 at sub-millisecond intervals — exactly the shape of white noise). Real DOOM percussion uses recorded drum kit samples; ours are synthesized.
- **Why deferred:** the drum samples land naturally in Tier 2b's SoundFont path (`63b-E` maps MUS channel 15 to SF2 preset bank 128). A Tier 2a-only "make the drum noise less harsh" fix would be a 1-h band-pass filter + envelope tweak, but the gains are limited — Tier 2a-quality drums by definition won't sound like Bobby Prince's recordings.
- **Effort:** 1 h if pursued (single-pole low-pass IIR on the noise samples, shaped attack envelope on cymbals). The `analyze_wav.rs` tool committed alongside this doc gives concrete before/after metrics if the experiment is run.
- **Depends on:** nothing technical; skip if 63b is the next phase.

### LF-10. Sub-tick audio sub-stepping (event-accurate timing)

- **What:** Phase 63a's `m3os_sound::Update` dispatches all 4 MUS ticks of a DOOM tic BEFORE running `audio_mixer_step` once. Events that should fire mid-tic (e.g. a 2-tick-long staccato note) get snapped to the tic boundary. Audible as slightly mistimed accents on rapid passages.
- **Effort:** 1 h. Split `Update`'s mixer step into 4 sub-steps of ~352 frames each, advancing one MUS tick between each.
- **Depends on:** nothing.
- **Caveat:** worth measuring whether the audible improvement justifies the 4× submit-call overhead before scheduling.

### LF-11. Dynamic BDL sizing and latency reporting to clients

- **What:** 63a uses a fixed 32-slot × 512-byte BDL ring (~85 ms). A future client (e.g. low-latency game audio) might want to query and adjust this.
- **Effort:** 1–2 h.
- **Depends on:** nothing in 63a.

### LF-12. Per-client volume control

- **What:** DOOM uses one global gain per category (`snd_SfxVolume`, `snd_MusicVolume`). Other clients might want their own. Currently `audio_server` doesn't have a per-client volume knob.
- **Effort:** 1 h.
- **Depends on:** Tier-4 mixer service (only meaningful with concurrent clients).

### LF-13. Sync `channel_active[]` with mixer auto-deactivation

- **What:** `m3os_sound_is_playing_inner` returns the C-side `g_state.channel_active[channel]` flag, which is only cleared on explicit `StopSound`. The mixer, however, auto-deactivates a non-looping channel when its cursor crosses `samples_len` (see `Mixer::step`'s "cursor_int >= samples_len" branch). The two states drift: `I_SoundIsPlaying` keeps returning `true` after a sample finishes naturally, so DOOM's `S_GetChannel` reuse logic believes channels are perpetually busy and falls back to aggressive voice-steal more often than necessary. Audible as slightly punchier reuse on rapid SFX bursts; the smoke gate still passes because the steal path is well-exercised.
- **Effort:** 1–2 h. Either add an `audio_mixer_channel_is_active(mixer, idx) -> bool` FFI query and have `m3os_sound_is_playing_inner` read it, or have `audio_mixer_step` return a "channels-that-became-inactive" bitset that `m3os_sound_update_inner` folds into `channel_active[]`. New host tests cover both directions of the sync.
- **Depends on:** nothing in 63a.

---

## Quick-pick ordering (engineer's-choice)

If picking one item to do next, in rough order of payoff per hour:

1. **LF-2** (15 min) — restore note dynamics.
2. **LF-5** (45 min) — driver-restart resilience.
3. **LF-3** (30 min) — smoother distance attenuation.
4. **LF-1** (1 h) — eliminate the final reclaim click.
5. **63b** as a phase (8 h) — the proper "DOOM-faithful music" path.

The drum-synth that lands alongside this doc is a small Tier-2a-plus addition (≈ 45 min) that doesn't appear in this list because it'll already be done by the time the doc is read.
