# Phase 54 - Deep Serverization

**Status:** Planned
**Source Ref:** phase-54
**Depends on:** Phase 16 (Network) ✅, Phase 18 (Directory and VFS) ✅, Phase 23 (Socket API) ✅, Phase 24 (Persistent Storage) ✅, Phase 28 (ext2 Filesystem) ✅, Phase 38 (Filesystem Enhancements) ✅, Phase 39 (Unix Domain Sockets) ✅, Phase 50 (IPC Completion) ✅, Phase 52c (Kernel Architecture Evolution), Phase 53 (Headless Hardening)
**Builds on:** Extends the first extracted-service pattern from console/input into the policy-heavy subsystems that most strongly determine whether m3OS is really becoming a microkernel-style OS
**Primary Components:** kernel/src/fs, kernel/src/net, kernel/src/arch/x86_64/syscall.rs, userspace/vfs_server, userspace/fat_server, docs/08-storage-and-vfs.md, docs/16-network.md

## Milestone Goal

m3OS moves beyond proof-of-concept extraction and pushes real storage, namespace, and networking responsibilities out of the kernel. The kernel becomes a thinner object and transport layer, while userspace services own more of the policy-heavy behavior that currently dominates the trusted computing base.

## Why This Phase Exists

The project can only defer the "real microkernel move" for so long. Storage, pathname resolution, and networking are the largest remaining policy-heavy subsystems in ring 0. As long as they stay there, the architecture docs describe an ambition, not the shipped system.

This phase exists because the microkernel story becomes materially true only when these larger subsystems start to move outward.

## Learning Goals

- Understand why storage, namespace, and networking are the hardest parts of microkernel convergence.
- Learn how to preserve a stable userspace ABI while moving implementation ownership behind service boundaries.
- See how restartability, fault isolation, and explicit data transport interact at larger subsystem scale.
- Understand the trade-offs between a thin kernel facade and a compatibility-heavy monolithic design.

## Feature Scope

### Storage and block-service extraction

Move at least one meaningful block/storage path behind a userspace-owned service boundary. Filesystem work should stop automatically being ring-0 work for the entire supported storage story.

### Namespace and VFS thinning

Separate kernel file-descriptor/object tracking from higher-level pathname, mount, and namespace policy. The goal is a thinner kernel VFS facade, not a duplicate policy layer in two places.

### Network and socket policy extraction

Move at least one meaningful network or socket-facing policy path out of ring 0. This phase should demonstrate that network behavior can follow the same transport and supervision model as other services.

### ABI preservation during migration

Keep existing applications working while the real work moves behind service calls. Compatibility should become a dispatch layer, not an excuse to keep the policy in the kernel forever.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| A real storage or filesystem path through userspace-owned service logic | Otherwise the phase is still only conceptual |
| Thinner kernel namespace/VFS ownership | Storage policy cannot stay duplicated indefinitely |
| A meaningful network or socket policy move outward | Networking is too large a TCB to leave untouched if the microkernel claim is serious |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Transport readiness | Phase 50 bulk-data and capability-grant semantics work for storage and network payloads | Add the missing transport pieces before extraction |
| Extraction pattern readiness | Phase 52 provides a proven ring-3 service pattern and restart model | Add the missing supervisor or reconnect behavior |
| Headless baseline | Phase 53 support boundaries and validation gates are stable enough to absorb subsystem movement | Add the missing release-discipline work instead of assuming it |
| Facade design | The kernel/object model for open files, sockets, and handles is explicit enough to thin safely | Add the missing handle-facade work to this phase |

## Important Components and How They Work

### Storage and namespace service boundaries

This phase should define which responsibilities stay in the kernel (object handles, low-level mediation) and which move outward (path resolution, mount policy, filesystem logic, block ownership). That split is the heart of the storage migration.

### Network service boundaries

The project needs a clear answer for what the kernel still owns in packet ingress/egress and what the userspace service owns in protocol policy, socket semantics, and connection management.

### Thin compatibility facade

Existing applications will still enter through a Linux-like syscall ABI. The key is that the ABI remains stable while the implementation behind it becomes thinner and more service-oriented.

## How This Builds on Earlier Phases

- Uses the transport model completed in Phase 50 for the first large subsystem migrations.
- Extends the service-extraction pattern validated in Phase 52 into more complex domains.
- Builds on Phase 53 so the project can explain these migrations inside a stable headless/reference-system story.
- Prepares the ground for real hardware work and any later graphical stack by reducing the kernel blast radius.

## Implementation Outline

1. Choose the first storage and network paths to move behind userspace services.
2. Define the thin kernel object/handle facade required to preserve the current ABI.
3. Move filesystem and namespace policy outward while keeping the syscall contract stable.
4. Move at least one meaningful network or socket-facing policy path outward.
5. Validate restartability, error handling, and performance well enough to document the trade-offs honestly.
6. Update architecture, storage, and network docs to reflect the shipped boundaries.

## Learning Documentation Requirement

- Create `docs/54-deep-serverization.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the storage, namespace, and network boundary decisions; the thin kernel facade; and the specific call/data paths that changed.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/08-storage-and-vfs.md`, `docs/16-network.md`, `docs/18-directory-vfs.md`, `docs/23-socket-api.md`, `docs/28-ext2-filesystem.md`, and `docs/38-filesystem-enhancements.md`.
- Update `docs/evaluation/microkernel-path.md`, `docs/evaluation/current-state.md`, and `docs/evaluation/roadmap/R07-deep-serverization.md`.
- Update `docs/appendix/architecture-and-syscalls.md` and `docs/roadmap/README.md` so the architecture docs and official roadmap reflect the real service boundaries.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.54.0`.

## Acceptance Criteria

- At least one real storage path runs through userspace-owned block/filesystem service logic.
- The namespace/VFS split is thinner and explicitly documented as kernel mechanism versus userspace policy.
- At least one meaningful networking or socket-facing path runs through userspace-owned service logic.
- Existing applications continue to work through a stable syscall facade while the implementation moves outward.
- Updated docs can explain the new call/data flow concretely instead of only describing the intended architecture.

## Companion Task List

- Phase 54 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature microkernels often accept more coordination overhead to gain stricter isolation earlier.
- Linux keeps storage and networking in the kernel for very different historical and performance reasons.
- m3OS should focus on making the ownership split explicit, testable, and understandable before chasing every optimization.

## Deferred Until Later

- Broader filesystem matrix beyond the first migrated path
- Full network-service ecosystem and higher-level userland daemons
- Aggressive performance tuning once the boundary is correct
- Complete POSIX policy removal from the kernel
