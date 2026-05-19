# Phase 70 - DOOM In-GUI Surface (fb-takeover Tier 3)

**Status:** Planned
**Source Ref:** phase-70
**Depends on:** Phase 47 (DOOM) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Builds on:** Replaces `userspace/doom/dg_m3os.c`'s direct framebuffer-write render path with Phase 56 surface-buffer protocol rendering, making DOOM a regular `display_server` client; retires the `fb-takeover` wrapper as the required invocation path; marks `SYS_FB_YIELD` / `SYS_FB_REACQUIRE` deprecated
**Primary Components:** userspace/doom/dg_m3os.c, userspace/lib/display_client_ffi (new), userspace/lib/surface_buffer, kernel-core/src/display/protocol.rs, userspace/fb-takeover, kernel/src/arch/x86_64/syscall/mod.rs

## Milestone Goal

DOOM runs as an ordinary `display_server` client. `doom -warp 1 1` opens a DOOM
window — no `fb-takeover` wrapper required. The compositor owns the framebuffer at
all times; DOOM renders into a Phase 56 `Toplevel` surface buffer. Multiple DOOM
windows run concurrently without conflict. The second-consecutive-takeover hang and
the mouse-pointer-reset residual documented in `docs/appendix/fb-takeover-tiers.md`
§ "Known Tier 1 reclaim residuals" are resolved structurally because there is no
longer a takeover.

Applying SOLID's Liskov Substitution Principle, DOOM after this phase is interchangeable with `term` or `gfx-demo` behind the surface-buffer protocol — any client that speaks `Hello` + `CreateSurface` + `AttachSharedBuffer` + `CommitSurface` can take DOOM's slot. The Phase 56 `LayoutPolicy` and `SurfaceRole` abstractions remain untouched (Open/Closed Principle): DOOM's render-path change requires zero modifications to compositor core. The test pyramid runs doomgeneric unit tests on the host, a surface-buffer protocol smoke in `kernel-core`, and the full-game concurrent-instance smoke in QEMU as the top tier.

## Why This Phase Exists

Tier 1 (`fb-takeover` wrapper + `SYS_FB_YIELD` / `SYS_FB_REACQUIRE`) was the
correct minimal landing for Phase 57d — it made DOOM run without redesigning the
rendering layer. Two known residuals remain open: the second consecutive takeover
hangs with a wedged IPC reply, and the mouse pointer resets to top-left after
reclaim. Both are architectural artifacts of the yield/reclaim protocol: the
compositor is frozen, input routing is undefined, and per-process state from the
first session pollutes the second.

Tier 3 removes the entire problem class. When DOOM is a `display_server` client,
there is no mode switch, no IPC-wake-propagation gap between sessions, no compositor
freeze, and no pointer-state reset. The display_server composes DOOM's surface
alongside every other surface; a second DOOM window is just another client instance.

This phase also serves as a protocol-correctness forcing function: the Phase 56
surface-buffer SHM transport has been validated by `term` (Track G of Phase 57) but
never exercised by a second independent client archetype with different rendering
cadence (game loop vs. text event loop).

## Learning Goals

- Understand how a platform-layer rewrite decouples a legacy application from a
  kernel ABI without changing the application's game logic.
- Learn how indexed-color palette rendering maps to a compositor's native pixel
  format without double-buffering in kernel space.
- See why "multiple instances work for free" is the structural benefit of a
  compositor model versus a mode-switch model.
- Understand how kernel ABI deprecation is handled in a toy OS — syscalls remain
  callable but emit log warnings and are scheduled for removal.

## Feature Scope

### `display_client_ffi` crate (Track A0)

DOOM is C and cannot call the Rust `surface_buffer` crate directly. Phase 70 adds
`userspace/lib/display_client_ffi/`, a Rust staticlib that wraps `surface_buffer` +
the Phase 56 `kernel_core::display::protocol::ClientMessage` codec behind a C ABI.
This mirrors the Phase 63a `audio_client_ffi` bridge that already gives
`m3os_sound.c` access to `audio_client`. Keeping the protocol encode/decode and the
buffer-lifecycle invariants in Rust avoids reimplementing them in C and preserves
the `BufferLifecycle` state machine that `display_server::client` already enforces
on the server side.

