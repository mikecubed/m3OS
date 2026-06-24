---
status: PLAN (not started) — scoping + phased roadmap for unifying the root ext2
  onto the ring-3 `vfs_server`, so metadata can be safely written back and the
  dual-engine cache-invalidation machinery retired. Grounded in a full inventory
  of in-kernel `EXT2_VOLUME` root usage (2026-06-24). Boot-critical; execute as a
  focused phase with per-step boot validation, NOT as a marathon-tail change.
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

## Companion

Builds directly on the FS-perf work in
`docs/handoffs/2026-06-23-rustc-runtime-null-deref-after-tls.md` (directory
index, bitmap free-search cursor, granular cache invalidation — all landed on
`feat/phase-95b-on-device-rustc`).
