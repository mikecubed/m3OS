# Phase 50 - IPC Completion

**Status:** Planned
**Source Ref:** phase-50
**Depends on:** Phase 6 (IPC Core) ✅, Phase 7 (Core Servers) ✅, Phase 8 (Storage and VFS) ✅, Phase 39 (Unix Domain Sockets) ✅, Phase 49 (Architectural Declaration) ✅
**Builds on:** Turns the existing capability and rendezvous primitives into a transport model that can support real ring-3 services without shared-address-space shortcuts
**Primary Components:** kernel/src/ipc, kernel-core/src/ipc, kernel/src/main.rs, kernel/src/arch/x86_64/syscall.rs, docs/06-ipc.md

## Milestone Goal

m3OS gains the missing IPC and bulk-data pieces required for serious userspace services: safe capability transfer, practical large-payload transport, ring-3-safe service registration, and clear failure semantics for the canonical server loop.

## Why This Phase Exists

The current IPC model is conceptually strong, but parts of the real system still rely on assumptions from the period when "servers" were kernel tasks living in the same address space. That makes later service extraction either fake isolation or a maze of special cases.

This phase exists to finish the transport model before the project moves more core services out of ring 0. Without it, later serverization work is built on sand.

## Learning Goals

- Understand why control flow and bulk-data transport must be designed together in a microkernel-style system.
- Learn how capability transfer, service discovery, and reply/receive semantics interact.
- See why kernel-pointer shortcuts become architectural bugs once services really move to ring 3.
- Understand the trade-offs between copying, grants, and zero-copy data paths.

## Feature Scope

### Capability transfer completion

Finish the end-to-end capability-grant path so services can safely exchange authorities without relying on ad hoc assumptions or privileged shortcuts.

### Bulk-data transport

Define and implement the grant or shared-buffer model for file blocks, packets, framebuffer spans, and other large payloads. This phase should produce one transport story the later roadmap can reuse instead of graphics-, storage-, and network-specific hacks.

### Ring-3-safe registry and service contracts

Remove any remaining kernel-task-only assumptions from service registration and service-address resolution. Real userspace services must be able to register, restart, and reconnect without a special case.

### Canonical server-loop failure semantics

Make the expected `recv -> handle -> reply_recv` pattern concrete enough that later services can share the same lifecycle, transport, and error assumptions.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Safe capability grants | Later ring-3 services need authority transfer, not just message passing |
| Bulk-data path without kernel-pointer shortcuts | Filesystems, networking, and graphics all depend on it |
| Ring-3-safe service registry | Service extraction is not real if discovery still assumes kernel callers |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Ownership contract | Phase 49 explicitly defines which later services will rely on the transport model | Add the missing service inventory before declaring IPC complete |
| Existing shortcut inventory | All shared-pointer and kernel-task assumptions in current service paths are enumerated | Pull any un-audited paths into this phase |
| Bulk-data target set | The transport design covers strings, file blocks, packets, and framebuffer-sized payloads | Add the missing payload contract instead of leaving a subsystem-specific hole |
| Failure model | Restart, disconnect, and reply/receive semantics are documented for supervised services | Add the missing lifecycle semantics needed by later phases |

## Important Components and How They Work

### Capability grants and ownership transfer

The capability path is what makes authority explicit. This phase should make grants atomic, well-documented, and visible enough that later services can rely on them without embedding policy back into the kernel.

### Shared-buffer or grant-backed bulk transport

Bulk transport is where microkernel designs either stay honest or quietly reintroduce hidden shared-state assumptions. The chosen contract should be generic enough for storage, network, and graphics workloads.

### Service registration and server-loop conventions

Real userspace services need a stable discovery story and a simple server loop. This phase should document and implement the contract those services are expected to follow.

## How This Builds on Earlier Phases

- Finishes the IPC direction introduced in Phase 6 by covering the data path, not just the control path.
- Reworks the early Core Server and Storage/VFS phases so their service model can survive ring-3 extraction.
- Builds on Phase 39 by aligning local socket-style service communication with the native capability/IPC model.
- Depends on the architecture contract from Phase 49 to know which later services must be supported.

## Implementation Outline

1. Audit current IPC shortcuts and identify every service path that still depends on shared kernel addresses.
2. Finish and document the capability-grant semantics.
3. Implement the generic bulk-data transport model and its ownership rules.
4. Remove kernel-only assumptions from the service registry and service-address paths.
5. Standardize the canonical server loop and service-death semantics.
6. Port at least one real service path to the new transport as proof.
7. Extend validation so later extraction phases can depend on this transport with confidence.

## Learning Documentation Requirement

- Create `docs/50-ipc-completion.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain capability grants, bulk-data paths, registry behavior, service loops, and the specific shortcuts that were removed in this phase.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/06-ipc.md`, `docs/07-core-servers.md`, `docs/08-storage-and-vfs.md`, and `docs/appendix/architecture-and-syscalls.md` to match the finished transport model.
- Update `docs/evaluation/microkernel-path.md` and `docs/evaluation/roadmap/R03-ipc-completion.md` so the evaluation overlay points at the official implementation milestone.
- Update `docs/roadmap/README.md` and any transport-related diagrams or subsystem docs that describe the old assumptions.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.50.0`.

## Acceptance Criteria

- Capabilities can be granted safely and atomically between isolated processes.
- There is a documented and implemented bulk-data path suitable for storage, packet, and framebuffer-sized transfers.
- Core service registration no longer relies on kernel-task-only assumptions.
- At least one representative service path uses the new transport without kernel-pointer shortcuts.
- The IPC docs and validation coverage describe the same control flow and failure semantics the code actually implements.

## Companion Task List

- Phase 50 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature microkernels often invest heavily in message layout, grant semantics, and zero-copy transport much earlier.
- Monolithic kernels avoid some of this design pressure by keeping drivers and services in one address space, at the cost of a larger TCB.
- m3OS should favor a clear, teachable, reusable transport contract over premature optimization.

## Deferred Until Later

- Deep performance tuning of zero-copy paths
- Rich typed service IDLs or code-generated message bindings
- Advanced delegation patterns beyond the basic capability and buffer model
