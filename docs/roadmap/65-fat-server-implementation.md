# Phase 65 - FAT Server Implementation

**Status:** Planned
**Source Ref:** phase-65
**Depends on:** Phase 54 (Deep Serverization) ✅, Phase 24 (FAT32 Filesystem) ✅, Phase 39 (Unix Domain Sockets) ✅
**Builds on:** Replaces the Phase 54 `fat_server` ENOSYS stub with real FAT32 file operations; routes VFS calls through the userspace service; closes the "deep serverization includes storage" claim from Phase 54
**Primary Components:** userspace/fat_server, kernel-core/fat32, userspace/vfs_server, kernel VFS

## Milestone Goal

`fat_server` handles real FAT32 operations — open, read, write, getdents, stat, unlink — with a server-side file-descriptor table and data stored on the FAT32 partition. VFS calls for the FAT32 mount point route to `fat_server` through `vfs_server`. A regression suite exercises write-then-read consistency and persistence across a simulated restart.

## Why This Phase Exists

Phase 54 claimed "deep serverization" of storage by extracting file-system policy into userspace. The `fat_server` binary was introduced, registered, and accepted IPC connections. However, `userspace/fat_server/src/main.rs:67` dispatches every request to a handler that immediately returns `-ENOSYS`. The storage extraction advertised in Phase 54 never actually moved FAT32 operations out of the kernel.

This phase delivers the FAT32 operations that Phase 54 claimed. Removing the stub would leave a gap in the storage architecture that Phase 54 explicitly targeted; the correct resolution is implementation.

## Learning Goals

- Understand how a userspace file server maintains a file-descriptor table separate from the kernel FD namespace.
- Learn how the FAT32 BPB, FAT chain, and directory entries map to file operations.
- See how a VFS dispatch layer routes requests to interchangeable backing servers.
- Understand why write-ordering and flush semantics matter for FAT32 consistency.

## Feature Scope

### IPC verb decoding

`fat_server` decodes the full set of VFS IPC verbs it must handle: `Open`, `Read`, `Write`, `Seek`, `Close`, `Getdents`, `Stat`, `Unlink`, `Rename`. Unknown verbs return `-ENOSYS` with a log event naming the unimplemented verb.

### FAT32 implementation routed from `fat_server`

The existing kernel FAT32 code (`kernel/src/fs/fat32/`) is lifted into `kernel-core::fat32` so it is callable from userspace. `fat_server` instantiates a `Fat32Volume` over a `RemoteBlockDevice` connection to `nvme_server`. File operations delegate to this in-process FAT32 instance.

### Server-side file-descriptor table

`fat_server` maintains a `FdTable` mapping caller-supplied FD tokens to open `FatFile` handles. FD tokens are per-client opaque integers. The table enforces that a close from one client cannot affect another client's FDs.

### VFS routing integration

`vfs_server` is extended to route requests for the FAT32 mount point to `fat_server` rather than to the kernel VFS. The routing is keyed on the mount-point path registered during `fat_server` startup.

### Regression suite

`userspace/fat_server/tests/` contains tests for: write then read consistency, multi-file directory listing, stat of a newly created file, unlink of an existing file, and persistence across a `fat_server` restart (the FAT32 data persists on the block device).

## Important Components and How They Work

### `kernel-core/src/fat32/` (lifted from `kernel/src/fs/fat32/`)

The FAT32 BPB parser, FAT chain walker, directory entry codec, and cluster allocator are moved to `kernel-core` so they are host-testable and usable from `fat_server` without kernel privileges. The kernel retains a thin wrapper that calls into `kernel-core::fat32` for its own VFS path; no kernel behavior changes for the kernel-side mount.

### `userspace/fat_server/src/dispatch.rs`

Decodes incoming IPC messages into typed `FatRequest` enum variants and dispatches to the appropriate `Fat32Volume` method. Returns typed `FatReply` values encoded for IPC. Every arm that was previously `return -ENOSYS` now has a real implementation.

### `userspace/fat_server/src/fd_table.rs`

`FdTable` maps `(client_cap, fd_token)` to `FatFile`. Provides `open`, `close`, `get_mut`, and `iter_client_fds`. On client disconnect (capability dropped) all associated FDs are closed automatically.

