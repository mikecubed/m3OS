---
status: PARTIAL — Phases A+B+C1 DONE (single post-boot root engine for all reads
  + exec + file-open, each boot-validated and committed on
  `feat/phase-95b-on-device-rustc`). Phase C2/C3 (metadata write-back + retiring
  the invalidation) are **architecturally BLOCKED**: the C1 counter proved the
  in-kernel engine is still the *trusted DAC authority* (uid/gid/mode for every
  path-component permission check, deliberately NOT trusting `vfs_server`), so
  deferring inode/dir metadata would make those security reads stale. The
  write-back perf win is gated on a separate DAC-architecture decision. See
  Progress + the C2/C3 block note below. Boot-critical; executed as per-step
  boot-validated increments, NOT a marathon-tail change.
---

# ext2 engine unification — make `vfs_server` the sole post-boot root engine

## Why

m3OS runs **two** ext2 engines on the root device:

- **`vfs_server`** (ring 3) — read-only opens + (when registered) all mutations
  via the `VFS_*` IPC protocol, with its own block cache (measured 99.9% hit).
- **in-kernel `crate::fs::ext2::EXT2_VOLUME`** — create/write/truncate opens'
  *fallbacks*, path resolve/stat fallbacks, **exec (binary load)**, and
  **getdents**, reading/writing the block device directly.

They stay coherent **through the disk**: every mutation is written through and the
other engine re-reads it (the `2026-06-23-rustc-runtime-null-deref-after-tls.md`
granular-invalidation fix records each `sys_block_write` and invalidates exactly
those blocks in the in-kernel cache). This is why **metadata write-back is unsafe
today**: if `vfs_server` deferred a dir-entry/inode block, the in-kernel engine's
exec/getdents/resolve would read the stale on-disk copy → a freshly-created file
or its contents would be **invisible to the other engine** (broken `open`/exec,
not merely slow). The ~7 metadata write-throughs per create are therefore the
**coherence mechanism**, not waste.

The fix is to make `vfs_server` the **single** root engine for everything that
runs after boot. Then there is one cache, write-back is safe, and the entire
`invalidate_cache` / `record_dirty_root_write` machinery can be deleted.

## The boot-window constraint (do NOT remove the in-kernel engine)

The in-kernel ext2 engine is **load-bearing for boot**: `init` (PID 1) and
`vfs_server` itself are `exec`'d from the root *before* `vfs_server` is
registered. So the in-kernel engine must survive for the early-boot window (load
the FS server from the FS). "Unification" means: **in-kernel for the boot window;
`vfs_server` for everything after it registers.** Write-back is safe only
*post-boot* (single live engine); the few in-kernel boot-window writes are fine
(no second engine exists yet).

## Current state (inventory, 2026-06-24)

Routing seam: `vfs_service_should_route` (syscall/mod.rs:8872) routes only
**read-only, non-creating, non-truncating** opens to `vfs_server`. Mutations use
`vfs_write_routable()` → route-to-`vfs_server`-primary, in-kernel-fallback.

| Op | Today | Gap to unify |
|---|---|---|
| create/write/truncate/unlink/rmdir/mkdir/rename/symlink/chmod/chown | vfs_server primary; in-kernel fallback (boot window) | none — already routed |
| read / pread / read-only open | vfs_server primary; in-kernel fallback | none |
| stat / `path_filemeta` | vfs_server primary (`vfs_service_stat_path`); in-kernel fallback (15934) | none |
| **getdents64** | **in-kernel only** (no `vfs_service_list_dir` for root) | **route to `vfs_service_list_dir` (exists, 8783)** |
| **readlink** | **in-kernel** (`path_node_nofollow`) | route to vfs_server |
| **exec — static** (`read_file_from_disk`, 8266) | **in-kernel** bulk read | route post-boot; keep in-kernel for boot window |
| **exec — streaming** (`open_exec_stream`/`ExecWindow`, 8447) | **in-kernel** demand read | route via `VFS_READ_WINDOW`/`PREAD`; keep boot-window in-kernel |
| `st_ino` for getdents `d_ino` (`path_ino`, 17502) | in-kernel resolve | must agree with vfs_server `st_ino` |

Riskiest coupling (from the inventory): the **exec loader holds `EXT2_VOLUME`
across a large binary read** (boot-critical; also the latent stall when an exec
read races a vfs write); **getdents is unrouted**; the in-kernel mutation
fallbacks skip `invalidate_cache` (benign — only the single-engine boot window).

## Phased plan (each step independently boot-validated)

**Phase A — route the remaining root READERS to `vfs_server` (additive, boot-safe).**
- A1. `getdents64` on the root → `vfs_service_list_dir` (already implemented,
  8783), in-kernel fallback retained. Validate: `smoke-test` (`ls`, shell),
  and `d_ino` == `stat().st_ino` (POSIX invariant; reuse the Phase-88 st_ino
  rigor exercised by `coreutils-smoke`).
- A2. `readlink` on the root → a vfs_server stat/readlink path, in-kernel
  fallback. Validate: a symlink read in `smoke-test`.
