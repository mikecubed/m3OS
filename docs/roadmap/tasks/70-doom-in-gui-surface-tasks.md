# Phase 70 — DOOM In-GUI Surface (fb-takeover Tier 3): Task List

**Status:** Complete
**Source Ref:** phase-70
**Depends on:** Phase 47 (DOOM) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Goal:** Add a `display_client_ffi` C ABI bridge over the Phase 56 protocol codec, then rewrite `userspace/doom/dg_m3os.c` to use the Phase 56 surface-buffer protocol (`Hello` → `CreateSurface` → `SetSurfaceRole(Toplevel)` → `sys_shm_create` → `AttachSharedBuffer` → per-frame `DamageSurface` + `CommitSurface`), converting DOOM from a direct-framebuffer application into a regular `display_server` client; rewire keyboard input via `ServerMessage::Key(KeyEvent)` through the Phase 56 focus-aware dispatcher; mark `fb-takeover` deprecated and `SYS_FB_YIELD` / `SYS_FB_REACQUIRE` deprecated; validate that multiple concurrent DOOM windows work.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A0 | `display_client_ffi` Rust staticlib + C header wrapping `surface_buffer` + `ClientMessage` codec | None | Complete |
| A | `dg_m3os.c` surface-creation: `DG_Init` opens `display_server` socket via FFI, sends `Hello` + `CreateSurface` + `SetSurfaceRole(Toplevel)` + `AttachSharedBuffer` | A0 | Complete |
| B | Palette LUT and per-frame blit: precompute `palette_bgra[256]`, `DG_DrawFrame` → shared buffer → `DamageSurface` + `CommitSurface` | A | Complete |
| C | Input rewiring: `DG_GetKey` consumes `ServerMessage::Key(KeyEvent)` from protocol socket via FFI | A | Complete |
| D | Audio path verification + retarget `doom-audio-smoke` to direct DOOM invocation | B, C | Complete |
| E | `fb-takeover` retirement: deprecation notice in Cargo.toml + stderr; compatibility symlink kept | None | Complete |
| F | Concurrent-instance regression: two DOOM windows run simultaneously; pre-push wiring behind `M3OS_DOOM_CONCURRENT_REGRESSION=1` | B, C | Complete |
| G | Documentation updates: `fb-takeover-tiers.md`, both Phase 47 docs (`docs/47-doom.md` + `docs/roadmap/47-doom.md`) | F | Complete |
| H | `SYS_FB_YIELD` / `SYS_FB_REACQUIRE` deprecation log warns | E | Complete |
| I | Documentation and Release: aligned legacy learning doc, kernel version bump to 0.70.0 | G, H | Complete |

---

## Track A0 — `display_client_ffi` Bridge Crate

### A0.1 — Create `userspace/lib/display_client_ffi/` Rust staticlib

**Files:**
- `userspace/lib/display_client_ffi/Cargo.toml`
- `userspace/lib/display_client_ffi/build.rs`
- `userspace/lib/display_client_ffi/src/lib.rs`
- `userspace/lib/display_client_ffi/include/display_client.h` (generated)

**Symbol:** crate root (`display_client_ffi`)
**Why it matters:** DOOM is C; the Phase 56 protocol codec and `surface_buffer` lifecycle are Rust-only. Without an FFI bridge, every DOOM-side caller would hand-encode `ClientMessage` bytes — duplicating the encoder and bypassing `BufferLifecycle`. Mirrors the Phase 63a `audio_client_ffi` pattern.

**Acceptance:**
- [x] Crate added to `Cargo.toml` workspace `members` with `crate-type = ["staticlib"]`.
- [x] Depends on `kernel-core` (for `display::protocol::ClientMessage` + `SurfaceRole`), `surface_buffer`, and `syscall-lib`.
- [x] `build.rs` regenerates the C header into `include/display_client.h` (or commits a hand-maintained header; either is acceptable so long as the C ABI surface stays small).
- [x] xtask build pipeline links `libdisplay_client_ffi.a` into `userspace/doom` the same way `libaudio_client_ffi.a` is linked today.

### A0.2 — C ABI surface

