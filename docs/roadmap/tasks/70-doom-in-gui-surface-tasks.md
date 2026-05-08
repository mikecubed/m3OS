# Phase 70 — DOOM In-GUI Surface (fb-takeover Tier 3): Task List

**Status:** Planned
**Source Ref:** phase-70
**Depends on:** Phase 47 (DOOM) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Goal:** Rewrite `userspace/doom/dg_m3os.c` to use the Phase 56 surface-buffer protocol (`SurfaceCreate` → `BufferCreate` → `Commit` → `DamageBuffer`), converting DOOM from a direct-framebuffer application into a regular `display_server` client; rewire keyboard input through the Phase 56 focus-aware dispatcher; mark `fb-takeover` deprecated and `SYS_FB_YIELD` / `SYS_FB_REACQUIRE` deprecated; validate that multiple concurrent DOOM windows work.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `dg_m3os.c` surface-creation: `DG_Init` opens display-server socket, creates Toplevel surface + buffer | None | Planned |
| B | Palette LUT and per-frame blit: precompute `palette_bgra[256]`, `DG_DrawFrame` → shared buffer → Commit + Damage | A | Planned |
| C | Input rewiring: `DG_GetKey` consumes `SurfaceInputEvent::Key` from surface endpoint | A | Planned |
| D | Audio path verification: smoke test that `audio_client` still works after render-path rewrite | B, C | Planned |
| E | `fb-takeover` retirement: deprecation notice in Cargo.toml + stderr; compatibility symlink kept | None | Planned |
| F | Concurrent-instance regression: two DOOM windows run simultaneously | B, C | Planned |
| G | Documentation updates: `fb-takeover-tiers.md`, Phase 47 doc | F | Planned |
| H | `SYS_FB_YIELD` / `SYS_FB_REACQUIRE` deprecation log warns | E | Planned |
| I | Documentation and Release: aligned legacy learning doc, kernel version bump to 0.70.0 | G, H | Planned |

---

## Track A — Surface Creation in `DG_Init`

### A.1 — Connect to display-server socket and create Toplevel surface

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_Init`
**Why it matters:** The entire rendering path change is gated on a surface existing; without this task all subsequent tracks have no surface to write into.

**Acceptance:**
- [ ] `DG_Init` opens the display-server control socket using the Phase 56 service-lookup path.
- [ ] `DG_Init` sends `SurfaceCreate { role: Toplevel, title: "DOOM" }` and stores the returned surface id.
- [ ] On failure to connect or create, `DG_Init` prints an error to serial and calls `exit(1)`.

### A.2 — Create shared-memory buffer

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_Init`
**Why it matters:** DOOM renders into a 320×200 pixel grid; the shared buffer is the medium between DOOM's render loop and the compositor.

**Acceptance:**
- [ ] `DG_Init` sends `BufferCreate { width: 320, height: 200, format: BGRA8888 }` on the surface socket.
- [ ] The returned buffer fd is mmap'd; the resulting pointer is stored as `g_bgra_buffer` (or equivalent module-level pointer) for use in `DG_DrawFrame`.
- [ ] Buffer fd is kept open for the lifetime of the process; no per-frame open/close.

---

## Track B — Palette LUT and Per-Frame Blit

### B.1 — Precompute BGRA8888 palette LUT

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `build_palette_lut`
**Why it matters:** DOOM's renderer produces 8-bit indexed pixels; the compositor expects BGRA8888; a precomputed LUT converts 320×200 pixels with 256 table lookups per frame rather than 64000 per-pixel multiply/shift sequences.

**Acceptance:**
- [ ] `build_palette_lut(palette_rgb: *const u8, out: *mut u32)` converts 256 RGB888 WAD palette entries to BGRA8888.
- [ ] LUT is computed once during `DG_Init` after WAD palette is loaded.
- [ ] Conversion formula: `out[i] = (r << 16) | (g << 8) | b | 0xFF000000` (alpha = opaque).

### B.2 — Per-frame blit in `DG_DrawFrame`

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_DrawFrame`
**Why it matters:** This replaces the previous direct framebuffer write with a shared-buffer write followed by a protocol commit — the core of the Tier 3 architecture.

**Acceptance:**
- [ ] `DG_DrawFrame` iterates the 320×200 indexed pixel array, performs a LUT lookup per pixel, and writes the BGRA8888 result into `g_bgra_buffer`.
- [ ] After the blit, `DG_DrawFrame` sends `Commit` followed by `DamageBuffer { x: 0, y: 0, w: 320, h: 200 }` on the surface socket.
- [ ] No heap allocation in the per-frame path.
- [ ] The previous `sys_framebuffer_acquire` + mmap + direct-write code is removed.

---

## Track C — Input Rewiring

### C.1 — Remove dedicated `kbd_server` connection from `DG_GetKey`

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_GetKey`
**Why it matters:** The old path opened its own connection to `kbd_server` and polled directly; this bypassed the Phase 56 focus-aware dispatcher, meaning DOOM received key events even when unfocused.

