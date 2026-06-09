# Adding ext4 or btrfs Support to m3OS: Feasibility & Library Evaluation

**Aligned Roadmap Phase:** Unassigned (research input for a future storage phase)
**Status:** Research / Evaluation — no code changes
**Source Ref:** ext4-btrfs-feasibility
**Date:** 2026-06-08
**Related:** [`codebase-map.md`](./codebase-map.md), [`legacy-os-comparison.md`](./legacy-os-comparison.md), Phase 54 (ring-3 VFS migration), Phase 88 (ext2 ring-3 write authority)

## Executive Summary

m3OS already ships a **read-write, non-journaled ext2** filesystem. Crucially,
the *authoritative* ext2 engine is **ring-3** — it lives in
`userspace/vfs_server` (2,291 lines), not in the kernel. The in-kernel
`kernel/src/fs/ext2.rs` (1,607 lines) is a deprecated shim that the Phase 54/93
migration ported into `vfs_server`. The on-disk *codec* (struct parse/serialize)
is factored into `kernel-core/src/fs/ext2.rs` (892 lines) which is **host-tested**
with `cargo test`. Any new on-disk filesystem must slot into this same shape.

**Goal (per the brief):** a **read-write ext4 engine that replaces the legacy
ext2** as m3OS's going-forward on-disk filesystem — not an ext4 reader bolted on
beside ext2. Read-only is an internal checkpoint, not a shippable end state.

**Recommendation, in one line:**

- **Build it in-house, RW, as a strict ext2 *superset*.** ext4 is ext2 plus
  feature bits; an ext4 engine reads existing ext2 disks unchanged, so it
  *subsumes* the current driver and the migration is not a flag day. Reuse the
  host-tested codec pattern (`kernel-core/src/fs/ext4.rs`) and the existing ring-3
  write machinery in `vfs_server` (allocator, directory writes, metadata flush).
  The headline read-side addition is **extent trees**; the write side reuses most
  of the existing ext2 allocator. **Journaling (jbd2)** is the feature that makes
  ext4 genuinely *better* than today's non-journaled ext2 (crash consistency) and
  is the last, hardest milestone.
- **Don't make `ext4_rs` the permanent foundation.** It's the best *accelerator*
  to prototype a read/write path quickly (MIT, `no_std`+alloc, `BlockDevice`
  trait), but as the going-forward base it locks m3OS into a mid-`refactor`,
  no-journaling crate with an infallible/`Vec`-per-read device trait and a pinned
  older nightly. Acceptable to *study/borrow* from; not the thing to depend on
  long-term for a filesystem that's meant to *be* the root.
- **btrfs — not recommended.** The two crates you named (`btrfs`, `libbtrfs`)
  are **Linux-`ioctl`/FFI wrappers that require a running Linux kernel** — unusable
  on bare metal. The only bare-metal-viable Rust code (`btrfs-diskformat` /
  `btrfs-no-std`) is **read-only struct parsing with no tree-walk, no checksum
  verification, and no writer**. A correct btrfs driver is a multi-person-year,
  correctness-critical effort with essentially no pure-Rust prior art.

---

## 1. How storage actually works in m3OS today

Read this before estimating effort — the architecture is **microkernel /
userspace-first**, and the integration surface for a new filesystem is *not* "add
a module to the kernel."

```text
  app (ring 3)
    │  open("/etc/passwd"), read(fd), write(fd), getdents64 …
    ▼
  kernel syscall handler  (kernel/src/arch/x86_64/syscall/)
    │  fd backend == FdBackend::VfsService  →  IPC call to "vfs"
    ▼
  vfs_server  (ring 3)  ── userspace/vfs_server/src/main.rs  (2291 lines)
    │  the ACTUAL ext2 driver: path resolution, inodes, dirs, bitmaps, writeback
    │  on-disk codec ← kernel_core::fs::ext2  (parse / write_into)
    │  wire protocol ← kernel_core::fs::vfs_protocol  (VFS_OPEN … VFS_LINK)
    ▼  syscall_lib::block_read(lba, count, &mut buf) / block_write(...)   [512B sectors]
  kernel block layer  (kernel/src/blk/)  ── dispatch:
    │  RemoteBlockDevice (ring-3 nvme.block / ahci.block driver)  — preferred
    │  in-kernel virtio-blk                                       — fallback
    ▼
  disk
```

