# Phase 56 - Display and Input Architecture

**Status:** Planned
**Source Ref:** phase-56
**Depends on:** Phase 46 (System Services) ✅, Phase 52 (First Service Extractions) ✅, Phase 47 (DOOM) ✅, Phase 55 (Hardware Substrate) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅
**Builds on:** Turns the single-app DOOM graphics proof into a real userspace-owned display/input architecture with explicit ownership, event routing, and crash boundaries
**Primary Components:** future userspace display server, input services, kernel/src/fb, kernel input/interrupt mediation, docs/09-framebuffer-and-shell.md, docs/29-pty-subsystem.md

> **Note on Phase 55c:** Phase 55c is not a hard prerequisite for Phase 56. The Phase 56
> compositor core is socket-centric and does not require the Phase 55c bound-notification
> primitives (`RecvResult`, `IrqNotification::bind_to_endpoint`, `sys_notif_bind`). The
> Phase 55c pattern serves as a later template for any IRQ-backed userspace driver
> introduced in Phase 56 or beyond (such as a future vsync or HID interrupt driver that
> genuinely mixes async hardware events with sync IPC requests). PS/2 and initial input
> services introduced in Phase 56 keep their existing wait/send split; the Phase 55c
> pattern is opt-in.

## Milestone Goal

m3OS gains a real display and input model: one userspace-owned display service controls presentation, keyboard and mouse events are routed through an explicit focus-aware protocol, and multiple graphical clients can coexist without raw framebuffer conflicts.

## Why This Phase Exists

A desktop is not "framebuffer plus mouse." It is a policy system for ownership, composition, input routing, focus, and recovery. The DOOM milestone proves that pixels can be drawn by a real graphical userspace program; it does not solve how multiple applications share the display or how the system recovers from UI-service failures.

This phase exists to design and implement the first genuine graphical architecture for m3OS.

## Learning Goals

- Understand how a userspace display server separates kernel mechanism from GUI policy.
- Learn how keyboard and mouse events become routable, focus-aware input streams.
- See why display ownership, buffer exchange, and window/application lifecycle must be solved together.
- Understand how a microkernel-style display architecture improves fault isolation and future GUI clarity.

## Feature Scope

### Display ownership and composition

One userspace display service owns presentation and arbitrates which client surfaces are visible. The framebuffer should no longer be a free-for-all once multiple graphical clients exist.

### Input event model

Define the keyboard and mouse event model that later GUI clients consume. Mouse support is part of this phase only to the degree needed for the first coherent display/input architecture.

### Client protocol and buffer exchange

Clients need a documented way to submit surfaces or damage updates and receive input/focus changes. The protocol should reuse the earlier IPC and buffer-sharing groundwork instead of inventing graphics-only escape hatches.

### Session and recovery behavior

The graphical stack must fit the existing service model. This includes startup, focus ownership, recovery after crashes, and the rules for falling back to text-mode administration when needed.

### Goal-A contract points (tiling-first compositor preconditions)

[`docs/appendix/gui/tiling-compositor-path.md`](../appendix/gui/tiling-compositor-path.md) identifies four design decisions that Phase 56 must make early so a later keyboard-driven tiling compositor (Goal A — the omarchy/Hyprland *experience* on the Phase 56 substrate, without GPU dependencies and without Wayland) can land as additive policy code rather than a protocol rewrite. Phase 56 delivers the contract points only; the tiling-specific implementations (layout engine, chord engine, workspace state machine, native bar/launcher/lockscreen clients, animation engine) are explicitly Phase 56b / 57b / 57c work.

Each contract point below names the Phase 56 tasks that design it (Track A) and deliver it (Tracks D / E), linked into the companion task list.

