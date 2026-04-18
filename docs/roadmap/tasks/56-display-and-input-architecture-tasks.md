# Phase 56 — Display and Input Architecture: Task List

**Status:** Planned
**Source Ref:** phase-56
**Depends on:** Phase 46 (System Services) ✅, Phase 47 (DOOM) ✅, Phase 50 (IPC Completion) ✅, Phase 51 (Service Model Maturity) ✅, Phase 52 (First Service Extractions) ✅, Phase 55 (Hardware Substrate) ✅
**Goal:** Replace the kernel-owned framebuffer and single-app input model with a single userspace display service that owns presentation, arbitrates surfaces for multiple graphical clients, routes focus-aware keyboard and mouse events, and exposes the four contract points a tiling-first compositor experience (Goal A in `docs/appendix/gui/tiling-compositor-path.md`) needs so the tiling UX can land on top without protocol rework.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Architecture and protocol design (adopts the four Goal-A design decisions as Phase 56 contract points) | None | Planned |
| B | Kernel substrate for ownership transfer (framebuffer handoff, mouse input path, vblank tick, surface buffer transport) | A | Planned |
| C | Display service (compositor core, software composer, surface state machine) | A, B | Planned |
| D | Input services and keybind-grab hook (key-event model, mouse service, focus-aware dispatch) | A, B, C | Planned |
| E | Layout policy, layer-shell-equivalent surfaces, and control socket | A, C, D | Planned |
| F | Session integration, supervision, and recovery | C, D, E | Planned |
| G | Validation: multi-client, grab hook, layer-shell, control socket, crash recovery | C, D, E, F | Planned |
| H | Documentation (learning doc, subsystem and evaluation updates) and version bump | G | Planned |

---

## Engineering Discipline and Test Pyramid

These are preconditions for every code-producing task in this phase. A task cannot be marked complete if it violates any of them. Where a later task re-states a rule for emphasis, the rule here is authoritative.

### Test-first ordering

- Tests for a code-producing task commit **before** the implementation that makes them pass. Git history for the touched files must show failing-test commits preceding green-test commits. "Tests follow" is not acceptable.
- Acceptance lists that say "at least N tests cover ..." name *minimums*. If the implementation reveals a new case, add the test before closing the task.
- A task is not complete until every test it names can be executed via `cargo test -p kernel-core` (unit, contract, property) or `cargo xtask test` (integration). Tests behind feature flags still run in the default CI invocation.

### Test pyramid

| Layer | Location | Runs via | Covers |
|---|---|---|---|
| Unit | `kernel-core/src/display/` and `kernel-core/src/input/` | `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` | Pure logic: protocol codec, keymap translation, mouse packet decode, surface state machine, damage and clip math, compose ordering, bind matching, control-socket command parse |
| Contract | `kernel-core` shared harness | Same | Traits with ≥2 implementations (`LayoutPolicy`, `FramebufferOwner`, `InputSource`) pass the same behavioral test suite against every impl |
| Property | `kernel-core` with `proptest` (available from Phase 43c) | Same | Codec round-trip (`decode(encode(x)) == x`) for every message variant; state-machine invariants; decoder robustness on arbitrary byte streams |
| Integration | `userspace/display_server/tests/` | `cargo xtask test` (QEMU) | Multi-process flows: client connect, surface commit, input dispatch, grab-hook, layer-shell exclusive zone, control socket, crash recovery, text-mode fallback |

Pure logic belongs in `kernel-core`. Hardware-dependent and IPC-dependent wiring belongs in `kernel/` or `userspace/`. Tasks that straddle the boundary split their code along it so the pure part is host-testable; no task may defer this split to "later".

### SOLID and module boundaries

- **Single Responsibility.** Modules under `userspace/display_server/src/` each own one concern: `fb.rs` → framebuffer writes, `surface.rs` → surface state machine, `compose.rs` → composition, `client.rs` → per-client protocol state, `input.rs` → input dispatcher + grab hook, `layout/` → layout policy, `control.rs` → control socket. No module accesses another's internal state directly; cross-module data flows only through typed function calls or trait objects.
- **Open/Closed and Dependency Inversion.** Public extension seams are named traits — `LayoutPolicy` (A.7/E.1), `FramebufferOwner` (C.2), `InputSource` (D.3), `CursorRenderer` (E.3) — and the composer, dispatcher, and tests consume them by trait, not by concrete type. New layouts, framebuffer backends, input sources, or cursor renderers land by implementing the trait, not by editing callers.
- **Interface Segregation.** Client-facing protocol exposes surfaces + input delivery only. Grab-hook, control-socket, layout-policy, and framebuffer-owner APIs are not on the client surface area.
- **Liskov Substitution.** Every impl of a trait defined here passes the shared contract-test suite for that trait (see the Contract row above). Impls that need escape hatches document the exact invariants they relax.

### DRY

- Protocol message types, opcodes, and binary layouts live once in `kernel-core::display::protocol` (A.0) and are consumed by both `display_server` and every client library. No protocol type is declared twice across the workspace; searching for a message's name must return exactly one definition.
- Input event types (`KeyEvent`, `PointerEvent`, `ModifierState`) live once in `kernel-core::input::events` and are shared by `kbd_server`, `mouse_server`, and `display_server`.
- `*_server` startup boilerplate (endpoint creation + registry registration + IRQ notification + standard panic handler) is factored into `syscall-lib` helpers where duplication crosses two sites; new duplication discovered during implementation is consolidated in the same PR, not deferred.

### Error discipline

- Non-test code contains no `.unwrap()`, `.expect()`, `panic!()`, `todo!()`, or `unreachable!()` outside of documented fail-fast initialization points. Every such site carries an inline comment naming the audited reason it is safe; `grep`-level review must be able to find and justify every occurrence.
- Every module boundary returns typed `Result<T, NamedError>` with a named error enum per subsystem (e.g. `FbError`, `SurfaceError`, `ProtocolError`). Error variants are data, not stringly-typed; callers can match and recover.
- No silent fallbacks: a fallback path always emits a structured log event naming the error it is recovering from.

### Observability

- `display_server`, `kbd_server`, and `mouse_server` emit structured log events keyed by subsystem (`fb`, `surface`, `compose`, `client`, `input`, `layout`, `control`, `kbd`, `mouse`). No ad-hoc `println!` or raw stderr writes outside of test-only debug paths.
- `display_server` maintains a rolling window of frame composition times and exposes a `frame-stats` control-socket verb returning the last N samples, giving the future animation engine (Phase 57c) and regression tests an observable pacing signal.

### Concurrency model

- `display_server` runs a **single-threaded event loop** multiplexing the frame-tick notification (B.3), the input endpoint (D.3), the client listening socket and per-client sockets (C.5), and the control socket (E.4). No worker threads in Phase 56; any future move to threads is deliberate and tracked as a later task. This eliminates an entire class of data-race bugs at the cost of no loss of throughput at Phase 56 workloads.

### Resource bounds

- Per-client surface count, per-client in-flight buffer count, and per-client outbound event-queue depth each carry a named high-water mark. Exceeding a bound closes the offending client's connection with a named reason; it never blocks the compositor or other clients. Defaults are recorded in the learning doc (H.1) and may be revised as real clients land.

---

## Track A — Architecture and Protocol Design

### A.0 — Shared protocol module in `kernel-core`

**Files:**
- `kernel-core/src/display/mod.rs` (new)
- `kernel-core/src/display/protocol.rs` (new)
- `kernel-core/src/input/mod.rs` (new or extended)
- `kernel-core/src/input/events.rs` (new)

**Symbol:** `ClientMessage`, `ServerMessage`, `ControlCommand`, `ControlEvent`, `SurfaceRole`, `KeyEvent`, `PointerEvent`, `ModifierState`, `encode`, `decode`
**Why it matters:** The client protocol (A.3), input event protocol (A.4), and control-socket protocol (A.8) all define message types that will be consumed by `display_server` and every client library. Declaring these types once, in `kernel-core`, is the DRY discipline for this phase and makes the codec host-testable in isolation. Without it, `display_server` and each client library will grow parallel definitions that drift.

**Acceptance:**
- [ ] Tests commit first (failing) and pass after implementation lands — evidence is in `git log --follow kernel-core/src/display/protocol.rs kernel-core/src/input/events.rs`
- [ ] All Phase 56 protocol message types, opcodes, and binary layouts are declared in `kernel-core::display::protocol` and `kernel-core::input::events`; no declaration is duplicated elsewhere in the workspace (a repo-wide grep proves this before closing the task)
- [ ] `encode` writes into a caller-supplied `&mut [u8]` and returns bytes-written; `decode` consumes from `&[u8]` and returns a typed `Result<(Message, bytes_consumed), ProtocolError>`; neither allocates on the hot path
- [ ] Per-variant unit round-trip tests exist for every message type
- [ ] A `proptest`-based round-trip test exists per message family (client, server, control-command, control-event, key, pointer) and proves `decode(encode(msg)) == msg` for arbitrary valid messages
- [ ] A corrupted-framing property test feeds arbitrary `&[u8]` into `decode` and asserts the decoder returns a typed `ProtocolError` without panicking, without infinite loops, and without unbounded allocation
- [ ] Visibility is tight: `kernel-core::display::protocol` and `kernel-core::input::events` are the only public surfaces; submodules for codec internals are `pub(crate)` or private
- [ ] No new external crate dependencies are added to `kernel-core` beyond what Phase 43c already enables for `proptest` in test builds

### A.1 — Adopt the four Goal-A design decisions as Phase 56 contract points