### `dg_m3os.c` surface-buffer rewrite (Track A)

DOOM's platform layer is entirely in `userspace/doom/dg_m3os.c`. The existing
implementation calls `sys_framebuffer_acquire`, mmaps the framebuffer pages, and
writes BGRA8888 pixels directly. The rewrite replaces this — calling through the
new `display_client_ffi` C ABI — with:

1. `Hello { protocol_version = PROTOCOL_VERSION }` handshake on the
   `display_server` socket resolved via the registered service name (same lookup
   path `term` uses in `userspace/term/src/lib.rs`).
2. `CreateSurface { surface_id }` followed by
   `SetSurfaceRole { surface_id, role: SurfaceRole::Toplevel }`.
3. `sys_shm_create(size = 320 * 200 * 4)` to allocate a 256 KiB pixel region;
   then `AttachSharedBuffer { surface_id, buffer_id, shm_id, width: 320, height: 200 }`
   to hand the region to `display_server`.
4. Per-frame: convert 8-bit indexed palette to BGRA8888 in the shared buffer; send
   `DamageSurface { surface_id, rect: { 0, 0, 320, 200 } }` followed by
   `CommitSurface { surface_id }`.

The game loop drives rendering at its own cadence; the compositor composes on its
own cadence. No synchronization primitive between the two is introduced in Phase 70;
tearing is acceptable on a toy OS demo.

### Pixel-format conversion (Track B)

DOOM's 256-entry indexed palette is loaded from the WAD at startup. The platform
layer precomputes a palette LUT: `palette_bgra: [u32; 256]`, converting each
palette entry (RGB888 from the WAD) to BGRA8888. Per-frame blit iterates the
320×200 indexed pixels and performs a LUT lookup per pixel into the shared buffer.
No heap allocation in the per-frame path.

### Input rewiring via Phase 56 dispatcher (Track C)

The existing `dg_m3os.c` polls keyboard events through its own `kbd_server` IPC
path. After the rewrite, DOOM receives keyboard events as `ServerMessage::Key(KeyEvent)`
from the same `display_server` protocol socket that delivers `FocusIn` / `FocusOut`
and `SurfaceResized`. The focus-aware dispatcher in `display_server::input` enforces
delivery only to the focused surface, so DOOM becomes a regular focusable client:
keyboard events arrive only when the DOOM surface is focused; they cease when focus
moves elsewhere.

### Audio path verification and gate retarget (Track D)

`audio_client` (Phase 57 E) is already independent of the framebuffer path. After
the `dg_m3os.c` rewrite, verify that PCM submission continues to work when DOOM
runs as a surface client. No DOOM-side code change is expected. The existing
`cargo xtask doom-audio-smoke` gate, which today launches DOOM through the
`fb-takeover` wrapper, is retargeted to launch DOOM directly (`doom -warp 1 1` with
no wrapper) so it doubles as the end-to-end audio + direct-invocation regression
for Tier 3. A separate, smaller compatibility test under Track E continues to
exercise the `fb-takeover doom ...` path until the wrapper is removed in a later
phase.

### `fb-takeover` wrapper retirement (Track E)

`userspace/fb-takeover/` is marked deprecated in its `Cargo.toml` and source
header. `/bin/fb-takeover` remains as a compatibility symlink for the Phase 57e
boot sequence and any scripts that reference it; it now emits a deprecation
warning to stderr before forwarding to its child. `m3ctl yield-fb` continues to
exist but prints a deprecation notice.

### `SYS_FB_YIELD` / `SYS_FB_REACQUIRE` deprecation (Track H)

Syscalls `0x101C` (`SYS_FB_YIELD`) and `0x101D` (`SYS_FB_REACQUIRE`) remain in
the dispatch table. Each emits a `log::warn!` naming the deprecated syscall and the
caller PID. They continue to function for any remaining callers (the `fb-takeover`
wrapper itself). Removal is deferred to a later cleanup phase.