### The four layers a new filesystem touches

| Layer | File | Role | Lines |
|---|---|---|---|
| **On-disk codec** | `kernel-core/src/fs/ext2.rs` | `no_std`+`std` struct parse/serialize, **host-tested** (`#[cfg(test)] mod tests`, e.g. `parse_superblock_valid` at line 537) | 892 |
| **Ring-3 driver** | `userspace/vfs_server/src/main.rs` | The real engine: `Ext2State`, path resolution, read/write/create/unlink/rename/link, bitmap alloc, metadata flush | 2291 |
| **IPC wire protocol** | `kernel-core/src/fs/vfs_protocol.rs` | `VFS_OPEN`/`READ`/`WRITE`/`STAT_PATH`/`LIST_DIR`/`CREATE`/`UNLINK`/`RENAME`/`LINK`/`TRUNCATE`/`PREAD`/`ACCESS_PATH` + mount actions | 252 |
| **Partition probe** | `kernel-core/src/fs/mbr.rs` | `parse_mbr`, `find_ext2_partition` (MBR type `0x83`), `find_fat32_partition` | 192 |

Supporting facts (verified in-tree):

- **Block backend is a sector syscall, already abstracted.**
  `vfs_server` reads/writes through `syscall_lib::block_read(start_lba, count, buf)`
  / `block_write(...)` returning `i64` (`<0` = error), **512-byte sectors**
  (`userspace/vfs_server/src/main.rs:92-123`). A new fs reuses this verbatim —
  there is *no* per-filesystem block-driver work.
- **One ring-3 server per filesystem.** ext2 → service `"vfs"`; FAT →
  `userspace/fat_server` (an 87-line stub; FAT32 file I/O still lives in the
  kernel). A new fs is naturally a **new ring-3 server** (e.g. `ext4_server`) or an
  extension of `vfs_server`, registered under a service name via
  `ipc_register_service` (`vfs_server/src/main.rs:1457`).
- **Mount = probe + select.** At boot `vfs_server` reads MBR sector 0, calls
  `mbr::find_ext2_partition` (type `0x83`), parses the superblock at byte 1024
  (`LBA+2`), reads the block-group-descriptor table, and registers `"vfs"`
  (`vfs_server/src/main.rs:1374-1469`). Kernel-side mount routing lives in
  `sys_linux_mount` (`kernel/src/arch/x86_64/syscall/mod.rs`) with actions
  `VFS_MOUNT_EXT2_ROOT` / `VFS_MOUNT_VFAT_DATA`.
- **Image creation shells out to `e2fsprogs`.** `xtask` builds the data disk by
  writing an MBR (partition type `0x83`), then `mkfs.ext2 -b 4096 …`, populating
  with `debugfs -w`, and validating with `e2fsck` (`xtask/src/main.rs`,
  `create_data_disk` / `populate_ext2_files`). It does **not** build the image in
  pure Rust. This matters: `mke2fs`/`debugfs`/`e2fsck` from the same `e2fsprogs`
  **already support ext4** (`mkfs.ext4`), so an ext4 build-time image is a flag
  change, not new tooling. btrfs would require `btrfs-progs` on the build host.
- **The current ext2 is non-journaled.** This is the single most important framing
  fact for ext4: m3OS already runs a **crash-unsafe (no journal) read-write**
  root. Adding *non-journaled* ext4 read-write is therefore **not a regression** in
  crash-consistency relative to today.

### What "add ext4" concretely requires

1. **Codec** (`kernel-core/src/fs/ext4.rs`, host-tested): extent-tree parsing,
   64-bit block fields, `flex_bg`, 256-byte inodes + the extra fields, the
   `s_feature_*` masks, and `dir_index`/htree (read path can treat htree dirs as
   linear). Mirror the existing ext2 test style (build byte arrays, assert parse).
