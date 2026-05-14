# Phase 68 - Display Server Closeout

**Status:** Planned
**Source Ref:** phase-68
**Depends on:** Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅, Phase 64 (Session Manager Lifecycle) ✅
**Builds on:** Closes the Phase 56 completion-gap items that the audit (Red Flag #7 supplemental, `docs/appendix/audit-status/08-supplemental-findings.md`) and the in-tree `TODO(subscription-push)` markers identified as unresolved when Phase 56 was declared Complete; extends the display-server control protocol with two new event kinds; introduces an extracted `manifest.rs` + `supervisor.rs` for `userspace/init` to support multi-service `depends=` and a distinct `on-restart=` directive.
**Primary Components:** userspace/display_server, userspace/mouse_server, userspace/kbd_server, userspace/init, kernel-core/src/display

## Milestone Goal

Five concrete Phase 56 completion gaps are closed: the four `publish_*` functions in the display-server control path actually transmit subscribed events over the wire (and two new event kinds — `LayerEvent` and `CursorEvent` — gain matching publish + flush wiring); the compositor tracks damage rectangles and avoids full repaints on cursor motion; the input wire format gains a `ModifierSide` field with the global `PROTOCOL_VERSION` bumped from 1 to 2 and the existing handshake updated end-to-end; the `userspace/init` parser is extracted into `manifest.rs` + `supervisor.rs` and gains comma-separated `depends=` plus a distinct `on-restart=` directive; and `mouse_server` becomes a managed init service with its own `mouse_server.conf` declaring `depends=kbd_server`. A `> Phase 68 closure note:` block appended to Phase 56's design doc names the five gaps closed; the README row for Phase 68 is updated.

## Why This Phase Exists

Phase 56 was declared Complete in both the design doc and the roadmap README, but four `TODO(subscription-push)` markers at `userspace/display_server/src/control.rs:670,690,696,703` confirm that server-initiated event push was deliberately deferred; the `compose` path at `userspace/display_server/src/compose.rs:164-175` documents that cursor-only frames still trigger a full-framebuffer blit; the `KeyEvent` wire format at `kernel-core/src/input/events.rs:82-88` carries no L/R modifier discrimination; `mouse_server` exists as a binary at `userspace/mouse_server/` but has no service config and is not under init's supervision; and the supervisor-side restart-budget exhaustion handler logs but cannot dispatch a typed action.

This phase exists to close those five gaps with a clean substrate: real event push, real damage tracking, a real protocol version bump (rather than a fragile per-event header), an extracted init manifest/supervisor pair (rather than continuing to grow `main.rs` inline), and a real managed-service entry for `mouse_server`.

## Learning Goals

- Understand how a compositor damage model reduces unnecessary framebuffer writes and why a union rectangle is sufficient for a single-plane software compositor.
- Learn how a versioned wire format with an explicit handshake permits coordinated client/server upgrades without per-message ambiguity.
- See how extracting an inline parser into a dedicated module prepares for richer manifest semantics (multi-service deps, typed restart actions) without complicating the entry point.
- Understand why subscription event transmission requires a push mechanism distinct from the request-reply path, and how a flush-after-enqueue keeps the producer non-blocking.

## Feature Scope

### Subscription event push wire transmission (4 existing + 2 new event kinds)

The four `publish_*` functions in `userspace/display_server/src/control.rs` currently enqueue events to a per-subscriber ring and return — every one carries a `TODO(subscription-push)` comment. This phase adds a `flush_subscriber_ring(endpoint, ring)` helper that drains the ring to each subscriber's AF_UNIX endpoint after every event batch, and wires it into all four existing publish call sites: `publish_surface_created`, `publish_surface_destroyed`, `publish_focus_changed`, `publish_bind_triggered`.

In addition, two new `ControlEvent` variants are introduced — `LayerEvent` (layer-shell-equivalent surface state changes: anchor, exclusive zone, keyboard-interactivity transitions) and `CursorEvent` (cursor visibility / hot-spot changes). New `publish_layer_event` and `publish_cursor_event` functions follow the same enqueue-then-flush pattern. The matching `EventKind` discriminants extend the existing 0..=3 range to 0..=5; the wire encoding/decoding in `kernel-core/src/display/control.rs` and `kernel-core/src/display/protocol.rs` is extended additively under the version 2 protocol bump described below.

### Compositor damage tracking

`userspace/display_server/src/compose.rs` currently repaints the entire framebuffer on every `compose` call (per the design note at lines 164-175). A `DamageTracker` accumulates dirty rectangles from surface damage hints and cursor motion; `compose` clips all blit operations to the union of dirty rectangles. A full blit is issued only when `DamageTracker::is_full_repaint_needed()` returns true (first frame, explicit invalidation, or capacity-cap overflow).

### L/R modifier chord differentiation via global protocol bump

The wire format for `KeyEvent` (`kernel-core/src/input/events.rs`) gains a `ModifierSide` field (`Left`, `Right`, `Either`). Rather than introducing a fragile per-event version byte, the global `PROTOCOL_VERSION` in `kernel-core/src/display/protocol.rs` is bumped from `1` to `2`. The existing handshake (issued in 6 sites across `kernel-core/src/display/protocol.rs` and the matching `control.rs` reply paths) carries the new version; clients that announce version 1 receive `ModifierSide::Either` for every key event via a server-side compatibility shim. All in-tree clients (`kbd_server`, `display_server`, `m3ctl`-style consumers, host tests) are updated to handshake at version 2 in the same change set. `kbd_server` emits the appropriate side from the PS/2 extended scancode (`0xE0` prefix → Right; bare scancode → Left) for Shift, Ctrl, and Alt.

### `mouse_server` becomes a managed init service with `depends=kbd_server`

`mouse_server` currently exists as a binary at `userspace/mouse_server/` but has no init service config. This phase adds `kernel/initrd/etc/services.d/mouse_server.conf` declaring `depends=kbd_server`, registers the binary in `xtask/src/main.rs` (`bins` array), `kernel/src/fs/ramdisk.rs` (BIN_ENTRIES + `include_bytes!`), and `userspace/init/src/main.rs` (`KNOWN_CONFIGS` fallback) — the four-place rule from `AGENTS.md`. The previously hardcoded boot ordering for the kbd → mouse pair in `userspace/init/src/main.rs` (KNOWN_CONFIGS at `:126`) is replaced by the manifest-declared dependency; the supervisor checks the dependency before starting `mouse_server`.

### Init manifest + supervisor extracted; `on-restart=` directive added

`userspace/init/src/main.rs` (currently the only file in init's src tree, parser inline) is refactored: the parsing logic is extracted to a new `userspace/init/src/manifest.rs` and the start/restart loop is extracted to a new `userspace/init/src/supervisor.rs`. The new modules introduce:

- `ServiceManifest` with a `depends: Vec<String>` field (replacing the inline `[[usize; MAX_DEPS]; MAX_SERVICES]` resolved-index array).
- A new `on_restart: OnRestartAction` field with variants `LogAndContinue` (default), `TextFallback`, and `Panic`. The supervisor consults this when a service exhausts its restart budget. `TextFallback` calls into the `session_manager` text-fallback path; `Panic` halts.
- A small parallel `init::supervisor::ServiceState` enum (independent of `session_manager::ServiceState` at `userspace/session_manager/src/table.rs:40-57` — init and session_manager have different lifecycles and should not be coupled across the supervised/supervisor boundary).
- Cycle detection at manifest load (DFS over the dep graph); cycle members refuse to start and log an error.

No existing manifests are broken: `depends=` already accepts a single name today; the parser now also accepts comma-separated lists. `on-restart=` is optional and defaults to `LogAndContinue` when absent.

## Important Components and How They Work

### `userspace/display_server/src/control.rs` — event push and new event kinds

`flush_subscriber_ring(endpoint, ring)` drains pending events to the subscriber's AF_UNIX endpoint via `sys_send`. On `-EAGAIN` the event is dropped, an `events_dropped` counter is incremented, and the loop continues — the producer is not blocked. The six `publish_*` functions (four existing + two new) each call `flush_subscriber_ring` after enqueue. The two new `ControlEvent::LayerEvent` and `ControlEvent::CursorEvent` variants are added to the `ControlEvent` enum (around `:397-400`); their `EventKind` discriminants (4 and 5) extend the existing 0..=3 mapping at `:184-200` and `:197-200`.

### `userspace/display_server/src/compose.rs` — `DamageTracker`

`DamageTracker` holds a bounded `Vec<DamageRect>` (capacity 16; merge-to-union on overflow). Surfaces report damage via `mark_dirty(rect)`. Cursor motion marks the old and new cursor bounding box. `compose` calls `union_rect()`, clips all surface and cursor blit operations to the returned rectangle, and resets the tracker after compositing. A mouse-only frame blits only the cursor-sized region rather than the entire framebuffer.

### Wire-format version bump for the input/control protocol

`kernel-core/src/display/protocol.rs` bumps `PROTOCOL_VERSION` from `1` to `2`. The `KeyEvent` struct in `kernel-core/src/input/events.rs` gains a `modifier_side: ModifierSide` field; `KEY_EVENT_WIRE_SIZE` is updated. All six handshake sites (3 in `protocol.rs:1478,1682,1883` + 1 in `control.rs:297` + the userspace mirrors at `userspace/display_server/src/control.rs:469` and `kernel-core/src/display/control.rs:297`) are updated to advertise version 2. A version-1-handshake-from-client compatibility path returns `KeyEvent` records with `modifier_side: ModifierSide::Either` so legacy capture tools or fixtures continue to function during the transition window.

### `userspace/init/src/manifest.rs` (new)

Owns `ServiceManifest`, `OnRestartAction`, `parse_manifest`, and the cycle-detection pass. `ServiceManifest::depends` is `Vec<String>`. `parse_manifest` splits `depends=` on commas, trims whitespace, and rejects empty names. `on-restart=` parses to `OnRestartAction` with unknown values defaulting to `LogAndContinue` and a logged warning.

### `userspace/init/src/supervisor.rs` (new)

Owns the start/restart loop. `start_services_ordered` defers starting a service until all named dependencies are in `ServiceState::Running`. `handle_budget_exhaustion` dispatches on the manifest's `OnRestartAction`. The supervisor keeps init's responsibilities (process spawning, fd plumbing, ramdisk lookup) in `main.rs` and exposes a small surface to it.

### `kernel/initrd/etc/services.d/mouse_server.conf` (new)

```
name=mouse_server
exec=/bin/mouse_server
depends=kbd_server
restart=on-failure
max_restart=10
```

Registered alongside the existing `kbd.conf` and `audio_server.conf`. The four-place rule (xtask `bins`, ramdisk, KNOWN_CONFIGS, services.d entry) is followed; the existing implicit ordering between kbd and mouse in KNOWN_CONFIGS is removed and replaced by the manifest-declared dep.

## How This Builds on Earlier Phases

- Extends Phase 56's existing `control.rs` event infrastructure — the enqueue mechanism is unchanged; the flush step is added; two additional event kinds extend the `ControlEvent` / `EventKind` enums additively.
- Bumps the Phase 56 protocol version (a coordinated breaking change across in-tree clients) rather than smuggling a per-event version byte; the handshake mechanism Phase 56 already established carries the bump.
- Refactors Phase 52's inline service-config parser into the dedicated `manifest.rs` + `supervisor.rs` modules; no existing config syntax is broken.
- Reuses Phase 64's `session_manager` text-fallback dispatch path as the target for `OnRestartAction::TextFallback`. Init defines its own small `ServiceState` rather than coupling to `session_manager::ServiceState`.

## Implementation Outline

The `ModifierSide` extension is an Interface Segregation example: rather than expanding every key event with a full modifier bitmap, the protocol adds only the single field that differentiates L/R chords. The version bump (1→2) is the right cost-of-coordination tradeoff — handshake-aware clients are already a Phase 56 invariant, so the bump is cheap and the alternative (per-event header byte) is fragile.

Follow TDD for `DamageTracker`: write the unit tests (empty, single rect, two non-overlapping, two overlapping, capacity overflow → full-repaint flag) against the pure-logic struct before wiring it into `compose`. The compose integration test in QEMU is a smoke gate, not a substitute for the rect-merge unit tests.

1. Write `DamageTracker` unit tests; implement `DamageTracker` in `compose.rs` (or sibling `damage.rs`) and wire into `mark_dirty` and the cursor-motion path.
2. Add `flush_subscriber_ring` to `control.rs`; call from the four existing `publish_*` functions.
3. Add `ControlEvent::LayerEvent` and `ControlEvent::CursorEvent` variants and matching `EventKind` discriminants; implement `publish_layer_event` and `publish_cursor_event` with flush wiring; extend host-side encode/decode.
4. Define `ModifierSide` in `kernel-core/src/input/events.rs`; add the field to `KeyEvent`; bump `PROTOCOL_VERSION` to `2`; update all six handshake sites; add the version-1 client compatibility shim. Update `kbd_server::ps2::scan_to_key_event` to emit the appropriate side.
5. Extract `userspace/init/src/manifest.rs` and `userspace/init/src/supervisor.rs` from `main.rs`. Add `Vec<String>` `depends`, comma-split parsing, and DFS cycle detection. Add `OnRestartAction` and `on-restart=` parsing. Implement dependency-ordered start and `handle_budget_exhaustion` dispatch.
6. Create `kernel/initrd/etc/services.d/mouse_server.conf` declaring `depends=kbd_server`. Register `mouse_server` in xtask `bins`, ramdisk `BIN_ENTRIES`, and `KNOWN_CONFIGS`. Remove the implicit kbd→mouse ordering from KNOWN_CONFIGS.
7. Append a `> Phase 68 closure note:` block to Phase 56's `## Deferred Until Later` section naming the five gaps closed; cross-reference from the Phase 56 task doc completion-gap items.

## Acceptance Criteria

- A subscriber connected to `display_server` receives a `SurfaceCreated`, `SurfaceDestroyed`, `FocusChanged`, `BindTriggered`, `LayerEvent`, or `CursorEvent` within 10 ms of the corresponding state change; confirmed via `cargo xtask test --test display_subscription_push`.
- A cursor-motion-only frame causes `DamageTracker` to mark only the cursor bounding box dirty; the total blit area is strictly less than the full framebuffer; confirmed by an instrumented `compose` test asserting blit-pixel counts.
- A `KeyEvent` for left-Shift carries `ModifierSide::Left`; right-Shift carries `ModifierSide::Right`; Either is the default for non-modifier keys; confirmed by `kbd_server` unit tests.
- `kernel-core/src/display/protocol.rs` `PROTOCOL_VERSION` is `2`. All six handshake sites announce `2`. A client handshaking at `1` receives `KeyEvent` records with `ModifierSide::Either`; confirmed by a host test in `kernel-core`.
- `mouse_server` is a managed init service: `kernel/initrd/etc/services.d/mouse_server.conf` exists; `mouse_server` appears in xtask `bins`, ramdisk `BIN_ENTRIES`, and `KNOWN_CONFIGS`; `cargo xtask test --test manifest_depends` confirms `mouse_server` does not start until `kbd_server` is in `Running`.
- `userspace/init/src/manifest.rs` and `userspace/init/src/supervisor.rs` exist; `parse_manifest` accepts `depends=a,b,c`; cycle detection logs and refuses to start cycle members.
- A service with `on-restart=text-fallback` that exhausts its restart budget triggers the `session_manager` text-fallback path rather than a log-only outcome; an integration test confirms the dispatch.
- `docs/roadmap/56-display-and-input-architecture.md` `## Deferred Until Later` contains a `> **Phase 68 closure note:**` block naming the five gaps closed; the matching items in `docs/roadmap/tasks/56-display-and-input-architecture-tasks.md` carry "(closed in Phase 68)" annotations.
- `docs/roadmap/README.md` Phase 68 row reflects the shipped scope (status moves from `Planned` to `Complete` at merge); `kernel/Cargo.toml` is bumped to `0.68.0`; `AGENTS.md` references `Kernel v0.68.0`.

## Companion Task List

- [Phase 68 Task List](./tasks/68-display-server-closeout-tasks.md)

## How Real OS Implementations Differ

- Wayland compositors use shared memory damage regions (wl_buffer damage hints) and explicit buffer release events; m3OS uses a simpler structured hint over AF_UNIX.
- Linux's input subsystem distinguishes left/right modifiers at the evdev layer through separate key codes; m3OS adds a `ModifierSide` field rather than expanding the key-code space.
- systemd's service units express `After=` and `Requires=` dependency ordering with rich activation semantics; m3OS uses a simpler `depends=` that provides ordering without conditional activation, and an `on-restart=` directive that is closer to systemd's `OnFailure=` action than to its restart policy.
- Linux init systems typically share a single supervisor for all services; m3OS splits responsibilities between `init` (boot + restart budget) and `session_manager` (graphical session lifecycle), so the `on-restart=text-fallback` action crosses a process boundary via a control socket.

## Deferred Until Later

- Multiple compositor damage regions per frame (currently merged to a union rectangle).
- Hardware plane overlays for cursor compositing without software blit.
- Input protocol extensions beyond `ModifierSide` (relative pointer, tablet pressure, gesture events).
- Full Wayland protocol compatibility.
- Dynamic service dependency hot-plug (dependencies declared at runtime rather than manifest load).
- Coupling `init::supervisor::ServiceState` and `session_manager::ServiceState` into a shared crate (kept independent for now to avoid premature abstraction).