### Concurrent instances and regression test (Track F)

Two DOOM windows (`doom -warp 1 1` + `doom -warp 1 2`) open simultaneously in the
same display_server session. Both render into independent surface buffers; the
compositor composes both. The second-takeover hang no longer applies because there
is no takeover. The mouse-pointer-reset no longer applies because there is no
reclaim.

### Documentation updates (Track G)

`docs/appendix/fb-takeover-tiers.md` disposition field updated: "Tier 3 landed in
Phase 70; Tier 1 retained as fallback for non-display_server boots (headless /
serial-only mode)." Phase 47 design doc updated to note DOOM is now a regular
display-server client.

## Important Components and How They Work

### `userspace/doom/dg_m3os.c`

The entire platform-specific surface of DOOM lives here: init, rendering, keyboard
input, mouse input, audio. After Phase 70, `DG_Init` opens the `display_server`
socket through `display_client_ffi`, sends `Hello` + `CreateSurface` +
`SetSurfaceRole(Toplevel)`, calls `sys_shm_create` for a 320×200×4 BGRA region,
and sends `AttachSharedBuffer`; `DG_DrawFrame` converts the indexed pixel buffer to
BGRA8888 in the shared region and sends `DamageSurface` + `CommitSurface`;
`DG_GetKey` drains `ServerMessage::Key(KeyEvent)` from the protocol socket rather
than from a dedicated `kbd_server` connection; `DG_SoundStart` continues to use
`audio_client_ffi` unchanged.

### `userspace/lib/display_client_ffi/` (new)

Rust staticlib wrapping `userspace/lib/surface_buffer` and the Phase 56
`ClientMessage` codec behind a C ABI. Mirrors the Phase 63a `audio_client_ffi`
pattern. Exposes a small surface: `dc_connect`, `dc_create_toplevel`,
`dc_attach_shm_buffer`, `dc_damage_and_commit`, and a non-blocking
`dc_poll_event` returning a tagged C union for `Key` / `FocusIn` / `FocusOut` /
`SurfaceResized`.

### `kernel-core/src/display/protocol.rs`

The Phase 56 display protocol codec. `Hello`, `CreateSurface`, `SetSurfaceRole`,
`AttachSharedBuffer`, `DamageSurface`, and `CommitSurface` messages are already
defined here (`ClientMessage` enum at line 335). Phase 70 adds no new protocol
messages; DOOM is a consumer of existing protocol, not a protocol extension.

### `userspace/fb-takeover/src/main.rs`

Marked deprecated. Emits a deprecation warning before exec'ing the child. Kept in
the image for backward compatibility with any scripts or service configs that invoke
`fb-takeover` directly.

### `kernel/src/arch/x86_64/syscall/mod.rs`

`SYS_FB_YIELD` (`0x101C`) and `SYS_FB_REACQUIRE` (`0x101D`) dispatch arms emit
`log::warn!` and continue to function. No behavioral change.

## How This Builds on Earlier Phases

- Replaces Phase 47's direct framebuffer rendering with Phase 56 surface-buffer
  protocol rendering; Phase 47's game logic and WAD loading are unchanged.
- Reuses Phase 56 `CreateSurface` / `SetSurfaceRole` / `AttachSharedBuffer` /
  `DamageSurface` / `CommitSurface` exactly as Phase 57 `term` uses them
  (`userspace/term/src/display.rs`), providing a second validation of the SHM
  transport path.
- Reuses Phase 57 `audio_client` for the PCM bell path; no audio change required.
- Closes the Tier 1 residuals documented in `docs/appendix/fb-takeover-tiers.md`
  (second-takeover hang, mouse-pointer reset) by eliminating the yield/reclaim cycle.

## Implementation Outline

1. Study `userspace/doom/dg_m3os.c` current rendering path and
   `userspace/term/src/display.rs` reference client; document exact mmaps and
   framebuffer write sites and the `Hello` → `CreateSurface` →
   `AttachSharedBuffer` sequence to mirror.