2. **Driver** (extend `vfs_server` or a new `ext4_server`): resolve file blocks
   through the extent tree instead of the 12 direct + indirect pointers
   (`vfs_server/src/main.rs:189` `resolve_block` is the seam), honor 64-bit sizes,
   and — for write — block/inode allocation against the larger group layout.
3. **Probe/select** (`kernel-core/src/fs/mbr.rs` + boot mount): ext4 also uses MBR
   type `0x83`, so disambiguate by reading the superblock feature flags
   (`s_feature_incompat & EXT4_FEATURE_INCOMPAT_*`) and route to the ext4 engine.
4. **xtask image** (`create_data_disk`): `mkfs.ext4` (decide journal on/off — see
   §5 caveat), keep the `debugfs`/`e2fsck` populate+validate steps.
5. **Tests + regression gate**: a `cargo xtask check` host test for the codec, an
   in-QEMU `ext4-smoke` mirroring the existing `ext2-coherence-smoke`
   (`userspace/ext2-coherence-smoke`), and an opt-in `M3OS_EXT4_REGRESSION` gate
   following the table in `AGENTS.md`.

This is the same five-step pattern the project already used to land ext2 in
ring-3 — the architecture is friendly to it.

---

## 2. Rust library evaluation — ext4

### 2.1 `ext4_rs` — github.com/yuoo655/ext4_rs *(the headline candidate)*

| Dimension | Finding |
|---|---|
| **`no_std`** | ✅ `#![no_std]` + `extern crate alloc` ("An OS-independent rust ext4 file system"). Heap required. |
| **Read / write** | ✅ **Both.** README checklist: mount, mkdir, read_file, create_file, write_file, link, unlink, truncate, remove, umount. |
| **Extents** | ✅ Extent tree + extent-block checksums (README example writes a 511 MB file to exercise the extent tree). |
| **Journaling** | ❌ **None.** No `jbd2`/journal module; only passive superblock fields + `JOURNAL_INODE = 8`. Writes are **non-journaled** → crash-unsafe. |
| **License / deps** | ✅ **MIT**; only `bitflags` + `log`. No `std`, no C, no FUSE in the core lib. |
| **Toolchain** | ⚠️ **Nightly** (`#![feature(error_in_core)]`; README pins `nightly-2024-06-01`), **edition 2021**. m3OS is nightly + edition 2024 — workable but a version-pin/edition friction point to validate. |
| **Maturity** | ⚠️ v1.3.3 (Jan 2026), actively pushed (May 2026), 0 open issues, ~68★ — **but** default branch is `refactor` and several releases (1.3.0/1.2.0/1.1.0/0.1.x) are **yanked**. API is unstable. |
| **Device trait** | Consumer implements: `trait BlockDevice { fn read_offset(&self, offset: usize) -> Vec<u8>; fn write_offset(&self, offset: usize, data: &[u8]); }` then `Ext4::open(disk)`. |

**Integration fit / impedance with m3OS:**

- ✅ The `BlockDevice` trait maps onto `syscall_lib::block_read/write` with a thin
  adapter (offset → `lba = base + offset/512`, length-rounding to sectors).
- ⚠️ `read_offset` **returns an owned `Vec<u8>` and is infallible** (no `Result`).
  m3OS block I/O *is* fallible (`i64 < 0`). An adapter must either `panic`/sentinel
  on I/O error or buffer — a real wart for a supervised ring-3 driver that should
  degrade gracefully. A small fork to make the trait return `Result<Vec<u8>, …>`
  is advisable.
- ⚠️ `Vec`-per-read allocates on every block touch — fine functionally, but the
  existing `vfs_server` is deliberately uncached; pairing per-read allocation with
  the ~200 KB/s ring-3 VFS path warrants a read cache.
