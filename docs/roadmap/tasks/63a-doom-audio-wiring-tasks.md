# Phase 63a — DOOM Audio Wiring: Task List

**Status:** Planned
**Source Ref:** phase-63a
**Depends on:** Phase 47 (DOOM) ✅, Phase 57 (Audio and Local Session) ✅, Phase 63 (Audio Stack Implementation) ✅
**Goal:** Make DOOM's SFX and Tier 2a synth music audibly play through `audio_server`. Add two new userspace crates (`audio_mixer`, `audio_client_ffi`), three new doomgeneric platform-layer C files (`m3os_sound.c`, `m3os_dmx.c`, `m3os_music.c`), one patches-overlay file that registers the modules with the engine, the build-wiring flip in `xtask::build_doom`, a new `doom-audio-smoke` xtask gate paralleling Phase 63's `audio-smoke`, and a kernel patch-version bump to `0.63.1`. The kernel does not change.

## Context: what Phase 57 and Phase 63 already shipped

Tracks that earlier drafts proposed are **not Phase 63a work** — they already exist. Reusing what exists is mandatory; do not add a parallel `AudioClient`, `Ac97Backend`, `Bell`, or `frames_consumed` counter.

| Already shipped (do not redo) | Location |
|---|---|
| `AudioClient::{connect, open, submit_frames, drain, get_stats, close}` + `AudioStats` | `userspace/lib/audio_client/src/lib.rs` |
| `Ac97Backend` real PCM emission over PIO + DMA | `userspace/audio_server/src/device.rs` (Phase 63) |
| `AudioControlCommand::GetStats` returning `frames_submitted` / `frames_consumed` / `underrun_count` | `kernel-core/src/audio/protocol.rs`; wired in `userspace/audio_server/src/irq.rs::encode_outcome` |
| Single-client `EBUSY`-on-second-connect + rate-limited reject log + 13 host tests | `userspace/audio_server/src/client.rs` |
| Underrun-zero-fill repost so a missed deadline does not stay stuck | `userspace/audio_server/src/irq.rs::apply_irq_event` (Phase 63) |
| BEL → `Bell::ring` → `AudioClientBellSink` → `audio_client::submit_frames` | `userspace/term/src/{screen.rs,main.rs,bell.rs}` |
| QEMU `-audiodev` selection: PulseAudio for `run-gui`, WAV for `audio-smoke` | `xtask/src/main.rs` (Phase 63) |
| `audio-smoke` gate asserting `frames_consumed > 0` + non-silent WAV | `xtask/src/main.rs::cmd_audio_smoke` (Phase 63) |
| Phase 47 patches-overlay mechanism (`userspace/doom/patches/` copied over upstream after `git checkout`) | `xtask/src/main.rs::build_doom:1255-1273` |

## What Phase 63a actually has to do

1. Add a pure-logic reusable mixer crate (`audio_mixer`) with Rust + C-ABI surfaces (Track A).
2. Add a C-ABI veneer around `audio_client` so C consumers reuse the Rust protocol implementation (Track B).
3. Decode WAD-embedded DMX SFX lumps into a `(rate, samples, len)` triple (Track C).
4. Implement the doomgeneric `sound_module_t` over the mixer + the FFI transport (Track D).
5. Implement the doomgeneric `music_module_t` as a Tier 2a square/triangle MUS synth feeding the same mixer (Track E).
6. Register both modules in a patches-overlay `i_sound.c` (Track F).
7. Flip `xtask::build_doom` to `-DFEATURE_SOUND`, add new C files, link the Rust staticlibs (Track G).
8. Add `cargo xtask doom-audio-smoke` paralleling Phase 63's `audio-smoke` (Track H).
9. Verify stream-leak resilience: relaunch DOOM within 1 s of a SIGKILL exit (Track I).
10. Kernel patch-version bump to `0.63.1`; design + release wiring + memo retire (Track J).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `audio_mixer` crate — pure-logic Rust mixer + C-ABI surface | None | **Complete** |
| B | `audio_client_ffi` crate — C-ABI veneer over `audio_client` | None | **Complete** |
| C | `m3os_dmx.c` — WAD DMX header parse + bounds check | None | **Complete** |
| D | `m3os_sound.c` — `sound_module_t` body with DI seam, `EBUSY` silent-fallback | A, B, C | **Complete** |
| E | `m3os_music.c` — `music_module_t` Tier 2a MUS synth feeding the mixer | A, D | **Complete** |
| F | `patches/i_sound.c` — register `m3os_sound_module` + `m3os_music_module` | D, E | **Complete** |
| G | `xtask::build_doom` wiring — flip `FEATURE_SOUND`, add C files, link staticlibs | A, B, C, D, E, F | **Complete** |
| H | `cargo xtask doom-audio-smoke` gate — scripted SFX trigger → non-silent WAV | G | **Complete** |
| I | Stream-leak resilience verification | H | **Complete** |
| J | Kernel patch bump, README/AGENTS update, memo retire, design-doc closure | I | **Complete** |

---

## Track A — `audio_mixer` Crate (pure-logic Rust mixer with C-ABI surface)

### A.1 — Create `userspace/lib/audio_mixer/` workspace member