| Goal-A decision (tiling-compositor-path.md) | Phase 56 contract point | Design task | Implementation task |
|---|---|---|---|
| Layout policy in a swappable module from day one | A `LayoutPolicy` trait plus a default `FloatingLayout`; no toplevel geometry logic lives outside the trait | [A.7 — Swappable layout module contract](./tasks/56-display-and-input-architecture-tasks.md) | [E.1 — `LayoutPolicy` trait + default](./tasks/56-display-and-input-architecture-tasks.md) |
| Keybind grab hook keyed on modifier sets ("swallow before client") | The input dispatcher checks a `(modifier_mask, keycode) → handler` bind table before focus routing; matched `KeyDown` (and paired `KeyUp`/`KeyRepeat`) are not forwarded to any client | [A.5 — Keybind grab-hook semantics](./tasks/56-display-and-input-architecture-tasks.md) | [D.4 — Keybind grab-hook implementation](./tasks/56-display-and-input-architecture-tasks.md) |
| Layer-shell-equivalent surface role | A `Layer` surface role with layer ordering, anchors, exclusive zones, and keyboard-interactivity modes | [A.6 — Layer-shell-equivalent surface roles](./tasks/56-display-and-input-architecture-tasks.md) | [E.2 — `Layer` surface role + anchors/exclusive zones](./tasks/56-display-and-input-architecture-tasks.md) |
| Control socket as a first-class protocol element | A separate AF_UNIX control endpoint with a minimum verb set (`version`, `list-surfaces`, `focus`, `register-bind`, `unregister-bind`, `subscribe`) and a subscribable event stream | [A.8 — Control-socket protocol](./tasks/56-display-and-input-architecture-tasks.md) | [E.4 — Control socket: endpoint, verbs, events](./tasks/56-display-and-input-architecture-tasks.md) |

Delivery of all four contract points is cross-checked by [A.9 — Evaluation Gate verification](./tasks/56-display-and-input-architecture-tasks.md) and recorded in the Phase 56 learning doc (H.1).

**Tiling-first *implementation* is explicitly out of scope for Phase 56.** The tiling layout engine (dwindle / master-stack / BSP / manual / grid), workspace state machines, keybind chord engine (leader keys, per-mode tables, config reload), the `m3ctl`-driven scripting surface beyond the minimum verb set, and the native bar / launcher / notification daemon / lockscreen client implementations are all the realistic next increments on top of the Phase 56 substrate. The Goal-A staging plan in [`docs/appendix/gui/tiling-compositor-path.md`](../appendix/gui/tiling-compositor-path.md) places them in the proposed Phase 56b / 57b / 57c area, not Phase 56.

**Explicitly non-Wayland.** The Phase 56 client protocol is a m3OS-native protocol over AF_UNIX + Phase 50 page grants. No `wl_shm`, no libwayland, no wlroots, no Mesa/llvmpipe. See [`docs/appendix/gui/wayland-gap-analysis.md`](../appendix/gui/wayland-gap-analysis.md) for the sizing of the three Wayland-direction paths (Path A `wl_shm` shim, Path B native compositor, Path C full GPU stack) and why none of them are in Phase 56.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| One userspace process owns display presentation | Without this, the system still has no real display architecture |
| Unified keyboard/mouse event routing | A GUI stack without a real input model is incomplete |
| Documented client protocol | Later apps and toolkit work depend on it |
| Crash/recovery story for the display service | The display stack must be operable, not just impressive |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Graphics bring-up baseline | Phase 47 proves userspace graphics and console handoff already work | Add missing graphics-substrate work here |
| Service-model baseline | Phase 46 and Phase 52 provide a reliable supervision and restart pattern for the UI services | Add missing service integration or restart semantics |
| Hardware/input baseline | The chosen mouse or input path exists on the supported targets or is explicitly pulled into this phase | Add the missing input-driver work rather than leaving it as an implicit dependency |
| Buffer transport baseline | The Phase 50 transport model is strong enough for surface or buffer exchange | Add the missing buffer/grant work needed by the display server |

### Evaluation Gate verification (A.9)

Phase 56 cannot be closed without demonstrating every gate above. This subsection pins the verification action for each gate, names the Phase 56 task that delivers the evidence, and names the regression artifact that carries the proof. Gate verification *results* — pass/fail plus linked evidence — are recorded in the Phase 56 learning doc under H.1's "Evaluation Gate results" subsection before the closing PR lands.