- **Verdict:** the *only* one of the named crates that is simultaneously `no_std`,
  read+write, pure-Rust, MIT, and device-trait-based. Best used **vendored** (git
  submodule / `path` dep under `ports/` or a workspace fork) so you can pin the
  toolchain, fix the infallible trait, and add journal-awareness — not as a moving
  crates.io dependency.

### 2.2 `ext4` (FauxFaux) — crates.io/crates/ext4

- ❌ **`std`-only** (`std::io::{Read,Seek}`, `HashMap`), ❌ **read-only** ("not a
  filesystem driver … doesn't support modifying anything"), 💤 **dormant** (last
  release 0.9.0, Jan 2021). Author explicitly disclaims resource-constrained
  targets. **Not viable** without a from-scratch `no_std` port + a whole write
  engine — at which point you've written your own.

### 2.3 Honorable mentions (not requested, but decision-relevant)

- **`ext4-view`** (nicholasbishop): pure-Rust, **`no_std`+alloc**, actively
  maintained (v0.9.2), **read-only by design**, also reads ext2. The cleanest
  option if you want a *mature, pure-Rust read-only* ext4 (and ext2) reader — e.g.
  to mount real Linux disks read-only, or as an independent verifier in tests. No
  write path.
- **`lwext4_rust`** (elliott10): `no_std` wrapper over the **C `lwext4`** library.
  Read+write **with journaling/transactions**, used by **ArceOS**. But it's
  **GPL-2.0** and pulls in **external C** — a hard mismatch for m3OS's MIT,
  pure-Rust posture and its musl-cross build story. Flagged only because it is the
  de-facto choice in Rust OSes when journaling matters.

### ext4 crate scorecard

| Crate | `no_std` | RW | Journal | License | Maturity | Bare-metal usable | Verdict |
|---|---|---|---|---|---|---|---|
| **ext4_rs** | ✅ | ✅ | ❌ | MIT | mid-refactor | ✅ | **Best accelerator; vendor + harden** |
| ext4 (FauxFaux) | ❌ | ❌ (ro) | n/a | MIT | dormant | ❌ | Reject |
| ext4-view | ✅ | ❌ (ro) | n/a | MIT/Apache | active | ✅ | Good read-only / verifier |
| lwext4_rust | ✅ | ✅ | ✅ | **GPL-2.0** | active | ✅ (C) | Reject (license + C) |

---

## 3. Rust library evaluation — btrfs

### 3.1 `btrfs` (docs.rs/btrfs) and `libbtrfs` — both Linux-only

- **`btrfs`** (wellbehavedsoftware/rust-btrfs): crates.io description "Interface for
  BTRFS ioctls etc." It wraps `BTRFS_IOC_*` ioctls via `nix`/`libc` — **`std`-only,
  Linux-only, abandoned since 2017** (Rust 2015). It does contain a `diskformat`
  module of on-disk structs, but the crate as a whole requires a **running Linux
  kernel with a mounted btrfs**. **Unusable on bare metal.**
- **`libbtrfs`** (crates.io/crates/libbtrfs): **bindgen FFI over btrfs ioctls**,
  `x86_64-unknown-linux-gnu` only, depends on `libc`. Operations are ioctls on fds
  of mounted filesystems. Maintained (v0.0.20, 2025) but fundamentally a **host
  Linux** library. **Unusable on bare metal.** (Same story for the `btrfsutil` /
  `btrfsutil-sys` / `libbtrfsutil` family — they link the system `libbtrfsutil`
  from `btrfs-progs`.)

**Neither crate you named can read or write a btrfs volume from a `no_std` kernel
that only has a block device.** Both require Linux underneath.

### 3.2 Pure-Rust from-scratch btrfs — essentially greenfield

- **`btrfs-diskformat`** (GodTamIt): "Clean-room implementation of the btrfs disk
  format", **`no_std` by default**, **BSD-2-Clause**. But it is **struct layouts
  only** (`zerocopy` parsing): `SuperBlock`, `Header`, `Item`, `Key`, `Chunk`,
  `InodeItem`, `RootItem`, … with **no directory/checksum items, no crc32c
  verification, no tree-walk driver, and no writer.** ~v0.5.1, ~25★.
