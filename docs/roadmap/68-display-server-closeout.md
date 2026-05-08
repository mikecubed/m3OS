# Phase 68 - Display Server Closeout

**Status:** Planned
**Source Ref:** phase-68
**Depends on:** Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Builds on:** Closes the Phase 56 completion-gap items that remained open after the phase was declared Complete; flips Phase 56's design-doc status from Planned to Complete; closes audit Red Flag #7 for Phase 56
**Primary Components:** userspace/display_server, userspace/mouse_server, userspace/kbd_server, userspace/init, m3ctl

## Milestone Goal

Five concrete Phase 56 completion gaps are closed: subscription events are transmitted over the wire rather than queued and dropped; the compositor tracks damage rectangles and avoids full repaints on cursor motion; the wire format gains a versioned L/R modifier chord differentiation; the `mouse_server` dependency on `kbd_server` is declared via the init manifest rather than hardcoded; and a distinct `on-restart=` supervisor directive replaces the overloaded `restart=` field. Phase 56's design-doc `**Status:**` field is flipped from `Planned` to `Complete`.

## Why This Phase Exists

Phase 56 was declared Complete in the roadmap README but its design doc retains `Status: Planned`. More substantively, the audit identified five functional gaps that survived the phase close: the four `publish_*` functions enqueue events but never transmit them; damage tracking is absent so every cursor motion triggers a full framebuffer blit; modifier chord differentiation is unimplemented at the wire level; the `mouse_server` dependency direction is implicit (hardcoded boot ordering) rather than manifest-declared; and the supervisor directive vocabulary conflates restart policy with restart behavior.

This phase exists to make Phase 56 actually complete rather than nominally complete.

## Learning Goals

- Understand how a compositor damage model reduces unnecessary framebuffer writes.
- Learn how a versioned wire format allows protocol extension without breaking existing clients.
- See how declaring service dependencies in a manifest separates policy from mechanism.
- Understand why subscription event transmission requires a push mechanism distinct from the request-reply path.

## Feature Scope

### Subscription event push wire transmission

The four `publish_*` functions in `userspace/display_server/src/control.rs` currently enqueue events to a per-subscriber ring and return. The wire transmission step was never implemented. This phase adds the send call that drains the ring to each subscriber's endpoint after every event batch.

### Compositor damage tracking

`userspace/display_server/src/compose.rs` currently repaints the entire framebuffer on every `compose` call. A `DamageTracker` accumulates dirty rectangles from surface damage hints and cursor motion; `compose` clips all blit operations to the union of dirty rectangles. A full blit is issued only when `DamageTracker::is_full_repaint_needed()` returns true (e.g., first frame or explicit invalidation).

### L/R modifier chord differentiation

The wire format for `KeyEvent` is extended with a `ModifierSide` field (Left, Right, Either). The format version number is bumped. Clients that do not send the version handshake receive the old `Either` default for backward compatibility. `kbd_server` emits the appropriate side from the PS/2 extended scancode.

### `mouse_server` dependency via manifest

The init manifest parser is extended to support comma-separated `depends=` entries. `mouse_server.conf` declares `depends=kbd_server` so the supervisor starts it only after `kbd_server` is ready. The hardcoded boot ordering in `session_manager` is removed for this pair.

### Distinct `on-restart=` supervisor directive

Service manifests gain a separate `on-restart=` field that controls the action taken when a service reaches its restart budget limit (e.g., `on-restart=text-fallback` or `on-restart=log-and-continue`). The existing `restart=` field retains its meaning (restart policy: always, on-failure, never). No existing manifest is broken; `on-restart=` defaults to `log-and-continue`.

## Important Components and How They Work

### `userspace/display_server/src/control.rs` — event push

`publish_surface_event`, `publish_focus_event`, `publish_layer_event`, and `publish_cursor_event` each call a new `flush_subscriber_ring(endpoint)` helper after enqueuing. The helper drains the ring to the subscriber's AF_UNIX endpoint using `sys_send`. If the send returns `-EAGAIN` the event is dropped and a counter incremented; the subscriber is not blocked.

### `userspace/display_server/src/compose.rs` — `DamageTracker`

`DamageTracker` holds a list of `DamageRect { x, y, w, h }`. Surfaces report damage via `mark_dirty(rect)`. Cursor motion marks the old and new cursor bounding box. `compose` merges overlapping rectangles, clips all blit calls, and resets the tracker after compositing. The net effect is that a mouse-only frame blits only the cursor-sized region rather than the entire framebuffer.

