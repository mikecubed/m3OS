# Release Phase R07 — Deep Serverization

**Status:** Proposed  
**Depends on:** [R05 — First Service Extractions](./R05-first-service-extractions.md)  
**Official roadmap phases covered:** [Phase 8](../../roadmap/08-storage-and-vfs.md),
[Phase 16](../../roadmap/16-network.md),
[Phase 18](../../roadmap/18-directory-vfs.md),
[Phase 23](../../roadmap/23-socket-api.md),
[Phase 24](../../roadmap/24-persistent-storage.md),
[Phase 28](../../roadmap/28-ext2-filesystem.md),
[Phase 38](../../roadmap/38-filesystem-enhancements.md),
[Phase 39](../../roadmap/39-unix-domain-sockets.md)  
**Primary evaluation docs:** [Path to a Proper Microkernel Design](../microkernel-path.md),
[Current State](../current-state.md),
[Hardware Driver Strategy](../hardware-driver-strategy.md)

## Why This Phase Exists

The early service extractions prove that the microkernel model can work. This
phase is where m3OS either becomes structurally narrower or settles into a
broad-kernel design. Storage, namespace resolution, and networking are the
largest policy-heavy subsystems still living in ring 0, and they dominate both
attack surface and architectural honesty.

This phase exists because a project cannot keep saying "the real microkernel move
comes later" forever. At some point, the large subsystems have to move.

```mermaid
flowchart LR
    A["Apps"] --> B["Syscall facade"]
    B --> C["Kernel IPC/capabilities"]
    C --> D["Block / FS / net services"]
    D --> E["Restartable policy outside ring 0"]
```

## Current vs. required vs. later

| Area | Current state | Required in this phase | Later extension |
|---|---|---|---|
| Storage | Block and filesystem policy remain kernel-heavy | Block and filesystem responsibilities move behind service boundaries | More filesystems and richer policy outside ring 0 |
| Namespace/VFS | High-level pathname and mount routing still live in the kernel | Thin kernel object facade with userspace namespace ownership | Richer namespace features and policy tooling |
| Networking | Stack and socket policy remain kernel-heavy | NIC/network/socket policy migrates outward where practical | Broader protocol and service ecosystems |
| POSIX compatibility | Filesystem/network behavior still implemented directly in kernel paths | Kernel syscalls become thinner dispatch layers where possible | More userspace adaptation over time |

## Detailed workstreams

| Track | What changes | Why now |
|---|---|---|
| Block serverization | Move block-device ownership and DMA-facing policy behind a service boundary | Storage is a large and failure-prone surface |
| Filesystem servers | Make FAT/ext2/tmpfs and related policy live in dedicated services | Filesystem bugs should stop automatically being kernel bugs |
| Namespace/VFS split | Turn the kernel into a thin handle/object layer while mount and path policy move outward | This is the hardest part of the storage migration |
| Network serverization | Move NIC driver ownership, packet processing, and socket-facing policy outward | Networking is a classic microkernel isolation candidate |
| POSIX thinning | Keep the ABI stable while shifting real work behind service calls | Compatibility must not become an excuse for permanent kernel growth |

## How This Differs from Linux, Redox, and production systems

- **Linux** keeps the VFS, block layer, and network stack in kernel space for
  performance and legacy reasons.
- **Redox** uses a userspace-service model much more aggressively, though its
  exact abstractions differ from m3OS's rendezvous-and-capability design.
- **Production microkernels** accept the coordination cost because they value
  fault isolation and explicit boundaries. This phase is where m3OS chooses that
  trade-off in practice rather than just in principle.

## What This Phase Teaches

This phase teaches the real engineering difficulty of microkernels: not the idea
of moving code out of the kernel, but the work of maintaining file, socket, and
process semantics across explicit service boundaries.

It also teaches why the earlier IPC and service-model phases were prerequisites.
Without them, this phase would collapse into special cases and backdoors.

## What This Phase Unlocks

Once this phase lands, m3OS can argue that the microkernel direction is no
longer mostly future tense. That directly improves the security story, the
restartability story, and the credibility of later driver and display work.

## Acceptance Criteria

- At least one real storage path runs through userspace-owned block/filesystem
  services
- The namespace/VFS boundary is thinner and more explicitly separated from
  high-level policy
- At least one meaningful networking or socket path runs through userspace-owned
  service logic
- The syscall ABI remains stable enough for existing applications while the
  underlying implementation moves outward
- The project can explain, with concrete call flows, how storage and networking
  now avoid depending on broad kernel policy

## Key Cross-Links

- [Path to a Proper Microkernel Design](../microkernel-path.md)
- [Current State](../current-state.md)
- [Phase 8 — Storage and VFS](../../roadmap/08-storage-and-vfs.md)
- [Phase 16 — Network](../../roadmap/16-network.md)
- [Phase 24 — Persistent Storage](../../roadmap/24-persistent-storage.md)

## Open Questions

- Which part of pathname and file-descriptor compatibility should remain in the
  kernel for 1.0, if any?
- Is the first network migration better framed around the NIC driver, the socket
  layer, or the whole netstack together?