**Acceptance:**
- [ ] The `kbd_server` lookup and connection code is removed from `dg_m3os.c`.
- [ ] No `sys_service_lookup("kbd_server")` call remains in `dg_m3os.c`.

### C.2 — Read `SurfaceInputEvent::Key` from surface endpoint in `DG_GetKey`

**File:** `userspace/doom/dg_m3os.c`
**Symbol:** `DG_GetKey`
**Why it matters:** Phase 56 routes key events to the focused surface's input endpoint; DOOM must read from that endpoint to become a properly focused client.

**Acceptance:**
- [ ] `DG_GetKey` calls `recv_nonblocking` on the surface input endpoint; on `SurfaceInputEvent::Key`, populates DOOM's key event queue.
- [ ] When the DOOM surface is not focused, no key events are delivered (the dispatcher does not send to unfocused surfaces — this is enforced by the Phase 56 protocol, not by DOOM).
- [ ] DOOM movement (WASD / arrow keys), fire (Ctrl), and Escape function correctly during a gameplay session.

---

## Track D — Audio Path Verification

### D.1 — Verify `audio_client` after render-path rewrite

**Files:**
- `userspace/doom/dg_m3os.c`
- `userspace/lib/audio_client/src/lib.rs`

**Symbol:** `DG_SoundStart`
**Why it matters:** The render-path rewrite should not affect `audio_client`; this task confirms it.

**Acceptance:**
- [ ] After the Track A + B + C rewrite, `DG_SoundStart` continues to call `audio_client::connect` and submit PCM frames.
- [ ] The Phase 57 audio smoke (`cargo xtask audio-smoke`) passes with DOOM running as a surface client.
- [ ] No audio-client connection code changed in this phase; any failure is treated as a regression to fix, not a new feature.

---

## Track E — `fb-takeover` Deprecation

### E.1 — Deprecation notice in `userspace/fb-takeover/`

**Files:**
- `userspace/fb-takeover/Cargo.toml`
- `userspace/fb-takeover/src/main.rs`

**Symbol:** `main` (fb-takeover binary)
**Why it matters:** The wrapper must remain functional for backward compatibility but must clearly signal that it is no longer the recommended path.

**Acceptance:**
- [ ] `userspace/fb-takeover/Cargo.toml` has a `[package.metadata]` key `deprecated = true` and a comment naming Phase 70 as the resolution.
- [ ] `userspace/fb-takeover/src/main.rs` prints to stderr before exec'ing the child: `"[fb-takeover] WARNING: fb-takeover is deprecated (Phase 70). Run your application directly; it should be a display_server client."`.
- [ ] `fb-takeover doom -warp 1 1` still works end-to-end (compatibility preserved) — it just prints the warning first.

---

## Track F — Concurrent-Instance Regression

### F.1 — Two simultaneous DOOM windows

**File:** `xtask/src/main.rs` (new `doom-concurrent-smoke` gate or sub-test)
**Symbol:** `cargo xtask doom-concurrent-smoke`
**Why it matters:** Concurrent instances are the primary structural benefit of Tier 3; a test that verifies this closes the known Tier 1 second-instance hang without relying on the "it didn't hang" observation.

**Acceptance:**
- [ ] `xtask doom-concurrent-smoke` spawns two `doom -warp 1 1` and `doom -warp 1 2` processes simultaneously.
- [ ] Both processes reach their respective game-loop renders without hanging in `BlockedOnReply`.
- [ ] Watchdog does not fire for either process within 30 seconds of launch.
- [ ] Both processes exit cleanly when sent `SIGTERM`.

---

## Track G — Documentation Updates

### G.1 — Update `fb-takeover-tiers.md`

**File:** `docs/appendix/fb-takeover-tiers.md`
**Symbol:** N/A
**Why it matters:** The disposition field at the top of the document currently says "Tiers 2 and 3 deferred"; it must reflect that Tier 3 landed in Phase 70.