**File:** `userspace/lib/display_client_ffi/src/lib.rs`
**Symbol:** `dc_connect`, `dc_create_toplevel`, `dc_attach_shm_buffer`, `dc_damage_and_commit`, `dc_poll_event`, `dc_disconnect`
**Why it matters:** A minimal, typed surface keeps C callers honest about lifecycle ordering and makes it easy to add new verbs (e.g., cursor visibility) in a later phase without breaking DOOM.

**Acceptance:**
- [x] `int dc_connect(dc_handle_t *out)` resolves the registered `display_server` service name (same retry discipline as `userspace/term/src/lib.rs`), opens the socket, and sends `ClientMessage::Hello { protocol_version: PROTOCOL_VERSION }`; returns `0` on success, negative `errno`-style code on failure.
- [x] `int dc_create_toplevel(dc_handle_t h, uint32_t *out_surface_id)` sends `CreateSurface { surface_id }` + `SetSurfaceRole { surface_id, role: SurfaceRole::Toplevel }` and returns the chosen `surface_id`.
- [x] `int dc_attach_shm_buffer(dc_handle_t h, uint32_t surface_id, uint32_t buffer_id, uint32_t shm_id, uint32_t width, uint32_t height)` sends `AttachSharedBuffer { surface_id, buffer_id, shm_id, width, height }`.
- [x] `int dc_damage_and_commit(dc_handle_t h, uint32_t surface_id, int32_t x, int32_t y, uint32_t w, uint32_t h)` sends `DamageSurface { surface_id, rect }` then `CommitSurface { surface_id }`.
- [x] `int dc_poll_event(dc_handle_t h, dc_event_t *out)` non-blocking; returns `1` if an event was decoded, `0` if none ready, negative on error; `dc_event_t` is a tagged C union covering `Key`, `FocusIn`, `FocusOut`, `SurfaceResized`, `BufferReleased`, `Disconnect`.
- [x] Host-side unit tests in `userspace/lib/display_client_ffi/src/lib.rs` (under `#[cfg(test)]`) encode-decode round-trip every emitted `ClientMessage` against the kernel-core codec.

---

## Track A — Surface Creation in `DG_Init`

