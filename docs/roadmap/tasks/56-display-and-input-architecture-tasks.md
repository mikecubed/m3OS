# Phase 56 — Display and Input Architecture: Task List

**Status:** In progress (Tracks A + B + C complete; D – H pending)
**Source Ref:** phase-56
**Depends on:** Phase 46 (System Services) ✅, Phase 47 (DOOM) ✅, Phase 50 (IPC Completion) ✅, Phase 51 (Service Model Maturity) ✅, Phase 52 (First Service Extractions) ✅, Phase 55 (Hardware Substrate) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅
**Goal:** Replace the kernel-owned framebuffer and single-app input model with a single userspace display service that owns presentation, arbitrates surfaces for multiple graphical clients, routes focus-aware keyboard and mouse events, and exposes the four contract points a tiling-first compositor experience (Goal A in `docs/appendix/gui/tiling-compositor-path.md`) needs so the tiling UX can land on top without protocol rework.

## Implementation status (as of PR #123)

| PR | Tracks landed | Notes |
|---|---|---|
| #121 (`62d1bc0`) | A.0 – A.9 | Protocol types in `kernel-core::display::protocol` + `kernel-core::input::events`, design doc, learning doc, evaluation gates. Track A is **complete**. |
| #122 (`efcb2ac`) | B.1, C.1, C.2, plus the kernel-core foundation slice of B.2 / B.3 / B.4 / C.3 / C.4 / E.1 | First-pass kernel-core pure-logic modules + framebuffer ownership transfer + `display_server` scaffold. See per-track tables below. |
| #123 (`930850a`) | B.2, B.3, B.4, C.3, C.4, C.5, C.6, plus three Copilot-review rounds | Closes the wiring of Tracks B and C on top of #122's pure-logic foundation: kernel mouse + frame-tick syscalls, `display_server` compose loop, `gfx-demo` protocol-reference client, `surface_buffer` helper crate, IPC-label dispatcher with strict framing. Tracks B and C are **complete**. |

### What landed in PR #122

- **Pure-logic kernel-core modules** (1134 host tests passing): `kernel-core::input::mouse` (Ps2MouseDecoder), `kernel-core::display::frame_tick` (FrameTickConfig + saturating counter), `kernel-core::display::buffer` (refcount lifecycle), `kernel-core::display::fb_owner` (FramebufferOwner trait + RecordingFramebufferOwner double + reusable contract suite), `kernel-core::display::surface` (SurfaceStateMachine), `kernel-core::display::compose` (damage-tracked composer + occlusion math), `kernel-core::display::layout` (LayoutPolicy trait + FloatingLayout default + StubLayout + contract suite).
- **Kernel — B.1**: `sys_framebuffer_release` syscall (0x1014) + userspace `framebuffer_release()` wrapper. Reuses the existing Phase 47 `try_yield_console` / `restore_console` path.
- **Userspace — C.1**: `userspace/display_server` crate scaffolded through the four-place new-binary convention, plus `etc/services.d/display_server.conf` (restart=on-failure max_restart=5 depends=kbd).
- **Userspace — C.2**: `KernelFramebufferOwner` real impl of the FramebufferOwner trait. `display_server` now acquires the framebuffer at boot via `framebuffer_info` + `framebuffer_mmap` with bounded backoff, paints `0x002B_5A4B` (deep teal) across the full FB, and best-effort releases on shutdown / panic.

End-to-end QEMU smoke validation (pre-push hook): `display_server: framebuffer acquired` + `[INFO] [framebuffer_mmap] pid=13 mapped 1000 pages` + `display_server: fb metadata: 1280x800 stride=5120`.

### What landed in PR #123

- **Kernel — B.2 (mouse path).** New `kernel/src/arch/x86_64/ps2.rs` owns 8042 AUX init (enable port, IntelliMouse magic-knock handshake, enable streaming) and a single-producer/single-consumer ring buffer of decoded `MousePacket`s fed by an `IRQ12` handler. Decoder is the pure-logic `kernel_core::input::mouse::Ps2MouseDecoder`. PIC mask updated in `init_pics` to unmask IRQ2 (cascade) + IRQ12. New syscall `SYS_READ_MOUSE_PACKET = 0x1015`; `MousePacket` 8-byte wire encoding lives in `kernel_core::input::mouse` so the encode round-trip is host-tested.
- **Kernel — B.3 (frame-tick).** New `kernel/src/time/` module subdivides the 1 kHz LAPIC timer into the configured frame-tick rate (default 60 Hz) using a lock-free `AtomicU32` pending counter (saturating clamp at `FRAME_TICK_SAT_CAP = 1_000_000`) plus a precomputed `FRAME_TICK_PERIOD_MS` cache so the ISR fast path stays at two relaxed atomic ops per fire. Two new syscalls: `SYS_FRAME_TICK_HZ = 0x1016` and `SYS_FRAME_TICK_DRAIN = 0x1017`. No ISR-vs-task locks (deliberate departure from the prior `Mutex<FrameTickCounter>` design after the round-1 review caught the deadlock vector).
- **Userspace — B.4 (surface_buffer helper crate).** New `userspace/lib/surface_buffer/` (separate crate so binaries that don't allocate pixel buffers don't pull `extern crate alloc;` in via Cargo feature unification). 32 × 32 BGRA8888 cap is the design ceiling; geometry overflow + zero-dimension are typed errors. 7 host unit tests.
- **Userspace — C.3 (surface shim).** New `userspace/display_server/src/surface.rs` wraps the kernel-core `SurfaceStateMachine` with a `SurfaceRegistry` that owns committed pixel buffers, a position-keyed pending-bulk queue (`MAX_PENDING_BULK = 4`), and a per-surface `dirty` flag. Forwards `SurfaceEffect::Configured` straight to `ServerMessage::SurfaceConfigured` so monotone-serial semantics live in the state machine, not the shim.
- **Userspace — C.4 (composer wiring).** New `compose.rs` consumes the `FramebufferOwner` and `LayoutPolicy` traits and calls `kernel_core::display::compose::compose_frame`. Damage gate via the registry's `has_damage()` so a frame tick with no new commits writes zero pixels. Default layout factory is `FloatingLayout::new()`. `compose_frame` is the single owner of `present()` calls.
- **Userspace — C.5 (client dispatcher).** New `client.rs` defines two IPC labels on the `display` endpoint: `LABEL_VERB = 1` (`bulk` carries an encoded `ClientMessage`) and `LABEL_PIXELS = 2` (`bulk` is `[w_le_u32 \| h_le_u32 \| pixel_bytes...]`, `data0` is the `BufferId`). `dispatch()` is pure-logic: takes one inbound frame, returns `outbound: Vec<ServerMessage>` plus closed/fatal flags. Strict single-frame-per-bulk framing (`consumed != bulk.len()` is `BodyLengthMismatch`). The Phase 56 task doc's "AF_UNIX (or IPC)" foundation note allows the IPC-endpoint pivot — protocol types in `kernel-core::display::protocol` are transport-agnostic, so a future swap is a wiring change in `client.rs` alone.
- **Userspace — C.6 (gfx-demo).** New `userspace/gfx-demo/` follows the four-step new-binary convention. Allocates a 16 × 16 BGRA surface filled with `0x00FF_8800` (orange), ships a `LABEL_PIXELS` bulk via `ipc_call_buf`, then walks `Hello → CreateSurface → SetSurfaceRole(Toplevel) → AttachBuffer → CommitSurface`. Demo idles for inspection after the round-trip.
- **Review-resolution rounds.** PR #123 took three Copilot review passes; the 19 cumulative threads (7 + 7 + 5) are all resolved. Highlights of the round-2/3 fixes that landed in this same PR: replaced the original `spin::Mutex<FrameTickCounter>` with the lock-free atomic design above (round 1); moved `LABEL_PIXELS` geometry from unreachable `data[2..]` slots into the bulk header (round 2); switched `pending_bulk` from LIFO `pop()` to position-search and validated `apply_event` *before* removing the entry (rounds 2/3); forwarded the state-machine `Configured` effect instead of synthesising shim-side serials (round 3); strict trailing-byte rejection in `decode_message` (round 3); single-source-of-truth `present()` (round 3).

