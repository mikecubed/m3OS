# DOOM In-GUI Surface (fb-takeover Tier 3)

**Aligned Roadmap Phase:** Phase 70
**Status:** Complete
**Source Ref:** phase-70
**Supersedes Legacy Doc:** new

## Overview

Phase 70 finishes the story Phase 47 started: it turns DOOM from a
direct-framebuffer program into a regular `display_server` client.
Before Phase 70, running DOOM under a graphical boot required the
`fb-takeover` wrapper, which sent a `YieldFb` verb to
`display_server`, asked the kernel to release the framebuffer via
`SYS_FB_YIELD` (`0x101C`), spawned DOOM, then asked the kernel to
reclaim with `SYS_FB_REACQUIRE` (`0x101D`) once DOOM exited. That
sequence carried two known residual bugs documented in
[`docs/appendix/fb-takeover-tiers.md`](appendix/fb-takeover-tiers.md):
a second consecutive takeover hung the new DOOM process in
`BlockedOnReply`, and the compositor mouse pointer reset to the
top-left after every reclaim.

Phase 70 removes those bugs structurally rather than chasing the
input-routing edge cases that caused them. After Phase 70, DOOM does
not "take over" the screen at all. It allocates a shared-memory pixel
region via `sys_shm_create`, hands it to `display_server` through the
Phase 56 `AttachSharedBuffer` verb, writes its frames into the SHM
region, and asks the compositor to recomposite via `DamageSurface` +
`CommitSurface`. The compositor never yields. A second DOOM window is
just another client surface.

A new userspace crate, `userspace/lib/display_client_ffi/`, exposes
the relevant slice of the Phase 56 protocol codec behind a small C
ABI so the C-language doomgeneric platform layer can speak it
directly. The pattern mirrors `audio_client_ffi`, which already
gives `m3os_sound.c` access to the Rust `audio_client` library.

## What This Doc Covers

- The `dg_m3os.c` surface-buffer rewrite — connect, create-toplevel,
  shm-create, attach-shared-buffer, per-frame damage + commit
- The pixel-format pass-through — doomgeneric in this repo
  pre-scales its 320×200 indexed canvas to a 1280×800 BGRA8888
  buffer before calling `DG_DrawFrame`, so the rewrite degenerates to
  a single `memcpy` per frame
- Input rewiring — keyboard events arrive as
  `ServerMessage::Key(KeyEvent)` on the protocol socket, drained
  through `dc_poll_event`, instead of a dedicated `kbd_server`
  connection. The focus-aware dispatcher in `display_server::input`
  decides whether DOOM sees keypresses
- `fb-takeover` deprecation — the wrapper still functions and the
  compatibility smoke continues to exercise it, but it emits a
  stderr warning on every invocation
- Concurrent-instance correctness — two DOOM windows in the same
  display_server session render simultaneously, validated by the new
  `cargo xtask doom-concurrent-smoke` gate

## Core Implementation

### `DG_Init` → `DG_DrawFrame` → `DG_GetKey` data flow

After Phase 70 the platform layer follows a single linear bring-up:

1. `DG_Init` calls `dc_connect`, which resolves the `"display"`
   registered service with bounded retry (mirroring `term`'s connect
   path) and sends `ClientMessage::Hello { protocol_version =
   PROTOCOL_VERSION }`. Returns an opaque `DcHandle *`.
2. `DG_Init` calls `dc_create_toplevel`, which sends
   `CreateSurface { surface_id }` and
   `SetSurfaceRole { surface_id, role: Toplevel }`. The returned
   `surface_id` is stored module-level for later verbs.
3. `DG_Init` allocates the pixel SHM region with
   `sys_shm_create(WIDTH * HEIGHT * 4)` and maps it via
   `sys_shm_map`. The mapped pointer is what doomgeneric will copy
   into every frame.
4. `DG_Init` calls `dc_attach_shm_buffer(handle, surface_id,
   buffer_id = 1, shm_id, WIDTH, HEIGHT)`. The compositor maps the
   same physical frames read-only into its own address space and
   begins reading from them on every compose pass.
5. `DG_DrawFrame` `memcpy`s the doomgeneric `DG_ScreenBuffer` into the
   mapped SHM region and calls `dc_damage_and_commit`, which sends
   `DamageSurface { rect: { 0, 0, WIDTH, HEIGHT } }` and then
   `CommitSurface { surface_id }`. No heap allocation in this path.
6. `DG_GetKey` drains pending events via `dc_poll_event`. `Key`
   events translate to DOOM's expected keycodes through a small
   keycode → DOOM map; `FocusIn` / `FocusOut` toggle a local
   `g_focused` flag (used for diagnostic logging today); a
   `Disconnect` from the compositor cleanly exits the process.