**File:** `Cargo.toml`, `userspace/lib/audio_mixer/Cargo.toml` (new), `userspace/lib/audio_mixer/src/lib.rs` (new)
**Symbol:** `audio_mixer` package
**Why it matters:** A new workspace crate is the cleanest seam for "pure mix engine reusable across DOOM and any future system-mixer service" (memo's Tier 4 path). Centralizing it here means Phase 63a's mixer is the same mixer a future service will consume.

**Acceptance:**
- [x] `userspace/lib/audio_mixer/` exists with `Cargo.toml`; default crate-type is `rlib`; `xtask::build_doom` produces the `staticlib` via `cargo rustc --crate-type=staticlib` for the musl target (host tests cannot link a `#![no_std]` staticlib without a panic-handler / global allocator, so `staticlib` is not in the default list).
- [x] `Cargo.toml` workspace `members` list includes `userspace/lib/audio_mixer`.
- [x] `lib.rs` is `#![cfg_attr(not(test), no_std)]` and exports `pub struct Mixer`, `pub struct ChannelState`, `pub fn step`.
- [x] `cargo xtask check` runs `audio_mixer` host tests (added to `USERSPACE_LIB_HOST_TEST_PACKAGES`).

### A.2 — Implement `Mixer` and `ChannelState` (Rust API)

**File:** `userspace/lib/audio_mixer/src/lib.rs`
**Symbol:** `Mixer::new`, `Mixer::set_channel`, `Mixer::clear_channel`, `Mixer::step`, `ChannelState`
**Why it matters:** Pure-logic mix engine: no I/O, no allocation in `step`, all hot-path math is deterministic. SRP — this module knows nothing about IPC, WAD files, or DOOM. Testable in isolation.

**Acceptance:**
- [x] `Mixer::new(channel_count: usize) -> Self` constructs a fixed-channel mixer; `channel_count <= 32`.
- [x] `Mixer::set_channel(idx, sample_slice, source_rate_hz, left_vol, right_vol)` (`unsafe fn` — slice contract) seeds a channel; `left_vol` / `right_vol` are clamped to `0..=127`.
- [x] `Mixer::clear_channel(idx)` zeroes the channel state (used by `S_StopSound`).
- [x] `Mixer::step(out: &mut [u8], frames: usize)` writes `frames * 4` bytes of stereo S16LE; returns the count of bytes written (or `0` on undersized `out`).
- [x] `step` uses 16.16-fixed-point linear interpolation: `inc = (source_rate << 16) / 48000`; per output frame, lookup two adjacent samples and interpolate.
- [x] Per-frame accumulator is `i32`; final clamp to `i16::MIN..=i16::MAX` before store.
- [x] No heap allocation in `step` (verified by `tests/no_alloc.rs`: a `#[global_allocator]` tripwire that panics on any allocation while armed, then runs `step` for 10 000 iterations against a pre-seeded mixer).

### A.3 — Host unit tests for `Mixer`

**File:** `userspace/lib/audio_mixer/src/lib.rs` (`#[cfg(test)] mod tests`)
**Symbol:** `tests::single_channel_mute`, `tests::single_channel_full_volume`, `tests::two_channel_pan`, `tests::clamp_at_bounds`, `tests::resampler_11025_to_48000`
**Why it matters:** TDD gate per CCC TEST-1 / TEST-6. The mixer is the only place where audio quality bugs can hide; exact-output assertions catch off-by-one, clamp inversion, and pan mis-assignment.

**Acceptance:**
- [x] `single_channel_mute` asserts `step` output is all-zero when `left_vol = right_vol = 0`.
- [x] `single_channel_full_volume` asserts `step` output matches an exact expected byte sequence (signed `[0, 16256, 16256, 0]` per stereo frame) for a 4-sample synthetic input at 48 kHz, vol 127.
- [x] `two_channel_pan` asserts the left output equals channel 0's contribution and the right output equals channel 1's contribution when channel 0 is pan-hard-left and channel 1 is pan-hard-right.
- [x] `clamp_at_bounds` asserts an overdriven mix (8 channels at full volume of `255`-valued samples → ~`i16::MAX`-magnitude) clamps to `i16::MAX` rather than wrapping.
- [x] `resampler_11025_to_48000` asserts that a synthetic ramp input at 11025 Hz produces a monotonically-non-decreasing output across resampled frames.
- [x] `cargo test -p audio_mixer --target x86_64-unknown-linux-gnu` passes (12 unit tests + 1 no-alloc integration test).

### A.4 — C-ABI surface and header

**File:** `userspace/lib/audio_mixer/src/ffi.rs` (new), `userspace/lib/audio_mixer/include/audio_mixer.h` (new)
**Symbol:** `audio_mixer_new`, `audio_mixer_set_channel`, `audio_mixer_clear_channel`, `audio_mixer_step`, `audio_mixer_drop`
**Why it matters:** DRY — the C-side mixer is the same code as the Rust-side mixer. Without the C ABI, `m3os_sound.c` would have to re-implement the resampler in C, duplicating the algorithm and the bugs.

**Acceptance:**
- [x] `#[unsafe(no_mangle)] pub extern "C" fn audio_mixer_new(channel_count: usize) -> *mut Mixer` returns `Box::into_raw(Box::new(Mixer::new(...)))` (NULL on `channel_count > MAX_CHANNELS`).
- [x] `audio_mixer_drop(*mut Mixer)` reclaims via `Box::from_raw`.
- [x] `audio_mixer_set_channel(*mut Mixer, idx, *const u8, len, source_rate_hz, left_vol, right_vol) -> int` returns 0 on success or a stable `AUDIO_MIXER_ERR_*` code.
- [x] `audio_mixer_step(*mut Mixer, *mut u8, byte_capacity, frames) -> isize` returns bytes written, or negative error.
- [x] `audio_mixer.h` declares matching `extern "C"` signatures and the `AUDIO_MIXER_*` constants.
- [x] `userspace/lib/audio_mixer/build.rs` reads `include/audio_mixer.h`, parses each `#define AUDIO_MIXER_* <int>` line, and `assert!`s the value matches the corresponding `pub const` in `src/ffi.rs`; mismatch fails the build with `panic!("audio_mixer.h drift: <NAME> header={h} rust={r}")` (verified by temporarily mutating the header and observing `audio_mixer.h drift: AUDIO_MIXER_ERR_INVAL header=-99 rust=-1`).
- [x] Five host tests in `ffi.rs` call the C-ABI surface through `unsafe { extern "C" }`: `ffi_round_trip_matches_rust_api`, `ffi_rejects_oversized_channel_count`, `ffi_rejects_null_handle`, `ffi_rejects_undersized_output`, `ffi_rejects_empty_sample_buffer`.

---

## Track B — `audio_client_ffi` Crate (C-ABI veneer over `audio_client`)

### B.1 — Create `userspace/lib/audio_client_ffi/` workspace member

**File:** `Cargo.toml`, `userspace/lib/audio_client_ffi/Cargo.toml` (new), `userspace/lib/audio_client_ffi/src/lib.rs` (new)
**Symbol:** `audio_client_ffi` package
**Why it matters:** A dedicated crate keeps the C-ABI surface separate from the pure Rust `audio_client` library. `audio_client` stays `#![no_std]` and Rust-idiomatic; `audio_client_ffi` adds the C concerns (handle ownership, error-int mapping, `Mutex` for thread-safety on the C side) without polluting the upstream API.

**Acceptance:**
- [x] `userspace/lib/audio_client_ffi/` exists with `Cargo.toml` (default crate-type `rlib`; xtask::build_doom produces the staticlib via `cargo rustc --crate-type=staticlib` for the musl target) and depends on `audio_client`.
- [x] `Cargo.toml` workspace `members` list includes `userspace/lib/audio_client_ffi`.
- [x] `lib.rs` exports `pub extern "C" fn audio_ffi_*` shims and an opaque `AudioFfiHandle` type.
- [x] `cargo xtask check` runs `audio_client_ffi` host tests (added to `USERSPACE_LIB_HOST_TEST_PACKAGES`).

### B.2 — Implement C-ABI shims

**File:** `userspace/lib/audio_client_ffi/src/lib.rs`
**Symbol:** `audio_ffi_connect`, `audio_ffi_open`, `audio_ffi_submit`, `audio_ffi_drain`, `audio_ffi_get_stats`, `audio_ffi_close`
**Why it matters:** Single-source the audio protocol. Without the FFI, `m3os_sound.c` would hand-encode `AudioControlCommand` bytes — a DRY violation and a long-term protocol-drift hazard.

**Acceptance:**
- [x] `audio_ffi_connect() -> *mut AudioFfiHandle` returns a `Box::into_raw` handle wrapping a `ProdHolder` (which holds an `Option<AudioClient<SyscallSocket>>`); returns `NULL` on `AudioClient::connect` failure.
- [x] `audio_ffi_open(*mut AudioFfiHandle) -> int` calls `AudioClient::open` with fixed 48 kHz / S16LE / stereo; returns 0 on success or a stable negative error code drawn from a flat table that covers every reachable `AudioClientError` variant *and* the inner `AudioError` payload when the outer variant is `Server(_)`. Entries: `Server(Busy)` → `AUDIO_FFI_ERR_BUSY`, `Server(WouldBlock)` → `AUDIO_FFI_ERR_WOULD_BLOCK`, `Server(InvalidFormat)` → `AUDIO_FFI_ERR_FORMAT`, `Server(Internal)` → `AUDIO_FFI_ERR_INTERNAL`, `Server(NoDevice)` → `AUDIO_FFI_ERR_NO_DEVICE`, `Server(BrokenPipe)` → `AUDIO_FFI_ERR_BROKEN_PIPE`, `Server(InvalidArgument)` → `AUDIO_FFI_ERR_INVALID_ARG`, `Io(_)` → `AUDIO_FFI_ERR_IO`, `Protocol(_)` → `AUDIO_FFI_ERR_PROTOCOL`, `AlreadyOpen` → `AUDIO_FFI_ERR_ALREADY_OPEN`, `NotOpen` → `AUDIO_FFI_ERR_NOT_OPEN`, `UnexpectedReply` → `AUDIO_FFI_ERR_UNEXPECTED_REPLY`. Full table exported in `audio_client.h`.
- [x] `audio_ffi_submit(*mut AudioFfiHandle, *const u8, len) -> isize` returns bytes submitted (always equals `len` on success per the all-or-nothing contract) or a negative error code. `Server(WouldBlock)` maps to a distinct `AUDIO_FFI_ERR_WOULD_BLOCK` so the C caller can distinguish "retry later" from fatal errors.
- [x] `audio_ffi_get_stats(*mut AudioFfiHandle, *mut AudioFfiStats) -> int` populates a C-struct mirror of `AudioStats`.
- [x] `audio_ffi_close(*mut AudioFfiHandle)` calls `close()` on the inner client (which sends `ClientMessage::Close` and waits for `Closed`) and frees the handle.
- [x] Userspace panic strategy is `panic = "abort"` (per workspace `[profile.release]`), so a Rust panic terminates the process rather than unwinding into the C caller — `catch_unwind` is moot under the chosen panic strategy and is intentionally omitted. The constant `AUDIO_FFI_ERR_PANIC` is reserved for a future panic = "unwind" build profile.

### B.3 — `audio_client.h` C header

**File:** `userspace/lib/audio_client_ffi/include/audio_client.h` (new)
**Symbol:** N/A (C header)
**Why it matters:** The C consumer needs declarations matching the Rust shims. Hand-writing the header (rather than generating with `cbindgen`) keeps the dependency surface minimal and lets a human reader scan the contract.

**Acceptance:**
- [x] `audio_client.h` declares `typedef struct AudioFfiHandle AudioFfiHandle;`, all `audio_ffi_*` function signatures, the `AudioFfiStats` struct, and the stable error-code constants.
- [x] `userspace/lib/audio_client_ffi/build.rs` reads `include/audio_client.h`, parses each `#define AUDIO_FFI_* <int>` line, and `assert!`s the value matches the corresponding `pub const` in `src/lib.rs`; mismatch fails the build with `panic!("audio_client.h drift: <NAME> header={h} rust={r}")` (verified by temporarily mutating the header and observing `audio_client.h drift: AUDIO_FFI_ERR_BUSY header=-77 rust=-1`).
- [x] Header passes `gcc -Wall -Wextra -pedantic -c` on a smoke C file that just includes it.

### B.4 — Host tests for the C-ABI veneer

**File:** `userspace/lib/audio_client_ffi/src/lib.rs` (`#[cfg(test)] mod tests`)
**Symbol:** `tests::open_close_round_trip`, `tests::ebusy_maps_to_constant`, `tests::wouldblock_maps_to_constant`, `tests::submit_all_or_nothing`
**Why it matters:** TEST-1 — the FFI seam is the highest-leverage place for an integer-mapping bug; exact-constant assertions catch every silent re-numbering.

**Acceptance:**
- [x] `open_close_round_trip` drives a `ScriptedSocket` through `open → submit → drain → get_stats → close` via the C-ABI surface and asserts each call returns its expected result (no error codes).
- [x] `ebusy_maps_to_constant` simulates an `AudioError::Busy` server reply and asserts `audio_ffi_submit` returns `AUDIO_FFI_ERR_BUSY`. `open_error_busy_path` covers the same mapping for the `OpenError(Busy)` path that DOOM's `Init` silent-fallback depends on.
- [x] `wouldblock_maps_to_constant` simulates a `Server(WouldBlock)` reply and asserts `audio_ffi_submit` returns the stable `AUDIO_FFI_ERR_WOULD_BLOCK` (distinct from `AUDIO_FFI_ERR_BUSY`, asserted by `assert_ne!`).
- [x] `submit_all_or_nothing` confirms `audio_ffi_submit` returns either the full `len` or a negative error; never a partial count.
- [x] `map_error_table_covers_every_variant` exhaustively verifies every documented branch of the error table.
- [x] `null_handle_rejected` verifies every shim returns `AUDIO_FFI_ERR_NULL_HANDLE` (or is a no-op for `close`) on a null pointer.
- [x] `cargo test -p audio_client_ffi --target x86_64-unknown-linux-gnu` passes (7 unit tests).

---

## Track C — `m3os_dmx.c` (DMX SFX Decoder)

### C.1 — Implement DMX header parse

**File:** `userspace/doom/m3os_dmx.c` (new), `userspace/doom/m3os_dmx.h` (new)
**Symbol:** `m3os_dmx_decode`, `m3os_dmx_decoded`
**Why it matters:** WAD SFX lumps are raw bytes with a 12-byte DMX header (`format_tag:u16le, rate:u16le, sample_count:u32le, padding:u16le[2]`, then unsigned 8-bit PCM). Without validation, a malformed WAD could trigger an out-of-bounds read in the mixer's per-sample lookup.

**Acceptance:**
- [x] `int m3os_dmx_decode(const uint8_t *lump, size_t lump_len, m3os_dmx_decoded *out)` returns 0 on success or `-1` on malformed input.
- [x] Rejects lumps with `lump_len < 16` (header + minimum 4-sample body).
- [x] Rejects lumps where the format tag is not 3.
- [x] Rejects lumps where `sample_count + 12 > lump_len`.
- [x] Rejects `rate_hz == 0` (would divide by zero in the mixer's `inc` math).
- [x] Rejects null `lump` and null `out`.
- [x] Populates `out->rate_hz`, `out->samples` (pointer into the lump, zero-copy), `out->len`.
- [x] No allocation; no I/O; pure C.

### C.2 — Host-side C unit tests for the decoder

**File:** `userspace/doom/tests/test_m3os_dmx.c` (new), `xtask/src/main.rs::doom_c_test_step` (new helper)
**Symbol:** `test_valid_lump`, `test_short_lump`, `test_bad_format_tag`, `test_oversize_sample_count`
**Why it matters:** TEST-1 — the DMX decoder is the second-most-likely place for a WAD-data bug; host tests catch malformed-input rejection before the smoke gate ever sees a frame.

**Acceptance:**
- [x] A new `doom_c_test_step` xtask helper compiles each `userspace/doom/<module>.c` + `userspace/doom/tests/test_<module>.c` pair with the host's `cc` (`-Wall -Wextra -pedantic -std=c11`, no musl required) into `target/doom-c-tests/`, then runs each binary.
- [x] All 6 DMX tests pass (`test_valid_lump`, `test_short_lump`, `test_bad_format_tag`, `test_oversize_sample_count`, `test_zero_rate`, `test_null_pointers`); failure prints `FILE:LINE: msg` and returns non-zero exit.
- [x] The step is invoked at the end of `cmd_check` so a `m3os_dmx.c` regression fails `cargo xtask check` without booting QEMU. New module/test pairs are added to the `MODULES` constant — no further xtask wiring needed.

---

## Track D — `m3os_sound.c` (`sound_module_t` body)

### D.1 — Module skeleton + DI seam

**File:** `userspace/doom/m3os_sound.c` (new), `userspace/doom/m3os_sound.h` (new)
**Symbol:** `m3os_sound_module`, `m3os_audio_submitter_t`, `m3os_sound_state`
**Why it matters:** DI — the submitter and mixer are injected via function-table structs held in module state, so the unit tests run the full state machine against a `FakeSubmitter` without `audio_server`. LSP — the module satisfies the unchanged upstream `sound_module_t` contract.

**Acceptance:**
- [x] `m3os_sound_module` is a file-scope `sound_module_t` (defined non-`static` so the patches/i_sound.c overlay can extern it) whose `Init`, `Shutdown`, `StartSound`, `StopSound`, `Update`, `UpdateSoundParams`, `SoundIsPlaying`, `GetSfxLumpNum`, `CacheSounds` slots point at functions defined in this file. Wrapped in `#ifndef M3OS_SOUND_HOST_TEST` so the host-test build (which lacks doomgeneric headers) still compiles.
- [x] `m3os_audio_submitter_t` is a struct of function pointers: `connect`, `open`, `submit`, `get_stats`, `close`. Production wires it to `audio_client_ffi` via `k_prod_submitter`; tests wire it to a fake via `m3os_sound_inject_submitter`.
- [x] `m3os_sound_state` holds `submitter`, `mixer` (`audio_mixer_t *` from `audio_mixer_new(32)` — 16 SFX + 16 music voices), per-channel SFX-decoded cache, `audio_disabled` flag, `scratch` buffer for the Update hot path.
- [x] No global mutable state outside `g_state`; the state is a `static` (file-private) singleton.

### D.2 — `Init` lifecycle with `EBUSY` silent-fallback

**File:** `userspace/doom/m3os_sound.c`
**Symbol:** `m3os_sound_init`
**Why it matters:** Phase 57's single-client policy means `audio_server` may return `EBUSY`. Gameplay must never block on audio — `EBUSY` degrades to silent operation with one INFO log line, not an error halt.

**Acceptance:**
- [x] `Init` calls `submitter.connect()` → on `NULL`, sets `audio_disabled = 1`, logs `doom.audio.unavailable code=connect-failed`, returns success (test: `test_init_connect_failure_silent`).
- [x] On successful connect, calls `submitter.open()` → on `EBUSY`, closes the handle, sets `audio_disabled = 1`, logs `doom.audio.unavailable code=ebusy`, returns success (test: `test_init_ebusy_silent`).
- [x] On successful open, creates the mixer via `audio_mixer_new(32)` (test: `test_init_happy_path`).
- [x] `audio_disabled = 1` makes `StartSound`, `Update`, `UpdateSoundParams` no-ops for the rest of the process (test: `test_update_skips_when_audio_disabled` + start/stop guards).
- [x] No allocation in the `audio_disabled` hot path (the branch is a single field check; the scratch buffer is a fixed-size `static uint8_t` in `g_state`).

### D.3 — `StartSound` / `StopSound` / `UpdateSoundParams` channel routing

**File:** `userspace/doom/m3os_sound.c`
**Symbol:** `m3os_sound_start`, `m3os_sound_stop`, `m3os_sound_update_params`, `m3os_sound_is_playing`
**Why it matters:** The engine's SFX state machine routes per-channel updates through this table; channel allocation must respect DOOM's `MAX_CHANNELS = 16` semantics and the SFX cache.

**Acceptance:**
- [x] `StartSound(sfxinfo, channel, vol, sep)` looks up the cached decode (decoding via `W_CacheLumpNum` + `m3os_dmx_decode` on first miss), then calls `m3os_sound_start_decoded` which seeds `audio_mixer_set_channel`. DOOM's `MAX_CHANNELS = 16` keeps SFX in mixer channels `0..15`; music (Track E) owns `16..31` (test: `test_start_sound_claims_channel`).
- [x] `StopSound(channel)` calls `audio_mixer_clear_channel` and zeroes the channel's `channel_active` entry (test: `test_stop_sound_clears_channel`).
- [x] `UpdateSoundParams(channel, vol, sep)` is a no-op for Tier 1 (per-tic volume drift is inaudible at this fidelity and `S_UpdateSounds` re-invokes `StartSound` on relevant changes — chocolate-doom precedent). Documented in the function body comment.
- [x] `SoundIsPlaying(channel)` reports the mixer-tracked `channel_active` flag.
- [x] All four entry points are no-ops when `audio_disabled = 1`.

### D.4 — `Update` per-tic submit loop

**File:** `userspace/doom/m3os_sound.c`
**Symbol:** `m3os_sound_update`
**Why it matters:** This is the audio hot path. Called once per 35 Hz tic, it produces one tic's worth (~11 BDL slots × 512 bytes = 5632 bytes) of mixed S16LE stereo and submits via `audio_ffi_submit`. `WouldBlock` is dropped — Phase 63's underrun-zero-fill recovers.

**Acceptance:**
- [x] One call to `audio_mixer_step` per `Update` invocation, sized to `M3OS_PCM_TIC_FRAMES = 1408` frames = 5632 bytes (one DOOM tic at 35 Hz / 48 kHz, rounded up to a multiple of `PCM_SLOT_STRIDE = 512`).
- [x] One call to `submitter.submit` per `Update` with the mixed bytes (test: `test_update_submits_when_enabled`).
- [x] `submit` returning `AUDIO_FFI_ERR_WOULD_BLOCK` is silent — no log, no exception, just drop this tic (test: `test_update_swallows_wouldblock`).
- [x] `submit` returning any other negative error is logged once per session via a `static int warned` rate-limit guard at WARN.
- [x] No allocation on the hot path; the scratch buffer is the fixed-size `g_state.scratch[M3OS_PCM_TIC_BYTES + 256]` in module state.
- [x] When `audio_disabled = 1`, `Update` returns immediately without touching the mixer (test: `test_update_skips_when_audio_disabled`).

### D.5 — `Shutdown` with audio-summary log line

**File:** `userspace/doom/m3os_sound.c`
**Symbol:** `m3os_sound_shutdown`
**Why it matters:** Track H's `doom-audio-smoke` gate parses an `M3OS_DOOM:audio_summary` line from this hook. Without it, the smoke gate has no deterministic post-run signal to assert against beyond the WAV file.

**Acceptance:**
- [x] `Shutdown` calls `submitter.get_stats` and prints `M3OS_DOOM:audio_summary frames_submitted=<N> frames_consumed=<M> underruns=<K>` to stdout (which `term` routes to the serial console) — test: `test_shutdown_emits_audio_summary` captures stdout and asserts the line + each field is present. Emitted even when `audio_disabled = 1` so the smoke gate can distinguish "audio path ran" from "DOOM crashed before audio was wired".
- [x] Then calls `submitter.close()` and `audio_mixer_drop(mixer)`.
- [x] Idempotent: a second call after the first is a no-op (test: `test_shutdown_idempotent`).

### D.6 — Host unit tests for the sound module

**File:** `userspace/doom/tests/test_m3os_sound.c` (new)
**Symbol:** `test_init_ebusy_silent`, `test_start_sound_claims_channel`, `test_stop_sound_clears_channel`, `test_update_skips_when_audio_disabled`
**Why it matters:** TEST-1 — the SFX state machine is the most behavior-rich module in the phase; host tests against a `FakeSubmitter` cover paths the smoke gate cannot easily reach (EBUSY fallback, channel-clear semantics, audio_disabled idempotency).

**Acceptance:**
- [x] `test_init_happy_path` exercises the connect → open → mixer create flow; asserts each delegate is called exactly once and `audio_disabled = 0`.
- [x] `test_init_ebusy_silent` sets the fake submitter to return `AUDIO_FFI_ERR_BUSY` on `open`; asserts `Init` returns success, `audio_disabled = 1`, the close path runs, and no mixer is created.
- [x] `test_init_connect_failure_silent` covers the NULL-connect fallback.
- [x] `test_start_sound_claims_channel` asserts the fake mixer's channel 3 is active with the expected rate after `StartSound(3, ...)` and channel 0 remains inactive.
- [x] `test_stop_sound_clears_channel` asserts the mixer channel goes inactive after `StopSound(3)`.
- [x] `test_update_skips_when_audio_disabled` asserts no `submit` calls when `audio_disabled = 1`.
- [x] `test_update_submits_when_enabled` asserts one `submit` per `Update` with one tic's bytes.
- [x] `test_update_swallows_wouldblock` asserts the per-tic submit-loop tolerates `AUDIO_FFI_ERR_WOULD_BLOCK` indefinitely.
- [x] `test_shutdown_emits_audio_summary` redirects stdout and verifies the `M3OS_DOOM:audio_summary` line includes the scripted stats.
- [x] `test_shutdown_idempotent` verifies the second `Shutdown` is a no-op.
- [x] All 10 tests are wired into `cargo xtask check` via `doom_c_test_step` with `-DM3OS_SOUND_HOST_TEST`.

---

## Track E — `m3os_music.c` (`music_module_t` Tier 2a MUS Synth)

### E.1 — MUS event parser + tick scheduler

**File:** `userspace/doom/m3os_music.c` (new), `userspace/doom/m3os_music.h` (new)
**Symbol:** `m3os_mus_parse_header`, `m3os_mus_dispatch_next_event`, `m3os_music_tick`
**Why it matters:** MUS is a compact MIDI-like format. The parser walks the event stream at the song's native 140 Hz tickrate, dispatching NoteOn / NoteOff / Controller / PitchBend / SystemEvent events.

**Acceptance:**
- [x] `m3os_mus_parse_header(lump, lump_len)` validates the MUS magic (`MUS\x1a`), reads the score-start offset, range-checks `score_offset + score_len <= lump_len`, returns a heap `m3os_mus_state_t *` or `NULL` on malformed input.
- [x] `m3os_music_tick(state)` is invoked from `m3os_sound::Update` (via `m3os_music_advance_for_doom_tic` running 4 ticks per DOOM tic, so MUS clocks at its native 140 Hz). Processes events until the next "last in group" flag fires, reads the trailing varint delay, and pauses for that many ticks. Returns 1 on ScoreEnd, 0 otherwise.
- [x] Host C unit tests (7 total in `tests/test_m3os_music.c`) cover: valid header parse, bad-magic rejection, score-range overflow, NoteOn → NoteOff round-trip, ScoreEnd termination, master-volume scaling, stop-all clear.

### E.2 — Square + triangle voice synth feeding the shared mixer

**File:** `userspace/doom/m3os_music.c`
**Symbol:** `m3os_synth_voice_t`, `m3os_synth_note_on`, `m3os_synth_note_off`
**Why it matters:** Voices feed the same `Mixer` instance as SFX (channels 16..32 in a 32-channel mixer) so there's exactly one mix path and one submit path — no parallel music submit loop to keep in sync.

**Acceptance:**
- [x] `m3os_voice_t` holds `channel`, `note`, `velocity`, `waveform`, `active` — the resampler's cursor is owned by the mixer, so the synth voice does not duplicate it. (Per design-doc Tier 2a clarification, the mixer's 16.16 cursor is the source of truth.)
- [x] On PlayNote: `claim_voice` seeds the per-voice waveform buffer on first use and `seed_voice_in_mixer` calls `audio_mixer_set_channel(M3OS_MUSIC_CHANNEL_BASE + voice_idx, voice_buf, ...)`.
- [x] On ReleaseNote: `release_voice` clears the matching voice and calls `audio_mixer_clear_channel`.
- [x] Tier 2a waveform selection: even MUS channels use square, odd channels use triangle — Tier 2a doesn't track Controller(0)=patch events so we use channel-id parity as the cheapest proxy. Documented inline as a deliberate Tier 2a simplification.

### E.3 — `music_module_t` table + MIDI fallthrough

**File:** `userspace/doom/m3os_music.c`
**Symbol:** `m3os_music_module`, `m3os_music_register_song`, `m3os_music_play_song`, `m3os_music_stop_song`, `m3os_music_set_volume`
**Why it matters:** LSP — the table satisfies the unchanged upstream `music_module_t` contract; the engine treats `m3os_music_module` interchangeably with any other music back-end.

**Acceptance:**
- [x] `RegisterSong(data, len)` detects MUS via magic-byte check (`MUS\x1a`). MIDI fallthrough is **deferred**: the function returns `NULL` for non-MUS data and the engine silently skips music. Tier 2b SoundFont synth (or a MIDI converter) is the right phase for that work — pinned in the "Deferred Until Later" section of the design doc.
- [x] `PlaySong(handle, looping)` stores the song + loop flag for `m3os_music_advance_for_doom_tic` to drive.
- [x] `StopSong()` calls `m3os_music_stop_all_inner` which iterates all active voices and clears each via `audio_mixer_clear_channel`.
- [x] `SetMusicVolume(vol)` clamps to 0..127 and stores as `g_master_volume`; the next NoteOn's seeded volume scales by `(master * velocity) / 127`.
- [x] `Poll()` is a no-op — music ticks are driven from `m3os_sound::Update` via `m3os_music_advance_for_doom_tic` to keep one submit cadence.
- [x] `MusicIsPlaying()` returns true while the current song is non-null and not finished.

### E.4 — Host unit tests for the music module

**File:** `userspace/doom/tests/test_m3os_music.c` (new)
**Symbol:** `test_mus_header_valid`, `test_mus_header_bad_magic`, `test_note_on_off_round_trip`, `test_music_volume_scales_voices`
**Why it matters:** TEST-1 — the MUS state machine is the second-most behavior-rich module; host tests cover paths the smoke gate cannot reach (malformed-MUS rejection, voice-volume scaling, looping).

**Acceptance:**
- [x] 7 tests pass under the Track C.2 C-test step (`m3os_music` added to xtask's `MODULES` list with `-DM3OS_SOUND_HOST_TEST`).

---

## Track F — `patches/i_sound.c` Registration Overlay

### F.1 — Add `m3os_sound_module` and `m3os_music_module` to the engine's registration list

**File:** `userspace/doom/patches/i_sound.c` (new)
**Symbol:** `sound_modules`, `music_modules` (file-scope `static` arrays)
**Why it matters:** Phase 47's patch-overlay mechanism (`xtask/src/main.rs::build_doom:1255-1273`) copies any `.c` or `.h` file from `userspace/doom/patches/` over the upstream doomgeneric source after `git checkout`. Adding our modules to the registration list is the smallest possible engine-side change.

**Acceptance:**
- [x] `userspace/doom/patches/i_sound.c` declares `extern sound_module_t m3os_sound_module;` and `extern music_module_t m3os_music_module;` and lists them in `sound_modules[]` and assigns `&m3os_music_module` in `InitMusicModule()`. Upstream's musicmodule init was a function body (not an array literal), so the music registration is a one-line body change rather than an array element.
- [x] The file is a drop-in replacement for upstream `i_sound.c` — every other function (`I_InitSound`, `I_ShutdownSound`, `I_StartSound`, etc.) is preserved verbatim; the patch only changes the registration sites.
- [x] `build_doom` log output shows `doom: applied patch i_sound.c` after the patch is copied — confirmed by the `cargo xtask image` log line.

---

## Track G — `xtask::build_doom` Wiring

### G.1 — Flip `-UFEATURE_SOUND` to `-DFEATURE_SOUND`

**File:** `xtask/src/main.rs::build_doom`
**Symbol:** `args` vec in `build_doom`, the entry currently containing `"-UFEATURE_SOUND".to_string()`
**Why it matters:** Without this flip the upstream `i_sound.c` dispatcher compiles to no-ops and our modules are never called.

**Acceptance:**
- [x] `-UFEATURE_SOUND` is replaced by `-DFEATURE_SOUND` in `build_doom`'s args vec.
- [x] `cargo xtask image` succeeds — the upstream `i_sound.c` (our patches overlay) and `s_sound.c` compile under `-DFEATURE_SOUND` and the DOOM binary links.

### G.2 — Add new C files to `build_doom`

**File:** `xtask/src/main.rs::build_doom`
**Symbol:** `c_files` push block in `build_doom`, immediately after the existing `c_files.push(platform.to_str()...)` line that adds `dg_m3os.c`
**Why it matters:** Three new platform-layer files must be compiled into the DOOM binary alongside `dg_m3os.c`.

**Acceptance:**
- [x] `userspace/doom/m3os_dmx.c`, `m3os_sound.c`, `m3os_music.c` are pushed onto `c_files` after the existing `dg_m3os.c` push.
- [x] Build log compiles all four platform files; the resulting DOOM binary is ~616 KB (up from ~480 KB baseline — the audio path adds ~136 KB of Rust + C code).

### G.3 — Link `audio_client_ffi` + `audio_mixer` staticlibs

**File:** `xtask/src/main.rs::build_doom`
**Symbol:** `args` vec
**Why it matters:** The musl-gcc invocation needs `-L<rust-target-dir>` and `-laudio_client_ffi -laudio_mixer` to resolve the C-ABI symbols the new modules call.

**Acceptance:**
- [x] Before calling musl-gcc, `build_doom` runs `cargo rustc --release --target x86_64-unknown-linux-musl -p audio_client_ffi --crate-type=staticlib` (which rolls in `audio_mixer` as an rlib dependency via Cargo.toml). RUSTFLAGS include `-C relocation-model=static -C target-feature=+crt-static` so the staticlib does not pull in libgcc_eh / `_dl_find_object`.
- [x] `-L<staticlib-dir>` and `-l:libaudio_client_ffi.a` (linker static-archive syntax) are appended to the musl-gcc args. Only one .a file is needed because `audio_mixer`'s code is rolled in transitively; the `mixer_reexport` module's `#[used]` keepalive static ensures Rust's dead-code analyzer preserves every `audio_mixer_*` C symbol.
- [x] `-I<workspace>/userspace/lib/audio_client_ffi/include -I<workspace>/userspace/lib/audio_mixer/include` is appended so the C `#include` directives resolve.
- [x] Build succeeds; `target/generated-initrd/doom` is 616 KB (up from ~480 KB baseline — > 100 KiB delta).
- [x] The DOOM build cache key now includes a fingerprint of the overlay files (`dg_m3os.c`, `m3os_dmx.c`, `m3os_sound.c`, `m3os_music.c`, `patches/*`, `m3os_*.h`) so changes to any of them force a rebuild — `DOOMGENERIC_COMMIT` alone would miss our edits.

### G.4 — `DG_DrawFrame` emits a one-shot `title_ready` marker

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_DrawFrame`, new `M3OS_DOOM:title_ready` print
**Why it matters:** Track H's smoke gate needs a deterministic post-boot signal before it sends keystrokes. A one-shot serial print on the first successful frame draw is the simplest reliable trigger. Note: engine-side sound-module init is *not* something `DG_Init` needs to call directly — upstream's `I_InitSound` iterates the `sound_modules` array Track F installs, so our `m3os_sound_module.Init` is invoked through the engine's normal startup flow.

**Acceptance:**
- [x] `DG_DrawFrame` prints `M3OS_DOOM:title_ready` exactly once on its first invocation, gated by a `static int title_ready_printed = 0;` flag flipped immediately before the `printf`.
- [x] The print goes to stdout (which `term` and the serial console both receive); `fflush(stdout)` ensures it lands before any further frame work.
- [ ] Manual `cargo xtask run-gui` confirms the line appears in serial output — deferred to manual smoke once Track H lands the automated harness.
- [x] `DG_Init` is *not* modified to call `m3os_sound_module.Init` directly — upstream `I_InitSound` handles that via the `sound_modules` array our patches overlay installs.

---

## Track H — `cargo xtask doom-audio-smoke` Gate

### H.1 — New `cmd_doom_audio_smoke` xtask subcommand

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_doom_audio_smoke`
**Why it matters:** A deterministic CI gate that asserts DOOM produces audible output. Mirrors Phase 63's `cmd_audio_smoke` shape; reuses the WAV `audiodev` backend.

**Acceptance:**
- [x] New subcommand `doom-audio-smoke` parses CLI args via the shared `parse_smoke_boot_args` helper (defaults to the same per-step timeouts the other smoke gates use; the overall budget is 120 s in the pre-push wiring).
- [x] Boots QEMU with `-audiodev wav,id=snd0,path=<smoke_dir>/doom-audio.wav` and the existing `-device AC97,audiodev=snd0,addr=0x5`.
- [x] Waits for kernel boot + `init: loaded service 'audio_server'` + login (`boot_and_login_steps`).
- [x] Writes `300` to `/tmp/doom-autoquit-tics` so the in-engine autoquit seam in `dg_m3os.c::DG_DrawFrame` fires `I_Quit()` after ~8.5 seconds of gameplay. The keystroke-injection path the design doc described (Ctrl → Esc → Q → Y) requires QEMU monitor / `sendkey` infrastructure that this xtask doesn't yet have because DOOM reads PS/2 scancodes via `sys_read_scancode`, not serial stdin. The autoquit seam replaces it: same end-state (engine Shutdown runs, `m3os_sound_shutdown_inner` prints `M3OS_DOOM:audio_summary`), simpler harness.
- [x] Sends `/bin/doom -warp 1 1\n` to the serial console. The `-warp 1 1` flag skips the title-screen menu and enters Episode 1 Map 1 directly, eliminating menu-navigation timing fragility.
- [x] Waits for the `M3OS_DOOM:title_ready` marker emitted by `dg_m3os.c::DG_DrawFrame` (see Track G.4).
- [x] Waits for `M3OS_DOOM:audio_summary frames_submitted=...` and asserts the line does *not* contain `frames_consumed=0 ` via `SmokeStep::WaitLineNotMatching`. A zero count proves the DMA engine never advanced the BDL — the audio-server-side regression Phase 63 Track A.2 already guards.
- [x] Post-QEMU step: opens `<smoke_dir>/doom-audio.wav`, asserts non-silence via the existing `assert_wav_non_silent` helper (same threshold the audio-smoke gate uses).
- [x] Three distinct failure modes produce three distinct error messages: `doom-audio-smoke: timeout waiting for audio_summary` (script timeout, exit code `SMOKE_EXIT_DOOM_AUDIO_FAILED`), `frames_consumed=0 ` substring inside the audio_summary line (same exit code), and `doom-audio-smoke: WAV is silent` (exit code `SMOKE_EXIT_WAV_SILENT`).

### H.2 — Wire `doom-audio-smoke` into the pre-push gate set

**File:** `.githooks/pre-push`, `AGENTS.md`
**Symbol:** pre-push hook script body; AGENTS.md First-Time Setup section
**Why it matters:** Phase 63's `audio-smoke` is *not* in `cmd_check` (which stays QEMU-free); it runs as a pre-push gate. 63a follows the same placement: `cmd_check` stays fast and headless-host-only, while QEMU-driven gates (`audio-smoke`, `bell-smoke`, `doom-audio-smoke`) run in the pre-push hook alongside `smoke-test` and `regression`.

**Acceptance:**
- [x] `.githooks/pre-push` invokes `cargo xtask doom-audio-smoke --timeout 120` gated by `M3OS_DOOM_AUDIO_REGRESSION=1`, mirroring the `M3OS_E1000_REGRESSION=1` pattern. The gate is off-by-default because the gate adds ~30 s of wall-clock + a DOOM rebuild (~5 s if cached, 30+ s if not); branches that touch DOOM / audio code should set the env var.
- [x] `AGENTS.md` First-Time Setup section lists `doom-audio-smoke` alongside `ssh-e1000-banner-check` as an env-gated pre-push regression.
- [x] `cmd_check` is *not* modified — keeping it QEMU-free is a deliberate parallel to Phase 63's gate placement.

---

## Track I — Stream-Leak Resilience Verification

### I.1 — Dual-launch test in `doom-audio-smoke`

**File:** `xtask/src/main.rs::cmd_doom_audio_smoke`
**Symbol:** `cmd_doom_audio_smoke` post-quit step
**Why it matters:** Phase 57's `audio_server` socket-disconnect → stream-close path is already tested at the server level; this verifies it at the DOOM consumer level — if DOOM crashes, a relaunch must still get the stream.

**Acceptance:**
- [x] After the first DOOM exit, `cmd_doom_audio_smoke` sends `/bin/fb-takeover /bin/doom -iwad /usr/share/doom/doom1.wad -warp 1 1\n` again over the serial console.
- [x] Waits for the second-instance `M3OS_DOOM:audio_summary frames_submitted=...` line via `WaitLineNotMatching`; the `frames_consumed=0 ` guard ensures the second run actually produced PCM. The first-run `audio_summary` step blocking on that pattern would not match the second-run output (the harness uses a separate `Wait` for `M3OS_DOOM:title_ready` between the two summaries to disambiguate).
- [x] Failure surfaces via the standard step-timeout path: `doom-audio-smoke: timeout waiting for audio_summary` plus exit code `SMOKE_EXIT_DOOM_AUDIO_FAILED`. End-to-end verified: 25 steps, 56 s wall-clock, both runs PASS.

### I.2 — BEL re-arm after DOOM exit

**File:** `xtask/src/main.rs::cmd_doom_audio_smoke`
**Symbol:** `cmd_doom_audio_smoke` final step
**Why it matters:** While DOOM holds the stream, the BEL silently drops; the moment DOOM exits, the BEL must re-arm. Mirrors Phase 63's `bell-smoke` assertion shape.

**Acceptance:**
- [x] After the second DOOM exit, the harness sends `/bin/bell-test\n` over the serial console — the same guest-side binary Phase 63's `bell-smoke` uses, which rings the BEL and prints `BELL_TEST:PASS:consumed=<N>` after calling `audio_client::get_stats`.
- [x] Harness asserts `BELL_TEST:PASS` appears via `SmokeStep::WaitPassOrFail` (with `BELL_TEST:FAIL` as the early-exit prefix and `SMOKE_EXIT_DOOM_AUDIO_FAILED` as the exit code). End-to-end verified: BEL re-arm step passes in <30 s after the second DOOM run.

---

## Track J — Kernel Patch Bump + Doc Wiring

### J.1 — Kernel `0.63.0` → `0.63.1`

**File:** `kernel/Cargo.toml`
**Symbol:** `version` field
**Why it matters:** 63a does not touch kernel source. A patch bump lets this phase release independently from the next kernel-touching phase (Phase 64 session-manager lifecycle).

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.63.1"`.
- [x] `Cargo.lock` regenerated.
- [x] `cargo xtask image` succeeds (verified above as part of Track G).

### J.2 — Update `AGENTS.md` project overview

**File:** `AGENTS.md`
**Symbol:** Project Overview paragraph
**Why it matters:** `AGENTS.md` describes the current kernel version and phase highlights; readers need 63a referenced once it lands.

**Acceptance:**
- [x] `Kernel v0.63.0` → `Kernel v0.63.1` in the project overview.
- [x] A one-line note added to the project overview summarizing 63a (DOOM SFX + Tier 2a music wired through `audio_server`; new `audio_mixer` + `audio_client_ffi` crates; `doom-audio-smoke` gate; BEL re-arm).

### J.3 — Add `63a` row to `docs/roadmap/README.md`

**File:** `docs/roadmap/README.md`
**Symbol:** Roadmap table near the Phase 63 / 64 rows
**Why it matters:** Roadmap README is the canonical phase index; 63a must appear between 63 and 64 with the standard column set per `docs/appendix/doc-templates.md`.

**Acceptance:**
- [x] Row updated with the 63a description plus the BEL re-arm + dual-launch verification scope, Status = `**Complete**`.

### J.4 — Retire the `doom-audio-wiring` appendix memo

**File:** `docs/appendix/doom-audio-wiring.md`
**Symbol:** Status line + Cross-links section
**Why it matters:** The memo's "Proposed (no implementation track scheduled yet)" status is no longer accurate once 63a merges; it should become a historical design pointer.

**Acceptance:**
- [x] Memo Status flips to `Implemented in Phase 63a — see [docs/roadmap/63a-doom-audio-wiring.md](../roadmap/63a-doom-audio-wiring.md)`.
- [x] Cross-links section gains the Phase 63a design + task doc entries.
- [x] Tier 2b (SoundFont synth) and Tier 4 (system mixer) call-outs are preserved verbatim — they remain valid forward references to deferred work.

### J.5 — Add manual smoke checklist to learning doc

**File:** `docs/63a-doom-audio-wiring.md` (new) (learning-doc tier)
**Symbol:** Manual Smoke Checklist heading
**Why it matters:** Phase 63 set the precedent of a separate learning doc with the audible-on-host checklist; readers expect the same shape for the consumer-side phase.

**Acceptance:**
- [x] Learning doc `docs/63a-doom-audio-wiring.md` exists, follows the aligned-roadmap template.
- [x] Manual Smoke Checklist enumerates: launch `cargo xtask run-gui`, `/bin/fb-takeover /bin/doom -iwad /usr/share/doom/doom1.wad`, confirm title-screen menu-cursor SFX is audible, confirm in-game gunshot SFX is audible, confirm Tier 2a title music is audible, exit DOOM via `Esc → Q → Y`, confirm `/bin/bell-test` BEL chime is audible plus `BELL_TEST:PASS:consumed=<N>` with `N > 0`, optionally relaunch DOOM and verify no `doom.audio.unavailable code=ebusy` line.

---

## Documentation Notes

- 63a is **userspace-only** — the kernel ABI does not change; the kernel patch-bumps to `0.63.1` so the phase can release independently.
- The mixer (`audio_mixer` crate) is named generically and lives outside `userspace/doom/` deliberately — the memo's Tier 4 system mixer service will consume it later without renaming.
- The C-ABI veneer (`audio_client_ffi` crate) is separate from `audio_client` so the upstream library stays `#![no_std]` and Rust-idiomatic; the C concerns (handle ownership, error-int mapping, `Mutex` for thread-safety) live in the wrapper.
- The mixer is shared between SFX (channels 0..15) and music (channels 16..31) of a 32-channel `Mixer` instance — exactly one mix path, exactly one submit path. The MUS synth never opens its own audio stream.
- `EBUSY` from `audio_server` is a non-fatal silent-fallback; gameplay never blocks on audio. The fallback is logged exactly once at INFO via `doom.audio.unavailable code=<reason>`.
- Producer thread is **out of scope** for 63a — in-loop submit is the chosen pattern. The `Submitter` interface is shaped so a future thread swap is one-line; see "Deferred Until Later" in the design doc.
- The `i_sound.c` patches overlay is the smallest possible engine-side change: it replaces only the registration arrays, preserving every other upstream function verbatim.
- All host tests run under `cargo xtask check`; the new C-test step in xtask compiles `m3os_dmx.c` / `m3os_sound.c` / `m3os_music.c` with the host `cc` against test drivers under `userspace/doom/tests/`, no QEMU required.
- The `doom-audio-smoke` gate uses shareware `DOOM1.WAD` (already shipped per Phase 47); the scripted trigger is `-warp 1 1` followed by a `Ctrl` keystroke (DOOM's default fire key), which discharges the spawn-loadout pistol and plays `DSPPISTOL`. A WAD swap that changes the E1M1 player loadout (no pistol, or a different starting weapon) must update the gate.