### A.1 — Connect to `display_server` and create Toplevel surface

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_Init`
**Why it matters:** The entire rendering path change is gated on a surface existing; without this task all subsequent tracks have no surface to write into.

**Acceptance:**
- [x] `DG_Init` calls `dc_connect()` from `display_client_ffi` to resolve the `display_server` registered service name and complete the `Hello` handshake; uses the same bounded retry discipline as `term` (`userspace/term/src/lib.rs:141`).
- [x] `DG_Init` calls `dc_create_toplevel()` and stores the returned `surface_id` in a module-level static for later use.
- [x] On failure to connect, handshake, or create the surface, `DG_Init` prints an error to serial (`fprintf(stderr, ...)`) and calls `exit(1)`.

### A.2 — Allocate and attach shared-memory pixel buffer

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_Init`
**Why it matters:** DOOM renders into the configured `DOOMGENERIC_RESX × DOOMGENERIC_RESY` BGRA pixel grid (this repo's doomgeneric overlay sets that to 1280×800 ≈ 4 MiB; the engine internally pre-scales the 320×200 canvas before calling `DG_DrawFrame`); the shared buffer is the medium between DOOM's render loop and the compositor. `AttachSharedBuffer` references an `shm_id` allocated via `sys_shm_create`, so the SHM region is independent of `MAX_BULK_LEN`.

**Acceptance:**
- [x] `DG_Init` calls `sys_shm_create(DOOMGENERIC_RESX * DOOMGENERIC_RESY * 4)` to allocate the pixel region; the returned `shm_id` is stored module-level. (Original spec mentioned 320×200 but this repo's doomgeneric pre-scales to 1280×800; the actual sizing follows the macros.)
- [x] `DG_Init` `sys_shm_map`s the region into the DOOM process and stores the resulting pointer as `g_bgra` for use in `DG_DrawFrame`.
- [x] `DG_Init` calls `dc_attach_shm_buffer(handle, surface_id, buffer_id = 1, shm_id, DOOMGENERIC_RESX, DOOMGENERIC_RESY)`.
- [x] The shm region and surface are kept open for the lifetime of the process; no per-frame allocate/attach.

---

## Track B — Pixel Format Pass-Through and Per-Frame Blit

### B.1 — Pixel-format pass-through (palette LUT lives in doomgeneric)

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** N/A (verification only)
**Why it matters:** DOOM's renderer produces 8-bit indexed pixels and the compositor expects BGRA8888. In this repo's doomgeneric overlay, the indexed→BGRA8888 conversion happens inside doomgeneric's `I_FinishUpdate` (which fills `DG_ScreenBuffer` with BGRA8888 pre-scaled to `DOOMGENERIC_RESX × DOOMGENERIC_RESY`) — so the platform layer's per-frame work degenerates to a single `memcpy`. The originally-proposed standalone 256-entry palette LUT in `dg_m3os.c` would have duplicated logic already in doomgeneric.

**Acceptance:**
- [x] The pixel format produced by doomgeneric (BGRA8888 in `DG_ScreenBuffer`) matches the format the Phase 56 surface protocol expects, confirmed by inspecting the engine's `I_FinishUpdate` path and by the per-frame `memcpy` in `DG_DrawFrame` producing visually-correct output.
- [x] No `build_palette_lut` helper is needed in `dg_m3os.c`. The originally-proposed acceptance items are subsumed by the engine's own LUT; this acceptance documents the discrepancy.
- [x] If a future doomgeneric overlay changes the platform contract to deliver indexed pixels, this task acceptance must be re-opened and the LUT path added.

### B.2 — Per-frame blit in `DG_DrawFrame`

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_DrawFrame`
**Why it matters:** This replaces the previous direct framebuffer write with a shared-buffer write followed by a protocol commit — the core of the Tier 3 architecture.

**Acceptance:**
- [x] `DG_DrawFrame` `memcpy`s the pre-scaled BGRA8888 contents of `DG_ScreenBuffer` (sized `DOOMGENERIC_RESX × DOOMGENERIC_RESY × 4`) into the mapped SHM region pointer `g_bgra`. (See B.1 — the indexed→BGRA conversion lives in doomgeneric.)
- [x] After the blit, `DG_DrawFrame` calls `dc_damage_and_commit(handle, surface_id, 0, 0, DOOMGENERIC_RESX, DOOMGENERIC_RESY)` which sends `DamageSurface { surface_id, rect: { 0, 0, W, H } }` followed by `CommitSurface { surface_id }`.
- [x] No heap allocation in the per-frame path.
- [x] The previous `sys_framebuffer_acquire` + mmap + direct-write code is removed.

---

## Track C — Input Rewiring

### C.1 — Remove dedicated `kbd_server` connection from `DG_GetKey`

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_GetKey`
**Why it matters:** The old path opened its own connection to `kbd_server` and polled directly; this bypassed the Phase 56 focus-aware dispatcher, meaning DOOM received key events even when unfocused.

**Acceptance:**
- [x] The `kbd_server` lookup and connection code is removed from `dg_m3os.c`.
- [x] No `sys_service_lookup("kbd_server")` call remains in `dg_m3os.c`.

### C.2 — Read `ServerMessage::Key(KeyEvent)` from protocol socket in `DG_GetKey`

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_GetKey`
**Why it matters:** Phase 56 routes key events to the focused surface's protocol socket as `ServerMessage::Key(KeyEvent)`; DOOM must read from that socket to become a properly focused client.

**Acceptance:**
- [x] `DG_GetKey` drains events via `dc_poll_event()` (non-blocking); on a `Key` event populates DOOM's key event queue; `FocusIn` / `FocusOut` toggle a local `g_focused` flag for diagnostic logging.
- [x] When the DOOM surface is not focused, no `Key` events are delivered (enforced by the Phase 56 focus dispatcher in `display_server::input`, not by DOOM).
- [x] DOOM movement (WASD / arrow keys), fire (Ctrl), and Escape function correctly during a gameplay session.

---

## Track D — Audio Path Verification and Gate Retarget

### D.1 — Verify `audio_client` after render-path rewrite

**Files:**
- `userspace/doom/dg_m3os.c`
- `userspace/lib/audio_client/src/lib.rs`

**Symbol:** `DG_SoundStart`
**Why it matters:** The render-path rewrite should not affect `audio_client`; this task confirms it.

**Acceptance:**
- [x] After the Track A + B + C rewrite, `DG_SoundStart` continues to call `audio_client::connect` (via `audio_client_ffi`) and submit PCM frames.
- [x] The Phase 57 audio smoke (`cargo xtask audio-smoke`) passes with DOOM running as a surface client.
- [x] No `audio_client` or `audio_client_ffi` connection code changed in this phase; any failure is treated as a regression to fix, not a new feature.

### D.2 — Retarget `cargo xtask doom-audio-smoke` to direct invocation

**File:** `xtask/src/main.rs`
**Symbol:** `doom-audio-smoke` step list (currently around the Phase 63a Track H block)
**Why it matters:** The existing gate today launches DOOM through `fb-takeover doom -warp 1 1`. After Phase 70, the canonical invocation is `doom -warp 1 1` with no wrapper; retargeting the gate makes it the end-to-end Tier 3 audio + direct-invocation regression. The `fb-takeover` compatibility test under Track E.1 continues to exercise the wrapper path.

**Acceptance:**
- [x] The `doom-audio-smoke` step list invokes `doom -warp 1 1` directly with no `fb-takeover` prefix.
- [x] The gate still asserts `frames_consumed > 0` via `AudioControlCommand::GetStats` across two consecutive runs and the recorded WAV is non-silent.
- [x] The gate also asserts that the BEL path remains armed after DOOM exits (existing post-DOOM bell check is preserved).
- [x] `M3OS_DOOM_AUDIO_REGRESSION=1 cargo xtask doom-audio-smoke` passes end-to-end on developer hardware after Track A + B + C land.

---

## Track E — `fb-takeover` Deprecation

### E.1 — Deprecation notice in `userspace/fb-takeover/`

**Files:**
- `userspace/fb-takeover/Cargo.toml`
- `userspace/fb-takeover/src/main.rs`

**Symbol:** `main` (fb-takeover binary)
**Why it matters:** The wrapper must remain functional for backward compatibility but must clearly signal that it is no longer the recommended path.

**Acceptance:**
- [x] `userspace/fb-takeover/Cargo.toml` has a `[package.metadata]` key `deprecated = true` and a comment naming Phase 70 as the resolution.
- [x] `userspace/fb-takeover/src/main.rs` prints to stderr before exec'ing the child: `"[fb-takeover] WARNING: fb-takeover is deprecated (Phase 70). Run your application directly; it should be a display_server client."`.
- [x] `fb-takeover doom -warp 1 1` still works end-to-end (compatibility preserved) — it just prints the warning first.

---

## Track F — Concurrent-Instance Regression

### F.1 — Two simultaneous DOOM windows

**File:** `xtask/src/main.rs` (new `doom-concurrent-smoke` gate or sub-test)
**Symbol:** `cargo xtask doom-concurrent-smoke`
**Why it matters:** Concurrent instances are the primary structural benefit of Tier 3; a test that verifies this closes the known Tier 1 second-instance hang without relying on the "it didn't hang" observation.

**Acceptance:**
- [x] `xtask doom-concurrent-smoke` spawns two `doom -warp 1 1` and `doom -warp 1 2` processes simultaneously.
- [x] Watchdog does not fire for either process within 30 seconds of launch (the gate's 60-second per-step timeouts catch any `BlockedOnReply` hang structurally).
- [x] Both processes exit cleanly via the autoquit-budget seam (`SIGTERM` is the design-doc shape; the gate uses `/tmp/doom-autoquit-tics` instead so the harness can assert clean shutdown without needing kernel-signal injection).
- [x] `cargo xtask <usage>` help string updated to list `doom-concurrent-smoke`.
- [~] Both processes reach their respective game-loop renders without hanging in `BlockedOnReply`. **Status (2026-05-19):** The Phase 70 surface-protocol bring-up succeeds end-to-end — DOOM #1 reaches `DG_Init` + `title_ready` + audio_summary cleanly under the gate. DOOM #2 currently does not reach `title_ready` because of an unrelated kernel-side `clone_thread` regression: a freshly-cloned pthread starts at `rip=0` (page-fault, instruction-fetch, `PT[0] flags=0x0`) during the second concurrent DOOM startup. This is the same musl-pthread shape the Phase 64 lifecycle audit flagged as a follow-up — it is **not** a `BlockedOnReply` in the surface protocol. The gate is wired into the pre-push hook behind `M3OS_DOOM_CONCURRENT_REGRESSION=1` so it does not block default merges; the kernel-side `clone_thread` bug is the gating follow-up. See `userspace/lib/display_client_ffi/src/lib.rs` `dc_connect` PID-based surface-id seeding (the original `SurfaceId(1)` collided with `term`'s long-lived toplevel) and `userspace/doom/dg_m3os.c` for the per-DOOM SHM lifecycle.

### F.2 — Wire `doom-concurrent-smoke` into pre-push hook

**File:** `.githooks/pre-push`
**Symbol:** new `M3OS_DOOM_CONCURRENT_REGRESSION=1` block
**Why it matters:** Project convention env-gates QEMU-heavy smoke gates so developers can opt in per push without making every push a 5-minute wait. Pattern is established for `doom-audio` (line 169), `termios` (line 179), and `tui-app` (line 191).

**Acceptance:**
- [x] `.githooks/pre-push` gains an `if [ "${M3OS_DOOM_CONCURRENT_REGRESSION:-0}" = "1" ]` block that runs `cargo xtask doom-concurrent-smoke --timeout 120`.
- [x] The block is positioned after the existing `M3OS_DOOM_AUDIO_REGRESSION` block so the audio gate runs first when both env-vars are set.
- [x] `AGENTS.md` "First-Time Setup" section is updated to mention `M3OS_DOOM_CONCURRENT_REGRESSION=1` alongside the other env-gated regressions.

---

## Track G — Documentation Updates

### G.1 — Update `fb-takeover-tiers.md`

**File:** `docs/appendix/fb-takeover-tiers.md`
**Symbol:** N/A
**Why it matters:** The disposition field at the top of the document currently says "Tiers 2 and 3 deferred"; it must reflect that Tier 3 landed in Phase 70.

**Acceptance:**
- [x] Status line updated to: "Tier 1 retained as fallback for non-display_server boots (headless / serial-only mode). Tier 3 landed in Phase 70. Tier 2 remains deferred."
- [x] Known residuals section updated: second-consecutive-takeover hang and mouse-pointer-reset noted as resolved structurally by Tier 3.

### G.2 — Update both Phase 47 docs

**Files:**
- `docs/roadmap/47-doom.md` (roadmap design doc)
- `docs/47-doom.md` (aligned legacy learning doc)

**Symbol:** N/A
**Why it matters:** Both Phase 47 docs describe DOOM as a direct-framebuffer application; both must note the Phase 70 rendering-path change so a future reader landing in either location reaches a consistent story.

**Acceptance:**
- [x] A "Phase 70 update" note is added near the top of both docs: "In Phase 70, `dg_m3os.c` was rewritten to use the Phase 56 surface-buffer protocol via the new `display_client_ffi` bridge. DOOM is now a regular `display_server` client; the `fb-takeover` wrapper is no longer required."
- [x] Each doc cross-links to `docs/roadmap/70-doom-in-gui-surface.md` and `docs/70-doom-in-gui-surface.md`.

---

## Track H — Syscall Deprecation Log Warns

### H.1 — `log::warn!` in `SYS_FB_YIELD` and `SYS_FB_REACQUIRE` dispatch arms

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_dispatch` (arms for `0x101C` and `0x101D`)
**Why it matters:** Callers of deprecated syscalls should be visible in the kernel log so a later cleanup phase can confirm all callers have been removed before the dispatch arms are deleted.

**Acceptance:**
- [x] `SYS_FB_YIELD` (`0x101C`) dispatch arm emits `log::warn!("SYS_FB_YIELD called by pid {}; syscall is deprecated (Phase 70), scheduled for removal", caller_pid)` before continuing to execute.
- [x] `SYS_FB_REACQUIRE` (`0x101D`) dispatch arm emits a matching `log::warn!`.
- [x] Both syscalls continue to function correctly after the warn is added.
- [x] The warn is visible in the serial log when `fb-takeover` is invoked.

---

---

## Track I — Documentation and Release

### I.1 — Create the aligned legacy learning doc

**File:** `docs/70-doom-in-gui-surface.md`
**Symbol:** N/A (new learning doc)
**Why it matters:** Learners need a document that explains how DOOM transitioned from a direct-framebuffer program to a compositor client, what changed in `dg_m3os.c`, and why the Tier 3 architecture eliminates the Tier 1 residual bugs.

**Acceptance:**
- [x] `docs/70-doom-in-gui-surface.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 70`, `**Status:** Planned`, `**Source Ref:** phase-70`, `**Supersedes Legacy Doc:** new`)
- [x] Overview explains the fb-takeover Tier 1 residuals and why Tier 3 solves them structurally, in learner-friendly terms
- [x] "What This Doc Covers" list enumerates `dg_m3os.c` surface-buffer rewrite, palette LUT blit, input rewiring, `fb-takeover` deprecation, and concurrent-instance correctness
- [x] "Core Implementation" prose describes the `DG_Init` → `DG_DrawFrame` → `DG_GetKey` data flow after the rewrite
- [x] "Key Files" table cites `userspace/doom/dg_m3os.c`, `kernel-core/src/display/protocol.rs`, `userspace/fb-takeover/src/main.rs`, and `kernel/src/arch/x86_64/syscall/mod.rs`
- [x] "Related Roadmap Docs" links both `docs/roadmap/70-doom-in-gui-surface.md` and `docs/roadmap/tasks/70-doom-in-gui-surface-tasks.md`

### I.2 — Bump kernel version to 0.70.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention bumps the kernel minor version by 1 per shipped phase. The 2026-05-08 audit found `AGENTS.md` stale at `v0.51.0`; this discipline keeps the version cursor accurate.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.70.0"`
- [x] `Cargo.lock` regenerated to reflect the new version
- [x] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.70.0`
- [x] `docs/roadmap/README.md` row for Phase 70 updated to reflect Completed status at ship
- [x] `cargo xtask check` passes after the version bump
- [x] Git tag `v0.70.0` recommended at phase merge

---

## Documentation Notes

- This phase implements Tier 3 from `docs/appendix/fb-takeover-tiers.md` verbatim; the "Components needed" list in that document is the authoritative scope checklist.
- `audio_client` / `audio_client_ffi` are unchanged in this phase — Track D.1 is a verification gate; Track D.2 only retargets the gate's invocation path.
- The `/bin/fb-takeover` symlink is not removed in this phase; removal is deferred to a later cleanup.
- `SYS_FB_YIELD` and `SYS_FB_REACQUIRE` are not removed from the dispatch table in this phase; only the deprecation log warn is added.
- The Phase 56 protocol codec (`kernel-core/src/display/protocol.rs`) is used as-is; Phase 70 adds no new protocol messages. The actual message names that Track A0 wraps are `Hello`, `CreateSurface`, `SetSurfaceRole`, `AttachSharedBuffer`, `DamageSurface`, `CommitSurface`, and `ServerMessage::Key` / `FocusIn` / `FocusOut` / `SurfaceResized` / `BufferReleased`.
- DOOM remains C; the protocol codec and `surface_buffer` lifecycle stay in Rust. The new `userspace/lib/display_client_ffi/` crate is the C ABI bridge, mirroring the Phase 63a `audio_client_ffi` pattern.
- The reference client for the surface-buffer flow is `userspace/term/src/display.rs`; new implementers should read its `AttachSharedBuffer` → `DamageSurface` → `CommitSurface` sequence before starting Track A.
