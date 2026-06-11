# Post-mortem & handoff: VFS `fstat` inode identity, and the ext2 dual-implementation hazard

**Incident:** In-OS `clang` intermittently failed to compile a trivial C program after a
heavy `pkg install`, with `error: redefinition of 'main'` — `#include <stdio.h>`
was being resolved to the program's own source file (`/usr/src/hello.c`), which
then recursively `#include`d itself ~16 levels deep.
**Status:** Acute bug fixed (Phase 85d, branch `feat/phase-85d-clang-llvm`). Systemic
issues (this doc) tracked as **[Phase 88 — VFS `stat` Conformance & ext2
Consolidation](../roadmap/88-vfs-stat-and-ext2-consolidation.md)**
([tasks](../roadmap/tasks/88-vfs-stat-and-ext2-consolidation-tasks.md)), which implements
the audit checklist below.
**Severity:** Medium-High — flaky (~1 compile in 2–3 after a fresh install), blocked the
Phase 85d in-OS clang acceptance gate non-deterministically. The *underlying* defect is a
POSIX `stat` correctness bug that affects **any** inode-identity-dependent userspace tool
(clang, make, git, ld/ar, python, autotools), not just clang.
**Owners:** Kernel (VFS/syscall stat path), vfs_server, ext2.
**Fix (acute):** `FdBackend::VfsService` now carries the real ext2 inode (resolved at
open) and `sys_linux_fstat` reports it as `st_ino`. Code: `kernel/src/process/mod.rs`
(`FdBackend::VfsService.inode`), `kernel/src/arch/x86_64/syscall/mod.rs`
(`open_via_vfs` inode resolution + the `VfsService` arm of `sys_linux_fstat`).

---

## TL;DR

`/usr` files are served to userspace by the **ring-3 `vfs_server`** (the `VfsService` fd
backend), not the in-kernel ext2 driver. The kernel's **`fstat`-by-fd path returned
`st_ino = 0`** for every `VfsService` file (it simply never wrote the inode field), while
**`fstatat`-by-path returned the *real* inode**. clang's `FileManager` identifies files by
`UniqueID = (st_dev, st_ino)`; with `st_ino = 0` for both the open main source and an
open `<stdio.h>`, clang concluded they were the *same file*, reused the main source's
buffer for stdio.h, and recursively self-included → "redefinition of main".

The acute fix makes `fstat` report the real inode. But the real lesson is structural:

1. **There are two independent ext2 implementations** — the kernel's (`EXT2_VOLUME`) and
   the vfs_server's (`Ext2State`) — and their metadata outputs **diverged silently**.
2. **`stat` is assembled per-path, field-by-field, in many places**, so a single omitted
   field (here `st_ino`; `st_dev`, `st_blocks`, and the three timestamps are *still* zero
   for `VfsService` `fstat`) ships unnoticed.
3. **Which implementation serves a given file is non-obvious**, which made diagnosis slow.

This doc records the incident, the structural hazards, what to do about them, and a
concrete checklist for a future audit pass.

---

## The two ext2 implementations (the heart of the matter)

m3OS reads ext2 through **two** completely separate code paths that share only the
low-level *byte parsing* in `kernel_core::fs::ext2` (`Ext2Inode::parse`,
`Ext2Superblock`, `Ext2DirEntry::parse_block`, `inode_block_group`,
`inode_index_in_group`). Everything above that — path resolution, inode reading, file-data
reading, directory walking, block-pointer (indirect/double-indirect) resolution — is
**implemented twice**:

| Concern | Kernel ext2 | vfs_server ext2 |
|---|---|---|
| Location | `kernel/src/fs/ext2.rs` | `userspace/vfs_server/src/main.rs` |
| Type | `EXT2_VOLUME: Mutex<Option<Ext2Volume>>` | `struct Ext2State` |
| Used by | `Ext2Disk` fds; the **streaming exec loader** (`DiskElfSource`); early boot/mount | `VfsService` fds (the userspace-facing VFS) |
| Disk access | `crate::blk::read_sectors` / `write_sectors` | `sys_block_read` syscall → `crate::blk::read_sectors` |
| Caching | **`block_cache`**: `Mutex<BTreeMap<u32, Vec<u8>>>`, fill-and-hold, `BLOCK_CACHE_MAX = 4096`, **no eviction**; `write_block` invalidates on write | **none** (raw `sys_block_read` every call) |
| Mutability | read-write (serialized by the outer `Mutex`) | **read-only** |
| `resolve_path` | `Ext2Volume::resolve_path` (ext2.rs:~480) | `Ext2State::resolve_path` (main.rs:~178) |
| `read_inode` | ext2.rs:~242 | main.rs:~101 |
| `read_file_data` | ext2.rs:~365 | main.rs:~228 |

