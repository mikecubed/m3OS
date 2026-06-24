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

### Step 1 (PRIMARY) — the `rust-lld` link wedge: a Phase-95b "lock held across a blocking lazy-file fault" CLASS

**DIAGNOSIS SOLVED (2026-06-24). It was never a codegen hang.** Empirical KVM runs peeled it:
1. rustc **compiles fine** — the handoff's "compile-thread hang" was the slow TCG compile.
2. The smoke harness's linker invocation was **mis-wired** — `-C linker-flavor=ld.lld` alone
   execs a binary named `lld`; the bundled one is `rust-lld`. **FIXED** by adding
   `-C linker=rust-lld` (xtask/src/main.rs ~23791; kept). With that, rustc invokes `rust-lld`.
3. **The real blocker:** once `rust-lld` (a *dynamic*, multithreaded LLVM linker) runs, the
   **whole system wedges — the stall-census watchdog stops too**. That "watchdog stops"
   ⇒ a kernel deadlock (a global lock stranded across a context switch), not a userspace
   futex block.

**Root cause (high confidence, agent-verified).** Phase 95b (commit `bc732cb8`) made dynamic
PT_LOAD pages `MAP_LAZY_FILE`, so a demand-fault now issues a **blocking vfs_server IPC**.
Any syscall that does `copy_from_user`/`copy_to_user` (which pre-faults via `try_demand_fault`)
**while holding an `IrqSafeMutex`** can now block → `block_current_until` → `switch_context`
**with the lock still held, IF masked, preempt raised**. The lock is never released; every
other core spinning on it wedges; the watchdog (needs scheduler progress) goes silent. This
is exactly why a **static** binary (node) passes `smp-smoke` but a **dynamic** one (rust-lld)
wedges — only dynamic binaries have lazy-file PT_LOAD pages.

**FIXED so far (this is a CLASS — more sites remain):**
- `sys_futex` `FUTEX_WAIT` + `FUTEX_CMP_REQUEUE` read the futex word under `FUTEX_TABLE`
  (`syscall/mod.rs` ~19677 / ~19833). **Fixed** by pre-faulting the word with no lock held
  before locking (Linux-style). Note: this did NOT clear the `rust-lld` link wedge — it's a
  *different* site (the link path completes `--version`/`--print sysroot`, which always
  worked, then wedges only in the actual link).

**REMAINING: find the other lock-held-across-lazy-file-fault site(s) in the `rust-lld` link
path** (the agent flagged `PROCESS_TABLE`, `ENDPOINTS`, and the scheduler locks as candidates;
the `PROCESS_TABLE`-held-across-`copy_from_user` case is also a non-reentrant self-deadlock —
`process/mod.rs:1293,1309` re-take `PROCESS_TABLE` from the demand path).

**NEXT STEP — the deadlock-guard (stop guessing which lock).** Add a one-shot diagnostic at
the blocking demand-fault chokepoint (`block_current_until`, scheduler.rs:3544, or
`demand_map_vma_page`, interrupts.rs ~908): when about to block **with a lock held / preempt
raised**, `log::error!` the offending **syscall number + user RIP + pid** (per-task
`syscall_snapshot` in task/mod.rs ~636; the IrqSafeMutex/preempt held-state is the trigger).
The **last line before the silence** names the culprit syscall → pre-fault its user copy
before the lock (same fix shape). Repeat until the wedge clears. Consider a **systemic** fix
too: pre-fault user-pointer args at syscall entry before locks, or a drop-lock-fault-retry on
`copy_*_user`, since the agent warns this hazard exists anywhere a held lock spans a user copy.

**FAST REPRO (build this first — ~2 min vs the 15-min rust gate).** A minimal **dynamic**
(`PT_INTERP` + lazy-file PT_LOAD) C program, **4 pthreads**, with the contended
`pthread_mutex_t`/`pthread_cond_t` in a **global `static` (.bss/.data)** — so its page is
lazy-file-cold on first touch — doing tight lock/cond contention. A stack-local lock faults
into the *non-blocking anonymous* path and misses the bug, so it MUST be a global. Build via
the `build_dynamic_hello_fixture` template (`-O2 -fPIE -pie`, `xtask/src/main.rs:21933`); wire
it as a `dynamic-mt-smoke` gate (the permanent regression guard for this Phase-95b class).

**Acceptance:** `rustc /usr/src/hello.rs` links via multithreaded `rust-lld` and the binary
runs → `RUSTC_OK` (serial, `M3OS_KVM=1`, clean disk); `dynamic-mt-smoke` passes; `smp-smoke`
stays green. Also handle the `--threads=1` path's `std::process` `ENOTTY` panic (a separate,
smaller bug) only if it resurfaces once the multithreaded path works.

**Other notes:** `unhandled syscall 324` = `membarrier(PRIVATE_EXPEDITED)` — appears benign
(a whole-system wedge is a lock deadlock, not a missing barrier); revisit only if a *single*
thread is left `BlockedOnFutex` after the lock fixes.

### Step 2 (DECISION — RESOLVED 2026-06-24) — `rustc-smoke` is KVM-gated; CI uses KVM

**Decision: KVM-gate `rustc-smoke`** (skip-with-reason without `M3OS_KVM=1`, like
`node-jit-smoke`). **Verified empirically** — a throwaway `ci/kvm-probe` workflow on
`ubuntu-latest` showed GitHub-hosted runners **DO expose `/dev/kvm`** and `kvm-ok` reports
"KVM acceleration can be used". The "GitHub runners have no KVM" assumption still written in
`build.yml`/`pr.yml` comments is **stale**. Findings:

- Host: Azure VM, **AMD EPYC 7763** (Zen 3), 4 vCPU, SVM exposed on all cores → nested KVM
  works; `pkg install rust` ~25 s and cold-load ~9.6 s apply in CI, not the ~40-min TCG wall.
- `/dev/kvm` is `root:kvm 0660`, so the non-root `runner` user needs the standard one-line
  udev enable step before QEMU can use it:
  ```yaml
  - name: Enable KVM
    run: |
      echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
        | sudo tee /etc/udev/rules.d/99-kvm4all.rules
      sudo udevadm control --reload-rules && sudo udevadm trigger --name-match=kvm
  ```
- **Caveat — PKU is NOT exposed** (`pku=no`/`ospke=no`; Azure masks it even on Zen 3). KVM
  buys `rustc-smoke` **speed**, which is all rustc needs (it does not JIT). It does **not**
  give the PKU-dependent JIT gates (`node-jit-smoke`, the `claude` JIT arm) what they need —
  those stay dev-machine / self-hosted-runner only.

**Consequences:** **Step 3 (95c B/D) is OPTIONAL for the milestone** — under KVM the FS is
already fast, so the 95-series closes on **Step 1** alone. `rustc-smoke` runs KVM-accelerated
in a dedicated heavy/nightly CI lane (the udev step + `M3OS_KVM=1`), matching how the other
heavy toolchain gates are opt-in rather than per-PR. **Do not** spend 95c making TCG fast for
a per-PR rustc gate. Follow-ups when we reach the milestone: (a) wire the KVM lane + udev step
for `rustc-smoke`; (b) refresh the stale "no KVM" comments in `build.yml`/`pr.yml`.

### Step 3 (95c — now OPTIONAL per the Step 2 decision) — finish VFS perf, in the driver

Step 2 resolved to KVM-gating `rustc-smoke`, so this is **not** on the milestone path; pursue
it only for a TCG-runnable gate or for the standalone wins (Track B helps KVM too — repeat /
shared loads). All **in the ring-3 `vfs_server` path** —
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
