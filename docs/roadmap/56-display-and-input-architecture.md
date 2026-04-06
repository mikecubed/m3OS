# Phase 56 - Display and Input Architecture

**Status:** Planned
**Source Ref:** phase-56
**Depends on:** Phase 46 (System Services) ✅, Phase 51 (First Service Extractions) ✅, Phase 54 (Hardware Substrate) ✅, Phase 55 (Graphics Bring-Up) ✅
**Builds on:** Turns the single-app graphics proof into a real userspace-owned display/input architecture with explicit ownership, event routing, and crash boundaries
**Primary Components:** future userspace display server, input services, kernel/src/fb, kernel input/interrupt mediation, docs/09-framebuffer-and-shell.md, docs/29-pty-subsystem.md

## Milestone Goal

m3OS gains a real display and input model: one userspace-owned display service controls presentation, keyboard and mouse events are routed through an explicit focus-aware protocol, and multiple graphical clients can coexist without raw framebuffer conflicts.

## Why This Phase Exists

A desktop is not "framebuffer plus mouse." It is a policy system for ownership, composition, input routing, focus, and recovery. The graphics bring-up phase proves that pixels can be drawn; it does not solve how multiple applications share the display or how the system recovers from UI-service failures.

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
| Graphics bring-up baseline | Phase 55 proves userspace graphics and console handoff already work | Add missing graphics-substrate work here |
| Service-model baseline | Phase 46 and Phase 51 provide a reliable supervision and restart pattern for the UI services | Add missing service integration or restart semantics |
| Hardware/input baseline | The chosen mouse or input path exists on the supported targets or is explicitly pulled into this phase | Add the missing input-driver work rather than leaving it as an implicit dependency |
| Buffer transport baseline | The Phase 49 transport model is strong enough for surface or buffer exchange | Add the missing buffer/grant work needed by the display server |

## Important Components and How They Work

### Display service and compositor

The display service owns presentation, composition, and final output. It is the graphical equivalent of a system daemon: a privileged userspace policy engine built on kernel substrate instead of a special in-kernel UI layer.

### Input services and focus routing

Keyboard and mouse input should be mediated by userspace-owned policy that can decide which client receives events, how focus changes, and how recovery works after restarts.

### Client protocol and shared surfaces

The client protocol defines the long-term shape of the GUI stack more than any single graphical demo does. It should be documented clearly enough that later apps and a small toolkit can build on it.

## How This Builds on Earlier Phases

- Builds on the graphical proof from Phase 55 by turning one-app graphics into a multi-client architecture.
- Reuses the service and restart model from Phases 46 and 51 so the UI stack remains supervised and recoverable.
- Depends on the hardware and transport groundwork from Phases 49 and 54 to avoid graphics-only special cases.

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

- Phase 56 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature desktop systems ship with far richer compositing, security, toolkit, and graphics-driver stacks.
- Linux desktops rely on much deeper DRM/KMS and Wayland/X11 ecosystems than m3OS should try to copy immediately.
- m3OS should prioritize one clear, restartable, userspace-owned display architecture over feature-rich but premature compatibility layers.

## Deferred Until Later

- Rich widget/toolkit ecosystems
- Hardware-accelerated composition
- USB HID breadth beyond the needs of the first supported session
- Desktop polish such as clipboard managers, notifications, and richer shells
