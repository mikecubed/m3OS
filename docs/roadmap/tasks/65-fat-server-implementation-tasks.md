# Phase 65 — FAT Server Implementation: Task List

**Status:** Planned
**Source Ref:** phase-65
**Depends on:** Phase 54 (Deep Serverization) ✅, Phase 24 (FAT32 Filesystem) ✅, Phase 39 (Unix Domain Sockets) ✅
**Goal:** Replace the `fat_server` ENOSYS stub with real FAT32 file operations (open, read, write, getdents, stat, unlink, rename); lift the FAT32 implementation to `kernel-core` for host-testability; extend `vfs_server` with mount-point routing; write a regression suite covering write/read consistency and restart persistence; update Phase 54 docs to mark the storage serverization complete and close audit Red Flag #14.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | IPC verb decoding: `FatRequest` / `FatReply` enum, `dispatch.rs` skeleton | None | Planned |
| B | FAT32 lift to `kernel-core`: `BlockDevice` trait, host-testable volume operations | A | Planned |
| C | Server-side FD table: `FdTable`, `FatFile`, per-client isolation | B | Planned |
| D | `vfs_server` mount-point routing: `MountTable`, forward-to-fat_server path | C | Planned |
| E | Regression suite: write/read, getdents, stat, unlink, restart-persistence | D | Planned |
| F | Phase 54 design doc + task doc closure; audit Red Flag #14 closure note | E | Planned |

---

## Track A — IPC Verb Decoding

### A.1 — Define `FatRequest` and `FatReply` enum types

**File:** `userspace/fat_server/src/protocol.rs`
**Symbol:** `FatRequest`, `FatReply`
**Why it matters:** A typed protocol enum replaces the ad-hoc byte dispatch that previously fell through to ENOSYS on every arm.

**Acceptance:**
- [ ] `FatRequest` has variants: `Open { path, flags }`, `Read { fd, len }`, `Write { fd, data }`, `Seek { fd, offset, whence }`, `Close { fd }`, `Getdents { fd }`, `Stat { path }`, `Unlink { path }`, `Rename { from, to }`.
- [ ] `FatReply` has variants mirroring each request plus a `Error(i32)` variant.
- [ ] Codec round-trip property test covers all variants.

### A.2 — Implement `dispatch.rs` skeleton with real handler stubs

**File:** `userspace/fat_server/src/dispatch.rs`
**Symbol:** `dispatch`
**Why it matters:** The previous `main.rs:67` catch-all ENOSYS must be replaced with a dispatch table that can be incrementally filled.

**Acceptance:**
- [ ] Each `FatRequest` variant routes to a named handler function.
- [ ] Handler functions return `FatReply::Error(-ENOSYS)` initially (to be replaced in Track B/C).
- [ ] A `grep -n ENOSYS userspace/fat_server/src/dispatch.rs` after Track C completes returns zero lines (all stubs replaced).

---

## Track B — FAT32 Lift to `kernel-core`

### B.1 — Move FAT32 BPB parser and FAT chain walker to `kernel-core`

**Files:**
- `kernel/src/fs/fat32/bpb.rs` → `kernel-core/src/fat32/bpb.rs`
- `kernel/src/fs/fat32/chain.rs` → `kernel-core/src/fat32/chain.rs`

**Symbol:** `Bpb`, `FatChain`
**Why it matters:** Host-testable FAT32 logic means regression tests can run without QEMU.

**Acceptance:**
- [ ] Files compile under `x86_64-unknown-linux-gnu` target with `--no-default-features`.
- [ ] At least four unit tests cover BPB field extraction and FAT chain traversal.
- [ ] Kernel-side callers updated to import from `kernel-core::fat32`.

### B.2 — Define `BlockDevice` trait and `RemoteBlockDevice` impl for `fat_server`

**File:** `kernel-core/src/fat32/block.rs`
**Symbol:** `BlockDevice`, `RemoteBlockDevice`
**Why it matters:** `Fat32Volume` must read and write sectors through an abstraction that works both on the host (test) and in userspace (production).

**Acceptance:**
- [ ] `BlockDevice` trait has `read_sector(lba: u64, buf: &mut [u8]) -> Result<(), BlockError>` and `write_sector(lba: u64, buf: &[u8]) -> Result<(), BlockError>`.
- [ ] A `MemBlockDevice` in-memory impl for unit tests is provided in `kernel-core`.
- [ ] `RemoteBlockDevice` wraps the Phase 55b `nvme_server` client IPC path.

### B.3 — Implement `Fat32Volume` file operations

**File:** `kernel-core/src/fat32/volume.rs`
**Symbol:** `Fat32Volume`
**Why it matters:** This is the core FAT32 engine; all `fat_server` handler calls delegate here.

**Acceptance:**
- [ ] `Fat32Volume::open`, `read`, `write`, `seek`, `close`, `getdents`, `stat`, `unlink`, `rename` are implemented.
- [ ] At least eight unit tests using `MemBlockDevice` cover: open existing file, create new file, write-then-read round-trip, getdents with 3 entries, stat size field, unlink file, rename file, seek past end returns EOF.