| Gate | Verification action (at phase close) | Delivered by | Evidence / regression |
|---|---|---|---|
| Graphics bring-up baseline | Boot under `cargo xtask run-gui --fresh`, observe `display_server` acquire the framebuffer via `sys_fb_acquire`, confirm the kernel framebuffer console is suspended while `display_server` is alive, and run the Phase 47 DOOM binary (or its regression harness) to prove B.1's ownership transfer did not regress the single-client graphics path. | [B.1 — framebuffer ownership transfer](./tasks/56-display-and-input-architecture-tasks.md) | Serial-log transcript from the manual smoke (G.7) plus the Phase 47 regression harness. |
| Service-model baseline | Confirm `display_server`, `kbd_server`, and `mouse_server` are supervised under `init` with Phase 46/51 manifests mirroring the Phase 55b `etc/services.d/*.conf` shape, and that the restart / crash-recovery paths land as tested code (not documentation only). | [F.1 — service manifests + supervision](./tasks/56-display-and-input-architecture-tasks.md), [F.2 — crash recovery](./tasks/56-display-and-input-architecture-tasks.md) | [G.5 — crash-recovery regression](./tasks/56-display-and-input-architecture-tasks.md) and the boot-log evidence that all three services are live. |
| Hardware / input baseline | Confirm the PS/2 AUX (IRQ12) mouse path is live in the default QEMU configuration on the Phase 55 reference targets, decoder + IRQ handler in place, `mouse_server` drains the ring and emits `PointerEvent` frames, and the Phase 56 keyboard path still functions. | [B.2 — PS/2 mouse path](./tasks/56-display-and-input-architecture-tasks.md), [D.1 — typed key-event publishing](./tasks/56-display-and-input-architecture-tasks.md), [D.2 — `mouse_server`](./tasks/56-display-and-input-architecture-tasks.md) | Serial-log transcript from G.7 showing both `kbd_server: IRQ1 attach` and `mouse_server: IRQ12 attach`, plus cursor-motion echoes in the `gfx-demo` event loop. |
| Buffer-transport baseline | Confirm a Phase 50 page grant produced by a client process (via the B.4 `SurfaceBuffer` helper) is reachable from `display_server`'s address space, the composer samples the client's pixels without a copy, and buffer lifetime (`AttachBuffer` → `CommitSurface` → `BufferReleased` → `DestroySurface`, including abnormal client exit) behaves as the C.3 surface state machine specifies. | [B.4 — cross-process shared-buffer transport](./tasks/56-display-and-input-architecture-tasks.md) | [G.1 — multi-client coexistence regression](./tasks/56-display-and-input-architecture-tasks.md) plus the `gfx-demo` page-grant smoke. |
| Goal-A contract points (A.5 / A.6 / A.7 / A.8) | All four design decisions from the Goal-A contract-points subsection are delivered (`LayoutPolicy` trait + `FloatingLayout`, keybind grab hook, `Layer` surface role with exclusive zone, control socket + minimum verb set) and validated by passing regression tests. | A.5 + [D.4](./tasks/56-display-and-input-architecture-tasks.md), A.6 + [E.2](./tasks/56-display-and-input-architecture-tasks.md), A.7 + [E.1](./tasks/56-display-and-input-architecture-tasks.md), A.8 + [E.4](./tasks/56-display-and-input-architecture-tasks.md) | [G.2 — grab-hook regression](./tasks/56-display-and-input-architecture-tasks.md), [G.3 — layer-shell exclusive-zone regression](./tasks/56-display-and-input-architecture-tasks.md), [G.4 — control-socket round-trip regression](./tasks/56-display-and-input-architecture-tasks.md), plus the `layout_contract_suite` running against `FloatingLayout` in `cargo test -p kernel-core`. |

Gate verification is performed in one sweep against the candidate closing PR. Any gate that cannot be ticked blocks phase close and the missing work is folded into Phase 56 rather than deferred — the task doc's "If missing, add it to this phase" column is binding.