**File:** `docs/roadmap/56-display-and-input-architecture.md`
**Symbol:** `Goal-A contract points` (new subsection)
**Why it matters:** `docs/appendix/gui/tiling-compositor-path.md` identifies four design decisions that must be built into Phase 56 so a later tiling-first compositor (Phase 56b/57 area) does not require protocol rework. Without a task that explicitly adopts them, later implementation can quietly drop one of the four and force a breaking protocol change to recover.

**Acceptance:**
- [ ] The Phase 56 design doc gains a `Goal-A contract points` subsection that names the four decisions verbatim from `docs/appendix/gui/tiling-compositor-path.md`: (1) swappable layout module from day one, (2) keybind grab hook keyed on modifier sets, (3) layer-shell-equivalent surface role in the protocol, (4) control socket as a first-class part of the protocol
- [ ] Each decision carries a forward link to the task in this doc that delivers it (A.7 → layout contract, A.5 → grab hook, A.6 → layer-shell role, A.8 → control socket — wiring cross-checked by A.9 / H.1)
- [ ] The subsection explicitly records that the task doc's tiling-first *implementation* (layout engine, chord engine, workspace state machine, native bar/launcher clients) is **out of scope** for Phase 56 and lives in the proposed Phase 56b/57 area
- [ ] The subsection cross-links `docs/appendix/gui/tiling-compositor-path.md` and `docs/appendix/gui/wayland-gap-analysis.md` so Wayland-adjacent readers see the scope boundary

### A.2 — Service topology and ownership boundaries

**File:** `docs/56-display-and-input-architecture.md` (learning doc, drafted in H.1; placeholder stub acceptable for A.2 completion)
**Symbol:** `Service topology` (new section)
**Why it matters:** A graphical stack that never names its processes, endpoints, and capabilities cannot be supervised or audited. Pinning the topology before implementation prevents "one big userspace blob" and prevents the kernel from quietly regaining presentation responsibility later.

