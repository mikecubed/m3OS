# Deep Serverization

**Aligned Roadmap Phase:** Phase 54
**Status:** Complete
**Source Ref:** phase-54
**Supersedes Legacy Doc:** (none -- new content)

> **Note:** This phase builds on [IPC Completion](./50-ipc-completion.md),
> [Service Model Maturity](./51-service-model-maturity.md),
> [First Service Extractions](./52-first-service-extractions.md), and
> [Headless Hardening](./53-headless-hardening.md). Phase 54 is where storage,
> namespace, and networking stop being only future microkernel work and start
> shipping through supervised ring-3 services.

## Overview

Phase 54 moves meaningful storage, pathname, and networking policy out of ring 0
without breaking the existing Linux-like syscall ABI. The kernel keeps the
object handles, transport, and bootstrap fallback paths; supervised userspace
services now own the migrated policy slices:

- `fat_server` owns the first extracted storage-service boundary
- `vfs_server` owns rootfs pathname, metadata, directory listing, and read-only
  file policy for the migrated ext2 slice
- `net_server` owns the migrated UDP policy slice behind the `net_udp` service

The result is not a fully serverized OS, but it is a materially narrower kernel
than the Phase 53 baseline and a more honest microkernel story.

## What This Doc Covers

- The shipped storage, namespace, and UDP boundary split
- What stayed in the kernel and why
- The degraded-mode contracts after `vfs` or `net_udp` stops
- The validation path that closed the phase
- The main trade-offs and the biggest later-scope gaps

## What Stayed in the Kernel

| Kernel responsibility | Why it stays in ring 0 |
|---|---|
| File descriptor / socket handle tables | Existing applications still speak a Linux-like syscall ABI, so the kernel must keep the durable object facade |
| Bootstrap ext2 and ramdisk access | Early boot, initrd embedding, and degraded-mode fallback still need a minimal in-kernel path |
| Bulk transport and capability mediation | IPC endpoints, registry, grants, and bulk data remain privileged transport primitives |
| Packet transport, TCP, and low-level device drivers | Phase 54 extracts one meaningful network slice, not the whole network stack |
| Signal delivery and blocked-task wakeup | Fatal signals must be able to break services out of blocking IPC waits so service shutdown and degraded-mode fallbacks remain real |

Phase 54 deliberately keeps mechanism in the kernel while pushing pathname,
mount-policy, and UDP policy outward.

## What Moved to Userspace

### `fat_server`

`fat_server` is the first storage-service boundary. It is supervised by `init`,
registered in the IPC registry, and run as the storage-service UID. The kernel
still owns the low-level block and bootstrap mechanisms, but meaningful storage
work now crosses a real ring-3 service boundary instead of staying purely in a
kernel task.

### `vfs_server`

`vfs_server` owns the migrated rootfs policy slice:

- pathname-based rootfs opens for the migrated ext2 slice
- metadata and access checks needed for `stat`, `access`, and directory listing
- read-only file-handle policy for files such as `/etc/passwd`
- mount-policy and namespace decisions for the migrated rootfs facade

The kernel still owns the fd table and fallback open path, but the higher-level
pathname logic is no longer purely ring-0 work for this slice.

### `net_server` / `net_udp`

`net_server` owns the migrated UDP policy path:

- UDP bind/connect/send/recv validation
- userspace-owned service state for the extracted UDP slice
- close-time coordination that preserves kernel fd/socket handles while policy
  lives in the service

This keeps the syscall ABI stable while proving that socket-facing policy can be
lifted into a supervised service boundary.

## Degraded-Mode Contracts

Phase 54 makes the degraded behavior explicit instead of pretending every
service is restartable today.

### When `vfs` stops

- new rootfs opens fall back to the kernel ext2/bootstrap path
- existing `vfs`-backed file descriptors may fail with `EIO`
- `init` logs that the system is in degraded rootfs mode until manual restart

### When `net_udp` stops

- new UDP syscalls fall back to the kernel-owned UDP path
- existing UDP sockets keep whatever kernel-owned state they already held
- `init` logs that the extracted UDP policy service is unavailable until manual
  restart