Both bottom out at the **same** `crate::blk` device (a ring-3 block driver presenting as
`RemoteBlockDevice`), so at the *device* level they are coherent. The hazard is everything
**above** the device: two implementations that must agree, plus the kernel's block cache,
plus a stat layer that doesn't share a single fill routine.

### Routing: which implementation serves a file?

This was **not** obvious during diagnosis and cost real time. As observed on
`feat/phase-85d-clang-llvm`:

- `init` mounts `/` via the vfs_server (`VFS_MOUNT_EXT2_ROOT`; see
  `vfs_server::mount_policy_action`, `("/", "ext2") => VFS_MOUNT_EXT2_ROOT`).
- Userspace `open("/usr/...")` therefore routes to the **vfs_server** → `VfsService`
  backend → reads via `vfs_service_read` (kernel side) → IPC → vfs_server raw reads.
- The **kernel** ext2 (`Ext2Disk` backend / `EXT2_VOLUME`) is used by the kernel itself —
  notably the streaming ELF exec loader (`DiskElfSource` reads the clang binary's blocks
  directly), and `sys_linux_openat`'s ext2 branch
  (`kernel/src/arch/x86_64/syscall/mod.rs`, `FdBackend::Ext2Disk { inode_num, .. }`).

So the *same path* can be reached through *either* implementation depending on who opens it
and how the mount table resolves. **Document and pin this routing** — see the audit
checklist.

---

## Root cause of the acute bug

clang's preprocessor resolves `#include <stdio.h>` through `FileManager::getFileRef`,
which keys file identity on `UniqueID = (st_dev, st_ino)` to (a) dedup the same file reached
via different paths and (b) recognize a file it already has open. On opening a candidate,
clang `fstat`s the fd to read that identity.

The kernel's `sys_linux_fstat`
(`kernel/src/arch/x86_64/syscall/mod.rs`) builds the `struct stat` **per backend**. The
`VfsService` arm returned a tuple `(mode, uid, gid, size, rdev)` to a common tail that
writes `st_mode`/`st_uid`/`st_gid`/`st_rdev`/`st_size`/`st_blksize` — **but never
`st_ino`** (offset 8). The stat buffer is zero-initialized, so **every `VfsService` file
reported `st_ino = 0`**.

Consequence: clang opens `hello.c` (main input) → `fstat` → `(dev, 0)`; opens
`/usr/include/stdio.h` → `fstat` → `(dev, 0)`. The UniqueIDs **collide**. clang returns the
cached `hello.c` `FileEntry` for the stdio.h lookup and reuses its buffer. `hello.c` line 1
(`#include <stdio.h>`) thus includes `hello.c`, which includes itself, … → `main` defined
many times → **redefinition**.

### Why it was flaky (and only after `pkg install`)

`fstatat`-by-path (`sys_linux_fstatat`) *did* return the real inode (via
`vfs_service_stat_path` → `vfs_stat.ino`). clang sometimes resolves a file's identity via
`fstatat(path)` (real, unique inode → no collision) and sometimes via `open`+`fstat(fd)`
(the buggy `0` → collision), depending on its include-cache state and search order. The
heavy in-OS install (writing ~1500 sysroot/header files, churning kernel memory and the
include search tree) shifted clang toward the `fstat(fd)` path often enough to make the
collision appear ~1 compile in 2–3. The plain fast-iter path (no install) effectively never
hit it, which is why it reproduced **only** through the full `clang-smoke` gate.

### Diagnostic note (for future debuggers)

The decisive evidence was a **full serial dump on failure**
(`M3OS_SMOKE_SERIAL_DUMP=<path>` — the smoke runner only writes the complete history to
that file on a failing/retryable step; a passing run shows just step narration + the last
80 lines, so kernel `log::warn!` traces are invisible on pass). The dump showed:

```
[vfsread] h=0xb0000 ... count=93 -> 93 :: "#include <stdio.h>\nint main(void) { ... }"
In file included from /usr/src/hello.c:1:        ← ×16, immediately after, with
In file included from /usr/src/hello.c:1:           NO new [vfsread] for stdio.h
...
/usr/src/hello.c:2:5: error: redefinition of 'main'
```

