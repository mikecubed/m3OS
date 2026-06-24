---
status: CANONICAL PLAN (pick up here) — the single source of truth for finishing the
  Phase 95 series (95b on-device `rustc` codegen + 95c VFS/block-I/O perf). Supersedes the
  *forward-planning* framing in the 95b/95c task + design docs and the 2026-06-23 handoff,
  all of which predate the page-table fix (`841fd53f`) and the KVM perf measurements and so
  carry an outdated "the wall is the slow VFS install" diagnosis. Those docs remain the
  technical *record*; THIS doc is the plan.
owner: unassigned
updated: 2026-06-24
---

# Phase 95 completion plan — finish on-device `rustc` (95b) + VFS perf (95c)

## TL;DR — where we actually are

- **`rustc` runs on m3OS.** After the loader/libc/page-table crash chain was fully fixed
  (see "What is done"), `rustc --version` → `rustc 1.96.0` and `rustc --print sysroot` →
  `/usr` both **execute on-device, serial-captured**. The multi-week `CR2=0` blocker is gone.
- **The ONE remaining blocker to `RUSTC_OK` is the `rustc hello.rs` compile.** It loads,
  spawns its codegen threads, then **goes silent** — a **multithreaded-compile hang or
  extreme slowness**, *not* a page-table, loader, or filesystem problem. The lead is a
  kernel `[sched] dequeue-drop … state-not-ready` warning on a rustc worker thread. This is
  a **scheduler / futex / threading** investigation — a *new* track, owned by neither task
  list as written.
- **The "slow VFS install is the wall" story was a TCG artifact.** Measured under KVM, the
  FS is fast (136 MB/s write, 422 MB/s read, 217 µs IPC RTT): `pkg install rust` is **~25 s**
  and the 169 MB `librustc_driver.so` cold-load is **~9.6 s**. So **95c's VFS perf work is
  NOT on the `RUSTC_OK` critical path under KVM** — it only matters for running the gate
  under plain TCG (CI without KVM).
- **Architecture decision (owner, 2026-06-24): the in-kernel ext2 read fast path
  (95c Track/Area F) is REJECTED.** It violates the microkernel boundary and directly
  conflicts with the just-completed ext2-engine-unification (vfs_server is now the sole
  post-boot root reader). Fix VFS performance *in the ring-3 ext2 driver*; do not move ext2
  reads back into ring 0 unless A+B+D **and** a recorded measurement prove the VFS path
  categorically cannot reach acceptable throughput — and even then prefer faster IPC.

## What is done (committed on `feat/phase-95b-on-device-rustc`)

The entire "rustc won't start" crash chain — the subject of
[`2026-06-23-rustc-runtime-null-deref-after-tls.md`](./2026-06-23-rustc-runtime-null-deref-after-tls.md):

| Fix | Commit |
|---|---|
| `__libc.auxv` NULL (mallocng first-malloc) → loader sets it before constructors | `610f8acd` |
| musl TLS globals + `tls_module` list (`__copy_tls td=0`) | `bbc510c8` |
| `dladdr` implemented in the loader (sysroot detection) | `500b3747` |
| writable+user intermediate page-tables (the 165 M-iteration fault loop) | `841fd53f` |
| multi-module static TLS for DT_NEEDED DSOs | `59cd0c00` |

95c supply-side perf, also landed: zero-copy SHM read-window (A.1), readahead caps 64→256 KiB
(A.2), `LruBlockCache` for `vfs_server` (C), `DirIndex` O(1) metadata + per-group free-search
cursor + granular `invalidate_cache` (the O(N²)-create + device-read fixes), the
`vfs-throughput-smoke` gate (E.1). And the **ext2-engine-unification** (vfs_server = sole
post-boot root engine for reads/exec/open; see
[`2026-06-24-ext2-engine-unification-plan.md`](./2026-06-24-ext2-engine-unification-plan.md),
closed).

## Corrected understanding (why this plan differs from the older docs)

Three reframes happened *after* the 95b/95c docs were written, and the older docs were never
fully reconciled (this plan does that):

1. **rustc now reaches userspace and executes.** The 95b "Outcome" + README row say "the
   loader wedges in the kernel loading the 162 MB `librustc_driver.so`; rustc never runs
   userspace." That was true *before* the page-table fix (`841fd53f`). It is now **false** —
   rustc runs; `--version`/`--print sysroot` complete.
2. **The FS is not the wall under KVM.** The "~100–200 KB/s VFS / ~40-min install /
   install-timeout is the immediate `RUSTC_OK` blocker" framing in 95b's Outcome and 95c's
   "Why This Phase Exists" was measured under **TCG**. Under **KVM** the FS is fast and the
   install is ~25 s. The throughput numbers are real for TCG but do not gate the milestone
   when KVM is available.