### Why the signal wakeup fix mattered

During Track E validation, `service stop vfs` and `service stop net_udp`
exposed a real contract bug: fatal signals did not interrupt services blocked in
IPC `recv`/`reply` waits. That meant a service could stay wedged until a new
client request arrived. The final Phase 54 closure includes the kernel fix that
removes blocked IPC waiters from endpoint queues, wakes them, and lets pending
fatal signals complete shutdown promptly.

## Validation and Closure

Phase 54 closed with the following evidence:

- targeted regression `cargo xtask regression --test serverization-fallback`
  proves that stopping `vfs` or `net_udp` still leaves the system able to use
  the documented degraded-mode fallback paths
- host-side harness validation via `cargo test -p xtask --target x86_64-unknown-linux-gnu`
- repository quality gate via `cargo xtask check`
- integrated smoke still fails only at the pre-existing TCC hello-world step,
  not in the Phase 54 storage/network extraction flow

The key point is that the new failure uncovered during validation was fixed in
kernel signal/IPC handling rather than papered over in the harness.

## Key Design Decisions

### Preserve the syscall ABI, thin the implementation

Phase 54 does not break existing programs. Applications still call the same
syscalls, but those syscalls increasingly dispatch into ring-3 services for the
policy-heavy work.

### Prefer degraded-mode fallback over silent restart for extracted core services

`vfs_server` and `net_server` ship with `restart=never`. This makes the
degraded contract explicit and keeps restart semantics honest while the system
still depends on kernel fallback paths for correctness.

### Fix the kernel contract when validation exposes a real boundary bug

The final signal/IPC wakeup fix is part of Phase 54 because it is required for a
service boundary to be operable. A microkernel-style service is not meaningfully
isolated if `SIGKILL` cannot break it out of blocking IPC.

## Key Files

| File | Purpose |
|---|---|
| `userspace/fat_server/src/main.rs` | Supervised storage-service endpoint |
| `userspace/vfs_server/src/main.rs` | Migrated rootfs pathname and metadata policy |
| `userspace/net_server/src/main.rs` | Migrated UDP policy service |
| `userspace/coreutils-rs/src/service.rs` | Service-stop completion wait used by validation and operators |
| `userspace/init/src/main.rs` | Degraded-mode logging, restart policy, and service-stop behavior |
| `userspace/login/src/main.rs` | Hardened passwd/shadow reads against early-boot timing during validation |
| `kernel/src/arch/x86_64/syscall/mod.rs` | Thin syscall facade, fallback paths, and service routing |
| `kernel/src/process/mod.rs` | Signal delivery now wakes blocked IPC waiters for dying services |
| `kernel/src/ipc/endpoint.rs` | IPC wait cancellation for signaled tasks |
| `kernel/src/task/scheduler.rs` | Blocked IPC task enumeration for signal wakeup |
| `xtask/src/main.rs` | Regression harness, smoke flow, and ext2 data-disk provisioning |

## How This Phase Differs From Later Work

- TCP, Unix sockets, TTY/PTY policy, and most of the network stack still live in
  the kernel
- ext2 bootstrap and broad filesystem behavior are still not wholly userspace
- the kernel still provides the compatibility-heavy syscall facade instead of a
  fully userspace POSIX layer
- page-grant and richer bulk-data transport are still future work for deeper
  serverization

## Related Roadmap Docs

- [Phase 54 roadmap doc](./roadmap/54-deep-serverization.md)
- [Phase 54 task doc](./roadmap/tasks/54-deep-serverization-tasks.md)
- [Architecture and Syscalls](./appendix/architecture-and-syscalls.md)
- [Storage and VFS](./08-storage-and-vfs.md)
- [Network Stack](./16-network.md)
- [Socket API](./23-socket-api.md)

## Deferred or Later-Phase Topics

- Full ext2/FAT/tmpfs/procfs serverization
- TCP and higher-level network-service extraction
- Removal of the remaining kernel console/input transitional tasks
- Richer userspace compatibility layers that shrink the syscall policy surface further
