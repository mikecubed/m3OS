# Phase 51 - First Service Extractions

**Status:** Planned
**Source Ref:** phase-51
**Depends on:** Phase 15 (Hardware Discovery) ✅, Phase 20 (Userspace Init and Shell) ✅, Phase 49 (IPC Completion) ✅, Phase 50 (Service Model Maturity) ✅
**Builds on:** Uses the finished IPC path and matured service model to move the first genuinely important kernel-resident services into restartable ring-3 processes
**Primary Components:** userspace/console_server, userspace/kbd_server, userspace/init, kernel/src/main.rs, kernel/src/ipc, docs/07-core-servers.md, docs/09-framebuffer-and-shell.md

## Milestone Goal

m3OS proves that its microkernel direction is real by extracting at least the first visible core services out of the kernel address space. Console and input-related functionality become supervised ring-3 services, and the system demonstrates that they can fail and be restarted without rebooting the machine.

## Why This Phase Exists

Until this point, the microkernel story is still mostly a transport and architecture argument. The project needs a concrete proof that a user-visible subsystem can cross the ring-0 boundary and still behave like part of one operating system.

This phase exists to create that proof with the least dangerous and most visible candidates: console and input-oriented services.

## Learning Goals

- Understand how a kernel-resident service becomes a real ring-3 process.
- Learn how IRQ-driven or event-driven behavior can be mediated by notifications and supervised services.
- See how service restartability changes the cost of failure.
- Understand why console and input policy are ideal first extraction targets before storage or networking.

## Feature Scope

### Console service extraction

Move rendering policy, console-session behavior, and related high-level logic out of kernel-resident task code and into a supervised userspace service.

### Keyboard and input translation extraction

Move scancode processing, input translation, and basic focus or routing policy out of the kernel and into a userspace-owned service contract.

### Service restart and reconnection behavior

Demonstrate that these services can crash, restart, and rejoin the system without requiring a reboot or silently corrupting shared state.

### Measurement and boundary documentation

Record the latency, complexity, and debugging cost of the new boundary. This phase should teach as much as it implements.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| At least one real ring-3 core service | The phase is meaningless if extraction remains conceptual |
| No kernel-pointer or same-address-space shortcuts | Otherwise the boundary is fake |
| Restartable service behavior | Restartability is part of the justification for extraction |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| IPC transport readiness | Phase 49 capability and bulk-data paths are strong enough for extracted console/input traffic | Pull the missing transport cleanup into this phase |
| Service-model readiness | Phase 50 supervision, restart, and status behavior can host extracted services | Add the missing lifecycle work before extracting anything important |
| Device mediation boundary | The kernel side of keyboard/framebuffer mediation is narrow enough to hand events or buffers outward cleanly | Add the missing mediation or notification work |
| Service crate readiness | Userspace service crates, build wiring, and init integration are ready for real use | Add the missing workspace/initrd/service-registration work to the phase |

## Important Components and How They Work

### Console service ownership

The console service should own rendering policy and console-session behavior in ring 3, while the kernel retains only the minimal privileged substrate needed to mediate hardware access or mapped resources.

### Input translation and routing

Keyboard and related input events should arrive in a form that allows userspace policy to decide how they are interpreted, routed, and recovered after service restart.

### Supervisor integration and restart path

The extracted services must be declared, supervised, and observable through the same Phase 50 service model the rest of the system uses.

## How This Builds on Earlier Phases

- Builds directly on the early core-server and framebuffer model from Phases 7 and 9.
- Depends on Phase 49 to remove the transport shortcuts that made earlier "service" code kernel-only in practice.
- Depends on Phase 50 to give extracted services a credible lifecycle and admin surface.
- Prepares the architectural pattern later reused by storage, networking, and the display stack.

## Implementation Outline

1. Choose the first extraction targets and define their ring-3 service contracts.
2. Narrow the kernel side to the minimal notification, mapping, and hardware-mediation responsibilities.
3. Wire the new services into build, initrd, and service-manager configuration.
4. Port console/input policy out of kernel-resident task code.
5. Implement restart and reconnect behavior for the extracted services.
6. Measure the boundary cost and document the new call/data flow.
7. Add smoke or focused validation that proves restartability and correct behavior.

## Learning Documentation Requirement

- Create `docs/51-first-service-extractions.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain what stayed in the kernel, what moved to userspace, how restartability works, and what the first extraction taught the project about later serverization.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/07-core-servers.md`, `docs/09-framebuffer-and-shell.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update `docs/evaluation/microkernel-path.md`, `docs/evaluation/roadmap/R05-first-service-extractions.md`, and any diagrams that still show the extracted services as kernel tasks.
- Update init/build wiring docs if new userspace service crates become active workspace members.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.51.0`.

## Acceptance Criteria

- At least one previously kernel-resident core service runs as a real supervised ring-3 process.
- The extracted service uses the Phase 49 IPC path without shared-address-space shortcuts.
- Restarting or crashing the extracted service does not require a full machine reboot to recover basic functionality.
- The service graph, build system, and docs all describe the extracted service as part of the normal system model.
- Boundary measurements and trade-offs are written down for later phases.

## Companion Task List

- Phase 51 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature microkernels often perform this extraction work earlier and across more subsystems at once.
- Monolithic kernels keep console and input behavior in-kernel for historical and performance reasons, not because that is the only workable design.
- m3OS should optimize here for clarity, restartability, and teaching value over perfect early performance.

## Deferred Until Later

- Storage, namespace, and networking extraction
- Rich multi-seat or multi-session input policy
- Fully graphical display ownership
- Broad performance tuning of the new boundary