- **`btrfs-no-std`** (kennystrawnmusic): a lagging `no_std` fork of the above (~2★).
- There is **no pure-Rust btrfs *driver*** (end-to-end tree traversal + checksum +
  free-space) and **no pure-Rust btrfs writer in existence.**

### 3.3 Why btrfs is so much harder than ext4

btrfs is a **copy-on-write forest of B-trees** with checksums on everything. Even
*reading one file* requires: parse the superblock (magic `_BHRfS_M` at 64 KiB) →
bootstrap the system chunk array → walk the **chunk tree** to build the
logical→physical mapping → traverse **root tree → fs tree** B-trees → decode
variable typed leaf items → (for correctness) verify **crc32c** on every node.

A *correct writer* additionally must implement:

- **CoW path-copying** up every affected tree to the superblock, with a single
  atomic commit point (the superblock write);
- **generation/transaction-id** bookkeeping on every block pointer ("detect
  phantom or misplaced writes");
- the **extent tree** with **reference counts + back references** for every extent;
- the **chunk/device allocation** layer;
- **free-space tree/cache** consistency;
- checksum (re)computation on all metadata **and every 4 KiB data block**.

A bug in any one silently corrupts the volume. For scale: the Linux `fs/btrfs`
driver is ~120+ source files vs ~59 for `fs/ext4` (commonly cited ≈150k+ vs ≈50k
LOC). **Verdict: out of scope** for the foreseeable roadmap. If btrfs *features*
(snapshots, checksums, subvolumes) are the actual goal, they are better served by
other means than a from-scratch driver.

---

## 4. ext4 vs btrfs — difficulty ladder

| Task | Relative difficulty | Pure-Rust prior art |
|---|---|---|
| Read-only ext4 | **Low–moderate** (extents + 64-bit on top of existing ext2) | ✅ `ext4-view`, `ext4_rs` |
| Read-write ext4, **no journal** | **Moderate** (allocators + extent writes; m3OS ext2 already does the allocator half) | ✅ `ext4_rs` |
| Read-write ext4, **with jbd2 journal** | **Moderate–high** (journal is contained + well-documented) | ⚠️ only via C `lwext4` |
| Read-only btrfs | **High** (chunk tree, B-tree forest, crc32c) — largely greenfield | ⚠️ struct layouts only |
| Read-write btrfs | **Very high** (CoW, transactions, backrefs, checksums) | ❌ none |

---

## 5. Risks & caveats (read before committing)

1. **Journaling / crash consistency.** `ext4_rs` and an extend-the-engine approach
   are both **non-journaled**. That's *consistent with today's ext2*, but note a
   sharp edge: **a real `mkfs.ext4` enables `has_journal` by default.** Mounting a
   *journaled* ext4 and writing without replaying/maintaining the journal can
   corrupt it. Two safe options: (a) for the m3OS-built data disk, create it with
   `mkfs.ext4 -O ^has_journal` so write is sound; (b) for arbitrary/real ext4
   disks, mount **read-only** unless/until journal replay-on-mount is implemented.
   Make this an explicit, enforced policy in the mount/probe code.
2. **Metadata checksums (`metadata_csum`).** Modern `mkfs.ext4` enables it. A
   writer that doesn't recompute crc32c on metadata produces a filesystem `e2fsck`
   flags as corrupt. Either disable the feature at `mkfs` time or implement the
   checksums. (The existing `e2fsck -n -f` validation step in `xtask` will catch
   this immediately — a useful guardrail.)
3. **Toolchain pinning with `ext4_rs`.** Edition 2021 + a pinned older nightly vs
   m3OS's edition-2024 current nightly. Validate a build before committing; budget
   for a vendored fork.
4. **Performance.** The ring-3 VFS path is slow (~200 KB/s, per the Python/clang
   port notes). `ext4_rs`'s `Vec`-per-read with no cache will be painful; plan a
   block cache in the server (the legacy kernel ext2 had one;
   `kernel/src/fs/ext2.rs` `block_cache`).
5. **Disambiguation on MBR type `0x83`.** ext2/3/4 share the partition type. Selection
   must read the superblock feature masks, not the partition byte.

---

## 6. Recommendation & phasing (replace ext2 with RW ext4, in-house)

**Pursue ext4 as a drop-in replacement; decline btrfs.** Because ext4 is a
superset, the new engine reads the *existing* ext2 disks, so this is a clean
takeover rather than a parallel filesystem. Suggested milestones, each landing as
its own PR:

- **M1 — codec + read path (checkpoint, not a release).** Add
  `kernel-core/src/fs/ext4.rs` (host-tested, mirroring the ext2 codec) for extent
  trees, 64-bit fields, 256-byte inodes, and the `s_feature_*` masks; replace the
  `resolve_block` seam in the ring-3 engine (`vfs_server/src/main.rs:189`) with
  extent resolution and treat htree dirs as linear on read. Acceptance: boot, read
  an `mkfs.ext4`-built disk, and *also* still read the old ext2 image. (I'll
  cross-check the codec against `ext4-view` behavior, but won't depend on it.)
- **M2 — RW, non-journaled, cut the data/root disk over to ext4.** Extend the
  allocator + extent-write path (the ext2 bitmap allocator already lives in
  `vfs_server`), switch `xtask create_data_disk` to `mkfs.ext4 -O ^has_journal`
  (and `^metadata_csum` unless/until checksums land — see §5), and migrate every
  ext2 assertion in the smoke/regression suite. Acceptance: full `cargo xtask
  check` + smoke + regression green on an ext4 root; `e2fsck -n -f` clean after a
  write workload. **This is the functional replacement.**
- **M3 — jbd2 journal (the actual upgrade over ext2).** Journal replay-on-mount +
  journaled writes, then flip the image to `has_journal`. This is what makes ext4
  *better* than the current crash-unsafe ext2 and lets m3OS safely write real
  journaled Linux disks. Hardest milestone; can ship M1+M2 first and follow with
  M3.
- **M4 — retire legacy ext2.** Delete the deprecated in-kernel
  `kernel/src/fs/ext2.rs` engine once the ext4 engine owns root; keep the ext2
  *codec* only as far as the superset needs it.

**btrfs:** keep on the "researched, declined" list. Revisit only if a concrete
requirement (snapshots/subvolumes/data checksums) emerges *and* a maintained
pure-Rust btrfs driver materializes — neither is true today.

### Effort estimate — me doing the work

These are **agent-paced** and replace the human-week figures above. Authoring the
codec/engine is fast and largely host-testable; the real clock is the **serial
iteration loop** (rebuild → QEMU boot → smoke → `e2fsck`), the **write-path
correctness debugging** (extent splitting + bitmap/free-space consistency are the
classic foot-guns), and **review gating between PRs**. "Elapsed" assumes that loop
plus your PR reviews; "focused work" is my hands-on-keyboard time.

| Milestone | Focused work | Elapsed (incl. iteration + review) | Main risk |
|---|---|---|---|
| M1 — codec + read (extents, 64-bit) | ~half a day | ~1 day | Low — host-testable; QEMU just confirms |
| M2 — RW non-journaled + cut over root/data disk + migrate gates | ~2–3 days | ~3–5 days | **Medium-high** — write-path corruption; lots of cross-cutting test churn |
| M3 — jbd2 replay + journaled write | ~2–3 days | ~4–6 days | **High** — correctness, and I can only crash-test in QEMU (kill mid-write → replay → `e2fsck`), not on bare metal |
| M4 — delete legacy ext2 engine | ~1–2 hours | ~half a day | Low |

- **Functional replacement (M1+M2+M4, non-journaled):** ~3–4 days focused, **~1
  week elapsed** across ~2–3 PRs. Delivers ext4 RW as root with extents/64-bit/big
  dirs, reading old ext2 disks unchanged.
- **Full "better than ext2" (adds M3 journaling):** **~2 weeks elapsed** total.

Honest constraints on *my* side: (1) no bare-metal validation — journaling
crash-consistency is validated by QEMU crash-injection + `e2fsck`/replay, not real
power-loss; (2) the build/boot/image-recreate cycle (`cargo xtask clean` rebuilds
a 1 GB disk) dominates wall-clock far more than coding; (3) `metadata_csum` is the
likeliest scope-creep — if you want it on from day one, add ~1 day to M2.

| (superseded) order-of-magnitude vendor/greenfield comparison | Estimate | Risk |
|---|---|---|
| ext4 RW non-journaled via vendored `ext4_rs` (prototype shortcut) | shaves ~1 day off M2 authoring, adds trait-fork + nightly-pin maintenance debt | Medium |
| btrfs read-only (greenfield) | months | High |
| btrfs read-write | person-years | Very high |

---

## 7. Concrete integration checklist (if/when greenlit)

Following the project's "Adding a New …" conventions in `AGENTS.md`:

1. **Codec** — `kernel-core/src/fs/ext4.rs`, `no_std`+`std`, with `#[cfg(test)]`
   tests mirroring `ext2.rs` (build byte arrays → assert parse). Add to
   `kernel-core/src/fs/mod.rs`.
2. **Driver** — extend `userspace/vfs_server` (preferred: it already owns `"vfs"`
   and the write path) or add `userspace/ext4_server` as a new workspace member +
   `xtask` bin + ramdisk entry + service `.conf` (the 4-place rule).
3. **Probe/select** — extend `kernel-core/src/fs/mbr.rs` and the boot mount in
   `vfs_server` to read the ext4 feature masks and route accordingly; add a mount
   action constant to `kernel-core/src/fs/vfs_protocol.rs` if a distinct root
   action is wanted.
4. **xtask image** — `create_data_disk` → `mkfs.ext4` with the journal/csum policy
   from §5; keep `debugfs -w` populate + `e2fsck -n -f` validate.
5. **Tests/gate** — host codec tests in `cargo xtask check`; an in-QEMU
   `ext4-smoke` modeled on `userspace/ext2-coherence-smoke`; an opt-in
   `M3OS_EXT4_REGRESSION` pre-push gate (mirror the `AGENTS.md` gate table).
6. **Docs** — a `docs/roadmap/NN-ext4.md` design doc + task list per the templates
   in `docs/appendix/doc-templates.md`; add the roadmap README row.

---

## 8. Sources

m3OS code (verified in-tree): `userspace/vfs_server/src/main.rs`,
`kernel-core/src/fs/{ext2.rs,vfs_protocol.rs,mbr.rs}`, `kernel/src/fs/ext2.rs`,
`kernel/src/blk/`, `userspace/fat_server/src/main.rs`, `xtask/src/main.rs`
(`create_data_disk`/`populate_ext2_files`), `userspace/ext2-coherence-smoke`.

External (fetched June 2026):

- ext4_rs — https://github.com/yuoo655/ext4_rs · https://crates.io/crates/ext4_rs · https://docs.rs/ext4_rs
- ext4 (FauxFaux) — https://crates.io/crates/ext4 · https://docs.rs/ext4 · https://github.com/FauxFaux/ext4-rs
- ext4-view — https://github.com/nicholasbishop/ext4-view-rs · https://crates.io/crates/ext4-view
- lwext4_rust — https://github.com/elliott10/lwext4_rust
- btrfs — https://crates.io/crates/btrfs · https://docs.rs/btrfs · https://github.com/wellbehavedsoftware/rust-btrfs
- libbtrfs — https://crates.io/crates/libbtrfs · https://docs.rs/libbtrfs
- btrfs-diskformat — https://github.com/GodTamIt/btrfs-diskformat · https://crates.io/crates/btrfs-diskformat
- btrfs-no-std — https://github.com/kennystrawnmusic/btrfs-no-std
- btrfs on-disk format — https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html · https://docs.kernel.org/filesystems/btrfs.html
