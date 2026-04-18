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

## Track A — Architecture and Protocol Design

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
- [ ] `Ps2MouseDecoder` lives in `kernel-core` as pure-logic state: feed bytes, emit `MouseEvent { dx, dy, buttons, wheel }` frames; at least 4 host tests cover the 3-byte standard packet, the 4-byte IntelliMouse wheel extension enablement handshake, overflow-bit handling, and out-of-sync recovery
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
- [ ] Page-grant leak behavior is defined: when a client dies without `DestroySurface`, the kernel drops its refcount and `display_server` sees `SurfaceDestroyed` + `BufferReleased` within the next dispatch cycle
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

**Symbol:** `FramebufferOwner`, `acquire_primary_output`, `present_damage`
**Why it matters:** The whole phase rests on `display_server` owning the primary framebuffer. This task wires the B.1 acquisition syscall to a typed owner inside the service.

**Acceptance:**
- [ ] On startup, `display_server` calls `sys_fb_acquire` exactly once; if it returns `EBUSY`, the service retries with bounded backoff up to a configured limit and then exits nonzero
- [ ] `FramebufferOwner` caches the framebuffer metadata (width, height, stride, pixel format) and exposes `write_pixels(rect, src: &[u8], src_stride)` using volatile writes with explicit bounds checks that clip `rect` to the framebuffer extents (preventing OOB writes even on malformed damage input)
- [ ] Initial presentation on startup draws a known background color across the full framebuffer so that the ownership handoff is visually unambiguous during bring-up and manual testing
- [ ] A shutdown path calls `sys_fb_release()` on normal exit; on panic the kernel reclaims ownership (validated in F.2)
- [ ] The presentation path is single-threaded inside `display_server` (no locking around the framebuffer owner); concurrency is handled above by the compositor event loop
- [ ] A smoke test confirms `display_server` can fill the framebuffer with a solid color on startup and clear it on shutdown

### C.3 — Surface state machine

**File:** `userspace/display_server/src/surface.rs` (new)
**Symbol:** `Surface`, `SurfaceId`, `BufferSlot`, `commit`
**Why it matters:** A surface is the compositor's atomic unit of client-provided pixels. Without a state machine that distinguishes *attached*, *committed*, *sampled*, and *released*, tearing, use-after-free, and double-commit bugs become structural rather than testable.

**Acceptance:**
- [ ] `Surface` tracks: unique id, role (`Toplevel` | `Layer` | `Cursor`), current committed buffer (page-grant handle + metadata), pending buffer, pending damage rectangles, geometry (position + size), focus state
- [ ] `AttachBuffer` stores the page-grant capability in the *pending* slot without sampling
- [ ] `DamageSurface` accumulates damage rectangles into the pending commit
- [ ] `CommitSurface` atomically swaps pending → current, releases the previously-current buffer (emitting `BufferReleased` to the owning client), and records the damage for the next composer pass
- [ ] `DestroySurface` releases the current buffer, marks the surface as dead, and notifies the layout policy (`LayoutPolicy::on_surface_removed`)
- [ ] At least 4 host-testable unit tests in a `surface_core` module (or in `kernel-core` if the logic is pure data): commit-with-no-attach is a protocol error, double-commit discards the first, damage accumulates across multiple `DamageSurface` calls, destroy releases the buffer exactly once

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
- [ ] If no surface reported damage on a tick, the composer performs no framebuffer writes
- [ ] Host tests in `kernel-core` cover: (a) damage rectangle union/intersection math, (b) layer-order traversal returns surfaces in the documented order, (c) clip-to-output correctly rejects an off-screen surface, (d) an opaque toplevel fully covered by a higher-layer surface is skipped
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
- [ ] Host tests cover at least 5 keymap cases: plain letter, shifted letter, caps-lock interaction, extended-key (`0xE0 0x4B` → `ArrowLeft`), pause sequence

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

**File:** `userspace/display_server/src/input.rs` (new)
**Symbol:** `InputDispatcher`, `route_key_event`, `route_pointer_event`, `focused_surface`
**Why it matters:** Once events arrive at `display_server`, they must be routed by policy — not by accident. Focus rules, grab rules, and layer-shell keyboard-interactivity modes all live in one dispatcher so the policy is auditable in one place.

**Acceptance:**
- [ ] The dispatcher holds current keyboard focus (a `SurfaceId` or `None`), current pointer position, current pointer-hit surface, and the active `Layer` surface with `exclusive` keyboard mode if any
- [ ] `route_key_event` consults the grab hook (D.4) **first**; if no bind matches and a `Layer` surface claims `exclusive` keyboard mode the event goes there; otherwise the focused `Toplevel` or `on_demand` `Layer` surface receives it; otherwise the event is dropped
- [ ] `route_pointer_event` updates the pointer-hit surface via geometry lookup; emits `PointerEnter` / `PointerLeave` when the hit surface changes; delivers motion and buttons to the hit surface subject to keyboard-focus and layer rules
- [ ] Click-to-focus is the Phase 56 default: a `PointerButton::Down` on a `Toplevel` surface moves keyboard focus to it unless an `exclusive` `Layer` is active
- [ ] Focus changes emit `FocusIn` / `FocusOut` events (A.4) to the affected surfaces
- [ ] At least 4 host tests cover: grab-hook swallow, layer-shell exclusive keyboard, pointer-hit routing, pointer-enter/leave on motion across a boundary