## Important Components and How They Work

### Display service and compositor

The display service owns presentation, composition, and final output. It is the graphical equivalent of a system daemon: a privileged userspace policy engine built on kernel substrate instead of a special in-kernel UI layer.

### Input services and focus routing

Keyboard and mouse input should be mediated by userspace-owned policy that can decide which client receives events, how focus changes, and how recovery works after restarts.

### Client protocol and shared surfaces

The client protocol defines the long-term shape of the GUI stack more than any single graphical demo does. It should be documented clearly enough that later apps and a small toolkit can build on it.

## How This Builds on Earlier Phases

- Builds on the graphical proof from Phase 47 by turning one-app graphics into a multi-client architecture.
- Reuses the service and restart model from Phases 46 and 52 so the UI stack remains supervised and recoverable.
- Depends on the hardware and transport groundwork from Phases 50 and 55 to avoid graphics-only special cases.
- Later IRQ-backed display/input drivers can reuse the Phase 55c bound-notification pattern, but the Phase 56 compositor core itself remains socket-centric and does not require `RecvResult` / `IrqNotification::bind_to_endpoint` as a prerequisite.

## Implementation Outline

1. Define the display-service ownership model and the client protocol.
2. Implement or complete the keyboard and mouse event model needed by the first graphical session.
3. Wire the display service into the existing service/session startup path.
4. Add multi-client presentation and focus management.
5. Validate crash/recovery behavior and fallback to administration paths.
6. Document the protocol, service graph, and non-goals for the first graphical architecture.

## Learning Documentation Requirement

- Create `docs/56-display-and-input-architecture.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain display ownership, input routing, buffer exchange, session behavior, and why this phase is the real GUI architecture milestone.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/09-framebuffer-and-shell.md`, `docs/29-pty-subsystem.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update `docs/evaluation/gui-strategy.md`, `docs/evaluation/usability-roadmap.md`, and `docs/evaluation/roadmap/R09-display-and-input-architecture.md`.
- Update any protocol or service docs that describe framebuffer ownership, input routing, or session startup.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.56.0`.

## Acceptance Criteria

- A userspace display service owns the primary display path.
- At least two graphical clients can coexist without raw-framebuffer conflicts.
- Keyboard and mouse events are routed through a documented focus-aware input model.
- The display/input protocol is documented well enough for later clients to target without guesswork.
- A display-service crash is recoverable or, at minimum, its failure mode and recovery path are explicitly documented and testable.

## Companion Task List

- [Phase 56 Task List](./tasks/56-display-and-input-architecture-tasks.md)

## How Real OS Implementations Differ

- Mature desktop systems ship with far richer compositing, security, toolkit, and graphics-driver stacks.
- Linux desktops rely on much deeper DRM/KMS and Wayland/X11 ecosystems than m3OS should try to copy immediately.
- m3OS should prioritize one clear, restartable, userspace-owned display architecture over feature-rich but premature compatibility layers.

## Deferred Until Later

- Rich widget/toolkit ecosystems
- Hardware-accelerated composition and live blur / shader effects
- Tiling layout algorithms (dwindle, master-stack, BSP, manual), workspace state machines, keybind chord engine, and `hyprctl`-style scripting surface beyond the minimum control-socket verbs — Phase 56b territory per `docs/appendix/gui/tiling-compositor-path.md`
- Native bar / launcher / notification daemon / lockscreen client implementations — Phase 57b territory
- Animation engine (timing curves, vblank-aligned scheduling, window-move/fade/workspace-slide animations) — Phase 57c territory
- Wayland protocol support of any kind (`wl_shm` shim, libwayland port, Mesa + llvmpipe, GPU-aware buffer transport) — see `docs/appendix/gui/wayland-gap-analysis.md` for sizing
- USB HID breadth beyond the PS/2 AUX mouse needed for the first supported session
- International / non-US keymap layouts, IME, dead keys, and compose sequences
- Desktop polish such as clipboard managers, notifications, drag-and-drop, and richer shells
