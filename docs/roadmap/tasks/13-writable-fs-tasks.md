# Phase 13 — Writable Filesystem: Task List

**Status:** Complete (reconciled 2026-05-08; see "Phase 58 reconciliation — verification" section below)
**Source Ref:** phase-13
**Depends on:** Phase 8 (Storage and VFS) ✅
**Goal:** Add the write path to the read-only VFS shipped in Phase 8: tmpfs for ephemeral RAM-backed scratch, FAT32 write support for persistence across reboots, and the POSIX write-oriented syscalls (`write`, `creat`, `mkdir`, `unlink`, `rmdir`, `rename`, `truncate`, `fsync`) wired through the VFS dispatch layer.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | tmpfs (RAM-backed in-memory filesystem) | — | ✅ Complete |
| B | FAT32 write path (file create / append / delete, directory ops) | — | ✅ Complete |
| C | VFS dispatch (FdBackend::VfsService → vfs_server path routing) | A, B | ✅ Complete |
| D | Syscall surface (`write`, `creat`, `mkdir`, `unlink`, `rmdir`, `rename`, `truncate`, `fsync`) | C | ✅ Complete |
| E | Validation and integration tests | A, B, C, D | ✅ Complete |

---

## Track A — tmpfs

In-memory filesystem mounted at `/tmp` during init. Pure-logic core lives in `kernel-core/src/fs/tmpfs.rs` (host-testable); the kernel binding sits in `kernel/src/fs/tmpfs.rs`.

- [x] tmpfs pure-logic core: `kernel-core/src/fs/tmpfs.rs::Tmpfs` (root + walk helpers); kernel binding: `kernel/src/fs/tmpfs.rs`.
- [x] Backing store is a directory tree of `TmpfsNode` variants (`File`/`Dir`/`Symlink`); `DirData::children` is `BTreeMap<String, TmpfsNode>`; `FileData::content` is `Vec<u8>` (per-file inline buffer, capped at 16 MiB by `MAX_FILE_SIZE`).
- [x] `mkdir`, `create`, `write`, `read`, `unlink`, `rmdir`, `symlink`, `stat` operations on the `Tmpfs` type.
- [x] tmpfs mounted at `/tmp` during init by the userspace `vfs_server`.
- [x] `fsync` on tmpfs is a no-op (all data lives in RAM-resident `Vec<u8>` buffers).
- [x] File content allocation uses ordinary heap `Vec<u8>` rather than a dedicated frame-allocator pool; the original design-doc "kernel-allocated pages" prose was a planning aspiration that did not survive into the shipped implementation.

## Track B — FAT32 Write Path

Extend the Phase 8 read-only FAT32 driver to support writes. Either kernel-side `kernel/src/fs/fat32.rs` or the userspace extraction `userspace/fat_server/`.

- [x] FAT32 file create: allocate cluster, write directory entry, append to FAT chain. `kernel/src/fs/fat32.rs` and `userspace/fat_server/`.
- [x] FAT32 append/overwrite writes against existing files (extend cluster chain when crossing cluster boundary).
- [x] FAT32 file delete: free clusters back to the FAT, mark directory entry deleted.
- [x] FAT32 directory create + remove (mkdir / rmdir).
- [x] FAT32 `fsync` flushes dirty sectors to the block device.
- [x] FAT32 write path tolerates writing past the end of a file (cluster allocation kicks in).

## Track C — VFS Dispatch

The Phase 13 design doc speculated about a `WriteableFs` trait alongside `ReadableFs`; the shipped implementation took a different shape. Write dispatch goes through a single `FdBackend::VfsService` variant on the kernel side, and the userspace `vfs_server` performs path-based routing to the correct backend (tmpfs, FAT32, ext2) using concrete per-backend types rather than a generic write trait.

- [x] `FdBackend::VfsService { service_handle, .. }` in `kernel/src/process/mod.rs::FdBackend` is the kernel-side dispatch point; `sys_read` / `sys_write` / `sys_open` route to it.
- [x] `userspace/vfs_server/src/main.rs` owns path dispatch and routes write requests to the appropriate backend (tmpfs / ext2 / FAT32) based on mount table lookup.
- [x] Mount table records mount point + backend type; read-only mounts surface `EROFS` on write attempts.
- [x] (Design-doc aspiration not shipped) — a generic `WriteableFs` trait with `impl WriteableFs for Tmpfs` / `impl WriteableFs for Fat32` was discussed but not implemented; backends are concrete types invoked from `vfs_server` rather than dyn-dispatched through a trait.