3. **The real blocker moved to the compiler's threads.** `rustc hello.rs` hangs/very-slows
   in multithreaded codegen — a kernel scheduler/futex issue, the class m3OS has hit before
   (Phase 89 `FUTEX_REQUEUE`, Phase 90b SMP futex keys, the 2026-06-14 lost-wakeup work).

## The plan — ordered work to close the 95-series

### Step 1 (PRIMARY, the real milestone blocker) — fix the `rustc hello.rs` compile-thread stall

This is the only thing between here and `RUSTC_OK`. It is **new** work, not in either task
list. Treat it as its own track.

**Files:** the scheduler block/wake path (`kernel/src/sched/` / `block_current_until` /
`wake_task_v2`), the futex syscall path (`kernel/src/arch/x86_64/syscall/` futex ops), the
`[sched] dequeue-drop … state-not-ready` emitter, the `clone(CLONE_THREAD)` / pthread path,
and `fork`/`exec`/`waitpid` (rustc spawns `rust-lld` as a subprocess).

**Investigation outline:**
1. **Reproduce in isolation, KVM, clean disk, serial dump.** Confirm `--version` /
   `--print sysroot` pass and only `rustc /usr/src/hello.rs` stalls:
   ```
   cargo xtask clean
   M3OS_SMOKE_SERIAL_DUMP=/tmp/s.txt M3OS_KVM=1 cargo xtask rustc-smoke --timeout 5400 &
   # after install + a few min in the compile, `pkill -TERM -x qemu-system-x86`, then:
   grep -a 'dequeue-drop\|BlockedOnFutex\|no waker\|process killed\|rust-lld\|panic' /tmp/s.txt
   ```
2. **Disambiguate hang vs. slow.** Re-use the 95b timer-ISR userspace-RIP sampler + the
   syscall sampler: zero forward progress over minutes ⇒ a **deadlock** (lost wakeup /
   futex / waitpid); slow-but-advancing ⇒ a throughput/contention problem.
3. **Trace the `dequeue-drop state-not-ready` path** — find where a task is dequeued while
   not runnable. This smells like a **wake/block race** (a worker signaled ready but
   dropped, or a futex wake delivered to the wrong key / lost). Cross-check against the
   2026-06-14 SMP lost-wakeup re-check (`block_current_until` re-validation) and the
   per-address-space futex keys (Phase 90b).