**Acceptance:**
- [ ] `display_server` is named as the sole userspace owner of the primary framebuffer and is identified as the single arbiter of surface composition and input focus
- [ ] `kbd_server` is confirmed to remain the raw keyboard source (scancode → keycode + modifier translation lives here) and is redefined to publish *key events* to `display_server` via a typed event endpoint rather than polled scancodes — see D.1
- [ ] A new `mouse_server` is named as the sole source of mouse events (motion, buttons, wheel); it shares the same dispatch endpoint shape as `kbd_server` — see D.2
- [ ] The document records which capability each service holds (`display_server` holds the framebuffer grant + vblank notification; input services hold their IRQ notification and a send-cap to `display_server`'s input endpoint)
- [ ] A process-level diagram (Mermaid) shows data flow: kbd/mouse → display_server → clients for output, clients → display_server for surface submit, control-socket clients ↔ display_server for commands/events

### A.3 — Client protocol wire format

**File:** `docs/56-display-and-input-architecture.md` (learning doc)
**Symbol:** `Client protocol wire format` (new section)
**Why it matters:** The client protocol is the long-term shape of the GUI stack more than any single demo. Writing the wire format down before coding prevents clients from each negotiating ad-hoc.

**Acceptance:**
- [ ] The transport is named: AF_UNIX stream socket for control/event messages; page-grant buffer transport (Phase 50) for surface pixel data
- [ ] The document enumerates the client→server messages needed to meet Phase 56 acceptance criteria: `Hello`, `CreateSurface`, `AttachBuffer`, `DamageSurface`, `CommitSurface`, `DestroySurface`, `SetSurfaceRole`, plus any minimum needed for focus acknowledgement
- [ ] The document enumerates the server→client messages: `SurfaceConfigured`, `FocusIn`, `FocusOut`, `KeyEvent`, `PointerEvent`, `BufferReleased`, `SurfaceDestroyed`
- [ ] Each message carries an exact field list with types and byte layout (`#[repr(C)]` or a small binary framing; no JSON on the pixel-adjacent path)
- [ ] Error handling is specified: unknown opcode closes the connection with a named reason; version negotiation happens in `Hello`
- [ ] The document explicitly calls out what is **not** in scope: subcompositors, viewporter, fractional scaling, output-hotplug, drag-and-drop, clipboard, xdg-foreign
- [ ] The format is versioned: `Hello` carries a `protocol_version: u32`, and mismatch closes the connection with a named error
- [ ] Wire-format types and their codec are implemented in `kernel-core::display::protocol` (A.0), not in `display_server`; the server and every client re-export from there
- [ ] Every message documented here has a corresponding A.0 codec test (per-variant round-trip, property round-trip, corrupted-framing)
- [ ] Resource bounds are documented inline with the protocol: max pending-attach buffers per surface, max surfaces per client, max outbound event queue — specific numeric defaults chosen and recorded

### A.4 — Input event protocol

**File:** `docs/56-display-and-input-architecture.md`
**Symbol:** `Input event protocol` (new section)
**Why it matters:** A GUI stack without a real key-event + modifier model cannot support chorded keybindings, text input, or focus rules. Scancodes alone are not enough.

**Acceptance:**
- [ ] Key events carry: keycode (hardware-neutral), key symbol (post-keymap), modifier state bitmask (`MOD_SHIFT`, `MOD_CTRL`, `MOD_ALT`, `MOD_SUPER`, `MOD_CAPS`, `MOD_NUM`), event kind (`KeyDown` / `KeyUp` / `KeyRepeat`), timestamp
- [ ] Modifier latch and lock state (`shift-lock`, `caps-lock`, `num-lock`) is tracked inside `kbd_server` and reflected in the modifier bitmask; clients never have to reconstruct it from raw events
- [ ] Pointer events carry: motion dx/dy (relative) and absolute x/y when available, button index + `PointerButton::{Down,Up}`, wheel axis + delta, timestamp, modifier state at event time
- [ ] Focus events (`FocusIn`, `FocusOut`) carry the window/surface id receiving focus, so clients can drive IME / repaint state without races
- [ ] The document explicitly names the keymap baseline: US QWERTY is mandatory; non-US layouts are deferred to Phase 57 or later and listed under "Deferred Until Later" in the learning doc
- [ ] The document explicitly names what pointer features are in scope (motion, 3 buttons + wheel) and what is deferred (precise touchpad gestures, tablet/pen input, touch)
- [ ] Event types and their codec live in `kernel-core::input::events` (A.0); `kbd_server`, `mouse_server`, and `display_server` all re-export from there
- [ ] Codec round-trip and corrupted-input property tests for key and pointer events are part of A.0's acceptance

### A.5 — Keybind grab-hook semantics (Goal-A decision 2)

**File:** `docs/56-display-and-input-architecture.md`
**Symbol:** `Keybind grab hook` (new subsection)
**Why it matters:** Mod-key chords are the entire tiling UX. If they have to be implemented as window-focus tricks later, the integration gets fragile. A first-class grab hook that swallows modifier+key before clients see it makes the chord engine a thin addition, not a protocol change.

**Acceptance:**
- [ ] The hook is defined: `display_server` maintains a small table of `(modifier_mask, keycode) → action` entries; when a `KeyEvent` matches, the event is **not** forwarded to the focused client — it is delivered only to `display_server`'s internal handler
- [ ] Matching uses the modifier bitmask from A.4 with mask equality (not "at least these modifiers") so chords like `SUPER+SHIFT+1` are distinguishable from `SUPER+1`
- [ ] `display_server` exposes two internal APIs: `register_bind(mask, keycode, handler)` and `unregister_bind(mask, keycode)` — used later by the control socket (E.4) and by unit tests; no direct client-facing API is exposed in Phase 56
- [ ] The hook evaluates *before* focus routing in the input dispatcher, and the event is dropped for clients regardless of which client is focused
- [ ] The Phase 56 learning doc records that the Phase 56 deliverable is the **hook mechanism only**; the keybind *chord engine / default bindings / config reload* ship in Phase 56b
- [ ] A regression test (see G.2) demonstrates that a registered bind swallows the key from the focused client and only the server-side handler fires

### A.6 — Layer-shell-equivalent surface roles (Goal-A decision 3)

**File:** `docs/56-display-and-input-architecture.md`
**Symbol:** `Surface roles` (new subsection)
**Why it matters:** Status bars, launchers, lockscreens, and notifications all need to render above or below normal windows with reserved screen space (exclusive zones). Without a layer-shell-equivalent role on day one, every one of those clients becomes a protocol hack.

**Acceptance:**
- [ ] The protocol defines at least three surface roles: `Toplevel` (normal application window), `Layer` (anchored overlay; Phase 56 is the layer-shell equivalent), and `Cursor` (pointer image). Additional roles may be declared for later phases but are not required to be implemented
- [ ] `Layer` surfaces carry: `layer: {Background, Bottom, Top, Overlay}` ordering, anchor edges (`top`, `bottom`, `left`, `right`, `center`), optional exclusive-zone (pixels reserved from tiled/toplevel surfaces), keyboard interactivity flag (`none`, `on_demand`, `exclusive`)
- [ ] `Layer` surfaces with exclusive zones shrink the usable area for `Toplevel` surfaces; the composer consults an exclusive-zone rectangle per output
- [ ] Keyboard interactivity mode is enforced: `none` never receives key events, `on_demand` receives events only when focused via input routing, `exclusive` claims keyboard focus while the surface is mapped
- [ ] The learning doc explicitly notes: Phase 56 ships the *role surface* and *anchor/exclusive-zone semantics*, not a bar/launcher/lockscreen binary. Client implementations live in Phase 57b
- [ ] A regression test (see G.3) creates a `Layer` surface anchored top with a 24-pixel exclusive zone and confirms that a concurrent `Toplevel` surface is laid out below the reserved band

### A.7 — Swappable layout module contract (Goal-A decision 1)

**File:** `docs/56-display-and-input-architecture.md`
**Symbol:** `Layout module contract` (new subsection)
**Why it matters:** If Phase 56 bakes "clients are floating with a titlebar" into the core, the later tiling-first compositor has to be a fork, not a module swap. A thin layout trait on day one keeps the tiling work additive.

**Acceptance:**
- [ ] The document defines a `LayoutPolicy` trait (Rust-level contract) consumed by `display_server` with at least: `fn arrange(&mut self, toplevels: &[SurfaceRef], output: OutputGeometry, exclusive_zones: &[Rect]) -> Vec<(SurfaceRef, Rect)>`, `fn on_surface_added(&mut self, surface: SurfaceRef)`, `fn on_surface_removed(&mut self, surface: SurfaceRef)`, `fn on_focus_changed(&mut self, surface: Option<SurfaceRef>)`
- [ ] `display_server` holds the current `LayoutPolicy` as a `Box<dyn LayoutPolicy>` (or equivalent generic seam) that is swappable at service startup; no module outside `display_server` reaches into toplevel geometry directly
- [ ] The Phase 56 deliverable is the *trait plus one simple default*: a `FloatingLayout` that places new toplevels at an output-centered default size. The tiling/dwindle/manual layouts are Phase 56b
- [ ] Exclusive zones from `Layer` surfaces (A.6) are passed to `LayoutPolicy::arrange` so later tiling layouts will not overlap the bar
- [ ] The learning doc cross-references `docs/appendix/gui/tiling-compositor-path.md` § Layout for the target set of future layouts

### A.8 — Control-socket protocol (Goal-A decision 4)

**File:** `docs/56-display-and-input-architecture.md`
**Symbol:** `Control socket protocol` (new subsection)
**Why it matters:** `hyprctl`-style tooling and the eventual native bar/launcher clients both depend on a command/event channel that is **not** the graphical client protocol. Adding it later means clients grow their own ad-hoc control planes.

**Acceptance:**
- [ ] The control socket is a separate AF_UNIX stream endpoint (distinct from the graphical client protocol in A.3); the endpoint path is documented (e.g. `/run/m3os/display-server.sock` — the chosen path is recorded in the learning doc)
- [ ] The wire format is a small line-delimited binary or JSON framing; the choice is recorded in the learning doc with rationale
- [ ] Phase 56 implements a minimum verb set sufficient to validate the protocol: `version`, `list-surfaces`, `focus <surface-id>`, `register-bind <mask> <keycode>`, `unregister-bind <mask> <keycode>`, `subscribe <event-kind>`. The richer `hyprctl`-style verbs (workspaces, layouts, gaps, animations) are Phase 56b
- [ ] Events are emitted on subscribed streams: `SurfaceCreated`, `SurfaceDestroyed`, `FocusChanged`, `BindTriggered`. Additional events are additive
- [ ] Authentication / ACL scope: Phase 56 restricts the socket to the owning user via filesystem permissions; richer ACLs are deferred
- [ ] The learning doc notes that the *bar/launcher/statusd client implementations* consuming this socket ship in Phase 57b
- [ ] A regression test (see G.4) uses a small `m3ctl` client to round-trip `version` and `list-surfaces` and to receive a `SurfaceCreated` event after a client surface is created

### A.9 — Verify evaluation gate checks before closing the phase

**File:** `docs/roadmap/56-display-and-input-architecture.md`
**Symbol:** `Evaluation Gate` (existing table — verification task)
**Why it matters:** The design doc defines four evaluation gates (graphics bring-up, service model, hardware/input, buffer transport). Without an explicit verification task the gates are likely to be skipped.

**Acceptance:**
- [ ] Graphics bring-up baseline: confirm Phase 47 + the kernel framebuffer handoff work end-to-end on the Phase 55 reference targets, and that the framebuffer-ownership transfer in B.1 did not regress Phase 47's single-client graphics path
- [ ] Service-model baseline: confirm Phase 46/51 supervision is wired to `display_server`, `kbd_server`, and `mouse_server` (see F.1)
- [ ] Hardware/input baseline: confirm that the chosen mouse path (B.2) exists on the supported Phase 55 targets and in the default QEMU configuration
- [ ] Buffer-transport baseline: confirm that Phase 50's page-grant transport is reachable from a userspace client process and can back a `wl_shm`-equivalent buffer pool (see B.4)
- [ ] The four design decisions from A.1 are all delivered (A.5/A.6/A.7/A.8) and have passing validation tests (G.2/G.3/G.4)
- [ ] Gate verification results are recorded in the Phase 56 learning doc (see H.1)

---

## Track B — Kernel Substrate for Ownership Transfer

### B.1 — Transfer framebuffer ownership from kernel to `display_server`

**Files:**
- `kernel/src/fb/mod.rs`
- `kernel/src/main.rs`
- `userspace/display_server/src/fb.rs` (new)

**Symbol:** `acquire_framebuffer`, `release_framebuffer`, `FB_OWNER`
**Why it matters:** Today `kernel/src/fb` is the unconditional presentation path for kernel log output, panics, and the Phase 9 console. For `display_server` to own presentation, the kernel must stop writing to the framebuffer *except* under well-defined failover conditions (panic, pre-init, or after `display_server` has voluntarily released it). Without this, the kernel and the compositor race over pixels.

**Acceptance:**
- [ ] A `sys_fb_acquire(flags)` syscall (or capability-gated IPC) lets a privileged userspace process take exclusive ownership of the framebuffer; it returns a page-grant capability covering the framebuffer region and metadata (resolution, stride, pixel format)
- [ ] Concurrent acquisition attempts return a distinct `EBUSY`-shaped error; the kernel serves at most one live framebuffer owner at a time
- [ ] While `display_server` holds the framebuffer, the kernel framebuffer console is suspended: no routine kernel log output is written to pixels
- [ ] Panic path still writes to the framebuffer (the TCB cannot rely on userspace during a panic) — this behavior is documented in the learning doc
- [ ] `display_server` may call `sys_fb_release()` to return ownership (used on graceful shutdown and on crash-handler-driven failover in F.3)
- [ ] An integration test confirms: (a) kernel log output is routed only to serial while `display_server` owns the framebuffer, (b) on `display_server` exit without release the kernel reclaims pixel output (see F.2)
- [ ] The pre-existing Phase 47 DOOM graphical path is either retired or migrated to acquire through the new API; no code path writes to raw framebuffer bytes without going through `sys_fb_acquire`

### B.2 — Mouse input path (PS/2 AUX)

**Files:**
- `kernel/src/arch/x86_64/ps2.rs` (new or extended)
- `kernel/src/arch/x86_64/interrupts.rs`
- `kernel-core/src/input/mouse.rs` (new)

**Symbol:** `Ps2MouseDecoder`, `mouse_irq_handler`, `MOUSE_EVENT_RING`
**Why it matters:** The Phase 56 evaluation gate requires a working mouse path. PS/2 AUX (IRQ12) is the minimum-viable path that works in the QEMU default config and on every x86 reference target without pulling USB HID into Phase 56. USB HID breadth is deferred per the design doc.

**Acceptance:**
- [ ] The 8042 PS/2 controller is initialized with the auxiliary (mouse) port enabled: `0xD4`-prefixed command bytes send the `Enable Streaming` command (`0xF4`) to the mouse
- [ ] Tests commit before implementation for the decoder
- [ ] `Ps2MouseDecoder` lives in `kernel-core` as pure-logic state: feed bytes, emit `MouseEvent { dx, dy, buttons, wheel }` frames; at least 4 host tests cover the 3-byte standard packet, the 4-byte IntelliMouse wheel extension enablement handshake, overflow-bit handling, and out-of-sync recovery
- [ ] A `proptest` property test drives arbitrary `&[u8]` streams into the decoder and asserts: no panic, bounded internal state size, recovery after any invalid prefix within a bounded number of bytes
- [ ] IRQ12 ingests bytes into a per-device lockless ring (or a small spinlocked ring) under the Phase 52c "no allocation in ISR" rule; no IPC is issued from inside the IRQ handler
- [ ] A kernel-side notification object fires on non-empty ring, allowing `mouse_server` (D.2) to wake and drain
- [ ] A `sys_read_mouse_packet` (or equivalent) syscall returns the next decoded `MouseEvent` to `mouse_server`; the kernel does not deliver events to any client other than the registered mouse service
- [ ] IntelliMouse (wheel) detection handshake is performed; on failure the driver falls back to the 3-byte packet model with wheel delta 0
- [ ] The existing keyboard path (`kbd_server` + IRQ1) is not regressed; the learning doc documents which IRQ vectors are owned by which userspace service

### B.3 — Vblank / frame-tick notification source

**Files:**
- `kernel/src/time/mod.rs` or `kernel/src/fb/mod.rs`
- `kernel-core/src/display/frame_tick.rs` (new or extended)

**Symbol:** `FRAME_TICK_NOTIFY`, `frame_tick_hz`
**Why it matters:** Software composition needs a periodic signal to know when to redraw. Without a frame tick, `display_server` either busy-loops or never redraws on a schedule. A real vblank source requires DRM/KMS (deferred past Phase 56); a timer-driven tick is the Phase 56 substitute and is also what the tiling-compositor-path document assumes for the animation engine later.

**Acceptance:**
- [ ] A kernel-owned periodic tick at a configurable rate (default 60 Hz) signals a notification object that `display_server` can wait on
- [ ] The tick uses the existing timer infrastructure (HPET/LAPIC timer per Phase 35) and does not require new hardware support
- [ ] Tick rate is discoverable from userspace (metadata on the notification or a read-only syscall returning `frame_tick_hz`) so `display_server` can adapt animation budgets in later phases
- [ ] Overrun behavior is documented: if `display_server` does not wait fast enough, missed ticks coalesce (no queue growth)
- [ ] The learning doc records that this is a *frame-pacing tick*, not a real vblank, and links forward to a later phase for the hardware vblank story — this is called out as an open question in `docs/appendix/gui/tiling-compositor-path.md` § Risks

### B.4 — Cross-process shared-buffer transport for surfaces

**Files:**
- `kernel/src/ipc/grant.rs` (existing; audit)
- `kernel/src/mm/mmap.rs` (existing; audit)
- `userspace/syscall-lib/src/surface_buffer.rs` (new)

**Symbol:** `SurfaceBuffer`, `attach_client_buffer`, `grant_surface_pages`
**Why it matters:** Clients submit pixel data by exposing pages to `display_server`. Phase 50's page-grant transport exists but has not been exercised for a *client-owned* buffer passed to a different userspace server (distinct from server-to-server capability grants). Before building the surface state machine, confirm the transport primitive is real and has a clean userspace API.

**Acceptance:**
- [ ] A userspace helper in `syscall-lib` lets a client allocate a refcounted shared-memory region and produce a page-grant capability that can be sent over AF_UNIX (via existing SCM_RIGHTS-equivalent capability transfer)
- [ ] `display_server` can accept the grant and map the same physical pages read-only into its address space; writes by the client become visible to `display_server` on the next `CommitSurface` (subject to the composer's latching rules — see C.4)
- [ ] A buffer lifetime model is documented: the client must not modify the buffer between `CommitSurface` and `BufferReleased`; `display_server` emits `BufferReleased` when the buffer is no longer sampled
- [ ] At least one allocation test (in `kernel-core` or via an integration harness) proves a client process and `display_server` can observe the same pixel data without copies
- [ ] Lifetime invariants are codified as unit tests on a pure-logic refcount state machine in `kernel-core` (attach → commit → release → detach, with all orderings including abnormal client exit); these are tests-first
- [ ] Page-grant leak behavior is defined and tested: when a client dies without `DestroySurface`, the kernel drops its refcount and `display_server` sees `SurfaceDestroyed` + `BufferReleased` within the next dispatch cycle — an integration test exercises this by killing a test client mid-commit
- [ ] The transport is explicitly **not** a DMA-BUF or GPU-aware path; the learning doc documents this alignment with `docs/appendix/gui/wayland-gap-analysis.md` § 1

---

## Track C — Display Service (Compositor Core)

### C.1 — Create `userspace/display_server` crate scaffolding

**Files:**
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`bins` array in `build_userspace`, ~line 141)
- `kernel/src/fs/ramdisk.rs` (include + `BIN_ENTRIES` tuple)
- `userspace/display_server/Cargo.toml` (new)
- `userspace/display_server/src/main.rs` (new)
- `xtask/src/main.rs` (`populate_ext2_files` — add `display_server.conf`)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS` fallback list)

**Symbol:** `program_main` (in `main.rs`)
**Why it matters:** Per the project "Adding a New Userspace Binary" convention, a new userspace binary requires four coordinated changes. Omitting any one leaves the service absent from the ramdisk or absent from init's boot list.

**Acceptance:**
- [ ] `userspace/display_server` builds with `needs_alloc = true` in the xtask `bins` table, declares `syscall-lib` with the `alloc` feature, and installs `BrkAllocator` as the global allocator
- [ ] The binary is embedded in the ramdisk via `include_bytes!` and `BIN_ENTRIES` in `kernel/src/fs/ramdisk.rs`
- [ ] A `display_server.conf` entry is added to the ext2 data disk builder in `xtask/src/main.rs::populate_ext2_files`, and the service name is listed in `userspace/init/src/main.rs::KNOWN_CONFIGS`
- [ ] After `cargo xtask clean && cargo xtask run`, `display_server` appears as a running process under `init` supervision
- [ ] The scaffolded `program_main` writes a banner to stdout, creates an IPC endpoint, and registers itself in the service registry as `"display"` — no graphical behavior yet

### C.2 — Framebuffer acquisition and exclusive presentation

**Files:**
- `userspace/display_server/src/fb.rs` (new)
- `userspace/display_server/src/main.rs`
- `kernel-core/src/display/fb_owner.rs` (new — trait + test double)

**Symbol:** `FramebufferOwner` (trait), `KernelFramebufferOwner` (real impl), `RecordingFramebufferOwner` (test double), `acquire_primary_output`
**Why it matters:** The whole phase rests on `display_server` owning the primary framebuffer. This task wires the B.1 acquisition syscall to a typed owner inside the service, and — critically — exposes the owner as a **trait** so the composer (C.4) and every compose-related test can run against a recording test double on the host. Without the trait seam, compose math can only be exercised in QEMU.

**Acceptance:**
- [ ] Tests commit before implementation (contract tests for `FramebufferOwner` exist and fail first, then pass)
- [ ] `FramebufferOwner` is a trait in `kernel-core::display::fb_owner` with methods `metadata() -> FbMetadata`, `write_pixels(rect, src, src_stride) -> Result<(), FbError>`, and `present() -> Result<(), FbError>` (the flush/commit point if any backend needs it); both `KernelFramebufferOwner` (real) and `RecordingFramebufferOwner` (test double that stores damage rects and pixel hashes) implement it
- [ ] A contract-test harness in `kernel-core` runs an identical test suite against both impls: writes inside bounds succeed, writes clipped to bounds succeed, writes fully out of bounds are a named `FbError::OutOfBounds` error without corrupting state, repeated writes to the same rect do not leak resources
- [ ] `KernelFramebufferOwner` caches the framebuffer metadata (width, height, stride, pixel format) and uses volatile writes with explicit bounds checks that clip `rect` to the framebuffer extents (preventing OOB writes even on malformed damage input)
- [ ] On startup, `display_server` calls `sys_fb_acquire` exactly once; if it returns `EBUSY`, the service retries with bounded backoff up to a configured limit and then exits nonzero with a named error reason
- [ ] Initial presentation on startup draws a known background color across the full framebuffer so that the ownership handoff is visually unambiguous during bring-up and manual testing
- [ ] A shutdown path calls `sys_fb_release()` on normal exit; on panic the kernel reclaims ownership (validated in F.2)
- [ ] An integration smoke test confirms `display_server` can fill the framebuffer with a solid color on startup and clear it on shutdown using `KernelFramebufferOwner`

### C.3 — Surface state machine

**Files:**
- `kernel-core/src/display/surface.rs` (new — pure-logic state machine)
- `userspace/display_server/src/surface.rs` (new — thin wiring over the kernel-core core)

**Symbol:** `SurfaceStateMachine`, `SurfaceId`, `BufferSlot`, `SurfaceEvent`, `commit`
**Why it matters:** A surface is the compositor's atomic unit of client-provided pixels. Without a state machine that distinguishes *attached*, *committed*, *sampled*, and *released*, tearing, use-after-free, and double-commit bugs become structural rather than testable. Keeping the state machine as pure logic in `kernel-core` makes these invariants verifiable on the host.

**Acceptance:**
- [ ] Tests commit before implementation; the initial test-only commit defines the invariants below as failing tests before any impl lands
- [ ] `SurfaceStateMachine` lives in `kernel-core::display::surface` as a pure-logic type that consumes input events (`AttachBuffer`, `DamageSurface`, `CommitSurface`, `DestroySurface`) and emits output effects (`ReleaseBuffer(BufferSlot)`, `EmitDamage(Rect)`, `NotifyLayoutRemoved`)
- [ ] `SurfaceStateMachine` tracks: unique id, role (`Toplevel` | `Layer` | `Cursor`), current committed buffer slot, pending buffer slot, pending damage rectangles (with resource bound on rect-count; overflow coalesces), geometry, focus state
- [ ] Unit tests cover at minimum: commit-with-no-attach is a typed error, double-attach replaces the pending slot without releasing, double-commit discards the older pending, damage accumulates across `DamageSurface` calls, destroy releases the current buffer exactly once and emits `NotifyLayoutRemoved`, destroy of a surface with a pending-but-uncommitted buffer releases both slots
- [ ] A `proptest` property test drives arbitrary event sequences and asserts: at most one `ReleaseBuffer` per buffer-slot ever emitted (no double-free), no `ReleaseBuffer` for a slot never attached, dead surfaces accept no further events except being queried
- [ ] `userspace/display_server/src/surface.rs` is a thin shim that maps protocol messages (A.3) to state-machine events and wires effects to the client and layout modules; no state logic lives in the userspace shim

### C.4 — Damage-tracked software composer

**Files:**
- `userspace/display_server/src/compose.rs` (new)
- `kernel-core/src/display/compose.rs` (new; pure-logic blending + damage math)

**Symbol:** `Composer`, `compose_frame`, `accumulate_damage`, `blend_surface`
**Why it matters:** A naive compositor that redraws the full framebuffer every tick burns CPU for no visible benefit. Damage tracking is the difference between a software composer that is comfortable at 1080p60 and one that is not — see the bandwidth table in `docs/appendix/gui/tiling-compositor-path.md` § Composition cost.

**Acceptance:**
- [ ] On each frame tick, the composer walks surfaces in layer order (`Background < Bottom < Toplevel < Top < Overlay < Cursor`) and blits damaged regions only
- [ ] Surface geometry is obtained from the `LayoutPolicy` for `Toplevel` surfaces; from the anchor/exclusive-zone logic for `Layer` surfaces; from pointer position for `Cursor`
- [ ] Damage rectangles are clipped to the output bounds and to the visible region of the surface
- [ ] Alpha blending is supported for `Cursor` and `Layer` surfaces; `Toplevel` surfaces are assumed opaque in Phase 56 (transparency for toplevels is deferred)
- [ ] If no surface reported damage on a tick, the composer performs no framebuffer writes (asserted by a test that runs the composer with `RecordingFramebufferOwner` and asserts zero writes)
- [ ] Tests commit first; unit tests in `kernel-core::display::compose` cover at minimum: (a) damage rectangle union/intersection math, (b) layer-order traversal returns surfaces in the documented order, (c) clip-to-output correctly rejects an off-screen surface, (d) an opaque toplevel fully covered by a higher-layer surface is skipped, (e) zero-damage tick yields zero framebuffer writes
- [ ] A `proptest` property test drives arbitrary `(surfaces, damage, output)` inputs and asserts: composed output exactly covers the union of (visible) damage rectangles clipped to the output — no pixels outside, no pixels inside the visible damage union skipped
- [ ] The composer consumes the `FramebufferOwner` trait (C.2) and the `LayoutPolicy` trait (A.7/E.1), never a concrete type; the same compose code runs against `RecordingFramebufferOwner` on the host and `KernelFramebufferOwner` in QEMU
- [ ] Software-only is explicit: no GL/GLES2 code paths; aligns with `docs/appendix/gui/wayland-gap-analysis.md` scope

### C.5 — Client connection handshake and event loop

**File:** `userspace/display_server/src/client.rs` (new)
**Symbol:** `Client`, `ClientId`, `handle_message`, `dispatch_event`
**Why it matters:** Clients must be able to connect, receive focus/input events, submit surfaces, and have a clean disconnect path. This task stitches Track A's protocol onto the C.1–C.4 machinery.

**Acceptance:**
- [ ] `display_server` listens on an AF_UNIX stream socket at a documented path (recorded in A.3 and H.1)
- [ ] A `Hello` handshake exchanges protocol version and capability flags; mismatched versions close the connection with a named reason
- [ ] Per-client state tracks: connection fd, subscribed event kinds, owned surfaces
- [ ] Client-to-server messages in A.3 dispatch to the surface state machine (C.3) and the layout policy (E.1)
- [ ] Server-to-client events in A.3 are serialized with backpressure: a slow client does not block other clients or the composer; when a per-client outbound queue overflows a named high-water mark, the server disconnects the client with a documented reason instead of blocking
- [ ] Client disconnect (explicit `Goodbye`, EOF on socket, or process exit) releases all surfaces owned by the client and notifies the layout policy
- [ ] At least two concurrent clients are supported (this is a Phase 56 acceptance-criterion precondition)
- [ ] Protocol framing is consumed through the A.0 codec exclusively; `client.rs` contains no hand-written field extraction
- [ ] A fuzz-style robustness test (driven by `proptest` over arbitrary `Vec<u8>` frames) feeds the client message handler and asserts: no panic, no allocation beyond a documented per-message budget, malformed messages produce a typed `ProtocolError` and a named-reason disconnect
- [ ] Per-client resource bounds (max surfaces, max in-flight buffers, outbound queue high-water mark) are enforced inside this module; exceeding a bound disconnects the offending client with a named reason and emits a structured log event — other clients and the composer are unaffected

---

## Track D — Input Services and Keybind-Grab Hook

### D.1 — Extend `kbd_server` to emit key events with modifier state

**Files:**
- `userspace/kbd_server/src/main.rs`
- `userspace/kbd_server/src/keymap.rs` (new)
- `kernel-core/src/input/keymap.rs` (new)

**Symbol:** `KeyEvent`, `ModifierState`, `translate_scancode`, `send_key_event`
**Why it matters:** The existing `kbd_server` returns raw scancodes one at a time in response to `KBD_READ` requests. That is sufficient for a line-oriented shell but insufficient for a GUI that needs per-key `Down`/`Up`/`Repeat` semantics, modifier latching, and chord detection. The keymap logic belongs in `kernel-core` so it is host-testable.

**Acceptance:**
- [ ] `kernel-core::input::keymap` translates AT-style set-1 scancodes (with 0xE0 prefixes, break codes, pause/print-screen sequences) into `(keycode, key_kind)` events
- [ ] A US QWERTY keymap layer maps keycodes to key symbols; non-US layouts are deferred
- [ ] Modifier tracking inside `kbd_server` maintains a `ModifierState` bitmask across events (`SHIFT`, `CTRL`, `ALT`, `SUPER`, `CAPS_LOCK`, `NUM_LOCK`) with correct latch/lock semantics
- [ ] Key repeat is generated by `kbd_server` on a configurable delay+rate (initial 500 ms / 30 Hz) and cancels when any key transitions or modifier changes
- [ ] Instead of (or alongside) the legacy `KBD_READ` label, `kbd_server` emits `KeyEvent` messages on a dedicated typed endpoint consumed by `display_server`; the cap-transfer handshake is established at service startup
- [ ] Legacy text-mode consumers (`ion`, the existing login path) continue to function — either via the legacy path kept intact, or via a small TTY-side shim that consumes `KeyEvent` and produces scancode-equivalent bytes for text consumers
- [ ] Tests commit before implementation; unit tests in `kernel-core::input::keymap` cover at least 5 keymap cases: plain letter, shifted letter, caps-lock interaction, extended-key (`0xE0 0x4B` → `ArrowLeft`), pause sequence
- [ ] A `proptest` property test feeds arbitrary `&[u8]` scancode streams into the decoder and asserts: no panic, progress is made on every well-formed prefix, recovery happens after any invalid prefix within a bounded number of bytes
- [ ] Modifier-state tracking is a pure-logic type in `kernel-core` with unit tests for every latch/lock transition (shift tap vs shift hold, caps-lock on/off, num-lock on/off, concurrent modifiers)

### D.2 — Create `mouse_server` userspace service

**Files:**
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`bins` array + `populate_ext2_files`)
- `kernel/src/fs/ramdisk.rs`
- `userspace/init/src/main.rs::KNOWN_CONFIGS`
- `userspace/mouse_server/Cargo.toml` (new)
- `userspace/mouse_server/src/main.rs` (new)

**Symbol:** `program_main`, `PointerEvent`, `send_pointer_event`
**Why it matters:** Mouse events need their own service for the same reason `kbd_server` exists: keep device drainage and event shaping out of the kernel, keep focus routing out of ring 0.

**Acceptance:**
- [ ] The new binary follows the "Adding a New Userspace Binary" convention (workspace member, xtask `bins` entry, ramdisk embedding, config entry)
- [ ] At startup, `mouse_server` creates an IRQ12 notification capability and a typed event endpoint for `display_server`
- [ ] The service loop waits on the notification, drains the kernel mouse-event ring via the B.2 syscall, and emits `PointerEvent` messages to `display_server`'s input endpoint
- [ ] Movement is delivered as *relative* deltas on PS/2; `display_server` is responsible for maintaining an absolute cursor position
- [ ] Button state is maintained inside `mouse_server` across packets so `PointerButton::{Down,Up}` edges are explicit
- [ ] Wheel delta is emitted only when the IntelliMouse extension is active
- [ ] The service registers in the service registry as `"mouse"`

### D.3 — Input dispatcher with focus-aware routing

**Files:**
- `kernel-core/src/input/dispatch.rs` (new — pure routing logic)
- `userspace/display_server/src/input.rs` (new — thin wiring)

**Symbol:** `InputDispatcher` (pure-logic type in `kernel-core`), `InputSource` (trait), `RouteDecision` (enum: `DeliverTo(SurfaceId)`, `Grab(BindId)`, `Drop`), `route_key_event`, `route_pointer_event`
**Why it matters:** Once events arrive at `display_server`, they must be routed by policy — not by accident. Focus rules, grab rules, and layer-shell keyboard-interactivity modes all live in one dispatcher so the policy is auditable in one place. Keeping the dispatcher as pure logic in `kernel-core` makes every routing decision host-testable without QEMU, and defining `InputSource` as a trait lets the dispatcher be driven by either real services or test doubles.

**Acceptance:**
- [ ] Tests commit before implementation
- [ ] `InputDispatcher` is a pure-logic type in `kernel-core::input::dispatch` that takes an input event plus the current compositor state (focused surface, active exclusive layer, pointer position, bind table reference) and returns a `RouteDecision` enum; it performs no I/O
- [ ] An `InputSource` trait lives in `kernel-core::input` and abstracts the service-side producer of `KeyEvent` / `PointerEvent`; the real `display_server` wires two impls (kbd, mouse), and tests substitute a scripted `MockInputSource`
- [ ] The decision order is tested: grab-hook match → `Grab(BindId)`; otherwise if an `exclusive` `Layer` is active → `DeliverTo(layer_surface)`; otherwise if a focused `Toplevel` or `on_demand` `Layer` exists → `DeliverTo(focused)`; otherwise → `Drop`
- [ ] Pointer routing is tested: hit-testing returns the correct surface for interior points, boundary points resolve deterministically (top-left-inclusive, bottom-right-exclusive or the reverse — pick one and test it), motion across a boundary emits `PointerLeave(old)` then `PointerEnter(new)` in order
- [ ] Click-to-focus is the Phase 56 default and is tested: a `PointerButton::Down` on a `Toplevel` surface moves keyboard focus to it unless an `exclusive` `Layer` is active
- [ ] Focus changes emit `FocusIn` / `FocusOut` effects (A.4) as ordered outputs of the decision, so the userspace shim forwards them without reordering
- [ ] At least 6 unit tests plus a `proptest` property test that drives arbitrary event sequences and asserts: no event is ever delivered to a destroyed surface; grab matches do not leak to clients even on interleaved key/pointer traffic; `PointerEnter`/`PointerLeave` always come in balanced pairs per surface

### D.4 — Keybind grab-hook implementation

**Files:**
- `kernel-core/src/input/bind_table.rs` (new — pure-logic bind matcher)
- `userspace/display_server/src/input.rs`

**Symbol:** `BindTable`, `BindId`, `BindKey(modifier_mask, keycode)`, `register_bind`, `unregister_bind`, `match_bind`, `GrabState`
**Why it matters:** A.5 specified the semantics; D.4 delivers them. Matching is pure logic and lives in `kernel-core` so it is unit-testable without wiring.

**Acceptance:**
- [ ] Tests commit before implementation
- [ ] `BindTable` lives in `kernel-core::input::bind_table`, is keyed by `BindKey(modifier_mask, keycode)`, and provides `register(BindKey) -> BindId`, `unregister(BindId)`, `match(modifier_mask, keycode) -> Option<BindId>`, all operating on pure data with no I/O
- [ ] Matching uses **exact mask equality** (not "at least these modifiers") so `SUPER+SHIFT+1` and `SUPER+1` are distinct; a unit test confirms this specifically
- [ ] `GrabState` tracks per-keycode grab presence so the dispatcher can suppress the matching `KeyUp` and any intervening `KeyRepeat` for a keycode whose `KeyDown` was grabbed — clients never see half a chord
- [ ] Unit tests cover: register → match → unregister → no-match; register two binds differing only in modifier mask, each matches only its exact mask; double-register returns a stable `BindId` or a typed error (pick one, document, and test); unregister of an unknown `BindId` is a typed error, not a panic; `KeyRepeat` and `KeyUp` for a grabbed keycode are suppressed until a `KeyUp` without an outstanding grab arrives
- [ ] `register_bind` and `unregister_bind` are callable only through the control socket (E.4) and through server-internal code; there is no direct client-protocol entry point in Phase 56
- [ ] On a match, the handler (in Phase 56 this is a small dispatch table — e.g. `focus-next`, `quit-focused` — used only by tests and by G.2's regression) runs on the dispatcher thread and no client sees the event
- [ ] On no match, the dispatcher (D.3) falls through to focus routing
- [ ] A regression test (G.2) validates that registering `MOD_SUPER + q` and pressing Super+Q produces a `BindTriggered` control-socket event and no `KeyEvent` at the focused client

---

## Track E — Layout Policy, Layer-Shell Surfaces, and Control Socket

### E.1 — `LayoutPolicy` trait and default floating layout

**Files:**
- `kernel-core/src/display/layout.rs` (new — trait + contract test harness + `FloatingLayout` + `StubLayout`)
- `userspace/display_server/src/layout/mod.rs` (new — re-export and wiring)

**Symbol:** `LayoutPolicy` (trait), `FloatingLayout`, `StubLayout`, `layout_contract_suite`, `arrange`
**Why it matters:** A.7 specified the contract; E.1 delivers the trait plus the minimum-viable default. The tiling-first engine lands later; Phase 56 just proves the seam works. A **shared contract-test suite** runs against every `LayoutPolicy` impl (present and future) so Liskov-substitutability is enforced by code, not by reviewer vigilance.

**Acceptance:**
- [ ] Tests commit before implementation; the contract suite is written against the trait before any impl lands
- [ ] The trait signature matches A.7's specification exactly and lives in `kernel-core::display::layout`
- [ ] A public `layout_contract_suite<P: LayoutPolicy>(construct: impl Fn() -> P)` runs an identical behavioral test suite against any impl; it is invoked once per impl in `kernel-core` tests (`FloatingLayout`, `StubLayout`) and will be invoked by the future tiling layout crate without modification
- [ ] The contract covers at minimum: empty-toplevel-list produces an empty arrangement; adding a toplevel produces exactly one rect inside the output minus exclusive zones; removing the most recently added toplevel returns the arrangement to its prior state; arrange is deterministic (identical inputs → identical outputs); no returned rect overlaps an exclusive zone unless the output cannot fit otherwise (documented degenerate case); focus changes do not change returned geometry for impls where they aren't supposed to (opt-in via a trait-level `focus_affects_geometry()` helper if needed)
- [ ] `FloatingLayout` places each new `Toplevel` at an output-centered default size with a small cascade offset; `StubLayout` returns rects from a pre-loaded script for test determinism
- [ ] Swappability is real: the policy is constructed once at startup through a named factory function; the compositor consumes `&mut dyn LayoutPolicy`, never a concrete type
- [ ] The contract suite is structured so that adding a new `LayoutPolicy` impl in Phase 56b (tiling) requires only a one-line registration, not a copy of the test suite

### E.2 — `Layer` surface role with anchors and exclusive zones

**File:** `userspace/display_server/src/surface.rs`
**Symbol:** `LayerRole`, `LayerConfig`, `compute_layer_geometry`
**Why it matters:** A.6 specified the role semantics; E.2 implements the geometry and event-routing plumbing.

**Acceptance:**
- [ ] `SetSurfaceRole` with role `Layer` accepts a `LayerConfig { layer, anchor, exclusive_zone, keyboard_interactivity, margin }` payload; the semantics match A.6 verbatim
- [ ] `compute_layer_geometry` derives the surface rectangle from the output geometry, the anchor edges, the surface's intrinsic size, and the margin
- [ ] Exclusive zones are collected per output and passed to the layout policy (E.1) on every `arrange` call
- [ ] Keyboard interactivity `exclusive` sets the active exclusive layer surface in the input dispatcher (D.3); at most one exclusive surface is active per seat at a time — a second `exclusive` attempt is rejected with a protocol error
- [ ] Layer ordering (`Background < Bottom < Toplevel-band < Top < Overlay`) is respected by the composer (C.4)
- [ ] At least 3 host tests cover: top-anchored exclusive zone shrinks the toplevel band, bottom-anchored zone shrinks from the opposite edge, conflicting `exclusive` keyboard claims resolve to a named error

### E.3 — `Cursor` surface role and pointer rendering

**Files:**
- `kernel-core/src/display/cursor.rs` (new — `CursorRenderer` trait + default arrow impl + damage math)
- `userspace/display_server/src/surface.rs`
- `userspace/display_server/src/compose.rs`

**Symbol:** `CursorRenderer` (trait), `DefaultArrowCursor`, `ClientCursor`, `CursorRole`, `cursor_damage`, `set_cursor_surface`
**Why it matters:** The pointer needs a bitmap that follows motion. In Phase 56 the cursor is a client-provided `Cursor`-role surface sampled at the current pointer position; providing the role is cheap, and without it every client reinvents cursor rendering. Exposing the cursor as a `CursorRenderer` trait with a default implementation keeps the seam open for future themed or scaled cursors and makes motion-damage math host-testable.

**Acceptance:**
- [ ] Tests commit before implementation
- [ ] `CursorRenderer` is a trait in `kernel-core::display::cursor` with methods `size() -> (w, h)`, `hotspot() -> (x, y)`, `sample(x, y) -> u32` (or equivalent blit helper); `DefaultArrowCursor` and `ClientCursor` both implement it
- [ ] A default cursor (a simple software-drawn arrow in `kernel-core`) is used when no client has set a cursor surface — this prevents an invisible pointer on a fresh boot
- [ ] `SetSurfaceRole` with role `Cursor` accepts a `CursorConfig { hotspot_x, hotspot_y }` and wires the surface as a `ClientCursor` impl
- [ ] `cursor_damage(prev_pos, prev_size, new_pos, new_size) -> SmallVec<Rect>` is a pure function in `kernel-core` returning the union of damage rectangles for a motion event; unit tests cover: stationary motion yields no rects, diagonal motion returns two disjoint rects when the cursor moves by more than its size, overlapping positions collapse to the bounding rect
- [ ] The composer samples the cursor surface at the current pointer position minus the hotspot, in the top-most layer, using the `CursorRenderer` trait
- [ ] A regression test confirms the cursor moves correctly across a motion event and that the damage rectangle math does not leave stale pixels

### E.4 — Control socket: endpoint, verbs, events

**Files:**
- `kernel-core/src/display/control.rs` (new — command/event codec + parser)
- `userspace/display_server/src/control.rs` (new — socket + dispatch wiring)
- `userspace/m3ctl/` (new minimal client — see G.4)

**Symbol:** `ControlCommand`, `ControlEvent`, `parse_command`, `encode_event`, `ControlServer`, `handle_command`, `emit_event`
**Why it matters:** A.8 specified the protocol; E.4 implements it. This is the seam the native bar/launcher clients (Phase 57b) will target. The parser lives in `kernel-core` so unknown-verb, malformed-framing, and verb-round-trip behavior can be unit-tested without a running compositor.

**Acceptance:**
- [ ] Tests commit before implementation
- [ ] `ControlCommand` and `ControlEvent` types and their parser / encoder live in `kernel-core::display::control` (alongside the other protocol types from A.0); the userspace `control.rs` contains no hand-written parsing
- [ ] Unit tests in `kernel-core` cover every verb in the minimum verb set: round-trip encode/parse for each; unknown-verb returns `ControlError::UnknownVerb(name)`; malformed framing returns `ControlError::MalformedFrame`; argument-count mismatches return `ControlError::BadArgs` with the expected count
- [ ] A `proptest` round-trip test covers arbitrary valid `ControlCommand` / `ControlEvent` values
- [ ] `display_server` opens a second AF_UNIX stream socket at the documented control-socket path from A.8; filesystem permissions restrict it to the owning user
- [ ] The minimum verb set from A.8 is implemented: `version`, `list-surfaces`, `focus <surface-id>`, `register-bind <mask> <keycode>`, `unregister-bind <mask> <keycode>`, `subscribe <event-kind>`, plus `frame-stats` (from the Engineering Discipline → Observability section)
- [ ] Events `SurfaceCreated`, `SurfaceDestroyed`, `FocusChanged`, `BindTriggered` are emitted on every subscribed stream
- [ ] An `UnknownCommand` error is returned for unrecognized verbs; the stream is not closed on unknown verbs (only on malformed framing)
- [ ] A minimal userspace `m3ctl` client binary is scaffolded in `userspace/m3ctl` (following the four-step new-binary convention) and implements at least `m3ctl version`, `m3ctl list-surfaces`, and `m3ctl frame-stats`; it is used by G.4's regression test
- [ ] The learning doc records that the control socket is **not** a Wayland adapter — it speaks only m3OS's native control language

---

## Track F — Session Integration, Supervision, and Recovery

### F.1 — Service manifests and supervision under `init`

**Files:**
- `userspace/init/src/main.rs`
- `xtask/src/main.rs::populate_ext2_files` (service conf files)

**Symbol:** `service_record`, `restart_policy`
**Why it matters:** The phase's service-model baseline (Phase 46/51) must actually supervise the graphical stack; otherwise a crash leaves the system without pixels.

**Acceptance:**
- [ ] `kbd_server`, `mouse_server`, and `display_server` all have service records with explicit startup order (`kbd_server` and `mouse_server` before `display_server`) and restart policies
- [ ] `display_server` has an `on-restart` policy that re-acquires the framebuffer via B.1 (retry with bounded backoff) and re-establishes the control socket
- [ ] Input services emit a one-time log on startup identifying which input endpoint they will target on `display_server` (useful for diagnosing reordering during the session bringup)
- [ ] The boot-log evidence that the three services are live at the expected point in boot is captured (e.g. a test harness reads the service-manager status)

### F.2 — Display-service crash recovery

**Files:**
- `userspace/display_server/src/main.rs`
- `kernel/src/fb/mod.rs`
- `userspace/init/src/main.rs`

**Symbol:** `on_display_server_death`
**Why it matters:** The design doc's acceptance criterion explicitly allows either "recoverable" *or* "failure-mode-and-recovery-path documented and testable." The recovery path has to be real, not rhetorical.

**Acceptance:**
- [ ] When `display_server` exits (crash or clean shutdown without `sys_fb_release`), the kernel reclaims the framebuffer and resumes the kernel console so the system is not left with a dead screen
- [ ] The init/service-manager restarts `display_server` within a bounded number of attempts; exceeding the cap triggers a documented fallback (serial shell remains usable, kernel console is active)
- [ ] Clients connected to `display_server` see their socket close cleanly and are responsible for reconnecting; no client-side crashes are required
- [ ] A regression test triggers a `display_server` crash (e.g. via a debug `panic!` gated behind a test-only verb), confirms the kernel console returns, confirms the service manager restarts `display_server`, and confirms a new client can connect after restart
- [ ] The learning doc documents the failure-and-recovery path explicitly

### F.3 — Fallback to text-mode administration

**Files:**
- `docs/56-display-and-input-architecture.md` (learning doc)
- `userspace/init/src/main.rs`

**Symbol:** `text_mode_fallback`
**Why it matters:** If the graphical stack cannot start at all (e.g. framebuffer metadata mismatch, critical service crash loop), the system must remain administrable. "Serial console works" is not automatic — it has to be validated.

**Acceptance:**
- [ ] If `display_server` fails to start within the service manager's restart budget, `init` leaves the kernel framebuffer console and the serial console active, and logs a named failure reason
- [ ] A login prompt is reachable over serial regardless of graphical state
- [ ] The learning doc documents exactly which administration paths remain live under graphical failure and which are disabled (e.g. graphical terminals are unavailable; serial `ion` works)
- [ ] A regression test simulates "graphical stack unavailable" by disabling `display_server`'s startup manifest and confirms a reachable serial shell

---

## Track G — Validation

### G.1 — Multi-client coexistence regression test

**Files:**
- `userspace/display_server/tests/` (new)
- `xtask/src/main.rs` (test harness invocation)

**Symbol:** `multi_client_coexistence`
**Why it matters:** Phase 56's headline acceptance criterion is "at least two graphical clients can coexist without raw-framebuffer conflicts." A regression test turns this from a promise into a check.

**Acceptance:**
- [ ] Two small test clients connect to `display_server`, each creates a `Toplevel` surface, attaches distinct pixel content, commits, and observes `SurfaceConfigured`
- [ ] The composer renders both surfaces at their layout-derived positions; a pixel-sampling harness in `display_server` (or a test-only control-socket verb) reads back the framebuffer region and confirms both colors are present
- [ ] Neither client wrote to the framebuffer directly; both used the B.4 page-grant transport
- [ ] The test runs under `cargo xtask test` and fails if either client's pixels are absent or if framebuffer writes occur outside the composer

### G.2 — Keybind grab-hook regression test

**Files:**
- `userspace/display_server/tests/`
- `userspace/m3ctl/` (for bind registration)

**Symbol:** `grab_hook_swallow`
**Why it matters:** A.5 and D.4 are the single largest risk for a later tiling compositor; G.2 is the integration-level proof they work.

**Acceptance:**
- [ ] A test client gains focus
- [ ] `m3ctl register-bind MOD_SUPER+q` registers a grab
- [ ] A synthetic `KeyDown` for `SUPER+q` is injected through the input path (via a test-only input-injection verb on the control socket)
- [ ] The focused client receives **no** `KeyEvent`
- [ ] A `BindTriggered` event is observed on the subscribed control stream
- [ ] A subsequent `KeyDown` for `q` (no modifier) is delivered normally to the focused client, confirming unregistered keys still route

### G.3 — Layer-shell exclusive-zone regression test

**File:** `userspace/display_server/tests/`
**Symbol:** `layer_shell_exclusive_zone`
**Why it matters:** E.2's exclusive-zone behavior is what will let the Phase 57b status bar actually reserve space; G.3 validates the math.

**Acceptance:**
- [ ] A `Layer` surface anchored `top` with a 24-pixel exclusive zone is created and committed
- [ ] A subsequent `Toplevel` surface is committed; its geometry from the layout policy is verified to begin at `y >= 24`
- [ ] Removing the `Layer` surface grows the toplevel band back; the `Toplevel` is re-arranged and the test observes the new geometry
- [ ] A `Layer` surface with `exclusive` keyboard interactivity captures focus while mapped and releases it on destroy

### G.4 — Control socket round-trip regression test

**File:** `userspace/display_server/tests/`
**Symbol:** `control_socket_roundtrip`
**Why it matters:** E.4's control socket is the seam for later tooling; G.4 proves the socket is real and the event stream is real.

**Acceptance:**
- [ ] `m3ctl version` returns a non-empty version string matching Phase 56's protocol version from A.3
- [ ] `m3ctl list-surfaces` is empty at startup; after a client creates a `Toplevel`, a second `m3ctl list-surfaces` lists it
- [ ] `m3ctl subscribe SurfaceCreated` receives an event when a client creates a new surface
- [ ] Malformed framing closes the control connection with a named reason; unknown verbs return an `UnknownCommand` error without closing

### G.5 — Display-service crash recovery regression test

**File:** `userspace/display_server/tests/`
**Symbol:** `display_server_crash_recovery`
**Why it matters:** F.2 is the acceptance criterion for recovery; G.5 is the runnable proof.

**Acceptance:**
- [ ] A test triggers `display_server` to exit abnormally (via a test-only control-socket verb or a deliberate `panic!` triggered by a debug flag)
- [ ] The kernel framebuffer console resumes within a bounded time window (recorded in the test)
- [ ] The service manager restarts `display_server`; a new client connection succeeds after restart
- [ ] Repeated crash/restart does not leak framebuffer ownership (no unrecoverable `EBUSY` after the Nth restart)
- [ ] The graphical-stack-unavailable fallback (F.3) is exercised in a variant of this test by disabling the restart policy; a serial shell remains reachable

### G.6 — xtask and CI plumbing for the new test suites

**File:** `xtask/src/main.rs`
**Symbol:** `run_phase56_tests`
**Why it matters:** Tests that cannot be run reliably are not tests. G.6 ensures the new regression tests are wired into `cargo xtask test` (QEMU-based for the graphical stack) and `cargo test -p kernel-core` (pure-logic keymap and compose math).

**Acceptance:**
- [ ] `cargo xtask test` includes the Phase 56 regression tests in its default run
- [ ] The kernel-core portion (keymap, compose math, surface state machine if kept in core) runs via `cargo test -p kernel-core`
- [ ] A failing Phase 56 regression test produces readable output that names the failing acceptance criterion
- [ ] Test runtimes are bounded: any single Phase 56 test must complete under 60 seconds or carry an explicit higher `--timeout` annotation

---

## Track H — Documentation and Version

### H.1 — Create Phase 56 learning doc

**File:** `docs/56-display-and-input-architecture.md` (new)
**Symbol:** N/A (documentation deliverable)
**Why it matters:** The learning doc is a required Phase 56 deliverable per the design doc. It must follow the aligned learning-doc template from `docs/appendix/doc-templates.md` and explain display ownership, input routing, buffer exchange, session behavior, and why this phase is the real GUI architecture milestone.

**Acceptance:**
- [ ] `docs/56-display-and-input-architecture.md` exists and follows the aligned learning-doc template
- [ ] Sections cover: display ownership, client protocol, input event model + grab hook, surface roles + layer-shell-equivalent, layout-module seam, control socket, session + recovery, and how Phase 56 differs from later GUI work (tiling engine, animations, native clients, Wayland)
- [ ] Cross-references `docs/appendix/gui/tiling-compositor-path.md` (Goal A) and `docs/appendix/gui/wayland-gap-analysis.md` (Path A/B/C scope)
- [ ] Key files table lists all new modules introduced in Phase 56 (`userspace/display_server`, `userspace/mouse_server`, `userspace/m3ctl`, `kernel-core/src/input/{keymap,mouse}.rs`, `kernel-core/src/display/{compose,frame_tick}.rs`)
- [ ] Doc is linked from `docs/README.md`

### H.2 — Update subsystem and roadmap docs

**Files:**
- `docs/09-framebuffer-and-shell.md`
- `docs/29-pty-subsystem.md`
- `docs/README.md`
- `docs/roadmap/README.md`
- `docs/roadmap/tasks/README.md`

**Symbol:** N/A (documentation updates)
**Why it matters:** Phase 56 changes the project's display posture from "kernel owns pixels" to "userspace owns pixels." All upstream docs describing framebuffer ownership, console behavior, and graphical capability must reflect the new reality.

**Acceptance:**
- [ ] `docs/09-framebuffer-and-shell.md` updated to record that framebuffer ownership is now transferable to `display_server` and that the kernel framebuffer console is suspended while userspace owns pixels; a forward link to the Phase 56 learning doc is added
- [ ] `docs/29-pty-subsystem.md` updated to record how PTY-driven text clients continue to work alongside a graphical compositor (serial + text-mode administration paths remain)
- [ ] `docs/README.md` gains an entry for the Phase 56 learning doc
- [ ] `docs/roadmap/README.md` Phase 56 row is updated from "Deferred until implementation planning" to link at `./tasks/56-display-and-input-architecture-tasks.md`
- [ ] `docs/roadmap/tasks/README.md` gains a Phase 56 row under a new or existing convergence/hardware section pointing at `./56-display-and-input-architecture-tasks.md`

### H.3 — Update evaluation docs

**Files:**
- `docs/evaluation/gui-strategy.md`
- `docs/evaluation/usability-roadmap.md`
- `docs/evaluation/roadmap/R09-display-and-input-architecture.md`

**Symbol:** N/A (documentation updates)
**Why it matters:** The evaluation track is where the project records *why* it chose a direction. Closing Phase 56 without updating these produces strategy-documentation drift.

**Acceptance:**
- [ ] `docs/evaluation/gui-strategy.md` is updated to reflect that the native-compositor recommendation is no longer hypothetical: the Phase 56 task list exists and names the four Goal-A contract points
- [ ] `docs/evaluation/usability-roadmap.md` Stage 3 GUI section is updated to reference the Phase 56 task doc
- [ ] `docs/evaluation/roadmap/R09-display-and-input-architecture.md` is updated to reflect the Phase 56 planning status (task doc exists; implementation status remains Planned)
- [ ] The evaluation docs explicitly record that the tiling-first UX, animation engine, and native bar/launcher clients are Phase 56b/57 work — not Phase 56

### H.4 — Version bump to 0.56.0 on phase landing

**Files:**
- `kernel/Cargo.toml`
- `AGENTS.md` (project-overview version string)
- `README.md`
- `docs/roadmap/README.md` (Phase 56 status column)
- `docs/roadmap/tasks/README.md` (Phase 56 status column)

**Symbol:** `version` field (Cargo.toml) and prose version mentions (docs)
**Why it matters:** The Phase 56 design doc requires the kernel version to be bumped to `0.56.0` when the phase lands. Phase 54 and Phase 55 both exposed that leaving "any other version references" open-ended permits drift between the crate version, the docs, and the roadmap status columns.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `[package].version` is `0.56.0`
- [ ] `AGENTS.md` project-overview paragraph reflects kernel `v0.56.0` and names the graphical architecture additions
- [ ] `README.md` project description reflects the new kernel version
- [ ] `docs/roadmap/README.md` Phase 56 row status is `Complete`
- [ ] `docs/roadmap/tasks/README.md` Phase 56 row status is `Complete`
- [ ] A repo-wide search for the previous `0.55.x` version string returns no user-facing references that should have been bumped (generated lockfiles excepted)

---

## Documentation Notes

- **The Engineering Discipline section above is authoritative.** Every code-producing task inherits its test-first ordering, test-pyramid placement, SOLID/DRY rules, error discipline, observability requirements, concurrency model, and resource-bounds requirements. Acceptance bullets in individual tasks add *task-specific* tests and invariants; they do not override the discipline section. A task that passes its own Acceptance list but violates the discipline section (e.g. implementation-first commits, protocol types duplicated across crates, `unwrap()` in a production path, raw `println!` outside tests) is not complete.
- **Testability is the primary axis of this phase.** Pure logic (codecs, state machines, keymap/mouse decoders, dispatcher decisions, bind matching, damage math, layout arranging, cursor damage) lives in `kernel-core` and is covered by unit, contract, and property tests on the host. Userspace modules are thin wiring shims around those pure cores. Integration tests under `cargo xtask test` exist for what cannot be verified off the real substrate (framebuffer handoff, IRQ delivery, service supervision, crash recovery). If a bug is only findable under QEMU, the task that introduced the regression is responsible for extracting the reproducer into a `kernel-core` unit or property test.
- **Tearing and double-buffering.** Phase 56 composes directly into the framebuffer without a back-buffer. Tearing under heavy motion is accepted as a Phase 56 limitation and called out in the learning doc; double-buffering is deferred to the same phase that introduces a real vblank source.
- Phase 9 introduced the kernel framebuffer console. Phase 56 moves pixel ownership out of the kernel during normal operation; the kernel retains framebuffer output only for pre-init, panic, and failover-to-text-mode scenarios.
- Phase 47 (DOOM) proved a single userspace program can draw pixels through a graphical substrate. Phase 56 generalizes that from one-app graphics to a multi-client architecture with explicit surface ownership and event routing.
- Phase 46/51 supply the service-supervision model. Phase 56 registers `display_server`, `kbd_server`, and `mouse_server` as supervised services with explicit startup ordering and restart policies (Track F).
- Phase 50 supplies the page-grant buffer transport. Phase 56 validates and uses this transport for client-to-server surface buffers (Track B.4); it does **not** introduce a new shared-memory mechanism.
- Phase 55's hardware-access layer is not used directly by Phase 56 — the chosen mouse path (PS/2 AUX via IRQ12) predates Phase 55's PCIe/MSI work. USB HID mouse support is explicitly deferred to a later phase per the design doc's "Deferred Until Later" list.
- **Goal-A contract points are explicit.** The four design decisions from `docs/appendix/gui/tiling-compositor-path.md` (swappable layout module, keybind grab hook, layer-shell-equivalent role, control socket) are delivered by A.7/E.1, A.5/D.4, A.6/E.2, and A.8/E.4 respectively. Each contract point ships a trait / role / hook / socket in Phase 56; the tiling-specific *implementations* built on top of them (tiling layout engine, chord engine, bar/launcher clients) ship in Phase 56b / 57b and are explicitly out of scope here.
- **Explicit non-Wayland framing.** The client protocol (A.3) is m3OS-native, not Wayland. `docs/appendix/gui/wayland-gap-analysis.md` Path A (`wl_shm` adapter) is not in Phase 56 scope and is only reachable as an *additive* phase after Phase 56 lands.
- **Mouse scope is narrow.** Phase 56 ships PS/2 AUX motion + 3 buttons + optional wheel. Touchpad gestures, tablet/pen input, touch, and USB HID breadth are all deferred.
- **Keymap scope is narrow.** Phase 56 ships US QWERTY with the five standard modifiers. International layouts, IME, dead keys, and compose sequences are deferred.
- **Software-only composition.** Phase 56 has no GL/GLES2/EGL/DRM code paths. Hardware-accelerated composition and live blur effects are deferred; see `docs/appendix/gui/tiling-compositor-path.md` § Software-only rendering budget for the bandwidth math that motivates this trade.
- **Pure-logic code belongs in `kernel-core` where practical.** Keymap translation (D.1), mouse decoding (B.2), compose math (C.4), and frame-tick metadata (B.3) are host-testable; put them in `kernel-core` and test them with `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`. Hardware-dependent wiring (syscalls, MMIO, ISR registration) belongs in `kernel/src/`.
- Host-side tests should use `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`; QEMU-driven integration tests use `cargo xtask test`.