## Track D — Syscall Surface

POSIX write-oriented syscalls dispatched through the VFS.

- [x] `sys_write` — wired in `kernel/src/arch/x86_64/syscall/mod.rs`.
- [x] `sys_creat` (or `sys_open` with O_CREAT) — same.
- [x] `sys_mkdir` and `sys_rmdir`.
- [x] `sys_unlink`.
- [x] `sys_rename` and `sys_truncate`.
- [x] `sys_fsync`.
- [x] `userspace/syscall-lib/src/lib.rs` exposes user wrappers for each.

## Track E — Validation and Integration Tests

- [x] `userspace/tmpfs-test/` exercises round-trip create / write / read / unlink against tmpfs.
- [x] FAT32 round-trip is exercised through the boot path (configs and `/etc/services.d/` entries are written to ext2/FAT32 by `xtask` and read back at boot).
- [x] `cargo xtask check` passes (clippy clean, formatting correct).

---

## Phase 58 reconciliation — verification

**Reconciliation date:** 2026-05-08 (Phase 58 Track B.1 — task doc newly authored from the existing design doc and shipping codebase)

This task doc was missing prior to Phase 58. The roadmap README row for Phase 13 read "Tasks: not yet created"; the reconciliation phase walked the existing design doc's five acceptance criteria and the shipping codebase, then authored this task doc against them. Anchor citations:

- **tmpfs:** pure-logic core at `kernel-core/src/fs/tmpfs.rs::Tmpfs` (with `BTreeMap<String, TmpfsNode>` directory children and `Vec<u8>` file content; not a hash-map-of-path-to-page-list as the original design-doc prose implied), kernel binding at `kernel/src/fs/tmpfs.rs`, mounted at `/tmp` by `userspace/vfs_server/`.
- **FAT32 write:** `kernel/src/fs/fat32.rs` (kernel-side) and `userspace/fat_server/` (extracted server).
- **VFS dispatch:** `FdBackend::VfsService` in `kernel/src/process/mod.rs` plus path-based routing in `userspace/vfs_server/src/main.rs` — concrete-backend dispatch, *not* a `WriteableFs`/`ReadableFs` trait pair (the design-doc trait language did not survive into the shipped code).
- **Syscall surface:** `kernel/src/arch/x86_64/syscall/mod.rs` plus `userspace/syscall-lib/src/lib.rs` wrappers.
- **Validation:** `userspace/tmpfs-test/` test program; FAT32 round-trip exercised by the boot service-config write/read cycle.

**Deviations from the original Phase 13 design doc** (recorded for traceability):

- The design doc described a `WriteableFs` trait alongside the existing `ReadableFs`. The shipped implementation routes writes through a single `FdBackend::VfsService` variant + `vfs_server` path dispatch; no generic write trait was ever introduced. Backends (tmpfs, ext2, FAT32) are concrete types selected by mount-table lookup.
- The design doc described tmpfs as "stores file data as kernel page lists with no disk involvement". The shipped implementation stores file content as a per-file `Vec<u8>` (heap-allocated, capped at `MAX_FILE_SIZE = 16 MiB`); directory children are `BTreeMap<String, TmpfsNode>`. Frame allocation is the ordinary heap allocator's responsibility — there is no dedicated page-list pool.

Phase 13 also predates Phase 24 (Persistent Storage) and Phase 28 (ext2 Filesystem). Persistent storage today is ext2-on-disk (`kernel/src/fs/ext2.rs`); FAT32 remains supported for the EFI System Partition and for cross-OS data sharing.

## Deferred Until Later

These items are explicitly out of scope for Phase 13 (and remain deferred or covered by later phases):

- Page cache / write-back buffering — write-through behaviour is sufficient for v1.
- Journaling or copy-on-write crash recovery — accepts the corruption risk on power loss.
- File permissions and ownership bits — added by Phase 27 (User Accounts) on top of ext2.
- Hard links and symbolic links.
- `mmap` of file-backed pages.
- Extended attributes.

## Related

- [Phase 13 Design Doc](../13-writable-fs.md)