4. **Bisect rustc's own parallelism** to localize: `rustc -C codegen-units=1 -Z threads=1
   /usr/src/hello.rs` (serial codegen) vs. the default parallel path. If serial passes and
   parallel hangs, the bug is in the threadpool's futex/condvar handshake; if both hang,
   suspect the `rust-lld` subprocess spawn/`waitpid`.
5. **Check the link step is reached.** Does `rust-lld` ever spawn (grep the dump)? A hang
   *before* lld is codegen-threadpool; a hang *at* lld is the fork/exec/wait path on a large
   dynamic binary.

**Acceptance:** `rustc /usr/src/hello.rs` completes and the binary runs → `RUSTC_OK`
(serial), under `M3OS_KVM=1`, clean disk; the scheduler warning is gone; `smp-smoke` stays
green (the fix must not regress the existing futex/threadpool guard).

### Step 2 (DECISION, do this before sinking more into 95c) — gate `rustc-smoke` on KVM?

Because the FS is only slow under TCG, the cheapest path to a green gate is to **require KVM
for `rustc-smoke`** (skip-with-reason without `M3OS_KVM=1`), exactly like `node-jit-smoke`
and the `claude` TUI arm already do. If we take that, **95c Steps 3a/3b below become optional
for the milestone** and the 95-series closes on Step 1 alone.

- **Recommended:** KVM-gate `rustc-smoke`. Rationale: the install/cold-load are already fast
  under KVM; CI runners that lack nested virt skip cleanly (established precedent); we avoid
  spending a whole subphase to make a heavyweight gate fit a TCG timeout.
- **Alternative:** keep `rustc-smoke` TCG-runnable → then Step 3 (95c B+D) is required so the
  ~368 MB install fits the timeout under TCG.

This is the pivotal sequencing call and should be made explicitly (owner input).

### Step 3 (95c, CONDITIONAL on Step 2 = "keep TCG-runnable") — finish VFS perf, in the driver

Only needed if `rustc-smoke` must run without KVM. All **in the ring-3 `vfs_server` path** —
**not** in the kernel (Track F is rejected, see below):
- **3a. Track B — kernel page cache for file-backed pages** (`(file-id, offset)` keyed;
  re-faults / second run / shared DSO maps served with zero server IPC). This is the
  external-pager amortizer and the one genuinely-new idiomatic win; it benefits KVM too
  (repeat/shared loads), so it is worth landing regardless.
- **3b. Track D — installer read/verify/write coalescing** (`userspace/pkg/`): use the
  Track A bulk caps, hash in-line with the read, coalesce writes; cut `pkg install` I/O.
- **Re-measure** (Track E) after each; the throughput gate is the guard.

### Step 4 — flip the milestone gate (95b Track D / 95c Track G converge)

With Step 1 (and Step 3 if TCG-gated) done: `rustc-smoke` PASSES end-to-end under
`M3OS_RUST_REGRESSION=1` — `pkg install rust` → `rustc --version` → `--print sysroot` →
`rustc hello.rs` → `RUSTC_OK`. Same gate scaffold; no new wiring.

### Step 5 (STRETCH) — `cargo` + proc-macros (95b Track E)

After the milestone: stage `cargo`, `cargo build` a proc-macro-free fixture (`CARGO_OK`),
then a derive-macro crate via on-device `dlopen` of the proc-macro `.so` against the Phase 93
`libc.so` (`CARGO_PROCMACRO_OK`), behind a separate `cargo-smoke` / `M3OS_CARGO_REGRESSION`
gate. Gated behind the milestone; decide separately whether it is in-scope for the 95-series
or a follow-up phase.

### Step 6 — closeout (95b Track F + 95c Track H)

Only after the milestone is green:
- Kernel version bump `0.95.0` → `0.95.1` (`kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md:7`).
- `AGENTS.md` Phase-95 capability bullet: flip "diagnosed but blocked" → "runs on m3OS".
- Learning docs (`docs/95b-*.md` streaming-loader/proc-macro; `docs/95c-*.md`
  external-pager/zero-copy) per the aligned-learning template; register in `docs/README.md`
  + `codebase-map.md`.
- Roadmap README rows + design-doc Status flips (`Planned`/`Partial` → `Complete`); mermaid
  `P95 → P95b → P95c` edges already present.

## Architecture decision — Track/Area F (in-kernel ext2 read fast path) is REJECTED

**Decision (owner, 2026-06-24): do not implement the in-kernel ext2 read bypass.** It is a
deliberate microkernel-boundary departure (read-only `/usr` demand pages straight from
`EXT2_VOLUME`, no IPC), and it now also conflicts head-on with the **ext2-engine-unification**
that just made `vfs_server` the *sole* post-boot root reader. Performance must be fixed **in
the ring-3 ext2 driver** (zero-copy + readahead + the kernel page cache — Tracks A/B), which
is also the idiomatic Mach/Zircon/L4Re recipe.

The only door left open: F may be reconsidered **only if** Tracks A+B+D are landed **and** a
recorded Track-E measurement proves the VFS path categorically cannot reach acceptable
throughput at large readahead clusters — i.e. raw IPC cost itself is the wall. Even then the
correct fix is **faster IPC**, not ext2-in-ring-0; F would be a flagged, last-resort,
retireable fallback with an explicit retirement condition, never the design. Absent that
proof, F is **intentionally not implemented** and the 95c docs say so.

## How to pick up (repro + the gotchas that cost hours)

Repro and the full infra gotchas (clean-disk-between-runs to avoid `disk.img` corruption,
why **not** to set `M3OS_SERIAL_LOG`, the dhcpv6 retransmit spam burying the crash line,
`M3OS_RUST_FAST_ITER` only on a complete prior install) are in the
[2026-06-23 handoff](./2026-06-23-rustc-runtime-null-deref-after-tls.md#test-infra-gotchas-these-cost-hours-last-time)
— read that section before the first run.

## Doc map (which doc holds what, after this cleanup)

| Doc | Role now |
|---|---|
| **this file** | the plan — what's left, in order, and the decisions |
| `2026-06-23-rustc-runtime-null-deref-after-tls.md` | technical record of the (now-fixed) crash chain + the repro gotchas |
| `2026-06-24-ext2-engine-unification-plan.md` | closed — vfs_server single-root-engine record + the DAC write-back blocker |
| `roadmap/tasks/95b-…-tasks.md` | 95b track checklist (crash chain + loader/mm) |
| `roadmap/tasks/95c-…-tasks.md` | 95c track checklist (VFS perf), Track F demoted |
| `roadmap/95b-…md`, `roadmap/95c-…md` | phase design docs (status reconciled to this plan) |