---

## Track C — Server-Side File-Descriptor Table

### C.1 — Implement `FdTable` and `FatFile`

**File:** `userspace/fat_server/src/fd_table.rs`
**Symbol:** `FdTable`, `FatFile`
**Why it matters:** Without a server-side FD table, concurrent clients would alias each other's file positions.

**Acceptance:**
- [ ] `FdTable` maps `(client_cap_id, fd_token: u32)` to `FatFile`.
- [ ] `FdTable::open` allocates a monotonically increasing `fd_token` per client.
- [ ] `FdTable::close_client` drops all FDs belonging to a disconnected client.
- [ ] At least three unit tests: open two files for same client, open same path for two clients (independent positions), close client clears its FDs.

### C.2 — Wire dispatch handlers to `FdTable` + `Fat32Volume`

**File:** `userspace/fat_server/src/dispatch.rs`
**Symbol:** `handle_open`, `handle_read`, `handle_write`, `handle_close`, `handle_getdents`, `handle_stat`, `handle_unlink`, `handle_rename`
**Why it matters:** Connects the protocol layer to the FAT32 engine.

**Acceptance:**
- [ ] Every handler that previously returned `-ENOSYS` now returns a real result.
- [ ] `handle_write` flushes dirty sectors to `RemoteBlockDevice` before returning `Ok`.
- [ ] A `grep -n ENOSYS userspace/fat_server/src/dispatch.rs` returns zero lines.

---

## Track D — `vfs_server` Mount-Point Routing

### D.1 — Add `MountTable` to `vfs_server`

**File:** `userspace/vfs_server/src/mount.rs`
**Symbol:** `MountTable`
**Why it matters:** Without routing, VFS calls for `/fat` paths never reach `fat_server`.

**Acceptance:**
- [ ] `MountTable` maps string path prefixes to capability endpoints.
- [ ] `MountTable::route(path)` returns the most specific prefix match.
- [ ] At least two unit tests: exact match, prefix match with longer path.

### D.2 — Wire forward path in `vfs_server` dispatch

**File:** `userspace/vfs_server/src/dispatch.rs`
**Symbol:** `vfs_dispatch`
**Why it matters:** The forward path is the plumbing that Phase 54 described but did not implement.

**Acceptance:**
- [ ] On `Open` for a path under `/fat`, the request is forwarded to the registered `fat_server` endpoint and the reply returned verbatim.
- [ ] An integration test verifies a VFS `Open("/fat/test.txt")` reaches `fat_server` by observing `fat_server` log output.

---

## Track E — Regression Suite

### E.1 — Write/read consistency test

**File:** `userspace/fat_server/tests/rw.rs`
**Symbol:** `test_fat_write_read`
**Why it matters:** Proves that written data is returned by a subsequent read on the same file.

**Acceptance:**
- [ ] Test runs via `cargo xtask test --test fat_server_rw` in QEMU.
- [ ] Write 4096 bytes, seek to 0, read 4096 bytes, assert byte-exact match.

### E.2 — Restart-persistence test

**File:** `userspace/fat_server/tests/persist.rs`
**Symbol:** `test_fat_persist`
**Why it matters:** Verifies that FAT32 data survives a `fat_server` restart (i.e., is not held only in RAM).

**Acceptance:**
- [ ] Test writes a file through `fat_server`, sends a controlled restart signal, waits for re-registration, reads the file through the new instance.
- [ ] Byte content matches after restart.

---

## Track F — Phase 54 Documentation Closure

### F.1 — Update Phase 54 design doc with closure note

**File:** `docs/roadmap/54-deep-serverization.md`
**Symbol:** (document section)
**Why it matters:** Phase 54 claimed storage serverization that was not delivered; the design doc must be corrected.

**Acceptance:**
- [ ] A `> **Phase 65 closure note:**` block is appended to the FAT-server section noting that real FAT32 operations were delivered by Phase 65.
- [ ] Audit Red Flag #14 (`fat_server` ENOSYS stub) is noted as closed.

### F.2 — Update Phase 54 task doc closure

**File:** `docs/roadmap/tasks/54-deep-serverization-tasks.md`
**Symbol:** (FAT server track)
**Why it matters:** Task acceptance items for `fat_server` were checked against stub behavior.

**Acceptance:**
- [ ] FAT server track acceptance items reference "(real implementation in Phase 65)".

---

## Documentation Notes

- Moving FAT32 code from `kernel/src/fs/fat32/` to `kernel-core/src/fat32/` is a mechanical lift; do not refactor the algorithm during the move — that is a separate cleanup task.
- The `BlockDevice` trait in `kernel-core` must be `no_std` compatible so the kernel can continue to use the same code.
- `vfs_server`'s `MountTable` is keyed on path strings at this phase; a more structured inode-based routing mechanism is deferred.