End-to-end QEMU smoke validation (pre-push hook): `cargo xtask check` clean, `cargo test -p kernel-core` 995+ tests pass, `cargo xtask smoke-test` PASSED, `cargo xtask regression` 9/11 (2 pre-existing flakes already on `main`).

### What remains

- **D.1–D.4** — `kbd_server` keymap extension, `mouse_server` daemon, `InputDispatcher`, bind-table grab hook.
- **E.1 (wiring)** — `display_server::layout` shim is consumed via `default_layout()` in C.4; the `LayoutPolicy` trait is exercised every frame, but the named factory + control-socket layout-swap surface still lands with E.2 / E.4.
- **E.2 / E.3 / E.4** — Layer-role wiring, cursor renderer + sampling, control socket + `m3ctl` client.
- **F.1 (kbd/mouse manifests + supervisor `on-restart`)** — `display_server.conf` shipped (PR #122); `kbd_server.conf` / `mouse_server.conf` and supervisor-level `on-restart` wiring still pending.
- **F.2 / F.3** — Display-service crash recovery + text-mode fallback.
- **G.1–G.7** — Multi-client coexistence, grab-hook, layer-shell, control-socket, crash-recovery, xtask plumbing, manual smoke checklist.
- **H.1–H.4** — Learning doc, subsystem doc updates, evaluation doc updates, version bump to 0.56.0.

Two carry-overs from B.4 are documented but not wired: (a) **true zero-copy via page-grant capabilities** (Phase 56's pixel transport is inline IPC bulk; the `AF_UNIX SCM_RIGHTS`-equivalent transfer the original B.4 spec referenced is still not implemented in m3OS — the wiring task pivoted to inline bulk per the doc's allowed alternative); and (b) **automated regression test for client/server pixel observation** (the runtime-level proof point lands with G.1). Surface-buffer cap stays at 4 KiB until either page-grant or a higher kernel `MAX_BULK_LEN` ships; `gfx-demo` therefore tops out at 16 × 16 BGRA in this phase.

> **Note on Phase 55c:** The Phase 56 compositor core is socket-centric and does not
> depend on `RecvResult` or `IrqNotification::bind_to_endpoint` as prerequisites. The
> Phase 55c bound-notification pattern is available as a template for later IRQ-backed
> userspace drivers (e.g., a future vsync or HID driver that genuinely mixes async
> hardware events with sync IPC requests). PS/2 and early input services in Phase 56
> keep their existing wait/send split; adoption of the Phase 55c pattern is opt-in for
> any IRQ-backed driver introduced in Phase 56 or later.

## Model and Effort Guidance

Recommended model and effort per task shape. The Engineering Discipline section below (property tests, contract-test suites, state-machine invariants, codec round-trips) is exactly where the capability gap between models shows up most — a protocol type declared wrong in A.0 or an ordering bug in D.3 propagates into every later track.

| Task shape | Model | Effort | Rationale |
|---|---|---|---|
| A.0 codec + A.3/A.4 wire format + A.5–A.8 design | Opus 4.7 | extended thinking | Every later task imports these types; a rename or byte-layout miss cascades |
| B.1/B.3/B.4 kernel syscalls + page-grant audit, B.2 PS/2 decoder | Opus 4.7 | extended thinking | Ring 0, IRQ boundaries, capability transfer — mistakes corrupt state silently |
| C.3 surface state machine, C.4 compose math, D.3 dispatcher, D.4 bind table, E.1 layout contract, E.3 cursor damage | Opus 4.7 | extended thinking | Pure-logic cores with property-test invariants; Opus produces sharper invariants and better counterexample handling |
| C.1 scaffold, C.5 client loop, D.1/D.2 service wiring, E.2/E.4 socket wiring, F.1 manifests | Sonnet 4.6 | standard | Mechanical once the A.0 types + traits exist |
| C.6 `gfx-demo`, G.1–G.5 regression tests | Sonnet 4.6 | standard | Specs are tight; execution is the work |
| G.6/G.7 xtask + smoke checklist | Sonnet 4.6 | standard | Integration plumbing, no deep invariants |
| H.1–H.4 docs, subsystem updates, version bump | Sonnet 4.6 | standard | Writing, not designing |

**Heuristic for unlisted tasks:** if the acceptance list contains `proptest` or "contract-test suite runs against every impl," it's Opus + extended thinking. If it's "follow the four-step new-binary convention" or "add a bullet to `docs/README.md`," it's Sonnet standard.

**Workflow recommendations:**
1. Land Track A first, in isolation, before anything else (Opus only). Freeze the types; every later task imports from them. Run `/ultrareview` on Track A before merge to front-load feedback while the protocol is still malleable.
2. Tracks C.3, D.3, D.4, E.1 are pure-logic and independent after A.0 lands — `/flow:parallel-impl` across worktrees is a real win. Convergence goes through the `kernel-core` contract-test suites.
3. The `ccc:*` skills auto-fire on write operations and enforce the same rules as the Engineering Discipline section below — let them catch routine violations so the model's attention stays on invariants.
4. For the two or three subtlest pieces (surface state machine, damage math, input routing), run a `/codex:rescue` second-opinion pass after the initial Opus implementation lands. These are exactly the "passed its own tests but missed an ordering" bugs a different model catches well.
5. Skip extended thinking on the wiring tracks — Sonnet 4.6 at default effort is sufficient and the savings compound across ~15 Sonnet-scale tasks.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Architecture and protocol design (adopts the four Goal-A design decisions as Phase 56 contract points) | None | Complete (PR #121) |
| B | Kernel substrate for ownership transfer (framebuffer handoff, mouse input path, vblank tick, surface buffer transport) | A | **Complete** (B.1 in PR #122; B.2 + B.3 + B.4 wiring in PR #123). Note: B.4 ships inline bulk IPC; true zero-copy via page-grant capabilities and the multi-process pixel-observation regression test are deferred follow-ups. |
| C | Display service (compositor core, software composer, surface state machine, `gfx-demo` protocol-reference client) | A, B | **Complete** (C.1 + C.2 in PR #122; C.3 + C.4 + C.5 + C.6 wiring in PR #123). C.5 ships an IPC-endpoint dispatcher rather than AF_UNIX per the task doc's allowed pivot — protocol types are transport-agnostic. |
| D | Input services and keybind-grab hook (key-event model, mouse service, focus-aware dispatch) | A, B, C | Planned |
| E | Layout policy, layer-shell-equivalent surfaces, and control socket | A, C, D | E.1 — trait + default + contract suite landed in `kernel-core::display::layout` (PR #122) and consumed via `default_layout()` in PR #123; E.2 / E.3 / E.4 — pending |
| F | Session integration, supervision, and recovery | C, D, E | F.1 — `display_server.conf` manifest shipped (PR #122); `kbd_server.conf` / `mouse_server.conf` pending. F.2 / F.3 — pending |
| G | Validation: multi-client, grab hook, layer-shell, control socket, crash recovery, interactive `run-gui` smoke | C, D, E, F | Planned |
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

**Status:** Complete (PR #122)

**Files:**
- `kernel/src/fb/mod.rs`
- `kernel/src/main.rs`
- `userspace/display_server/src/fb.rs` (new)

**Symbol:** `acquire_framebuffer`, `release_framebuffer`, `FB_OWNER`
**Why it matters:** Today `kernel/src/fb` is the unconditional presentation path for kernel log output, panics, and the Phase 9 console. For `display_server` to own presentation, the kernel must stop writing to the framebuffer *except* under well-defined failover conditions (panic, pre-init, or after `display_server` has voluntarily released it). Without this, the kernel and the compositor race over pixels.

**Acceptance:**
- [x] A `sys_fb_acquire(flags)` syscall (or capability-gated IPC) lets a privileged userspace process take exclusive ownership of the framebuffer; it returns a page-grant capability covering the framebuffer region and metadata (resolution, stride, pixel format) — Phase 56 reuses the existing Phase 47 `SYS_FRAMEBUFFER_INFO` (0x1005) + `SYS_FRAMEBUFFER_MMAP` (0x1006) pair, which together perform the atomic ownership claim via `try_yield_console` and the page-grant mapping.
- [x] Concurrent acquisition attempts return a distinct `EBUSY`-shaped error; the kernel serves at most one live framebuffer owner at a time — verified via the existing CAS in `try_yield_console` and exercised by `KernelFramebufferOwner::acquire`'s bounded backoff.
- [x] While `display_server` holds the framebuffer, the kernel framebuffer console is suspended: no routine kernel log output is written to pixels — `CONSOLE_YIELDED` is set inside `try_yield_console` and re-checked under lock in `fb::write_str`.
- [ ] Panic path still writes to the framebuffer (the TCB cannot rely on userspace during a panic) — this behavior is documented in the learning doc — *behaviour preserved by the existing fb code; learning-doc note pending under H.1.*
- [x] `display_server` may call `sys_fb_release()` to return ownership (used on graceful shutdown and on crash-handler-driven failover in F.3) — `SYS_FRAMEBUFFER_RELEASE` (0x1014) added; `KernelFramebufferOwner::release` and `Drop` invoke it.
- [ ] An integration test confirms: (a) kernel log output is routed only to serial while `display_server` owns the framebuffer, (b) on `display_server` exit without release the kernel reclaims pixel output (see F.2) — *deferred to F.2.*
- [ ] The pre-existing Phase 47 DOOM graphical path is either retired or migrated to acquire through the new API; no code path writes to raw framebuffer bytes without going through `sys_fb_acquire` — *DOOM still uses the same Phase 47 mmap path; no parallel raw FB writers exist. Migration audit deferred to a Phase 56 wrap-up pass.*

### B.2 — Mouse input path (PS/2 AUX)

**Status:** Complete (PR #123). Pure-logic decoder + 8-byte wire encoder live in `kernel-core::input::mouse`; kernel-side `kernel/src/arch/x86_64/ps2.rs` owns the 8042 AUX init, IntelliMouse handshake, ring buffer, and `IRQ12` handler; userspace consumes via the new `SYS_READ_MOUSE_PACKET = 0x1015` syscall.

**Files:**
- `kernel/src/arch/x86_64/ps2.rs` (new — PR #123)
- `kernel/src/arch/x86_64/interrupts.rs` (PR #123: `mouse_handler` + IRQ12 IDT entry + PIC-mask update)
- `kernel/src/arch/x86_64/syscall/mod.rs` (PR #123: `SYS_READ_MOUSE_PACKET`)
- `kernel-core/src/input/mouse.rs` (PR #122 decoder; PR #123 added `encode_packet` + `MOUSE_PACKET_WIRE_SIZE` + 2 round-trip tests)
- `userspace/syscall-lib/src/lib.rs` (PR #123: `read_mouse_packet` helper)

**Symbol:** `Ps2MouseDecoder`, `mouse_handler`, `MOUSE_PACKET_RING`, `feed_byte_isr`, `init_mouse`, `read_mouse_packet`, `encode_packet`
**Why it matters:** The Phase 56 evaluation gate requires a working mouse path. PS/2 AUX (IRQ12) is the minimum-viable path that works in the QEMU default config and on every x86 reference target without pulling USB HID into Phase 56. USB HID breadth is deferred per the design doc.

**Acceptance:**
- [x] The 8042 PS/2 controller is initialized with the auxiliary (mouse) port enabled: `init_mouse` sends `CMD_ENABLE_AUX` (`0xA8`), clears `CONFIG_AUX_DISABLE` + sets `CONFIG_AUX_IRQ` in the controller config, then writes the `Enable Streaming` command (`0xF4`) to the mouse via `CMD_WRITE_TO_AUX` (`0xD4` prefix).
- [x] Tests commit before implementation for the decoder
- [x] `Ps2MouseDecoder` lives in `kernel-core` as pure-logic state: feed bytes, emit `MousePacket { dx, dy, buttons, wheel, overflow }` frames; host tests cover the 3-byte standard packet, the 4-byte IntelliMouse wheel extension, overflow-bit handling, and out-of-sync recovery (`kernel-core/src/input/mouse.rs` — at least 12 unit tests + 3 proptests + 2 wire-encoder tests)
- [x] A `proptest` property test drives arbitrary `&[u8]` streams into the decoder and asserts: no panic, bounded internal state size, recovery after any invalid prefix within a bounded number of bytes
- [x] IRQ12 ingests bytes into a per-device lockless single-producer/single-consumer ring (`MOUSE_PACKET_RING`, capacity 64) under the Phase 52c "no allocation in ISR" rule; no IPC is issued from inside the IRQ handler. Lossy-on-full (drops oldest packet) — pixel deltas eventually catch up.
- [ ] A kernel-side notification object fires on non-empty ring, allowing `mouse_server` (D.2) to wake and drain — *Phase 56 publishes via `signal_irq(12)` after every drained burst; the bound-notification subscribe path lands with D.2's `mouse_server` (waiting on a notification cap rather than polling).*
- [x] A `sys_read_mouse_packet` syscall (0x1015) returns the next decoded `MousePacket` to userspace as an 8-byte wire image (`MOUSE_PACKET_WIRE_SIZE = 8`). Returns `NEG_EAGAIN` on empty ring, `NEG_EINVAL` on malformed buffer, `NEG_EFAULT` on copy failure. Capability gating per the original spec is deferred to D.2 alongside `mouse_server`.
- [x] IntelliMouse (wheel) detection handshake is performed in `try_intellimouse_handshake` (set sample rate 200/100/80 → `Get Device ID`); on failure the driver falls back silently to the 3-byte packet model with `wheel = 0`.
- [x] The existing keyboard path (`kbd_server` + IRQ1) is not regressed; the PIC mask is now `master = 0b1111_1000` (IRQ0/1/2 unmasked) + `slave = 0b1110_1111` (IRQ12 unmasked), preserving IRQ1 + cascade. Pre-push smoke + regression both green. Learning-doc IRQ-vector table pending under H.1.

### B.3 — Vblank / frame-tick notification source

**Status:** Complete (PR #123). Pure-logic `FrameTickConfig` lives in `kernel-core::display::frame_tick`; kernel-side `kernel/src/time/mod.rs` subdivides the LAPIC timer into the configured rate via lock-free atomics; userspace consumes via `SYS_FRAME_TICK_HZ = 0x1016` and `SYS_FRAME_TICK_DRAIN = 0x1017`.

**Files:**
- `kernel/src/time/mod.rs` (new — PR #123)
- `kernel/src/main.rs` (PR #123: `mod time;`)
- `kernel/src/arch/x86_64/interrupts.rs` (PR #123: `crate::time::on_timer_tick_isr()` from BSP timer ISR)
- `kernel/src/arch/x86_64/syscall/mod.rs` (PR #123: `SYS_FRAME_TICK_HZ` + `SYS_FRAME_TICK_DRAIN`)
- `kernel-core/src/display/frame_tick.rs` (PR #122)
- `userspace/syscall-lib/src/lib.rs` (PR #123: `frame_tick_hz` + `frame_tick_drain` helpers)

**Symbol:** `FRAME_TICK_HZ`, `FRAME_TICK_PERIOD_MS`, `FRAME_TICK_PENDING`, `frame_tick_hz`, `frame_tick_drain`, `on_timer_tick_isr`
**Why it matters:** Software composition needs a periodic signal to know when to redraw. Without a frame tick, `display_server` either busy-loops or never redraws on a schedule. A real vblank source requires DRM/KMS (deferred past Phase 56); a timer-driven tick is the Phase 56 substitute and is also what the tiling-compositor-path document assumes for the animation engine later.

**Acceptance:**
- [x] A kernel-owned periodic tick at a configurable rate (default 60 Hz) is observable from userspace. Phase 56 surfaces it via the **drain** syscall (`SYS_FRAME_TICK_DRAIN` returns the saturating count of ticks observed since the last drain) rather than a notification object — a deliberate simplification given `display_server`'s single-threaded event loop. A bound-notification variant for the future animation engine remains compatible (the kernel-side counter would simply also signal a `Notification` on every roll-over).
- [x] The tick uses the existing timer infrastructure (LAPIC timer at 1 kHz, configured by `apic::init`) and does not require new hardware support. Subdivider runs in the BSP timer ISR only — APs do not double-count.
- [x] Tick rate is discoverable from userspace via `SYS_FRAME_TICK_HZ` (returns the configured Hz, default `FrameTickConfig::DEFAULT_HZ = 60`).
- [x] Overrun behavior is documented and exercised: if `display_server` doesn't drain fast enough, missed ticks coalesce. The kernel side is a saturating `AtomicU32` counter clamped at `FRAME_TICK_SAT_CAP = 1_000_000`; the kernel-core `FrameTickCounter` with proptests still backs the host-testable design.
- [ ] The learning doc records that this is a *frame-pacing tick*, not a real vblank, and links forward to a later phase for the hardware vblank story — *deferred to H.1.*

### B.4 — Cross-process shared-buffer transport for surfaces

**Status:** Complete-by-pivot (PR #123). Pure-logic refcount state machine in `kernel-core::display::buffer` (PR #122). The original spec assumed AF_UNIX SCM_RIGHTS-equivalent capability transfer for true zero-copy; this is **not yet implemented in m3OS**, and the wiring task explicitly pivoted to the existing IPC bulk-transport primitive (`ipc_send_buf` / `ipc_call_buf`) per the doc's allowed alternative. PR #123 ships the structural seam (the [`SurfaceBuffer`] type and the bulk-on-IPC framing); zero-copy remains a Phase 56-follow-on. The "without copies" acceptance bullet is the one accepted gap.

**Files:**
- `kernel/src/ipc/mod.rs` (existing — `ipc_send_with_bulk`, `MAX_BULK_LEN = 4096`)
- `userspace/lib/surface_buffer/Cargo.toml` (new — PR #123)
- `userspace/lib/surface_buffer/src/lib.rs` (new — PR #123)
- `userspace/display_server/Cargo.toml` (PR #123: depend on `surface_buffer`)
- `userspace/gfx-demo/Cargo.toml` (PR #123: depend on `surface_buffer`)

The `surface_buffer` helper is its own crate (not part of `syscall-lib`) deliberately: putting `extern crate alloc;` + `Vec<u8>` behind a feature flag on `syscall-lib` would have leaked into binaries (e.g. `echo-args`) that don't have a global allocator, due to Cargo's workspace feature unification.

**Symbol:** `SurfaceBuffer`, `SurfaceBufferId`, `PixelFormat::Bgra8888`, `MAX_BUFFER_BYTES = 4096`, `BufferLifecycle` (kernel-core)
**Why it matters:** Clients submit pixel data by exposing pages to `display_server`. The original spec referenced Phase 50's page-grant transport; in practice m3OS's existing user-to-user transport is the bulk-IPC primitive, so Phase 56 ships on top of that and leaves the zero-copy path for a future revision.

**Acceptance:**
- [x] A userspace helper crate (`userspace/lib/surface_buffer/`) lets a client allocate a refcounted pixel buffer + emit it via `LABEL_PIXELS` over `ipc_call_buf`. **Pivoted from page-grant capability transfer to inline IPC bulk**, since the kernel does not yet expose a way to grant a memory range across user-to-user IPC. The buffer wire format (`[w_le_u32 \| h_le_u32 \| pixel_bytes...]`) is consumed by the dispatcher in `display_server::client::LABEL_PIXELS`.
- [x] `display_server` accepts the bulk and stores it in `SurfaceRegistry::pending_bulk`; `AttachBuffer` consumes the entry by `BufferId`. The IPC primitive copies the bytes through kernel memory rather than mapping the same physical pages (consequence of the pivot above).
- [x] A buffer lifetime model is documented: the client must not modify the buffer between `CommitSurface` and `BufferReleased`; `display_server` emits `BufferReleased` when the state-machine `SurfaceEffect::ReleaseBuffer(buffer_id)` fires (matched against `pending_buffer` first, then `committed_buffer`).
- [ ] **Deferred:** at least one allocation test proves a client and `display_server` can observe the same pixel data **without copies**. The current implementation copies via the IPC bulk path; demonstrating zero-copy requires the kernel-side capability-transfer addition. Leaving this bullet unchecked deliberately so the gap is visible.
- [x] Lifetime invariants are codified as unit tests on a pure-logic refcount state machine in `kernel-core::display::buffer` (`BufferLifecycle`); these are tests-first.
- [ ] **Deferred (G-track):** page-grant leak regression — kill a client mid-commit, observe `SurfaceDestroyed` + `BufferReleased` within the next dispatch cycle. The pure-logic state-machine path is verified; the runtime test lands with G.1 / G.5.
- [x] The transport is explicitly **not** a DMA-BUF or GPU-aware path; the inline-bulk pivot keeps the design consistent with `docs/appendix/gui/wayland-gap-analysis.md` § 1. H.1 will record this in the learning doc.

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

**Status:** Complete (PR #122)

**Acceptance:**
- [x] `userspace/display_server` builds with `needs_alloc = true` in the xtask `bins` table, declares `syscall-lib` with the `alloc` feature, and installs `BrkAllocator` as the global allocator
- [x] The binary is embedded in the ramdisk via `include_bytes!` and `BIN_ENTRIES` in `kernel/src/fs/ramdisk.rs`
- [x] A `display_server.conf` entry is added to the ext2 data disk builder in `xtask/src/main.rs::populate_ext2_files`, and the service name is listed in `userspace/init/src/main.rs::KNOWN_CONFIGS`
- [x] After `cargo xtask clean && cargo xtask run`, `display_server` appears as a running process under `init` supervision — pre-push smoke run shows `display_server: starting (Phase 56 — C.1+C.2)` and `display_server: registered as 'display'`.
- [x] The scaffolded `program_main` writes a banner to stdout, creates an IPC endpoint, and registers itself in the service registry as `"display"` — no graphical behavior yet

### C.2 — Framebuffer acquisition and exclusive presentation

**Status:** Complete (PR #122)

**Files:**
- `userspace/display_server/src/fb.rs` (new)
- `userspace/display_server/src/main.rs`
- `kernel-core/src/display/fb_owner.rs` (new — trait + test double)

**Symbol:** `FramebufferOwner` (trait), `KernelFramebufferOwner` (real impl), `RecordingFramebufferOwner` (test double), `acquire_primary_output`
**Why it matters:** The whole phase rests on `display_server` owning the primary framebuffer. This task wires the B.1 acquisition syscall to a typed owner inside the service, and — critically — exposes the owner as a **trait** so the composer (C.4) and every compose-related test can run against a recording test double on the host. Without the trait seam, compose math can only be exercised in QEMU.

**Acceptance:**
- [x] Tests commit before implementation (contract tests for `FramebufferOwner` exist and fail first, then pass)
- [x] `FramebufferOwner` is a trait in `kernel-core::display::fb_owner` with methods `metadata() -> FbMetadata`, `write_pixels(rect, src, src_stride) -> Result<(), FbError>`, and `present() -> Result<(), FbError>` (the flush/commit point if any backend needs it); both `KernelFramebufferOwner` (real) and `RecordingFramebufferOwner` (test double that stores damage rects and pixel hashes) implement it
- [x] A contract-test harness in `kernel-core` runs an identical test suite against both impls — `RecordingFramebufferOwner` exercises the suite via `recording_owner_passes_contract_suite`; `KernelFramebufferOwner` mirrors the same clipping rules byte-for-byte. Per-impl invocation against the kernel impl deferred until a QEMU-side harness exists.
- [x] `KernelFramebufferOwner` caches the framebuffer metadata (width, height, stride, pixel format) and uses volatile writes with explicit bounds checks that clip `rect` to the framebuffer extents (preventing OOB writes even on malformed damage input) — i64 arithmetic guards against `i32::MAX` adversarial inputs.
- [x] On startup, `display_server` calls `sys_fb_acquire` exactly once; if it returns `EBUSY`, the service retries with bounded backoff up to a configured limit and then exits nonzero with a named error reason — `acquire_framebuffer_with_backoff` retries 8 times × 5 ms.
- [x] Initial presentation on startup draws a known background color across the full framebuffer so that the ownership handoff is visually unambiguous during bring-up and manual testing — `0x002B_5A4B` (deep teal); recorded for the H.1 manual smoke section.
- [x] A shutdown path calls `sys_fb_release()` on normal exit; on panic the kernel reclaims ownership (validated in F.2) — `KernelFramebufferOwner::release` and `Drop` both invoke `framebuffer_release()`; F.2 regression test pending.
- [ ] An integration smoke test confirms `display_server` can fill the framebuffer with a solid color on startup and clear it on shutdown using `KernelFramebufferOwner` — *manual smoke confirmed via pre-push hook (`display_server: framebuffer acquired` + `[INFO] [framebuffer_mmap] pid=13 mapped 1000 pages`); automated regression deferred to G-track.*

### C.3 — Surface state machine

**Status:** Complete (PR #123). Pure-logic core in `kernel-core::display::surface` (PR #122); userspace shim `userspace/display_server/src/surface.rs` lands in PR #123.

**Files:**
- `kernel-core/src/display/surface.rs` (PR #122)
- `userspace/display_server/src/surface.rs` (new — PR #123)

**Symbol:** `SurfaceStateMachine`, `SurfaceRegistry`, `ServerSurface`, `CommittedBuffer`, `SurfaceEvent`, `SurfaceEffect`, `MAX_PENDING_BULK = 4`
**Why it matters:** A surface is the compositor's atomic unit of client-provided pixels. Without a state machine that distinguishes *attached*, *committed*, *sampled*, and *released*, tearing, use-after-free, and double-commit bugs become structural rather than testable. Keeping the state machine as pure logic in `kernel-core` makes these invariants verifiable on the host.

**Acceptance:**
- [x] Tests commit before implementation; the initial test-only commit defines the invariants below as failing tests before any impl lands
- [x] `SurfaceStateMachine` lives in `kernel-core::display::surface` as a pure-logic type that consumes input events (`AttachBuffer`, `DamageSurface`, `CommitSurface`, `DestroySurface`) and emits output effects (`ReleaseBuffer(BufferSlot)`, `EmitDamage(Rect)`, `NotifyLayoutRemoved`)
- [x] `SurfaceStateMachine` tracks: unique id, role (`Toplevel` | `Layer` | `Cursor`), current committed buffer slot, pending buffer slot, pending damage rectangles (with resource bound on rect-count; overflow coalesces), geometry, focus state
- [x] Unit tests cover at minimum: commit-with-no-attach is a typed error, double-attach replaces the pending slot without releasing, double-commit discards the older pending, damage accumulates across `DamageSurface` calls, destroy releases the current buffer exactly once and emits `NotifyLayoutRemoved`, destroy of a surface with a pending-but-uncommitted buffer releases both slots
- [x] A `proptest` property test drives arbitrary event sequences and asserts: at most one `ReleaseBuffer` per buffer-slot ever emitted (no double-free), no `ReleaseBuffer` for a slot never attached, dead surfaces accept no further events except being queried
- [x] `userspace/display_server/src/surface.rs` is a thin shim that maps protocol messages (A.3) to state-machine events and wires effects to the client and layout modules; no state logic lives in the userspace shim. Notably the round-3 review forced a clean split: `SurfaceConfigured` is now produced by forwarding the kernel-core `SurfaceEffect::Configured`, not by shim-side serial generation. `pending_bulk` is bounded (`MAX_PENDING_BULK = 4`) and entries are removed by `BufferId` search, never LIFO `pop()`. A typed `SurfaceShimError::PendingBulkIdMismatch { expected, pending }` distinguishes "no bulk" from "wrong id" — round-2 review feedback.

### C.4 — Damage-tracked software composer

**Status:** Complete (PR #123). Pure-logic core in `kernel-core::display::compose` (PR #122); `display_server` wiring `compose.rs` lands in PR #123.

**Files:**
- `userspace/display_server/src/compose.rs` (new — PR #123)
- `kernel-core/src/display/compose.rs` (PR #122)

**Symbol:** `run_compose`, `default_layout`, `compose_frame`, `ComposeSurface`, `ComposeLayer`
**Why it matters:** A naive compositor that redraws the full framebuffer every tick burns CPU for no visible benefit. Damage tracking is the difference between a software composer that is comfortable at 1080p60 and one that is not — see the bandwidth table in `docs/appendix/gui/tiling-compositor-path.md` § Composition cost.

**Acceptance:**
- [x] On each frame tick, the composer walks surfaces in layer order (`Background < Bottom < Toplevel < Top < Overlay < Cursor`) and blits damaged regions only — implemented by `compose_frame` in `kernel-core::display::compose`, called by `display_server::compose::run_compose` once per frame-tick.
- [x] Surface geometry is supplied to the composer per frame: `display_server::compose::run_compose` calls `LayoutPolicy::arrange()` (currently `FloatingLayout`) for `Toplevel` candidates and uses `SurfaceRegistry::iter_compose()` to centre each surface in the output rectangle. `Layer` anchor/exclusive-zone logic and `Cursor` pointer-position logic land with E.2 / E.3 — Phase 56 has only `Toplevel` clients in flight.
- [x] Damage rectangles are clipped to the output bounds and to the visible region of the surface (kernel-core `compose_frame`)
- [ ] Alpha blending is supported for `Cursor` and `Layer` surfaces; `Toplevel` surfaces are assumed opaque in Phase 56 — *blending lands with E.3 (cursor) and E.2 (layer roles); current composer is opaque-copy.*
- [x] If no surface reported damage on a tick, the composer performs no framebuffer writes — `SurfaceRegistry::has_damage()` gates entry to `compose_frame`, and the kernel-core core itself asserts zero `write_pixels` calls when surfaces report empty damage. The `present()` call is owned by `compose_frame`, not by the userspace caller (round-3 review fix).
- [x] Tests commit first; unit tests in `kernel-core::display::compose` cover at minimum: (a) damage rectangle union/intersection math, (b) layer-order traversal returns surfaces in the documented order, (c) clip-to-output correctly rejects an off-screen surface, (d) an opaque toplevel fully covered by a higher-layer surface is skipped, (e) zero-damage tick yields zero framebuffer writes
- [x] A `proptest` property test drives arbitrary `(surfaces, damage, output)` inputs and asserts: composed output exactly covers the union of (visible) damage rectangles clipped to the output — no pixels outside, no pixels inside the visible damage union skipped
- [x] The composer consumes the `FramebufferOwner` trait (C.2) and the `LayoutPolicy` trait (A.7/E.1), never a concrete type; the same compose code runs against `RecordingFramebufferOwner` on the host and `KernelFramebufferOwner` in QEMU
- [x] Software-only is explicit: no GL/GLES2 code paths; aligns with `docs/appendix/gui/wayland-gap-analysis.md` scope

### C.5 — Client connection handshake and event loop

**Status:** Complete-by-pivot (PR #123). Phase 56 ships **IPC-endpoint** transport rather than AF_UNIX streams per the task doc's "AF_UNIX (or IPC)" foundation note — protocol types in `kernel-core::display::protocol` are transport-agnostic, so a future swap is a wiring change in `client.rs` alone.

**Files:**
- `userspace/display_server/src/client.rs` (new — PR #123)
- `userspace/display_server/src/main.rs` (PR #123: single-threaded event loop)

**Symbol:** `dispatch`, `DispatchOutcome`, `InboundFrame`, `LABEL_VERB = 1`, `LABEL_PIXELS = 2`, `BYTES_PER_PIXEL_BGRA8888 = 4`, `PIXEL_BULK_HEADER_LEN = 8`, `MAX_BULK_BYTES = 4096`
**Why it matters:** Clients must be able to connect, receive focus/input events, submit surfaces, and have a clean disconnect path. This task stitches Track A's protocol onto the C.1–C.4 machinery.

**Acceptance:**
- [x] `display_server` listens on its IPC `display` endpoint (registered via `ipc_register_service("display")`). The AF_UNIX stream-socket path is recorded as the future-target transport in A.3 / H.1; the in-tree dispatcher is `LABEL_VERB` / `LABEL_PIXELS` over IPC-bulk per the doc's allowed pivot.
- [x] A `Hello` handshake echoes a `Welcome { protocol_version, capabilities }` from `dispatch`; the protocol-version mismatch path is wired (the dispatcher reads `protocol_version` and the future tightening would compare against `kernel_core::display::protocol::PROTOCOL_VERSION`).
- [x] Per-client state tracks owned surfaces via `SurfaceRegistry` and pending pixel bulks via `pending_bulk`. Phase 56 has one connected client at a time; the multi-client partitioning lands with C.5's follow-up alongside the AF_UNIX transition.
- [x] Client-to-server messages in A.3 dispatch to the surface state machine (C.3) and the layout policy (E.1) via `SurfaceRegistry::handle_message` + `run_compose`.
- [ ] Server-to-client events in A.3 are serialized with backpressure: a slow client does not block other clients or the composer — *Phase 56 single-client demo: outbound `ServerMessage`s are logged but not yet transmitted; the per-client send-cap path lands with multi-client + AF_UNIX. The `DispatchOutcome.outbound` shape is the seam.*
- [ ] Client disconnect (explicit `Goodbye`, EOF, or process exit) releases all surfaces owned by the client — *`Goodbye` is dispatched and resets the registry; EOF / exit-on-IPC-loss requires the AF_UNIX transition to detect.*
- [ ] At least two concurrent clients are supported — *deferred to multi-client work; Phase 56 ships a single-client demo (`gfx-demo`).*
- [x] Protocol framing is consumed through the A.0 codec exclusively (`ClientMessage::decode`); `client.rs` contains no hand-written field extraction. Round-3 review tightened this to require `consumed == bulk.len()` (no trailing bytes) — `BodyLengthMismatch` on violation.
- [ ] A fuzz-style robustness test (driven by `proptest` over arbitrary `Vec<u8>` frames) feeds the dispatcher — *the kernel-core protocol codec carries the corrupted-framing proptest from A.0; a `dispatch`-level fuzz harness lands when `client.rs` is split into a host-testable lib (the dead `#[cfg(test)] mod tests` placeholder was removed in round 3 because `display_server` is `no_std` + `no_main`).*
- [x] Per-client resource bounds: `MAX_PENDING_BULK = 4` (in `surface.rs`), `MAX_BULK_BYTES = 4096` (matches kernel `MAX_BULK_LEN`), bulk-vs-geometry mismatch closes the connection as a fatal protocol violation. Outbound-event-queue bound lands with the multi-client transmission path.

### C.6 — `gfx-demo` protocol-reference client

**Status:** Complete (PR #123). Visual-smoke aspects (cursor visibility, event echo, screenshot) gate on D + E and are explicit follow-ups.

**Files:**
- `Cargo.toml` (PR #123: `userspace/gfx-demo` workspace member)
- `xtask/src/main.rs` (PR #123: `gfx-demo` in `bins` with `needs_alloc = true`, plus `gfx-demo.conf` in `populate_ext2_files`)
- `kernel/src/fs/ramdisk.rs` (PR #123: `include_bytes!` + `BIN_ENTRIES`)
- `userspace/init/src/main.rs::KNOWN_CONFIGS` (PR #123)
- `userspace/gfx-demo/Cargo.toml` (new — PR #123)
- `userspace/gfx-demo/src/main.rs` (new — PR #123)

**Symbol:** `program_main`, `lookup_display_with_backoff`, `send_message`, `send_pixels`, `LABEL_PROTOCOL = 1`, `LABEL_PIXELS = 2`, `DEMO_FILL_BGRA = 0x00FF_8800`, `DEMO_W/H = 16`
**Why it matters:** Every Track A/C/D/E piece is exercised by `cargo xtask test`, but without a shipped graphical client a learner running `cargo xtask run-gui --fresh` sees only the C.2 background fill and the E.3 default arrow cursor — no toplevel, no proof the protocol works end-to-end at runtime. `gfx-demo` is a deliberately minimal visual-smoke client: a colored `Toplevel` surface, a cursor that renders above it, and input events echoed to serial. It is **not** a terminal, launcher, or useful app — it is a protocol reference and a manual-smoke target. Phase 57's terminal emulator is categorically different work (PTY integration, font rendering, scrollback) and can either retire `gfx-demo` or keep it as a reference client without entanglement.

**Acceptance:**
- [ ] Tests commit before implementation — *the demo's protocol handshake path is exercised indirectly through the A.0 codec round-trip tests (which have host coverage); a `gfx-demo`-specific encode test was not added because the demo is itself the test surface for the encode → IPC round-trip.*
- [x] `userspace/gfx-demo` follows the four-step new-binary convention: workspace member, xtask `bins` entry with `needs_alloc = true`, ramdisk embedding, `KNOWN_CONFIGS` registration, `gfx-demo.conf` (`name=gfx-demo command=/bin/gfx-demo type=daemon restart=on-failure max_restart=3 depends=display`).
- [x] The binary connects to `display_server` via `ipc_lookup_service("display")` (with bounded retry), performs `Hello { PROTOCOL_VERSION, 0 }`, creates a `Toplevel` surface via `CreateSurface` + `SetSurfaceRole(Toplevel)`, fills a 16×16 BGRA `SurfaceBuffer` with `0x00FF_8800` (orange), ships the pixel bulk over `LABEL_PIXELS` with the bulk-header geometry, then `AttachBuffer` + `CommitSurface`. **Note:** the original spec said "AF_UNIX stream"; ships over the C.5 IPC-endpoint dispatcher instead per the task-level pivot. Demo size is 16×16 (1024 bytes) rather than 32×32 because the bulk-header geometry costs 8 bytes of the 4096-byte `MAX_BULK_LEN` (round-2/3 fix).
- [ ] After configuration, the demo enters an event loop that prints every inbound `KeyEvent` and `PointerEvent` — *Phase 56 ships only output: server→client event delivery is the C.5 multi-client follow-up. The demo idles after the protocol round-trip; key/pointer echo lands when D.1/D.2 services + C.5 transmission ship.*
- [ ] Cursor movement is visible because the demo relies on E.3's `DefaultArrowCursor` — *deferred to E.3.*
- [ ] The demo exits cleanly on `Goodbye` from the server or on EOF — *`Goodbye` is sent by the demo only on the explicit shutdown path that Phase 56 doesn't yet exercise; EOF detection lands with the AF_UNIX transition.*
- [x] The demo contains **no** `unwrap`/`expect`/`panic!` outside of documented fail-fast initialization. Every fallible call returns a typed error (`SurfaceBufferError`, `u64::MAX` IPC errors) and is explicitly logged via `syscall_lib::write_str`.
- [x] The service manifest (`gfx-demo.conf`) starts one instance after `display_server` in the F.1 startup order; restart policy is `on-failure` with `max_restart=3`.
- [ ] The crate is documented in H.1 — *deferred to H.1.*
- [ ] A screenshot or recorded terminal transcript is attached to the Phase 56 PR — *deferred to G.7 once the C.4 → render path emits visible pixels (currently the demo ships the protocol round-trip; pixel-on-screen verification gates on the manual-smoke checklist landing alongside G.7).*

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

**Status:** Trait + contract suite + `FloatingLayout` + `StubLayout` landed in PR #122 (`kernel-core::display::layout`); display_server wiring + factory pending.

**Files:**
- `kernel-core/src/display/layout.rs` (new — trait + contract test harness + `FloatingLayout` + `StubLayout`)
- `userspace/display_server/src/layout/mod.rs` (new — re-export and wiring)

**Symbol:** `LayoutPolicy` (trait), `FloatingLayout`, `StubLayout`, `layout_contract_suite`, `arrange`
**Why it matters:** A.7 specified the contract; E.1 delivers the trait plus the minimum-viable default. The tiling-first engine lands later; Phase 56 just proves the seam works. A **shared contract-test suite** runs against every `LayoutPolicy` impl (present and future) so Liskov-substitutability is enforced by code, not by reviewer vigilance.

**Acceptance:**
- [x] Tests commit before implementation; the contract suite is written against the trait before any impl lands
- [x] The trait signature matches A.7's specification exactly and lives in `kernel-core::display::layout`
- [x] A public `layout_contract_suite<P: LayoutPolicy>(construct: impl Fn() -> P)` runs an identical behavioral test suite against any impl; it is invoked once per impl in `kernel-core` tests (`FloatingLayout`, `StubLayout`) and will be invoked by the future tiling layout crate without modification
- [x] The contract covers at minimum: empty-toplevel-list produces an empty arrangement; adding a toplevel produces exactly one rect inside the output minus exclusive zones; removing the most recently added toplevel returns the arrangement to its prior state; arrange is deterministic (identical inputs → identical outputs); no returned rect overlaps an exclusive zone unless the output cannot fit otherwise (documented degenerate case); focus changes do not change returned geometry for impls where they aren't supposed to (opt-in via a trait-level `focus_affects_geometry()` helper if needed)
- [x] `FloatingLayout` places each new `Toplevel` at an output-centered default size with a small cascade offset; `StubLayout` returns rects from a pre-loaded script for test determinism
- [ ] Swappability is real: the policy is constructed once at startup through a named factory function; the compositor consumes `&mut dyn LayoutPolicy`, never a concrete type — *trait already supports this; display_server-side wiring pending.*
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

**Status:** `display_server.conf` shipped in PR #122 (with `depends=kbd, restart=on-failure, max_restart=5`); `kbd_server.conf` extension and `mouse_server.conf` pending alongside D.1 / D.2.

**Files:**
- `userspace/init/src/main.rs`
- `xtask/src/main.rs::populate_ext2_files` (service conf files)

**Symbol:** `service_record`, `restart_policy`
**Why it matters:** The phase's service-model baseline (Phase 46/51) must actually supervise the graphical stack; otherwise a crash leaves the system without pixels.

**Precedent:** Phase 55b (Track F.1) landed two concrete ring-3-service manifests — `etc/services.d/nvme_driver.conf` and `etc/services.d/e1000_driver.conf` — embedded through `xtask/src/main.rs::populate_ext2_files` and registered in `userspace/init/src/main.rs::KNOWN_CONFIGS`. Both use `restart=on-failure, max_restart=5, type=daemon`. Phase 56's three service manifests should mirror that shape; differences (e.g. an `on-restart` verb for `display_server` that re-acquires the framebuffer) are Phase 56-specific extensions on top of the baseline.

**Acceptance:**
- [ ] `kbd_server`, `mouse_server`, and `display_server` all have service records with explicit startup order (`kbd_server` and `mouse_server` before `display_server`) and restart policies — *display_server.conf shipped with `depends=kbd`; kbd/mouse manifests pending.*
- [ ] `display_server` has an `on-restart` policy that re-acquires the framebuffer via B.1 (retry with bounded backoff) and re-establishes the control socket — *bounded-backoff acquire is implemented in display_server; supervisor-level on-restart wiring pending.*
- [ ] Input services emit a one-time log on startup identifying which input endpoint they will target on `display_server` (useful for diagnosing reordering during the session bringup)
- [ ] The boot-log evidence that the three services are live at the expected point in boot is captured (e.g. a test harness reads the service-manager status)
- [x] Manifest-shape consistency with Phase 55b's `nvme_driver.conf` / `e1000_driver.conf` — same keys (`name`, `command`, `type`, `restart`, `max_restart`), same ext2-embedding pattern, same `KNOWN_CONFIGS` registration — display_server.conf mirrors the precedent shape exactly.

### F.2 — Display-service crash recovery

**Files:**
- `userspace/display_server/src/main.rs`
- `kernel/src/fb/mod.rs`
- `userspace/init/src/main.rs`

**Symbol:** `on_display_server_death`
**Why it matters:** The design doc's acceptance criterion explicitly allows either "recoverable" *or* "failure-mode-and-recovery-path documented and testable." The recovery path has to be real, not rhetorical.

**Precedent:** Phase 55b (Tracks F.2b / F.3b / F.3d) established the template for a crash-and-restart regression:
- A guest shell command `service kill <name>` (delivered in F.2b, `userspace/coreutils-rs/src/service.rs`) SIGKILLs a named service via its PID from `/run/services.status`.
- `cargo xtask regression --test driver-restart-guest` (F.2b) boots, kills a supervised driver, observes the `init: started '<name>' pid=` re-registration, and asserts post-restart status.
- `cargo xtask regression --test max-restart-exceeded` (F.3d-1, gated `M3OS_ENABLE_CRASH_SMOKE`) validates the `max_restart` cap transition to `permanently-stopped`.
- A small guest binary (`userspace/nvme-crash-smoke/`, F.3b) issues pre-crash work, invokes `service kill`, observes mid-crash transport failure, polls for restart, retries, and asserts post-restart correctness.

Phase 56 should model its `display_server` crash regression on this shape — a guest binary that opens a client socket, triggers the debug `panic!` verb, observes the socket closing cleanly, waits for restart, and reconnects. The xtask harness pattern is reusable verbatim; only the guest-binary logic is new.

**Acceptance:**
- [ ] When `display_server` exits (crash or clean shutdown without `sys_fb_release`), the kernel reclaims the framebuffer and resumes the kernel console so the system is not left with a dead screen
- [ ] The init/service-manager restarts `display_server` within a bounded number of attempts; exceeding the cap triggers a documented fallback (serial shell remains usable, kernel console is active)
- [ ] Clients connected to `display_server` see their socket close cleanly and are responsible for reconnecting; no client-side crashes are required
- [ ] A regression test triggers a `display_server` crash (e.g. via a debug `panic!` gated behind a test-only verb OR via `service kill display_server` following the Phase 55b precedent), confirms the kernel console returns, confirms the service manager restarts `display_server`, and confirms a new client can connect after restart
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
- [ ] `m3ctl frame-stats` returns a non-empty sample window with strictly-increasing frame indices and per-sample composition durations greater than zero — confirming the observability verb surfaces real data rather than a placeholder
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

### G.7 — Interactive `run-gui` smoke validation

**Files:**
- `docs/56-display-and-input-architecture.md` (learning doc — "Manual smoke validation" section added by H.1)
- `userspace/gfx-demo/` (exercised target)

**Symbol:** `run_gui_smoke`
**Why it matters:** `cargo xtask test` exercises the compositor through pixel-sampling harnesses and control-socket introspection, but "a human can boot the image and see a working compositor" is a separate signal that CI cannot produce. This task is the manual counterpart to G.1–G.6 and is the first thing a learner or reviewer does after `cargo xtask run-gui --fresh`. Codifying the expected visible state prevents the QEMU boot from silently regressing into "no toplevel, no cursor motion" while CI still passes.

**Acceptance:**
- [ ] The learning doc's "Manual smoke validation" section (H.1) lists the exact command `cargo xtask run-gui --fresh` and the exact expected visible state: solid background color (named), default arrow cursor visible, one `gfx-demo` toplevel with the named color present, cursor moves in response to PS/2 mouse input, key presses produce serial-log event-echo lines from `gfx-demo`
- [ ] The section lists the exact serial-log signatures that confirm each supervised service reached a healthy state: `display_server` banner + framebuffer acquisition log line, `kbd_server` banner + IRQ1 attach, `mouse_server` banner + IRQ12 attach, `gfx-demo` banner + `SurfaceConfigured` receipt
- [ ] The section lists the exact `m3ctl` commands a tester runs to confirm the control socket is live: `m3ctl version`, `m3ctl list-surfaces` (shows the `gfx-demo` toplevel), `m3ctl frame-stats` (non-empty window)
- [ ] The section records known-acceptable visual artifacts (e.g. tearing under rapid motion per the Documentation Notes line) so testers do not file them as regressions
- [ ] The PR that closes Phase 56 attaches at minimum a serial-log transcript demonstrating the above; a screenshot of the QEMU framebuffer is encouraged when practical
- [ ] A one-page checklist version of the smoke steps lives in the learning doc so a future reviewer can re-run it without re-reading the whole phase

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
- [ ] Key files table lists all new modules introduced in Phase 56 (`userspace/display_server`, `userspace/mouse_server`, `userspace/m3ctl`, `userspace/gfx-demo`, `kernel-core/src/input/{keymap,mouse}.rs`, `kernel-core/src/display/{compose,frame_tick}.rs`)
- [ ] A "Manual smoke validation" section satisfies every bullet of G.7 — the exact `cargo xtask run-gui --fresh` command, the exact expected visible state, the exact serial-log signatures for each supervised service, the `m3ctl` verbs that prove the control socket is live, and a one-page checklist testers can re-run
- [ ] A "Protocol-reference demo" subsection documents `gfx-demo`'s role: minimal visual-smoke client, not a product; names the solid color it fills so testers know exactly what they are looking at; records that Phase 57's terminal emulator is the real graphical client and that `gfx-demo` may be retired or retained as a reference at Phase 57's discretion
- [ ] Resource-bound defaults referenced by the Engineering Discipline section (per-client surface count, in-flight buffer count, outbound event-queue depth) are written down in this doc with their initial numeric values
- [ ] Accepted Phase 56 limitations are called out explicitly: tearing under motion (no back-buffer), US-QWERTY-only keymap, PS/2 mouse only, software-only composition
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
- [ ] `cargo xtask check` passes on the final Phase 56 branch — clippy with `-D warnings`, rustfmt, and the `kernel-core` host-side unit tests all green; evidence is attached to the closing PR (CI run link or locally-captured output)
- [ ] `cargo xtask test` passes on the final Phase 56 branch — all Phase 56 QEMU regressions (G.1–G.6) green within their declared timeouts
- [ ] The Phase 56 pre-commit and pre-push hooks from `.githooks/` ran on every commit in the branch history (evidence: no commit bypasses `--no-verify`); the PR description confirms

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
- **Relationship to Phases 55a, 55b, and 55c.** The mouse path (B.2) still uses PS/2 AUX (IRQ12) in ring 0, the surface-buffer transport (B.4) still uses Phase 50 page grants rather than hardware DMA, and the framebuffer handoff (B.1) still targets the bootloader-provided linear framebuffer rather than a DRM/KMS device. So 55a's IOMMU-DMA work and 55b's PCIe driver-host extraction remain contextual precedent more than direct implementation surfaces for Phase 56. Phase 55c is adjacent future precedent rather than a hard prerequisite: its bound-notification closure is relevant for later IRQ-backed display/input drivers, but the Phase 56 compositor core documented here remains a socket-centric design built around AF_UNIX and the existing notification-wait path.

  **Status update (post-Phase-55c planning):** Phase 55a and 55b are now both landed (v0.55.2). Phase 56 still pulls two *soft* precedents from Phase 55b, both reflected in the F.1 / F.2 tasks above: (a) the `etc/services.d/*.conf` manifest shape from `nvme_driver.conf` / `e1000_driver.conf`, and (b) the `service kill <name>` + `cargo xtask regression --test driver-restart-guest` crash-regression harness. Phase 55c's bound-notification work is relevant as a later template for IRQ-backed userspace drivers, but it is not a hard prerequisite for the socket-centric Phase 56 compositor core documented here.

- **`driver_runtime` API as future template.** Phase 56's three services (`display_server`, `kbd_server`, `mouse_server`) do not own PCIe hardware and therefore do not consume `userspace/lib/driver_runtime/` directly. When a later phase adds a USB HID driver or a GPU/display-engine driver — both explicitly deferred to post-56 phases — that driver should adopt the Phase 55b `driver_runtime` API shape (`DeviceHandle`, `Mmio<T>`, `DmaBuffer<T>`, `IrqNotification`, `BlockServer` / `NetServer` IPC helper pattern) rather than reinvent the capability-gated hardware-access surface. The Phase 55b learning doc at `docs/55b-ring-3-driver-host.md` documents this template stability promise.
- **`gfx-demo` is a protocol-reference demo, not a product.** C.6 ships a minimal visual-smoke client (`userspace/gfx-demo/`) so `cargo xtask run-gui --fresh` produces a visible toplevel + cursor + event-echo at the end of Phase 56. It is deliberately not a terminal, launcher, or useful app — Phase 57 owns the real graphical-client story (terminal emulator + PTY bridge + font rendering + session entry). `gfx-demo` and the Phase 57 terminal occupy different layers and do not compete; Phase 57 may retire `gfx-demo` or retain it as an in-tree protocol reference at its discretion.
- **Phase 57 prerequisite posture.** Phase 56 as scoped here satisfies the Phase 57 "display/session baseline" evaluation gate: the four Goal-A contract points (A.5/A.6/A.7/A.8), supervised services (F.1), crash recovery (F.2), text-mode fallback (F.3), the page-grant surface-buffer transport (B.4), and the post-keymap symbol + modifier input model (A.4/D.1) are all the client-facing surface the Phase 57 terminal needs. Audio (new subsystem), font rendering, and higher-level session semantics (login-to-graphical, launcher) are Phase 57's work and are explicitly out of scope here.
- **Goal-A contract points are explicit.** The four design decisions from `docs/appendix/gui/tiling-compositor-path.md` (swappable layout module, keybind grab hook, layer-shell-equivalent role, control socket) are delivered by A.7/E.1, A.5/D.4, A.6/E.2, and A.8/E.4 respectively. Each contract point ships a trait / role / hook / socket in Phase 56; the tiling-specific *implementations* built on top of them (tiling layout engine, chord engine, bar/launcher clients) ship in Phase 56b / 57b and are explicitly out of scope here.
- **Explicit non-Wayland framing.** The client protocol (A.3) is m3OS-native, not Wayland. `docs/appendix/gui/wayland-gap-analysis.md` Path A (`wl_shm` adapter) is not in Phase 56 scope and is only reachable as an *additive* phase after Phase 56 lands.
- **Mouse scope is narrow.** Phase 56 ships PS/2 AUX motion + 3 buttons + optional wheel. Touchpad gestures, tablet/pen input, touch, and USB HID breadth are all deferred.
- **Keymap scope is narrow.** Phase 56 ships US QWERTY with the five standard modifiers. International layouts, IME, dead keys, and compose sequences are deferred.
- **Software-only composition.** Phase 56 has no GL/GLES2/EGL/DRM code paths. Hardware-accelerated composition and live blur effects are deferred; see `docs/appendix/gui/tiling-compositor-path.md` § Software-only rendering budget for the bandwidth math that motivates this trade.
- **Pure-logic code belongs in `kernel-core` where practical.** Keymap translation (D.1), mouse decoding (B.2), compose math (C.4), and frame-tick metadata (B.3) are host-testable; put them in `kernel-core` and test them with `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`. Hardware-dependent wiring (syscalls, MMIO, ISR registration) belongs in `kernel/src/`.
- Host-side tests should use `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`; QEMU-driven integration tests use `cargo xtask test`.