The full surface size matches `DOOMGENERIC_RESX × DOOMGENERIC_RESY`
(1280 × 800 in this repo's doomgeneric overlay) — the engine
pre-scales the 320×200 indexed canvas into the surface buffer before
calling `DG_DrawFrame`, so the dg_m3os.c side never needs a separate
palette LUT. The pixel format is BGRA8888, which is exactly what the
compositor expects, so the per-frame conversion collapses to one
straight-line `memcpy`.

### Multi-client correctness

The Phase 56 display server already supports multiple concurrent
clients (term and gfx-demo share the compositor today). Phase 70 adds
no new code to display_server itself — the same `SurfaceRegistry`,
`LayoutPolicy`, and per-client `BufferLifecycle` machinery that
serves `term` now serves DOOM. The
`cargo xtask doom-concurrent-smoke` gate is the regression that
makes this property visible: two DOOM processes launched in
background under a single shell must both reach
`M3OS_DOOM:title_ready` and both complete the autoquit-budget
lifecycle inside the global timeout. If either hangs in
`BlockedOnReply`, the shell-side `wait` blocks indefinitely and the
harness times out — that timeout is the gate's failure signal.

### Syscall deprecation

`SYS_FB_YIELD` and `SYS_FB_REACQUIRE` remain in the kernel dispatch
table because the `fb-takeover` compatibility shim still calls them.
Both arms emit a `log::warn!("SYS_FB_{YIELD,REACQUIRE} called by pid
{pid}; syscall is deprecated (Phase 70), scheduled for removal")` so
a future cleanup phase can confirm all callers have gone away before
deleting the dispatch arms. The wrapper itself prints a stderr
warning before exec'ing its child so any user typing
`fb-takeover doom` sees the deprecation message immediately.

## Key Files

| File | Role |
|---|---|
| `userspace/doom/dg_m3os.c` | doomgeneric platform layer; the Phase 70 rewrite lives here |
| `userspace/lib/display_client_ffi/src/lib.rs` | Rust staticlib wrapping the Phase 56 protocol codec behind a C ABI (`dc_connect`, `dc_create_toplevel`, `dc_attach_shm_buffer`, `dc_damage_and_commit`, `dc_poll_event`, `dc_disconnect`) |
| `userspace/lib/display_client_ffi/include/display_client.h` | Hand-maintained C header for the FFI surface; the crate's `build.rs` validates that every `DC_*` `#define` matches the corresponding `pub const` |
| `kernel-core/src/display/protocol.rs` | Phase 56 message types — `Hello`, `CreateSurface`, `SetSurfaceRole`, `AttachSharedBuffer`, `DamageSurface`, `CommitSurface`, `ServerMessage::Key` / `FocusIn` / `FocusOut` / `SurfaceResized` / `BufferReleased` / `Disconnect` — all single-sourced here |
| `userspace/fb-takeover/src/main.rs` | Now deprecated; emits a stderr warning before exec'ing its child |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `sys_fb_yield` / `sys_fb_reacquire` now emit `log::warn!` deprecation lines that name the caller PID |
| `xtask/src/main.rs` | Hosts the new `cargo xtask doom-concurrent-smoke` gate; the existing `doom-audio-smoke` is retargeted to invoke DOOM directly (no `fb-takeover` prefix) |

## Related Roadmap Docs

- [Phase 70 Design Doc](roadmap/70-doom-in-gui-surface.md)
- [Phase 70 Task List](roadmap/tasks/70-doom-in-gui-surface-tasks.md)
- [Phase 47 (DOOM)](roadmap/47-doom.md) — the framebuffer-takeover
  baseline this phase replaces
- [Phase 56 (Display and Input Architecture)](roadmap/56-display-and-input-architecture.md)
  — the surface-buffer protocol Phase 70 consumes
- [`docs/appendix/fb-takeover-tiers.md`](appendix/fb-takeover-tiers.md)
  — historical context on Tiers 1 / 2 / 3 and the residual bugs
  Phase 70 closes

## Learning Notes

A future reader looking at this phase as a case study should notice
three things:

1. **Substitutability bought you concurrency for free.** The
   compositor never learned about DOOM; DOOM learned how to be a
   compositor client. The second DOOM window works without a single
   line of new code in `display_server` because the protocol was
   already client-count-agnostic.
2. **Deprecation is a process, not a deletion.** `SYS_FB_YIELD` and
   `SYS_FB_REACQUIRE` are still callable. The `log::warn!` arms give
   future cleanup phases a paper trail of who still calls them, and
   the wrapper's stderr message gives any human user a nudge toward
   the right invocation path. Removal will land in a later phase once
   the warns stop showing up in serial logs.
3. **Two staticlibs can share a Rust runtime if they're identical.**
   Both `libaudio_client_ffi.a` and `libdisplay_client_ffi.a` ship
   their own `staticlib_runtime` (panic-on-abort,
   libc-malloc-allocator). The DOOM link step uses
   `-Wl,--allow-multiple-definition` to keep the linker happy; the
   resulting binary picks one copy and silently drops the other. This
   is fine because the two copies are byte-for-byte identical.