### D.4 — Keybind grab-hook implementation

**File:** `userspace/display_server/src/input.rs`
**Symbol:** `BindTable`, `register_bind`, `unregister_bind`, `match_bind`
**Why it matters:** A.5 specified the semantics; D.4 delivers them.

**Acceptance:**
- [ ] `BindTable` is a small associative map keyed by `(modifier_mask, keycode)`; lookups are constant-time on the mask and keycode combination (a hash or sorted-vector over the expected small N is acceptable)
- [ ] `register_bind` and `unregister_bind` are callable only through the control socket (E.4) and through server-internal code; there is no direct client-protocol entry point in Phase 56
- [ ] `match_bind` returns `Some(handler)` on a `KeyDown` that matches a registered `(mask, keycode)` with exact mask equality; `KeyUp` and `KeyRepeat` events associated with a swallowed `KeyDown` are also suppressed from clients (tracked via per-keycode grab state) so clients do not see half a chord
- [ ] On a match, the handler (in Phase 56 this is a small dispatch table — e.g. `focus-next`, `quit-focused` — used only by tests) runs on the dispatcher thread and no client sees the event
- [ ] On no match, the event falls through to focus routing (D.3)
- [ ] A regression test (G.2) validates that registering `MOD_SUPER + q` and pressing Super+Q produces a `BindTriggered` control-socket event and no `KeyEvent` at the focused client

---

## Track E — Layout Policy, Layer-Shell Surfaces, and Control Socket

### E.1 — `LayoutPolicy` trait and default floating layout

**Files:**
- `userspace/display_server/src/layout/mod.rs` (new)
- `userspace/display_server/src/layout/floating.rs` (new)

**Symbol:** `LayoutPolicy`, `FloatingLayout`, `arrange`
**Why it matters:** A.7 specified the contract; E.1 delivers the trait plus the minimum-viable default. The tiling-first engine lands later; Phase 56 just proves the seam works.

**Acceptance:**
- [ ] The trait signature matches A.7's specification exactly
- [ ] `FloatingLayout` places each new `Toplevel` at an output-centered default size with a small cascade offset so concurrent surfaces are visually distinguishable during bring-up
- [ ] `FloatingLayout::arrange` respects exclusive-zone rectangles from `Layer` surfaces: no `Toplevel` is placed overlapping an exclusive zone unless the output is too small to avoid it
- [ ] Swappability is real: the layout policy is constructed once at startup through a named factory function; a second implementation exists as a test double (`StubLayout`) used in G.1
- [ ] Host tests confirm the trait can be implemented in at least two non-trivial ways (the real `FloatingLayout` plus the test double)

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
- `userspace/display_server/src/surface.rs`
- `userspace/display_server/src/compose.rs`

**Symbol:** `CursorRole`, `set_cursor_surface`
**Why it matters:** The pointer needs a bitmap that follows motion. In Phase 56 the cursor is a client-provided `Cursor`-role surface sampled at the current pointer position; providing the role is cheap, and without it every client reinvents cursor rendering.

**Acceptance:**
- [ ] `SetSurfaceRole` with role `Cursor` accepts a `CursorConfig { hotspot_x, hotspot_y }`
- [ ] The composer samples the cursor surface at the current pointer position minus the hotspot, in the top-most layer
- [ ] A default cursor (a simple software-drawn arrow in `kernel-core`) is used when no client has set a cursor surface — this prevents an invisible pointer on a fresh boot
- [ ] Pointer motion damage is computed correctly: the old cursor rectangle and the new cursor rectangle are both damaged, so trails are not left behind
- [ ] A regression test confirms the cursor moves correctly across a motion event and that the damage rectangle math does not leave stale pixels

### E.4 — Control socket: endpoint, verbs, events

**Files:**
- `userspace/display_server/src/control.rs` (new)
- `userspace/m3ctl/` (new minimal client — see G.4)

**Symbol:** `ControlServer`, `handle_command`, `emit_event`
**Why it matters:** A.8 specified the protocol; E.4 implements it. This is the seam the native bar/launcher clients (Phase 57b) will target.

**Acceptance:**
- [ ] `display_server` opens a second AF_UNIX stream socket at the documented control-socket path from A.8; filesystem permissions restrict it to the owning user
- [ ] The minimum verb set from A.8 is implemented: `version`, `list-surfaces`, `focus <surface-id>`, `register-bind <mask> <keycode>`, `unregister-bind <mask> <keycode>`, `subscribe <event-kind>`
- [ ] Events `SurfaceCreated`, `SurfaceDestroyed`, `FocusChanged`, `BindTriggered` are emitted on every subscribed stream
- [ ] An `UnknownCommand` error is returned for unrecognized verbs; the stream is not closed on unknown verbs (only on malformed framing)
- [ ] A minimal userspace `m3ctl` client binary is scaffolded in `userspace/m3ctl` (following the four-step new-binary convention) and implements at least `m3ctl version` and `m3ctl list-surfaces`; it is used by G.4's regression test
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
