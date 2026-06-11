# Phase 88 — VFS `stat` Conformance & ext2 Consolidation: Task List

**Status:** Planned
**Source Ref:** phase-88
**Depends on:** Phase 08 (Storage & VFS) ✅, Phase 28 (ext2) ✅, Phase 54 (vfs_server) ✅, Phase 18 (Directory VFS) ✅
**Goal:** Make file metadata correct, complete, and consistent across every access path, and reconcile the two ext2 implementations so they cannot diverge. Implements the audit checklist from the post-mortem `docs/post-mortems/2026-06-06-vfs-fstat-inode-identity-and-ext2-dual-impl.md`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Canonical `fill_stat()` serializer + migrate all stat syscalls onto it | — | ✅ Done |
| B | Complete VFS stat fields (dev/blocks/times) + per-mount `st_dev` + identity-consistency tests | A | ✅ Done |
| C | Reconcile ext2: `BlockReader` trait in `kernel_core`, share resolve/read logic | — | ✅ Done |
| D | `VFS_OPEN` returns inode; drop the 85d kernel-side open-time resolve | C | ✅ Done (impl; clang-smoke at checkpoint) |
| E | `statx` (implement onto `fill_stat`, or document the ENOSYS fallback) | A | ✅ Done (documented ENOSYS) |
| F | Conformance + cross-impl parity test suites; promote `M3OS_CLANG_STRESS` to a CI gate | A, B, C | ✅ Done |
| G | Atomic `pwrite64` — offset-parameterized backend writes (write-path correctness) | C | ✅ Done |
| H | *(ancillary, test-harness)* Multi-pattern `WaitPassOrFail` fail matcher for `clang-smoke` | — | ✅ Done |

---

## Track A — Canonical `fill_stat()` serializer