**Acceptance:**
- [ ] Status line updated to: "Tier 1 retained as fallback for non-display_server boots (headless / serial-only mode). Tier 3 landed in Phase 70. Tier 2 remains deferred."
- [ ] Known residuals section updated: second-consecutive-takeover hang and mouse-pointer-reset noted as resolved structurally by Tier 3.

### G.2 — Update Phase 47 design doc

**File:** `docs/roadmap/47-doom.md` (or equivalent path)
**Symbol:** N/A
**Why it matters:** Phase 47's doc describes DOOM as a direct-framebuffer application; it must note the Phase 70 rendering-path change.

**Acceptance:**
- [ ] A "Phase 70 update" note added to Phase 47 doc: "In Phase 70, `dg_m3os.c` was rewritten to use the Phase 56 surface-buffer protocol. DOOM is now a regular `display_server` client; the `fb-takeover` wrapper is no longer required."

---

## Track H — Syscall Deprecation Log Warns

### H.1 — `log::warn!` in `SYS_FB_YIELD` and `SYS_FB_REACQUIRE` dispatch arms

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_dispatch` (arms for `0x101C` and `0x101D`)
**Why it matters:** Callers of deprecated syscalls should be visible in the kernel log so a later cleanup phase can confirm all callers have been removed before the dispatch arms are deleted.

**Acceptance:**
- [ ] `SYS_FB_YIELD` (`0x101C`) dispatch arm emits `log::warn!("SYS_FB_YIELD called by pid {}; syscall is deprecated (Phase 70), scheduled for removal", caller_pid)` before continuing to execute.
- [ ] `SYS_FB_REACQUIRE` (`0x101D`) dispatch arm emits a matching `log::warn!`.
- [ ] Both syscalls continue to function correctly after the warn is added.
- [ ] The warn is visible in the serial log when `fb-takeover` is invoked.

---

---

## Track I — Documentation and Release

### I.1 — Create the aligned legacy learning doc

**File:** `docs/70-doom-in-gui-surface.md`
**Symbol:** N/A (new learning doc)
**Why it matters:** Learners need a document that explains how DOOM transitioned from a direct-framebuffer program to a compositor client, what changed in `dg_m3os.c`, and why the Tier 3 architecture eliminates the Tier 1 residual bugs.

**Acceptance:**
- [ ] `docs/70-doom-in-gui-surface.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 70`, `**Status:** Planned`, `**Source Ref:** phase-70`, `**Supersedes Legacy Doc:** new`)
- [ ] Overview explains the fb-takeover Tier 1 residuals and why Tier 3 solves them structurally, in learner-friendly terms
- [ ] "What This Doc Covers" list enumerates `dg_m3os.c` surface-buffer rewrite, palette LUT blit, input rewiring, `fb-takeover` deprecation, and concurrent-instance correctness
- [ ] "Core Implementation" prose describes the `DG_Init` → `DG_DrawFrame` → `DG_GetKey` data flow after the rewrite
- [ ] "Key Files" table cites `userspace/doom/dg_m3os.c`, `kernel-core/src/display/protocol.rs`, `userspace/fb-takeover/src/main.rs`, and `kernel/src/arch/x86_64/syscall/mod.rs`
- [ ] "Related Roadmap Docs" links both `docs/roadmap/70-doom-in-gui-surface.md` and `docs/roadmap/tasks/70-doom-in-gui-surface-tasks.md`

### I.2 — Bump kernel version to 0.70.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention bumps the kernel minor version by 1 per shipped phase. The 2026-05-08 audit found `AGENTS.md` stale at `v0.51.0`; this discipline keeps the version cursor accurate.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.70.0"`
- [ ] `Cargo.lock` regenerated to reflect the new version
- [ ] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.70.0`
- [ ] `docs/roadmap/README.md` row for Phase 70 updated to reflect Completed status at ship
- [ ] `cargo xtask check` passes after the version bump
- [ ] Git tag `v0.70.0` recommended at phase merge

---

## Documentation Notes

- This phase implements Tier 3 from `docs/appendix/fb-takeover-tiers.md` verbatim; the "Components needed" list in that document is the authoritative scope checklist.
- `audio_client` is unchanged in this phase — Track D is a verification gate, not a code-change task.
- The `/bin/fb-takeover` symlink is not removed in this phase; removal is deferred to a later cleanup.
- `SYS_FB_YIELD` and `SYS_FB_REACQUIRE` are not removed from the dispatch table in this phase; only the deprecation log warn is added.
- The Phase 56 protocol codec (`kernel-core/src/display/protocol.rs`) is used as-is; Phase 70 adds no new protocol messages.