2. Add `userspace/lib/display_client_ffi/` crate: Rust staticlib + C header
   wrapping `surface_buffer` and the `ClientMessage` codec behind a C ABI.
3. Implement `DG_Init` surface-creation path through `display_client_ffi`:
   `dc_connect` (registered service `display_server`), `dc_create_toplevel`,
   `sys_shm_create(320 * 200 * 4)`, `dc_attach_shm_buffer`. Store buffer mapping.
4. Implement palette LUT precomputation in `DG_Init`; compute `palette_bgra[256]`
   from WAD palette entries.
5. Implement `DG_DrawFrame` per-frame blit: LUT lookup → shared buffer;
   `dc_damage_and_commit` (sends `DamageSurface` + `CommitSurface`).
6. Rewire `DG_GetKey` to consume `ServerMessage::Key(KeyEvent)` via
   `dc_poll_event` from the protocol socket instead of a separate `kbd_server`
   connection.
7. Verify audio path (`DG_SoundStart`, `DG_SoundUpdate`) compiles and functions
   after the render-path rewrite; retarget `cargo xtask doom-audio-smoke` to
   launch DOOM directly (no `fb-takeover` wrapper).
8. Mark `userspace/fb-takeover/` deprecated in `Cargo.toml` and source header;
   add stderr deprecation message.
9. Add `log::warn!` to `SYS_FB_YIELD` and `SYS_FB_REACQUIRE` dispatch arms.
10. Update `docs/appendix/fb-takeover-tiers.md`, `docs/47-doom.md`, and
    `docs/roadmap/47-doom.md`.
11. Concurrent-instance regression test: two DOOM windows run simultaneously;
    wire `M3OS_DOOM_CONCURRENT_REGRESSION=1` env-gated step into
    `.githooks/pre-push`.
12. Author `docs/70-doom-in-gui-surface.md` learning doc and bump kernel to
    `0.70.0`.

## Acceptance Criteria

- `doom -warp 1 1` (no `fb-takeover` wrapper) opens a DOOM window under the
  display_server compositor.
- Keyboard input (movement, fire, escape) reaches DOOM only when the DOOM window
  is focused; typing in a focused `term` while DOOM is minimized/unfocused does not
  affect DOOM.
- Two DOOM windows (`doom -warp 1 1` and `doom -warp 1 2`) run concurrently without
  conflict; both render correctly; no second-window hang.
- The `audio_client` bell/sound path continues to function while DOOM is running.
- `fb-takeover doom` still works (compatibility preserved) but emits a deprecation
  warning.
- `SYS_FB_YIELD` and `SYS_FB_REACQUIRE` appear in the kernel log as deprecated
  when called.

## Companion Task List

- [Phase 70 Task List](./tasks/70-doom-in-gui-surface-tasks.md)

## How Real OS Implementations Differ

- On Linux/Wayland, SDL2 and similar game libraries use `wl_shm` or DMA-BUF for
  shared-memory surface rendering; m3OS uses its own typed Phase 56 SHM protocol.
- Wayland's `wl_surface.frame` callback allows a client to synchronize rendering
  with the compositor's refresh cycle; m3OS Phase 70 does not implement a frame
  callback — DOOM renders at its own pace and the compositor picks up the latest
  committed buffer.
- Real game engines use double- or triple-buffering to avoid tearing; Phase 70 uses
  single-buffer committed rendering, which is acceptable for a toy demo.
- On Linux, deprecated syscalls are typically kept for years through the stable-ABI
  guarantee; m3OS can schedule removal aggressively once callers are confirmed gone.

## Deferred Until Later

- Frame-callback synchronization between DOOM's render loop and the compositor
- Wayland-equivalent damage coalescing (batching multiple `DamageSurface` calls)
- Removal of `SYS_FB_YIELD` and `SYS_FB_REACQUIRE` from the kernel dispatch table
- Full retirement of the `fb-takeover` binary from the image
- Port of other legacy framebuffer programs to surface-buffer clients
- Double-buffering or vsync-paced rendering for DOOM