- A3. Audit `path_node_nofollow`/`path_filemeta`/`path_ino` so that *whenever
  `vfs_server` is registered* the root read path never touches `EXT2_VOLUME`
  (in-kernel only as the boot-window fallback). Validate: boot + the cache
  probe should show ~0 in-kernel root reads post-boot.

**Phase B — route EXEC (the crux; boot-critical).**
- B0. Add a clean predicate `exec_should_route()` = "`vfs_server` registered AND
  path on the ext2 root" so the **boot window keeps the in-kernel loader** (it
  loads `vfs_server`/`init`).
- B1. Static load (`read_file_from_disk`): when routed, read the binary through
  `vfs_server` (open read-only handle + `VFS_PREAD` loop, or reuse the demand
  read-window) into the exec buffer; do NOT hold any ext2 lock across it.
- B2. Streaming load (`open_exec_stream`/`ExecWindow`): back the demand-fault
  fills with `vfs_server`'s `VFS_READ_WINDOW` (the dynamic-loader path already
  uses it) rather than `EXT2_VOLUME`.
- Validate after B: every smoke gate that `exec`s from the root
  (`smoke-test`, `tui-app-smoke`, the port gates) — a regression here breaks
  *all* program launch, so this phase gets the heaviest validation and is done
  in isolation.

**Phase C — retire the dual-engine cost + enable write-back.**
- C1. With Phases A+B done, the in-kernel engine touches the root **only** in the
  boot window. Assert this (a counter / the cache probe).
- C2. Enable `vfs_server` metadata write-back: route inode-table / dir-block /
  bitmap writes through the existing `write_block_deferred` path; flush on an
  **ordered** drain (inodes+bitmaps+data before dir-entry blocks, so a crash
  leaves at most fsck-reconcilable orphans, never a dangling dirent), bounded by
  a threshold + the existing periodic flush + a real `fsync`. Validate:
  `ahci-persist-smoke` (reboot persistence) + `ext2-coherence-smoke` (fresh-
  process read-back) + `vfs-bulkio-smoke` (read-back-compare) + the
  `vfs-throughput-probe` (writes/create should drop).
- C3. Delete `record_dirty_root_write` + the granular `invalidate_cache`
  (no second engine to keep coherent post-boot).

## Expected payoff

- Writes/create fall from ~7 (write-through) toward ~2–3 (write-back +
  cross-request coalescing of the repeatedly-rewritten inode-table/dir/bitmap
  blocks), shrinking the ~1.6 ms/create floor further.
- The per-mutation `invalidate_cache` round-trip + the dual-engine reasoning
  disappear.

## Risks / why this is its own phase

- **Boot path**: a bug in Phase B means *nothing execs* → no boot. Must keep the
  boot-window in-kernel loader and validate exec heavily, in isolation.
