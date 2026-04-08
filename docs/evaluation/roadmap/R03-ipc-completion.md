# Release Phase R03 — IPC Completion

**Status:** Complete (v0.50.0)  
**Depends on:** [R02 — Architectural Declaration](./R02-architectural-declaration.md)  
**Official roadmap phases covered:** [Phase 50](../../roadmap/50-ipc-completion.md)
(builds on [Phase 6](../../roadmap/06-ipc-core.md),
[Phase 7](../../roadmap/07-core-servers.md),
[Phase 8](../../roadmap/08-storage-and-vfs.md),
[Phase 39](../../roadmap/39-unix-domain-sockets.md),
[Phase 49](../../roadmap/49-architectural-declaration.md))  
**Primary evaluation docs:** [Path to a Proper Microkernel Design](../microkernel-path.md),
[Current State](../current-state.md)

## Why This Phase Exists

Everything interesting in the later microkernel roadmap depends on one fact:
ring-3 services need a transport model that is both **safe** and **practical**.
Right now, parts of the system still rely on assumptions that made sense when
"servers" were kernel tasks sharing kernel address space.

This phase exists to finish the IPC and bulk-data model so later service
extractions are real instead of decorative. It is the phase that converts
microkernel aspiration into transport infrastructure.

```mermaid
flowchart LR
    A["Message control path"] --> B["Capability grants"]
    A --> C["Bulk data grants"]
    B --> D["Real ring-3 services"]
    C --> D
    D --> E["Restartable, isolated servers"]
```

## Current vs. required vs. later

| Area | Current state | Required in this phase | Later extension |
|---|---|---|---|
| Message passing | Synchronous IPC exists | Stable server-loop semantics and ring-3-safe registry behavior | Richer service protocols and typed wrappers |
| Capability transfer | Core concept exists, but the full handoff path is incomplete | End-to-end capability grant support | More elaborate delegation patterns |
| Bulk data | Some paths still assume shared pointers or kernel shortcuts | Page/buffer grant model for strings, blocks, framebuffers, packets | Performance tuning and zero-copy refinements |
| Failure model | Static assumptions and limited lifecycle semantics | Clean reply/receive and service-death handling rules | Better observability and protocol tooling |

## Detailed workstreams

| Track | What changes | Why now |
|---|---|---|
| Capability handoff | Finish `sys_cap_grant`-style transfer semantics and document ownership rules | Real services need to exchange authorities cleanly |
| Bulk-data transport | Implement page or buffer grants for file blocks, packet buffers, and framebuffer regions | Storage, networking, and GUI work depend on this |
| Registry safety | Remove remaining service-registry or payload assumptions that rely on kernel-only callers | A service manager cannot supervise fake ring-3 services |
| Server fast path | Standardize `recv → handle → reply_recv` patterns and their failure semantics | Good servers need a simple, repeatable loop |
| Message contract | Define the stable message layout, inline payload rules, and optional grant descriptors | Later services need one consistent transport story |

## How This Differs from Linux, Redox, and production systems

- **Linux** does not need this layer for most subsystems because drivers and core
  services share the kernel address space.
- **Redox** solves a similar problem with schemes, events, and controlled
  resource handles; the user-visible abstraction is more file-like than the
  seL4-style rendezvous model m3OS documents.
- **Production microkernels** treat IPC as the center of the design, not an
  afterthought. m3OS needs to do the same if it wants later service migrations
  to stay clean.

## What This Phase Teaches

This phase teaches that a microkernel is only as real as its IPC path. If the
message-control path works but the data path is still a set of shared-address
shortcuts, then the system has not actually crossed the boundary it claims.

It also teaches an important systems-design lesson: bulk data and control flow
must be designed together. A good message protocol without a good buffer-sharing
story is not enough.

## What This Phase Unlocks

After this phase, service supervision and service extraction can happen on top
of a transport that is designed for isolated processes. That makes every later
architecture move cheaper and more trustworthy.

## Acceptance Criteria

- Capabilities can be granted safely and atomically between processes
- There is a documented and working page/buffer grant path for at least one real
  bulk-data use case
- Core IPC paths no longer depend on raw shared kernel pointers in message
  payloads
- Service-registry or discovery paths are ring-3-safe
- A standard server loop using `reply_recv` or equivalent is documented and used
  consistently

## Key Cross-Links

- [Path to a Proper Microkernel Design](../microkernel-path.md)
- [IPC](../../06-ipc.md)
- [Phase 6 — IPC Core](../../roadmap/06-ipc-core.md)
- [Phase 8 — Storage and VFS](../../roadmap/08-storage-and-vfs.md)

## Resolved Design Questions

- **Bulk-data path**: Phase 50 chose a hybrid copy-and-grant model. Small payloads
  (up to 64 KiB) use `copy_from_user` / `copy_to_user` with validated page-table
  access. Large payloads (framebuffer spans) use `Capability::Grant` page transfers.
- **Typed IPC support**: Deferred. Phase 50 standardizes the raw message and
  capability-grant contract. Typed IDLs and code-generated bindings are later work.
