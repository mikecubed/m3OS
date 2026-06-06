# Phase 93 - VFS `stat` Conformance & ext2 Dual-Implementation Consolidation

**Status:** Planned
**Source Ref:** phase-93
**Depends on:** Phase 08 (Storage & VFS) ✅, Phase 28 (ext2 Filesystem) ✅, Phase 54 (Deep Serverization / `vfs_server`) ✅, Phase 18 (Directory VFS) ✅
**Builds on:** Hardens the filesystem **metadata** path (`stat` family + file identity) and removes the long-standing **two-independent-ext2-implementations** hazard (kernel `EXT2_VOLUME` vs the ring-3 `vfs_server`'s `Ext2State`). Adjacent to Phase 92 (VFS Bulk-I/O), which addresses the same layer's **throughput**; this phase addresses its **correctness/consistency**. No on-disk format change.
**Primary Components:** `kernel/src/arch/x86_64/syscall/mod.rs` (the `stat` family + `vfs_service_stat_path` + `open_via_vfs`), `kernel/src/fs/ext2.rs`, `userspace/vfs_server/src/main.rs`, `kernel-core/src/fs/ext2.rs`, `kernel/src/process/mod.rs` (`FdBackend`)

> **Origin:** Surfaced in Phase 85d (in-OS clang). See the post-mortem
> `docs/post-mortems/2026-06-06-vfs-fstat-inode-identity-and-ext2-dual-impl.md`
> for the full incident, root cause, and the audit checklist this phase implements.

## Milestone Goal

Every file on m3OS reports **correct, complete, and consistent** `stat` metadata — the same
`(st_dev, st_ino)`, size, mode, link count, and timestamps — **regardless of how it is
accessed** (by path vs by fd; via the kernel ext2 driver vs the ring-3 `vfs_server`). The
two ext2 implementations are reconciled so a fix in one is a fix in both. The learner sees
why POSIX file *identity* is load-bearing for real toolchains (clang/make/git/python all
key off `(dev, ino, mtime)`), and how a microkernel that splits a filesystem across a kernel
driver and a userspace server must guard the metadata contract at the seam.

## Why This Phase Exists

Phase 85d's in-OS clang gate failed intermittently with `redefinition of 'main'`: the
kernel's `fstat`-by-fd returned `st_ino = 0` for `vfs_server`-backed files while
`fstatat`-by-path returned the real inode, so clang's `(st_dev, st_ino)` file-dedup
collapsed `<stdio.h>` onto the open main source → recursive self-include. The acute
`st_ino` bug was fixed in 85d, but it exposed three systemic problems:

1. **`struct stat` is hand-assembled field-by-field in many syscall paths**, so omitted
   fields ship silently. The 85d fix added `st_ino` for VFS files, but that *same path
   still leaves `st_dev`, `st_blocks`, and all three timestamps at `0`* — the next
   clang/make/git-class bug, already loaded.
2. **ext2 logic is implemented twice** — `kernel/src/fs/ext2.rs` and
   `userspace/vfs_server/src/main.rs` reimplement `resolve_path` / `read_inode` /
   `read_file_data` / directory walking / indirect-block resolution; they share only
   byte-parsing in `kernel_core::fs::ext2` and **can diverge** (they did, at the stat seam).
3. **File identity is not guaranteed stable/unique across access paths or filesystems**
   (`st_dev = 0` almost everywhere → cross-fs `(dev, ino)` collisions are possible).

## Learning Goals

- POSIX `stat` semantics and why `(st_dev, st_ino)` identity + `st_mtim` are a hard contract
  that real userspace depends on (file dedup, incremental builds, VCS index, import caches).
- The cost/benefit of splitting a filesystem across a kernel driver and a userspace server,
  and how to keep their observable behavior identical.
- Designing a *single source of truth* for metadata serialization and for filesystem logic.

## Feature Scope

**In scope**
- A single canonical `fill_stat()` serializer (a normalized metadata struct → the full
  `struct stat`/`statx` buffer), routed through `fstat`, `stat`, `lstat`,
  `newfstatat`/`fstatat`, and `statx`. No syscall hand-assembles `stat` offsets.
- Complete, correct `stat` fields for every `FdBackend` and path type, verified by an
  identity-consistency test (same file by fd and by path, via both ext2 implementations).
- Distinct `st_dev` per mounted filesystem so identity is unique cross-fs.
- Reconcile the two ext2 implementations: lift `resolve_path` / `read_inode` /
  `read_file_data` / `resolve_block` / directory parsing into `kernel_core::fs::ext2` over a
  `BlockReader` trait, so kernel and `vfs_server` share **one** implementation.
- Replace the 85d open-time kernel-side inode resolution with the `vfs_server` returning the
  inode in its `VFS_OPEN` reply (removes the double-resolve + cross-impl coupling).
- A cross-implementation parity test and a `stat`-conformance test suite.
- `statx` (syscall 332): implement (mapping onto `fill_stat`) or document the deliberate
  fallback to `fstat`.

**Out of scope**
- Bulk-I/O throughput / readahead / write-back / fairness (that is Phase 92).
- On-disk ext2 format changes; writable-VFS feature growth.
- A unified VFS that eliminates one of the two ext2 front-ends entirely (a possible
  *longer-term* architecture decision; this phase reduces divergence via shared logic).

## Important Components and How They Work

- **`fill_stat()` (new, kernel)** — the one serializer. Input: `FileMeta { dev, ino, nlink,
  mode, uid, gid, rdev, size, blksize, blocks, atime, mtime, ctime }`. Output: the
  144-byte `struct stat` (and a `statx` variant). Every stat syscall builds a `FileMeta`
  for its backend and calls this; none touch byte offsets directly.
- **`kernel_core::fs::ext2` `BlockReader` trait (new)** — `fn read_block(&self, n: u32) ->
  Result<Vec<u8>, Ext2Error>`. The kernel supplies a `crate::blk`-backed reader (+ its
  cache); the `vfs_server` supplies a `sys_block_read`-backed reader. The higher-level
  ext2 operations move here as functions generic over the reader, so both call sites share
  one implementation.
- **`VFS_OPEN` reply (extended)** — `vfs_server::handle_open` already resolves the inode; it
  returns it to the kernel so `FdBackend::VfsService { inode }` is seeded without a second
  resolve.
- **Identity (`st_dev`)** — assign each mount (ext2 root, tmpfs, ramdisk, procfs) a stable,
  distinct device id so `(st_dev, st_ino)` is globally unique.

## How This Builds on Earlier Phases

Phase 28 built the kernel ext2; Phase 54 moved the userspace-facing filesystem behind the
ring-3 `vfs_server` (introducing the second ext2 implementation); Phase 18 built the
directory VFS. This phase pays down the metadata/consistency debt those splits accrued,
which Phase 85d made acute. It is complementary to Phase 92 (same layer, throughput) and a
prerequisite-quality gate for the heavy-toolchain phases (87 Node.js, 88 Claude Code) that
lean on `make`/`git`/`stat` correctness.

## Implementation Outline

1. Introduce `FileMeta` + `fill_stat()`; migrate `fstat`/`fstatat`/`lstat`/`stat` onto it
   (behavior-preserving), with the previously-missing VFS fields (dev/blocks/times) filled.
2. Add `kernel_core::fs::ext2::BlockReader` and move `resolve_path`/`read_inode`/
   `read_file_data`/`resolve_block`/dir parsing onto it; reroute kernel + `vfs_server`.
3. Extend `VFS_OPEN` to return the inode; drop the 85d kernel-side open-time resolve.
4. Assign per-mount `st_dev`.
5. Implement (or explicitly defer) `statx`.
6. Land the conformance + parity + identity test suites (host + in-OS).

## Acceptance Criteria

- [ ] A host test opens the same ext2 file by path (`fstatat`) and by fd (`open`+`fstat`)
      and asserts byte-identical `struct stat` (incl. `st_ino`, `st_dev`, `st_mtim`,
      `st_blocks`).
- [ ] A test reaching the same file through the kernel ext2 (`Ext2Disk`) and the
      `vfs_server` (`VfsService`) asserts identical `(st_dev, st_ino)`, size, mode, times.
- [ ] No stat syscall assembles a `stat` buffer by offset; all route through `fill_stat()`
      (enforced by review + a grep gate in `cargo xtask check`).
- [ ] `getdents64` `d_ino` equals `stat` `st_ino` for the same entry.
- [ ] Distinct `st_dev` for ext2 vs tmpfs vs ramdisk vs procfs (no cross-fs identity
      collision).
- [ ] A cross-implementation parity test (regular/dir/symlink/sparse/indirect/large-file/
      large-dir corpus) passes between kernel ext2 and `vfs_server` ext2.
- [ ] `M3OS_CLANG_STRESS` multi-compile mode is wired as a CI-able stat-identity regression
      guard (it directly exercises the dedup path that 85d broke).
- [ ] `statx` implemented (onto `fill_stat`) or its `ENOSYS`-fallback documented + tested.

## Companion Task List

See [Phase 93 Tasks](./tasks/93-vfs-stat-and-ext2-consolidation-tasks.md).

## How Real OS Implementations Differ

Linux assembles `struct stat` once in `cp_new_stat`/`cp_statx` from a kernel-internal
`struct kstat`; every `stat` variant funnels through it, so a missing field is structurally
impossible — exactly the `fill_stat()` discipline this phase adopts. Linux assigns each
mount a real `st_dev` from its `super_block`, and a filesystem has **one** implementation in
the kernel (a userspace FUSE server is the consistency authority for its mount, not a
second copy of the same fs). m3OS's microkernel split (a kernel ext2 *and* a userspace ext2
over the same disk) is unusual; production microkernels (e.g. seL4-based systems, QNX) keep
a single filesystem server authoritative per mount. This phase moves m3OS toward that single
-source-of-truth model without (yet) deleting the kernel-side reader the exec loader needs.

## Deferred Until Later

- Fully eliminating one of the two ext2 front-ends (architectural; needs the exec loader to
  obtain binaries through the VFS, or a kernel-only boot reader with a hard read-only
  contract). Tracked as a future decision.
- Extended attributes, ACLs, `O_PATH`/`AT_EMPTY_PATH` corner cases, `st_birthtime`.
- Performance work (Phase 92).