### Wire format versioning for `KeyEvent`

A 2-byte header added to each `KeyEvent` message carries the format version. `kbd_server` sets the version. Clients that do not read the header (older clients) receive the version byte as a harmless extended scan code — a deliberate backward-compatible encoding.

### Init manifest parser extension

`userspace/init/src/manifest.rs` extended to parse `depends=a,b,c` as a `Vec<String>` of service names. The supervisor start loop checks that all listed dependencies are in `ServiceState::Running` before starting the dependent. Cyclic dependencies are detected at manifest load time and logged as errors.

### `on-restart=` supervisor directive

`userspace/init/src/manifest.rs` adds a `on_restart: OnRestartAction` field. `OnRestartAction` is an enum with variants `LogAndContinue` (default), `TextFallback`, and `Panic`. The supervisor consults this field when a service's restart budget is exhausted.

## How This Builds on Earlier Phases

- Extends Phase 56's existing `control.rs` event infrastructure — the enqueue mechanism is unchanged; only the flush step is added.
- Extends Phase 52's service manifest format with two new optional fields; no existing manifests require modification.
- Uses Phase 64's authentic `ServiceState` to implement the dependency readiness check.
- The wire format version bump is additive; Phase 56 clients with the original format continue to work.

## Implementation Outline

The `ModifierSide` wire format extension is a natural Interface Segregation example: rather than expanding every key event with a full modifier bitmap, the protocol adds only the single field that differentiates L/R chords — nothing more. The versioned header and `Either` backward-compatible default ensure old clients are not broken by the extension. Any future modifier extension (e.g., relative pointer, tablet pressure) should follow the same additive pattern rather than redesigning the wire format.

Follow TDD for `DamageTracker`: write the five unit tests (empty, single rect, two non-overlapping, two overlapping, full-repaint flag) against the pure-logic struct before wiring it into `compose`. The compose integration test is the QEMU-level smoke gate, not a substitute for the rect-merge unit tests.

1. Write `DamageTracker` unit tests; then implement `DamageTracker` in `compose.rs` and wire into `mark_dirty` and cursor motion path.
2. Add `flush_subscriber_ring` to `control.rs`; call from all four `publish_*` functions.
3. Extend `KeyEvent` wire format with `ModifierSide`; bump version; update `kbd_server` encoder and decoder.
4. Extend init manifest parser to support comma-separated `depends=`; implement dependency-ordered start in supervisor loop; add cycle detection.
5. Add `on-restart=` field to manifest parser and supervisor action dispatch.
6. Update `mouse_server.conf` to declare `depends=kbd_server`.
7. Flip Phase 56 design-doc status to Complete; add closure note.

## Acceptance Criteria

- A subscriber connected to `display_server` receives a `SurfaceEvent` within 10 ms of a surface damage hint; confirmed via a `cargo xtask test --test display_subscription_push` test.
- A cursor-motion-only frame causes `DamageTracker` to mark only the cursor bounding box dirty; total blit area is less than the full framebuffer; confirmed by an instrumented compose call count.
- A `KeyEvent` for left-Shift carries `ModifierSide::Left`; right-Shift carries `ModifierSide::Right`; confirmed by a `kbd_server` unit test.
- `mouse_server` does not start until `kbd_server` is in `Running` state; confirmed by `cargo xtask test --test manifest_depends`.
- A service with `on-restart=text-fallback` that exhausts its budget triggers the text-fallback path rather than logging-only.
- `docs/roadmap/56-display-and-input-architecture.md` has `**Status:** Complete`.

## Companion Task List

- [Phase 68 Task List](./tasks/68-display-server-closeout-tasks.md)

## How Real OS Implementations Differ

- Wayland compositors use shared memory damage regions (wl_buffer damage hints) and explicit buffer release events; m3OS uses a simpler structured hint over AF_UNIX.
- Linux's input subsystem distinguishes left/right modifiers at the evdev layer through separate key codes; m3OS adds a `ModifierSide` field to avoid wire format expansion for every modifier key.
- systemd's service units express `After=` and `Requires=` dependency ordering; m3OS uses a simpler `depends=` that provides ordering without complex activation semantics.

## Deferred Until Later

- Multiple compositor damage regions per frame (currently merged to a union rectangle)
- Hardware plane overlays for cursor compositing without software blit
- Protocol extensions beyond modifier chord differentiation (e.g., relative pointer, tablet input)
- Full Wayland protocol compatibility
- Dynamic service dependency hot-plug (dependencies declared at runtime rather than manifest load)