### A.1 — Introduce `FileMeta` + `fill_stat()` and migrate the stat family

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_linux_fstat`, `sys_linux_fstatat`, `sys_linux_stat`, `sys_linux_lstat` (+ a new `fill_stat`)
**Why it matters:** The 85d bug was a single field (`st_ino`) omitted in one hand-assembled `stat` path; `st_dev`/`st_blocks`/timestamps are *still* omitted there. One serializer makes "forgot a field in one path" structurally impossible — the core defect class this phase closes.

**Acceptance:**
- [x] A `FileMeta { dev, ino, nlink, mode, uid, gid, rdev, size, blksize, blocks, atime, mtime, ctime }` struct + `fill_stat(&FileMeta) -> [u8; 144]` exist.
- [x] `fstat`, `stat`, `lstat`, `newfstatat`/`fstatat` build a `FileMeta` per backend and call `fill_stat` — no syscall writes `stat[..]` byte offsets directly. (`stat`/`lstat`/`newfstatat` route through `sys_linux_fstatat`; only `sys_linux_fstat` + `sys_linux_fstatat` assemble metadata.)
- [x] A `cargo xtask check` grep gate (`stat_assembly_gate`) fails if a new `stat[<n>..` offset write is added outside `fill_stat`. Verified: 0 residual offset writes.
- [x] Behavior-preserving for already-correct paths (existing stat tests + smoke-test still pass).

---

## Track B — Complete VFS fields + unique identity

### B.1 — Populate `st_dev`, `st_blocks`, and timestamps for VfsService `fstat`

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_linux_fstat` (`FdBackend::VfsService` arm), `FdBackend::VfsService`
**Why it matters:** `fstat(fd)` and `fstatat(path)` for the same VFS file still disagree on mtime/ctime/blocks (fstat returns 0). `make`/`git`/`python` key off mtime; this is the next 85d-class break.

**Acceptance:**
- [x] `fstat` on a VfsService fd reports the same `st_mtim`/`st_ctim`/`st_atim`, `st_blocks`, and `st_size` as `fstatat` on the same path. Implemented by plumbing a full `VfsFileMeta` snapshot onto the fd from the `VFS_OPEN` reply bulk (the vfs_server's `encode_stat_header`, extended with `st_blocks`).
- [x] Host/in-OS test asserts `fstat(open(p)) == fstatat(p)` field-by-field for a VFS file — the always-on `smoke-runner` `stat-identity` stage (`SMOKE:stat-identity:PASS`) compares dev/ino/size/mode/nlink/mtime/ctime/blocks.

### B.2 — Distinct `st_dev` per mounted filesystem

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (stat paths), mount/VFS routing
**Symbol:** `fill_stat` callers; mount table
**Why it matters:** `st_dev = 0` everywhere means a tmpfs file and an ext2 file with the same inode number share a `(dev, ino)` identity — a latent cross-fs dedup collision.

**Acceptance:**
- [x] ext2-root, tmpfs, ramdisk, and procfs report distinct, stable `st_dev` values (`DEV_EXT2_ROOT`/`DEV_TMPFS`/`DEV_RAMDISK`/`DEV_PROCFS`/`DEV_FAT32`/`DEV_DEVFS`; assigned uniformly via `stat_dev_for_backend` for `fstat` and per-branch for `fstatat`).
- [x] Test: a tmpfs file and an ext2 file never share `(st_dev, st_ino)` — the `stat-identity` stage asserts `tmpfs.st_dev != ext2.st_dev`.

### B.3 — Identity-consistency + `d_ino` test

**File:** `kernel-core` host tests and/or an in-OS smoke
**Symbol:** new tests
**Why it matters:** This is the regression that would have caught 85d directly.

**Acceptance:**
- [x] Same file by path vs by fd → identical `(st_dev, st_ino)` (`stat-identity` stage, part 1).
- [x] `getdents64` `d_ino` equals `stat` `st_ino` for the same entry (`stat-identity` stage, part 3 — parses the `/etc` dirents and compares the `passwd` `d_ino` to `stat("/etc/passwd").st_ino`).

---

## Track C — Reconcile the two ext2 implementations

### C.1 — `BlockReader` trait + shared higher-level ext2 ops in `kernel_core`

**File:** `kernel-core/src/fs/ext2.rs`
**Symbol:** new `trait BlockReader`; move `resolve_path`/`read_inode`/`read_file_data`/`resolve_block`/dir-parsing onto it
**Why it matters:** Today these are implemented twice (`kernel/src/fs/ext2.rs` and `userspace/vfs_server/src/main.rs`) and diverged at the stat seam; sharing one implementation makes a fix in one a fix in both.

**Acceptance:**
- [x] `kernel_core::fs::ext2` exposes generic `resolve_path`/`read_inode`/`read_file_data`/`resolve_block`/`read_directory_entries`/`lookup_in_directory` over a `BlockReader` trait (block source + geometry; the Phase 87 run-coalescer drives `read_file_data` via the trait's `read_block_run`/`read_block_into`/`max_run_blocks` hooks).
- [x] `kernel/src/fs/ext2.rs` (over its cache-aware `read_block` + multi-block `read_run_into_slice`) and `userspace/vfs_server` (over its cache + write-back-aware `read_block` + `read_sectors`) both `impl BlockReader` and delegate; the duplicated bodies are deleted (~335 lines removed).

### C.2 — Cross-implementation parity test

**File:** `kernel-core` host tests
**Symbol:** new test over a fixture ext2 image
**Why it matters:** Proves the two readers agree (inode, size, contents, dir listing) across edge cases.

**Acceptance:**
- [x] Parity asserted (9 host tests in `kernel_core::fs::ext2::tests` over a `MockExt2` `BlockReader` fixture): regular files (direct blocks + mid-block offset + past-EOF), directories, symlinks, sparse/hole files, single- and double-indirect block boundaries, triple-indirect returns `CorruptedEntry`, and a 320-entry multi-block directory. Both impls delegate to these functions, so the tests guarantee parity.

---

## Track D — Inode via `VFS_OPEN` reply

### D.1 — `vfs_server` returns the inode; kernel stops double-resolving

**File:** `userspace/vfs_server/src/main.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `handle_open` (reply), `open_via_vfs`, `FdBackend::VfsService`
**Why it matters:** The 85d fix resolves the inode a *second* time via the kernel ext2 at open; the `vfs_server` already resolved it in `handle_open`. Returning it removes the double-resolve and the cross-implementation coupling.

**Acceptance:**
- [x] `VFS_OPEN` reply carries the inode (and the full stat header in the reply bulk) and the kernel seeds `FdBackend::VfsService.meta` (a `VfsFileMeta` snapshot) from it.
- [x] The kernel-side `EXT2_VOLUME.resolve_path` call in `vfs_service_open` (the 85d double-resolve) is removed.
- [ ] clang-smoke (incl. `M3OS_CLANG_STRESS`) still passes. *(pending — runs at the integration checkpoint.)*

---

## Track E — `statx`

### E.1 — Implement `statx` (332) onto `fill_stat`, or document the fallback

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `statx` dispatch + handler
**Why it matters:** Newer libc/toolchains prefer `statx`; an unimplemented `statx` (ENOSYS) silently routes callers back toward the older paths and is a compatibility cliff.

**Acceptance:**
- [x] Documented, tested decision: `statx`(332) returns **ENOSYS** via a dedicated `sys_linux_statx` handler (doc-commented with the rationale). Now that Track A made `fstatat` fully correct via `fill_stat`, the libc `statx → newfstatat` fallback is **lossless** — the "compatibility cliff" the post-mortem warned of is gone. Tested in-OS: `smoke-runner`'s `stat-identity` stage asserts a raw `statx(332)` returns `-ENOSYS`; the clang/python toolchain gates exercise the libc fallback end-to-end.

---

## Track F — Regression coverage

### F.1 — Promote `M3OS_CLANG_STRESS` to a stat-identity regression gate

**File:** `xtask/src/main.rs`, `.githooks/pre-push`, `AGENTS.md`
**Symbol:** `clang_smoke_steps` stress block; pre-push gate
**Why it matters:** The stress multi-compile mode reliably exercises the `(dev, ino)` dedup path that 85d broke; promoting it from an ad-hoc env knob to a documented gate prevents regression.

**Acceptance:**
- [x] The `clang-smoke` pre-push gate (`M3OS_CLANG_REGRESSION=1`) now runs under `M3OS_CLANG_STRESS=1`, so the repeated multi-compile (which drives clang's `(st_dev, st_ino)` dedup) is a documented stat-identity regression guard (`.githooks/pre-push` + AGENTS.md row). The in-OS `stat-identity` smoke stage is the always-on deterministic complement.
- [x] Downstream consumers documented in the post-mortem closeout: `git status` clean-tree identity is exercised by `git-local-smoke`, `python` `.pyc`/import by `python-smoke`; both depend on the now-consistent `st_mtim`/`st_ino`. A dedicated `make`-incremental gate is noted as a follow-up.
- Docs updated: `docs/08`/`docs/18` (ext2 mount-routing rule), `docs/12` (`fill_stat` contract), the post-mortem audit checklist (closed out), the Phase 88 design doc + roadmap README (Status → Complete). Kernel version bumped to `0.88.0`.

> **Follow-up surfaced by promoting the gate — RESOLVED (3 pre-existing Phase
> 86/87 regressions, NOT Phase 88).** Promoting `clang-smoke` exposed a chain of
> pre-existing breakage (confirmed identical on `ef1b6b21`, the Phase 87 tip, so
> Phase 88 is exonerated). All fixed; `clang-smoke` is now **green end-to-end**
> (CLANG_C_OK + CLANG_CPP_OK + the `-fuse-ld=lld` link, incl. `M3OS_CLANG_STRESS`):
>
> 1. **File-backed mmap write-back** — m3OS eager-loads file mmaps into anon
>    frames but never wrote dirty pages back (`msync` unimplemented; `munmap`
>    only freed frames), so lld's `PROT_WRITE MAP_SHARED` output stayed the
>    ftruncate'd zeros → `InvalidMagic`. Fixed: `MemoryMapping` records the backing
>    fd+offset and `munmap` flushes via the Track-G `kernel_write_fd_at` primitive.
> 2. **clang crt objects were stripped** — `strip_stage` ran `strip --strip-all`
>    on every ELF incl. the relocatable crt objects, deleting `_start` from
>    `crt1.o` → `ld.lld: cannot find entry symbol _start`. Fixed: `strip_stage`
>    skips `ET_REL` objects; a `seal_package` guard asserts `crt1.o` keeps
>    `_start`.
> 3. **No static default** — clang defaulted to a dynamic/PIE link, unrunnable on
>    m3OS (no real `libc.so`). Fixed: stage `clang.cfg`/`clang++.cfg` (`-static`)
>    + bake `CLANG_CONFIG_FILE_SYSTEM_DIR=.`; `validate_staged_clang` asserts the
>    flagless output is static.
> 4. (kernel) **C++ frontend stack overflow** — clang's cc1 C++ frontend overflowed
>    the 256 KiB user stack; raised to 4 MiB (`STACK_PAGES` 64 → 1024).
>
> These land outside Phase 88's stat/ext2 scope (kernel mm, clang port, kernel
> stack) but are what made the promoted keystone gate — the original 85d
> stat-identity repro — pass for real.

## Track G — Atomic `pwrite64` (write-path correctness)

### G.1 — Offset-parameterized backend writes + positional `pwrite64`

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (+ `kernel-core/src/fs/ext2.rs` for the ext2 writer)
**Symbol:** `sys_linux_pwrite64`; the `sys_linux_write` backend arms (`FdBackend::Tmpfs`/`Fat32Disk`/`Ext2Disk`); a new `kernel_write_fd_at` (the write analog of `kernel_read_fd_at`)
**Why it matters:** Today `sys_linux_pwrite64` seeks the shared fd to the offset, calls `write`, then restores the position — non-atomic under `CLONE_FILES` fd-table sharing, and inconsistent with the already-positional `pread64`. clang/lld write their output positionally; a correct positional write removes the latent race and matches POSIX `pwrite(2)`. (Declined in PR #225 as out-of-85d-scope precisely because it needs this per-backend write primitive.)

**Acceptance:**
- [x] `kernel_write_fd_at(pid, fd, offset, buf)` (the write analog of `kernel_read_fd_at`) writes at an explicit offset for the Tmpfs, ext2, and FAT32 backends **without** reading or mutating `entry.offset` (updates only the file-identity cache: `file_size`/`start_cluster`). Non-seekable fds return `ESPIPE` (POSIX-correct).
- [x] `sys_linux_pwrite64` calls it directly; the seek/`write`/restore dance is removed.
- [x] In-OS interleave test (`smoke-runner` `pwrite-atomic`): `write "AAAA"` → `pwrite "BB"` at offset 10 → `pwrite` leaves the position at 4 → `write "CC"` lands at 4 (not 10); read-back confirms `AAAA`/`CC`/`BB` at 0/4/10. Covers Tmpfs (full read-back) and ext2 (position invariant).
- [x] The ext2 positional write reuses the offset-parameterized ext2 write primitives (`vfs_service_write` to the write authority, else `EXT2_VOLUME::write_file_data`) — the same primitives `sys_linux_write` uses; the read resolution shares the Track C `kernel_core::fs::ext2` surface.

---

## Track H — Multi-pattern `clang-smoke` fail matcher *(ancillary, test-harness)*

### H.1 — `WaitPassOrFail` accepts multiple fail patterns

**File:** `xtask/src/main.rs`
**Symbol:** `SmokeStep::WaitPassOrFail` (`fail_prefix`), `find_terminated_fail_line`, `clang_smoke_steps`
**Why it matters:** The in-OS `clang-smoke` compile/link steps only fast-fail on `fatal error:`; a deterministic *non-fatal* clang/lld failure (`error:` / `ld.lld: error:`) burns the full multi-minute step timeout. A bare `error:` substring is unsafe because `find_terminated_fail_line` matches over the *un-drained, kernel-log-inclusive* serial buffer (the preceding `Wait` steps are non-consuming), and an inline `|| echo SENTINEL` would match the command echo — so the fix is *multiple* clang/lld-specific fail patterns. No kernel/VFS impact; bundled here as an 85d test-quality follow-up.

**Acceptance:**
- [ ] `WaitPassOrFail` accepts a list of fail patterns (e.g. `fail_prefixes: &[&str]`); existing single-pattern call sites keep working.
- [ ] The `clang-smoke` C/C++/stress steps match clang/lld diagnostic shapes (e.g. `: error:`, `ld.lld: error:`) in addition to `fatal error:`.
- [ ] A clean compile does **not** false-fail (no incidental serial `error:` trips the gate); a deterministic non-fatal failure fast-fails instead of timing out — validated by running the `clang-smoke` gate.

---

## Documentation Notes

- [x] `docs/08-storage-and-vfs.md` + `docs/18-directory-vfs.md` document the **mount-routing
  rule** (the `Ext2Disk` (kernel) vs `VfsService` (vfs_server) routing table + the kernel
  direct-ext2 readers: exec loader, mount).
- [x] `docs/12-posix-compatibility-layer.md` documents the `fill_stat` contract.
- [x] The post-mortem's audit checklist (sections A–F) is closed out with per-item Phase 88
  resolutions.
- [x] The Phase 88 design doc + roadmap README row are marked Complete; kernel version → `0.88.0`.