- **Crash consistency** (Phase C2): write-back on a journal-less ext2 needs
  ordered flushing; validate with the reboot-persistence gate. A process restart
  cannot prove power-loss durability (the gate's own caveat), so the ordered
  drain must be reasoned, not just tested.
- **`d_ino`/`st_ino` POSIX invariant** across the engine switch (Phase A1).

## Progress

- **Phase A — DONE** (commit `feat(kernel/ext2-unify): Phase A — route root
  READERS to vfs_server`). Discovery found readlink + stat
  (`path_filemeta`/`path_node_nofollow`) were *already* routed in earlier
  phases; this landed the remaining root readers:
  - `path_ino` (getdents64 `d_ino` source) → cached `vfs_service_stat_path`
    (same source as `stat`, so `d_ino == st_ino`); in-kernel fallback.
  - getdents64 root `/` + ramdisk-overlaid subdirs → new
    `vfs_service_list_dir_entries()` helper (drains paginated `VFS_LIST_DIR`,
    parses `dirent64` records → real `d_ino` + entry-inode `d_type`). The
    pure-ext2 subdir fast-path was already routed; this covered the merge dirs.
  - statfs existence probes (`statfs_for_path`/`statfs_path_exists`) →
    `vfs_service_stat_path`.
  - New `vfs_can_list_ext2_dir()` predicate captures listing's MERGE semantics
    (ramdisk overlays ext2) so `/` and overlaid dirs route their ext2 contents.
  - Validated: `cargo xtask check` + `smoke-test` PASSED.
- **Phase B — DONE** (commit `feat(kernel/ext2-unify): Phase B — route EXEC
  through vfs_server`). Post-boot, ext2-root binaries load through `vfs_server`;
  the boot window keeps the in-kernel loader so `init`/`vfs_server` come up.
  - `exec_should_route(path)` = `vfs_write_routable()` (vfs registered AND not
    `vfs_server` itself) AND ext2-root path AND not ramdisk/`/data`.
  - B1 static (`read_file_from_disk`): routed read via cached
    `vfs_service_stat_path` (size → E2BIG/streaming) + `vfs_exec_open` +
    `vfs_exec_read_bytes` (raw VFS handle, kernel `VFS_READ` client, no fd,
    handle always closed). In-kernel fallback.
  - B2 streaming (`open_exec_stream`/`DiskElfSource`): an `ExecStreamSource`
    enum — `Kernel(inode)` boot-window vs `Vfs(handle)` routed; `refill()` reads
    the window via the kernel `VFS_READ` client; `Drop` closes the handle.
    `execve` diverges into userspace, so `exec_stream` is `drop()`-ed explicitly
    after the ELF load (before the CR3 switch) so `VFS_CLOSE` lands.
  - The PT_INTERP loader already routes via `read_file_from_disk`.
  - Validated: `smoke-test` PASSED — the tcc-version/tcc-compile steps exec the
    ext2-resident `/usr/bin/tcc`, which now loads through `vfs_server` (B1 proven
    end-to-end). B2 streaming rides the clang/rustc gates; the boot-window
    fallback guarantees no boot regression.
- **Phase A audit completion** (folded into the Phase B follow-up commit):
  routed the LAST hot-path in-kernel root reader, `vfs_service_should_route`'s
  `is_ext2_regular_file` (`EXT2_VOLUME.metadata`) → cached `vfs_service_stat_path`
  kind check; removed the now-dead `is_ext2_regular_file`. **Full audit of the
  remaining `EXT2_VOLUME` read sites in `syscall/mod.rs`**: every root read/write
  handler routes to `vfs_server` first with the in-kernel engine as a
  boot-window / degraded (vfs-IPC-failed) fallback; the only unconditional
  post-boot in-kernel root reads left are `/mnt/usbN` secondary mounts (a
  separate engine, by design) and `ext2_statfs`'s superblock free-counts
  (benign/approximate). The structural unification (single post-boot root engine
  for all reads + exec) is therefore complete.
- **Phase C1 — DONE** (commit `Phase C1 — in-kernel root-read counter`) +
  **file-OPEN routing** (commit `route the file-OPEN path through vfs_server`).
  C1 added `IN_KERNEL_ROOT_READS` (a count of LOGICAL root-volume reads the
  in-kernel engine serves, cache hits included), exposed via `/proc/blkstats` and
  asserted by `vfs-throughput-smoke`. It is BOTH the empirical proof of the A+B
  audit AND a permanent regression guard. The OPEN routing then moved
  `open_ext2_file`'s fd-construction off the in-kernel engine onto vfs_server.
  Measured residual dropped 68 → 51 root reads per probe.
- **Phase C2 / C3 — ARCHITECTURALLY BLOCKED (not merely deferred).** C1 +
  tracing the residual surfaced the blocker the plan's premise missed: **the
  in-kernel engine is the kernel's TRUSTED DAC authority, not a removable legacy
  second engine.** `require_search_permission` → `path_metadata` →
  `data_file_metadata` → in-kernel `EXT2_VOLUME.metadata` reads every path
  component's uid/gid/mode **on purpose** (syscall/mod.rs ~11642): "DAC decisions
  must stay on kernel-verified metadata — a compromised ring-3 `vfs_server` could
  spoof uid/gid/mode via `VFS_STAT_PATH` and defeat the access checks." Those
  reads (the measured ~51 residual) **cannot** be routed to vfs without making
  the security boundary trust the very thing it distrusts. Consequences:
  - **C2 (write-back) is unsafe.** `vol.metadata` does `resolve_path` (dir
    blocks) + `read_inode` (inode-table block) — *exactly* the blocks C2 would
    defer. Deferring them makes the trusted DAC read observe **stale on-disk
    uid/gid/mode** (the disk lacks the deferred change) → wrong, security-relevant
    access decisions on a freshly-`chmod`'d/created file. The only blocks the DAC
    path never reads (indirect/pointer, data) are *already* deferred / coalesced,
    so C2 offers **no additional safe deferral**.
  - **C3 (retire the block-cache `invalidate_cache`) is unsafe.** That
    invalidation is what keeps the DAC reads coherent after a routed mutation
    (e.g. `data_chmod` routes the chmod to vfs, then the next in-kernel DAC read
    must see the new mode — `invalidate_cache` drops the stale cached block so it
    re-reads fresh). It is the DAC coherence mechanism, not dual-engine legacy.
    (`metacache::bump()` stays regardless.)
  - **To unblock C2/C3 you must first resolve the DAC architecture** — e.g. move
    DAC enforcement into `vfs_server` (and trust it, a security-model change), or
    give the in-kernel DAC path a coherent view of vfs's deferred state (which
    couples the engines again). Both are a **separate security-design phase**,
    out of scope here. The write-back perf win (~7→~2-3 writes/create) is real
    but gated on that decision; it must not be taken by silently weakening DAC.

## Companion

Builds directly on the FS-perf work in
`docs/handoffs/2026-06-23-rustc-runtime-null-deref-after-tls.md` (directory
index, bitmap free-search cursor, granular cache invalidation — all landed on
`feat/phase-95b-on-device-rustc`).