### `vfs_server` mount-point routing

A new `MountTable` in `vfs_server` maps path prefixes to service capability endpoints. On `Open` for a path under `/fat`, the VFS server forwards the request to the registered `fat_server` endpoint and returns the reply verbatim. This is the routing plumbing Phase 54 described but did not deliver.

## How This Builds on Earlier Phases

- Lifts Phase 24's FAT32 implementation into `kernel-core` so it is host-testable; no algorithm is reimplemented.
- Uses Phase 54's IPC request format — the wire format from the `fat_server` stub is unchanged; only the handler bodies change.
- Uses Phase 55b's `RemoteBlockDevice` facade to route block I/O to `nvme_server` without kernel involvement.
- Extends Phase 54's `vfs_server` with mount-point routing that was described but not implemented.

## Implementation Outline

This phase is a canonical SRP example: FAT32 operations belong entirely in `fat_server`, not in the kernel. The kernel retains only a thin call-through to `kernel-core::fat32` for its own mount path; all policy (FD tracking, mount routing, client isolation) lives in the supervised userspace service. Keep this boundary sharp — resist any temptation to add helper logic back to the kernel VFS during the lift.

Follow TDD for the `kernel-core::fat32` lift: write the `MemBlockDevice`-backed unit tests for BPB parsing, FAT chain traversal, and write-then-read round-trips before porting any code from the kernel. Eight host-testable tests must pass before connecting `Fat32Volume` to a `RemoteBlockDevice` in QEMU.

The `shadow_write_atomic` pattern used in Phase 66 for credential stores parallels the flush discipline required here: `handle_write` must flush dirty sectors to `RemoteBlockDevice` before returning `Ok`, ensuring the block device (not `fat_server`'s process memory) is the authoritative store.

1. Write host-side `MemBlockDevice` tests for BPB parsing, FAT chain walk, write/read round-trip, getdents, stat, unlink, rename before moving any source file.
2. Move `kernel/src/fs/fat32/` to `kernel-core/src/fat32/`; add a `BlockDevice` trait abstracting the I/O layer; add feature gates for host-test vs. kernel use.
3. Implement `FdTable` and `FatFile` wrapper in `fat_server`.
4. Implement `dispatch.rs` verb arms: Open, Read, Write, Seek, Close, Getdents, Stat, Unlink, Rename.
5. Wire `Fat32Volume` to a `RemoteBlockDevice` client in `fat_server`'s init path.
6. Extend `vfs_server` with `MountTable` and forward logic for FAT32 path prefix.
7. Write regression tests.
8. Update Phase 54 design doc and task doc with closure note; close audit Red Flag #14.

## Acceptance Criteria

- `cargo xtask test --test fat_server_rw` passes: write a file, read it back, values match.
- `cargo xtask test --test fat_server_persist` passes: write a file, restart `fat_server`, read the file through the new instance.
- `cargo xtask test --test fat_server_getdents` passes: create three files, `getdents` returns all three names.
- A path under `/fat` resolved through `vfs_server` reaches `fat_server` and returns correct data.
- No call path returns `-ENOSYS` for the implemented verbs (grep confirms no unguarded `-ENOSYS` in `dispatch.rs`).
- Phase 54 design doc carries a closure note referencing Phase 65; audit Red Flag #14 is closed.

## Companion Task List

- [Phase 65 Task List](./tasks/65-fat-server-implementation-tasks.md)

## How Real OS Implementations Differ

- Linux's VFS layer uses `file_operations` tables with per-filesystem implementations; the dispatch is in-kernel.
- FUSE provides a user-kernel boundary similar to `fat_server`, but uses a dedicated kernel module and a standardized request/reply wire format.
- Production FAT32 drivers implement the short-name (8.3) to long-name (VFAT LFN) mapping; m3OS defers LFN support.

## Deferred Until Later

- VFAT long filename (LFN) support
- FAT12 and FAT16 variants
- fsck / consistency checking tooling
- Multiple concurrent FAT32 volumes
- Cross-FAT32-volume rename (requires copy-then-unlink)