The "no fresh read for stdio.h" was the tell: clang didn't *read* stdio.h, it **deduped**
it onto the already-open main source — i.e. a `stat`-identity bug, not a read/content bug.

Earlier hypotheses that were **wrong** (recorded so the next person doesn't re-walk them):
read-doubling in `read_file_data`; ext2 block-cache incoherence; the `pread64`/mmap path;
frame-staleness in file-backed mmap; an IPC `pending_bulk` mixup. All were excluded by
auditing (the kernel ext2 is `Mutex`-serialized and cache-invalidates on write; the
vfs_server is uncached; the bulk path is task-keyed and `.take()`-cleared). The bug was
*not* in the data path at all — it was in file *identity*.

---

## The fix (acute)

1. `FdBackend::VfsService` gained an `inode: u32` field (`kernel/src/process/mod.rs`).
2. `open_via_vfs` resolves the real ext2 inode at open time via the kernel ext2
   (`EXT2_VOLUME.resolve_path(path)` — serialized, coherent, same disk as the vfs_server)
   and stores it on the fd.
3. The `VfsService` arm of `sys_linux_fstat` writes that inode to `st_ino`.

Now distinct VFS files always have distinct, stable `(st_dev, st_ino)` identities that
**agree between `fstat` and `fstatat`**, so clang can no longer false-dedup. The fix is
deterministic (not a probability reduction).

**Caveat on the fix's shape:** resolving the inode through the *kernel* ext2 at open
introduces a quiet dependency that the kernel ext2 and vfs_server agree on inode numbers
(they do — same on-disk fs). A cleaner long-term shape is for the **vfs_server to return
the inode in its `VFS_OPEN` reply** (it already resolves it in `handle_open`; today it only
packs `handle | file_size`). That removes the double-resolve and the cross-implementation
coupling. Tracked as a follow-up.

---

## Systemic findings (what the audit should actually fix)

### 1. `stat` is assembled field-by-field in N places → fields silently go missing

`st_ino` was the field that broke clang. But the **same `VfsService` `fstat` path still
omits**: `st_dev` (offset 0), `st_blocks` (offset 64), and **all three timestamps**
(`st_atim`/`st_mtim`/`st_ctim`, offsets 72/88/104) — they remain `0`. `fstatat` for the
same file *does* populate times (from `vfs_service_stat_path`). So `fstat(fd)` and
`fstatat(path)` for the same VFS file **still disagree** on mtime/ctime/blocks. That is the
*next* clang-class bug waiting to happen:

- `make` / `cmake` / `ninja` decide staleness by `st_mtim`; a `0` mtime via `fstat` makes
  every target look ancient (or, combined with a real mtime elsewhere, inconsistent).
- `git` stores `(st_ino, st_size, st_mtim, st_ctim)` in its index; inconsistent values
  force constant re-hashing or mark a clean tree dirty.
- `python` `importlib` caches `.pyc` validity on source mtime.

**Recommendation:** introduce **one canonical `fill_stat(...)`** helper (kernel-side) that
takes a normalized metadata struct `{ dev, ino, nlink, mode, uid, gid, rdev, size,
blksize, blocks, atime, mtime, ctime }` and writes the full `struct stat`. Route
`fstat`, `fstatat`/`newfstatat`, `lstat`, and any future `statx` through it. No syscall
should hand-assemble a `stat` buffer offset-by-offset. This structurally prevents
"forgot a field in one path" forever.

### 2. Two ext2 implementations diverge silently

The kernel ext2 and the vfs_server ext2 reimplement `resolve_path`, `read_inode`,
`read_file_data`, directory walking, and indirect-block resolution. They can — and here
did, at the boundary — produce different observable behavior. Bugs/edge-cases fixed in one
won't be fixed in the other.

**Recommendation (near-term):** lift the higher-level operations into
`kernel_core::fs::ext2` as **pure functions over a `BlockReader` trait**
(`fn read_block(&self, n: u32) -> Result<Vec<u8>, _>`), so the kernel (`crate::blk`) and
the vfs_server (`sys_block_read`) supply only the block source and share *one*
implementation of `resolve_path` / `read_inode` / `read_file_data` / dir parsing. Today
only the *byte struct parsing* is shared; the *logic* is not.

**Recommendation (longer-term, architectural decision needed):** decide whether two ext2
front-ends should exist at all. Options:
- (a) Kernel ext2 limited to boot + the exec loader; the vfs_server is the sole userspace
  filesystem authority.
- (b) The kernel's `Ext2Disk` path delegates to the vfs_server (one reader, IPC cost).
- (c) Keep both but force them through the shared `kernel_core` logic (the near-term rec).

(c) is the cheap risk reducer; (a)/(b) are the real fix to "two sources of truth".

### 3. Identity (`st_dev`, `st_ino`) is not guaranteed stable/unique across access paths

POSIX requires that two paths refer to the same file **iff** they share `(st_dev, st_ino)`,
and that the pair is **stable** for a file's lifetime. m3OS currently:
- leaves `st_dev = 0` almost everywhere (tmpfs, ramdisk, ext2 via VFS). If two different
  filesystems both report `dev = 0` with overlapping inode numbers, **cross-fs identity
  collisions** are possible (e.g. a tmpfs file and an ext2 file colliding). Assign each
  mounted filesystem a distinct `st_dev`.
- derived ext2 `st_ino` correctly only on *some* paths (now both `fstat` and `fstatat` for
  VFS files, post-fix — but verify ramdisk/procfs/tmpfs and the `Ext2Disk` path too).

### 4. Block-cache policy is worth a deliberate look

`block_cache` is fill-and-hold, `BLOCK_CACHE_MAX = 4096`, **no eviction**. After 4096
distinct blocks are read (trivially exceeded by a 125 MB install), **all further reads
bypass the cache** — a silent performance cliff, and a coherency model that *relies on*
`write_block` being the only mutator that touches cached blocks (true today: the only
direct `write_sectors` are the superblock/BGD, which aren't block-cached). Document this
contract explicitly and consider an LRU/clock eviction so the cache stays useful under
large workloads.

---

## Audit checklist — RESOLVED by [Phase 88](../roadmap/88-vfs-stat-and-ext2-consolidation.md)

Phase 88 implemented this checklist. Boxes below are checked with the resolution.

**A. `stat` family conformance (highest priority — this is where the bug lived)**
- [x] Stat syscalls enumerated: `stat`(4)/`lstat`(6)/`newfstatat`(262) all route through
      `sys_linux_fstatat`; `fstat`(5) is `sys_linux_fstat`; `statx`(332) returns a
      *documented* ENOSYS so libc falls back to the now-correct `newfstatat` (Track E).
- [x] Every `struct stat` field populated per backend via the canonical
      `FileMeta`/`fill_stat` builders; `VfsService` `fstat` now carries a full
      `VfsFileMeta` snapshot (dev/ino/nlink/mode/uid/gid/size/blocks/a-m-c-times) seeded
      from the `VFS_OPEN` reply (Track A/B/D).
- [x] **Identity consistency** asserted in-OS: `smoke-runner`'s always-on `stat-identity`
      stage checks `fstat(open(p)) == fstatat(p)` field-for-field for a VFS file (Track B.3).
- [x] `(st_dev, st_ino)` unique across filesystems — synthetic per-mount `st_dev`
      (`DEV_EXT2_ROOT`/`DEV_TMPFS`/`DEV_RAMDISK`/`DEV_PROCFS`/`DEV_FAT32`/`DEV_DEVFS`) via
      `stat_dev_for_backend`; `st_dev = 0` no longer used for real files (Track B.2).
- [x] Per-path hand-assembly replaced by the single `fill_stat`; a `cargo xtask check`
      stat-assembly gate fails any reintroduced `stat[<n>..` offset write (Track A).

**B. Two-implementation parity (kernel ext2 vs vfs_server ext2)**
- [x] Parity host tests (`kernel_core::fs::ext2::tests`, 9 tests over a `MockExt2`
      `BlockReader`): regular/dir/symlink/sparse/single+double-indirect/large-dir, plus
      triple-indirect-unsupported (Track C.2).
- [x] The two `resolve_path`/`read_inode`/`read_file_data`/`resolve_block`/dir-parsing
      copies are **collapsed onto one shared `kernel_core::fs::ext2` implementation** over
      a `BlockReader` trait; both engines delegate (Track C.1, finding #2 closed).
- [x] Symlink/`.`/`..`/`AT_SYMLINK_NOFOLLOW` behaviour is identical **by construction** —
      there is now a single `resolve_path`.

**C. Directory / `readdir` consistency**
- [x] `getdents64` `d_ino` now equals `stat` `st_ino` — the synthetic `d_ino = idx+1` was
      replaced by `path_ino()` (same routing `stat` uses); asserted in `stat-identity`
      (Track B.3).
- [~] `st_nlink` for directories: the ext2 inode's on-disk `links_count` is reported
      directly (correct for ext2-backed dirs); a synthetic `2 + subdir_count` for virtual
      dirs is out of scope.

**D. Block cache & coherency** — largely owned by Phase 87/88 throughput work, not the
   stat pass; recorded here as cross-references.
- [x] Mutations route through the write-through cache; out-of-band routed writes call
      `invalidate_block_cache` (Phase 87/88).
- [ ] LRU/clock eviction past `BLOCK_CACHE_MAX` (finding #4) — **deferred** (still
      fill-and-hold).
- [x] Cross-process write→read coherency is covered by the always-on `ext2-coherence`
      smoke (Phase 88).

**E. Mount routing**
- [x] Documented in `docs/08-storage-and-vfs.md` + `docs/18-directory-vfs.md` (the
      `Ext2Disk` vs `VfsService` rule + the kernel direct-ext2 readers) — Track F.

**F. Downstream consumers (regression-grade)**
- [x] clang multi-header compile (the original symptom): the `clang-smoke` gate runs under
      `M3OS_CLANG_STRESS=1` as a promoted pre-push stat-identity regression guard (Track F).
      **NOTE — separate pre-existing failure surfaced:** the `clang-smoke` gate currently
      fails (confirmed identical on the pre-Phase-88 commit `ef1b6b21`): clang compiles fine,
      but `lld`'s `PROT_WRITE` file-backed-mmap output on `/tmp` is not written back →
      all-zeros binary (`cannot find _start` / `InvalidMagic`). This is a Phase 86/87
      file-backed-mmap-write-back regression unrelated to the stat work — tracked as a
      follow-up. The deterministic `stat-identity` smoke stage is the green stat-identity
      guard meanwhile.
- [~] `make`/`git`/`python` rely on the now-correct, consistent `st_mtim`/`st_ino`; the
      existing `git-local-smoke` (clean-tree `git status`) and `python-smoke` gates
      exercise them. A dedicated `make`-incremental gate is a follow-up.

---

## Key code references

- Kernel ext2: `kernel/src/fs/ext2.rs` — `EXT2_VOLUME` (Mutex), `read_inode`,
  `resolve_path`, `read_file_data`, `read_block`/`read_block_into_slice`, `write_block`
  (cache invalidation), `allocate_block`, `block_cache` (`BLOCK_CACHE_MAX`).
- vfs_server ext2: `userspace/vfs_server/src/main.rs` — `Ext2State`, `read_block`
  (uncached), `read_inode`, `resolve_path`, `read_file_data`, `handle_open`, `handle_read`.
- Shared parsing: `kernel_core::fs::ext2` — `Ext2Inode`, `Ext2Superblock`, `Ext2DirEntry`,
  `inode_block_group`, `inode_index_in_group`.
- Stat paths: `kernel/src/arch/x86_64/syscall/mod.rs` — `sys_linux_fstat` (the bug site,
  `VfsService` arm), `sys_linux_fstatat` (`vfs_service_stat_path` → real inode),
  `vfs_service_stat_path`.
- VFS fd backend: `kernel/src/process/mod.rs` — `FdBackend::VfsService { service_handle,
  file_size, inode }`.
- VFS open/read kernel side: `open_via_vfs`, `vfs_service_read` (the `[vfsread]` trace).
- Block syscall used by vfs_server: `sys_block_read` (→ `crate::blk::read_sectors`).
- Smoke serial dump: `M3OS_SMOKE_SERIAL_DUMP=<path>` (full history on failure) — see
  `dump_serial` in `xtask/src/main.rs`.

## Related docs

- `docs/28-ext2-filesystem.md` — Phase 28 ext2 design.
- `docs/08-storage-and-vfs.md`, `docs/18-directory-vfs.md`, `docs/13-writable-filesystem.md`,
  `docs/24-persistent-storage.md` — storage/VFS architecture.
- `docs/12-posix-compatibility-layer.md` — the POSIX surface that `stat` is part of.
- `docs/appendix/file-backed-mmap.md` — file-backed mmap (an early, excluded suspect).
