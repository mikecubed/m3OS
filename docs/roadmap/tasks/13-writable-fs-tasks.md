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
| C | VFS dispatch + `WriteableFs` trait | A, B | ✅ Complete |
| D | Syscall surface (`write`, `creat`, `mkdir`, `unlink`, `rmdir`, `rename`, `truncate`, `fsync`) | C | ✅ Complete |
| E | Validation and integration tests | A, B, C, D | ✅ Complete |

---

## Track A — tmpfs

In-memory filesystem backed by kernel-allocated pages, mounted at `/tmp` during init.

- [x] tmpfs filesystem implementation: `kernel/src/fs/tmpfs.rs`.
- [x] Hash-map-of-path-to-page-list backing store; `mkdir`, `create`, `write`, `read`, `unlink`, `rmdir` all routed through tmpfs's `WriteableFs` impl.
- [x] tmpfs mounted at `/tmp` during init in `kernel/src/fs/mod.rs`.
- [x] `fsync` on tmpfs is a no-op (all data lives in RAM).
- [x] Frame allocation for file data uses the Phase 3 frame allocator (no separate cache).

## Track B — FAT32 Write Path

Extend the Phase 8 read-only FAT32 driver to support writes. Either kernel-side `kernel/src/fs/fat32.rs` or the userspace extraction `userspace/fat_server/`.

- [x] FAT32 file create: allocate cluster, write directory entry, append to FAT chain. `kernel/src/fs/fat32.rs` and `userspace/fat_server/`.
- [x] FAT32 append/overwrite writes against existing files (extend cluster chain when crossing cluster boundary).
- [x] FAT32 file delete: free clusters back to the FAT, mark directory entry deleted.
- [x] FAT32 directory create + remove (mkdir / rmdir).
- [x] FAT32 `fsync` flushes dirty sectors to the block device.
- [x] FAT32 write path tolerates writing past the end of a file (cluster allocation kicks in).

## Track C — VFS Dispatch + WriteableFs Trait

VFS gains a `WriteableFs` trait alongside the existing `ReadableFs`; mount-point routing dispatches writes to the correct backend.

- [x] `WriteableFs` trait defined in `kernel-core/src/fs/` (or equivalent VFS module). Implemented for tmpfs and FAT32.
- [x] VFS dispatch in `kernel/src/fs/vfs.rs` and `userspace/vfs_server/` routes write calls to the correct backend by mount point.
- [x] Mount table records whether each mount supports writes; read-only mounts return `EROFS` on write attempts.

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

- **tmpfs:** `kernel/src/fs/tmpfs.rs`, mounted at `/tmp` from `kernel/src/fs/mod.rs`.
- **FAT32 write:** `kernel/src/fs/fat32.rs` (kernel-side) and `userspace/fat_server/` (extracted server).
- **VFS dispatch:** `kernel/src/fs/vfs.rs` and `userspace/vfs_server/`.
- **Syscall surface:** `kernel/src/arch/x86_64/syscall/mod.rs` plus `userspace/syscall-lib/src/lib.rs` wrappers.
- **Validation:** `userspace/tmpfs-test/` test program; FAT32 round-trip exercised by the boot service-config write/read cycle.

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
