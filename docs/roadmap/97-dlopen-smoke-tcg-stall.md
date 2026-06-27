# Phase 97 - `dlopen-test-smoke` Intermittent TCG Stall (debugging)

**Status:** In Progress
**Source Ref:** phase-97
**Depends on:** Phase 95b ✅ (the `MAP_LAZY_FILE` demand-paged loader + blocking page-fault→`vfs_server` read), the SMP TLB-shootdown / lost-wakeup hardening (`docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md`) ✅
**Builds on:** the Phase 95b demand-paging path, the SMP cross-core TLB shootdown (`kernel/src/smp/tlb.rs`), the scheduler block/wake + stuck-task watchdog, and the always-on `smoke-test` harness
**Primary Components:** `ld-musl-x86_64.so.1` loader (`dlopen`/`dlclose`/`unmap_dso`), kernel `smp::tlb` (`tlb_shootdown_range`/`wait_for_shootdown_acks`/`active_cores`), the scheduler block/wake + watchdog, the in-kernel ramdisk demand-fault path, the `smoke-test` harness step

## Milestone Goal

Turn the intermittent, observability-blind `smoke-test` **step 26** (`dlopen-test-smoke`) failure into a *diagnosable, then deterministic* gate. This phase reframes the root cause away from the handoff's (falsified) "blocking-`vfs_server` demand-read lost-wakeup" hypothesis, gets real observability so the actual cause is **confirmed** before any fix is committed, lands the minimal confirmed fix plus a small set of always-safe hardenings, and replaces the PASS-or-SKIP step with a falsifiable, CI-deterministic regression gate.

## Why This Phase Exists

The always-on `smoke-test` gate (also run by the **pre-push** hook) intermittently fails at step 26 under plain **TCG** (not KVM), in two shapes: (a) the `SMOKE:dlopen-test-smoke:PASS` sentinel arrives but *after* the wait window (slow), and (b) **no** dlopen sentinel arrives at all within the window (a hard stall) — with **no** `KERNEL PANIC` / `#PF` / `#DF` line. The mitigation in `ae01ed4` (wait widened 30s→120s) addresses only the slow-PASS shape and cannot fix a true stall.

Two facts make this its own phase rather than a one-line patch:

1. **The handoff's leading hypothesis is falsified at the artifact level.** `readelf -d` shows `dlopen_test`'s only `DT_NEEDED` is `libdl.so`, and `libhello.so` / `libhello_fini.so` / `libdl.so` have **zero** `DT_NEEDED` (no `libc.so`). The entire dependency graph is **ramdisk-embedded** (`kernel/src/fs/ramdisk.rs:428` + the `/usr/lib` entries). Path resolution checks the ramdisk **first** (`kernel/src/arch/x86_64/syscall/mod.rs:8286`), so every fd is `FdBackend::Ramdisk`, and `kernel_read_fd_at`'s ramdisk arm (`mod.rs:12098-12113`) is a **pure synchronous `copy_from_slice`** with no `call_msg`, no parking, no `vfs_server` dependency. The blocking-`vfs_server` demand-read path the handoff names is **not even reachable** on this gate's hot path — that path is only exercised by `/usr` files on the real ext2 root (the `rustc`/`clang`/install reads). Any fix that touches the demand-read/IPC-reply path and claims to fix `dlopen-test-smoke` would be a misdiagnosis.

2. **The gate is observability-blind, so we cannot yet tell slow from stalled from faulted.** The step is a `WaitEither` with `pattern_a = SMOKE:…:PASS` / `pattern_b = SMOKE:…:SKIP` and **no FAIL pattern** (`xtask/src/main.rs:8365`); anything short of PASS just trips the generic 120s timeout. Worse, the guest `smoke-runner` runs `dlopen_test` as a forked child whose stdout/stderr are `dup2`'d to an **unlinked tmpfs capture file** (`userspace/smoke-runner/src/main.rs:1499`); only the parent's post-`waitpid` `SMOKE:…:PASS` crosses serial. On a stall the host sees the begin sentinel then silence — the child's `DLOPEN_TEST:FINI_PENDING` / `LIBHELLO_FINI:RAN` / `DLOPEN_TEST:PASS` progress is invisible.

Phase 97 exists to (1) **get observability**, (2) **confirm** the cause, (3) **fix** what is confirmed (plus a couple of always-safe hardenings), and (4) **replace** the blind gate with a falsifiable one.

## Learning Goals

- The real cost of a **cross-core TLB shootdown** on an *oversubscribed* TCG host, and why it amplifies into both a latency tail and a hard stall.
- Distinguishing **latency amplification** (slow-but-progressing) from a **true wedge** (lost wakeup / stranded lock / shootdown handshake stall) in an SMP microkernel.
- **Observability-driven** debugging of an intermittent race: instrument-and-bucket before patching; never "fix" an unconfirmed mechanism.
- Designing a **falsifiable** regression gate for a flake — base-rate measurement, controlled-load soak, and a real FAIL pattern — instead of widening a timeout.

## Feature Scope

### Area A — Observability & reproduction

Make the intermittent failure **reproducible and classifiable** before any kernel change. A standalone `cargo xtask run` loop autoruns `/bin/dlopen_test` (whose sentinels go straight to serial in standalone mode, `userspace/dlopen_test/dlopen_test.c:114`) at the smoke default `-smp 4`, buckets each run as *slow-PASS* / *partial-then-silent* / *faulted*, and measures a **base rate** under controlled host oversubscription (the flake's native habitat). Runtime instrumentation then answers the decisive questions: which backend serves each demand fill (proving the ramdisk/synchronous claim), whether the `dlclose` munmaps target remote cores and how long `wait_for_shootdown_acks` spins, whether a `(no waker registered)` or `[deadlock-guard] demand-fault under lock` line appears, and whether the stall **vanishes at `M3OS_SMP=1`** (the single decisive cross-core-vs-same-core discriminator).

### Area B — Root-cause fix (keyed to what Area A confirms)

Apply the **minimal** fix for the cause Area A confirms. The **leading surviving hypothesis** is the cross-core TLB shootdown that `dlclose`'s `munmap` performs **twice** on the `FINI_PENDING`→`PASS` critical path (`unmap_dso` → `sys_munmap` → `sys_linux_munmap` → `crate::smp::tlb::tlb_shootdown_range`, verified `dl.rs:670` / `mod.rs:12820,13006`): under `-smp 4` TCG with host oversubscription an idle/host-descheduled AP is slow to take the shootdown **NMI**, so the sender either spins a long wall-clock interval in `wait_for_shootdown_acks` (slow-PASS) or hits the documented re-NMI/mark-offline degrade window (the 2026-06-14 SMP TLB-shootdown class — a candidate no-output wedge). This path is **not** a `vfs`/demand-read path, so it survives the readelf falsification, is intrinsically SMP-TCG-specific (matching the profile: TCG-only, `-smp 4`, fine under KVM where NMIs land promptly), and can explain **both** manifestations with one mechanism. Area B also lands two **always-safe** hardenings that hold regardless of which hypothesis wins, and an honest-gate fail-fast (below).

### Area C — Honest, CI-deterministic regression gate

Replace the observability-blind `WaitEither` with a serial-direct sequence that adds the **FAIL pattern** the current matcher lacks and preserves the `FINI_PENDING < RAN < PASS` ordering assertion, hoist the existing `cmd_smp_smoke` fatal-pattern fail-fast into `run_smoke_script` so every always-on step fails fast with a *named* cause instead of an opaque 120s timeout, guard the new injected-command step against the COM1-RX-under-SMP byte-drop class, decide the TCG posture (always-on-but-fixed vs KVM-gated skip-with-reason), and prove the result with a controlled-load soak on the **original** execution path.

### Area D — Docs & version bump

Author this design doc + the companion task doc to template, repoint the roadmap README row, and apply the version-bump policy as a task when the fix lands (a debugging phase does not bump on the docs PR).

## Important Components and How They Work

### Component 1 — `dlopen_test` and where the two `munmap`s land

`dlopen_test` performs ~5 `dlopen`/`dlclose` ops but **loads only two DSOs** (`libhello.so`, `libhello_fini.so`); the rest are refcount / negative / double-close paths that do no I/O. The critical region is: print `DLOPEN_TEST:FINI_PENDING` → `dlclose(hf)` (refcount 1→0, the **last close**: `run_destructors_for` runs `libhello_fini`'s `DT_FINI_ARRAY` destructor, which writes `LIBHELLO_FINI:RAN`, then `unmap_dso`) → `dlclose(h2)`/`dlclose(h)` (the last close of `libhello` also `unmap_dso`s) → print `DLOPEN_TEST:PASS`. So **two** `unmap_dso` → `munmap` → `tlb_shootdown_range` calls sit on the `FINI_PENDING`→`PASS` path (`dl.rs:668-670`). All DSO pages are ramdisk-backed, so the demand **fills** are synchronous in-kernel memcpys — the linker/dlclose **logic** does no blocking I/O; the only SMP-sensitive operations on this path are the two shootdowns.

### Component 2 — the SMP cross-core TLB shootdown (`kernel/src/smp/tlb.rs`)

`tlb_shootdown_range` snapshots `AddressSpace::active_cores()`, executes a local `invlpg`, and for each *other* online core broadcasts an `IPI_TLB_SHOOTDOWN` delivered as an **NMI**, then spins in `wait_for_shootdown_acks` (`tlb.rs:130`) until every targeted core sets its ack bit. On timeout it dumps per-core diagnostics, **re-NMIs** the laggards, waits a grace window, and finally **marks a still-silent core offline** (degrade, no panic). There is a **single-core fast path** (`tlb.rs:313`: "If only one core is online, skips the IPI") — so at `M3OS_SMP=1` the shootdown collapses to a local `invlpg` and the cross-core cost disappears entirely (the basis for the discriminating `-smp 1` test). The module's own comments cite a **500 ms** ack budget as a "comfortable margin under TCG" — i.e. the path is known to be TCG-timing-sensitive. Conditional on `active_cores()` carrying remote-core bits, which the single-threaded child plausibly accumulates by **migrating across cores** during its demand-fault blocks earlier in the run.

### Component 3 — the observability-blind harness step

`xtask/src/main.rs:8365` pushes a `SmokeStep::WaitEither { pattern_a: SMOKE:…:PASS, pattern_b: SMOKE:…:SKIP, timeout_secs: 120 }` — PASS-or-SKIP, no FAIL. Matched serial is **consumed** (`drain_serial_through_match`, `mod.rs:7763/7770`); the full history is retained only with `M3OS_SMOKE_SERIAL_DUMP`, and even then the child's sentinels never reach serial because `smoke-runner` `dup2`s them to the unlinked tmpfs capture file. The stuck-task **watchdog** is **log-only** (`kernel/src/task/scheduler.rs:6602`) and *exempts* `BlockedOnRecv`-no-deadline — so a true wedge can be entirely silent, exactly matching "no panic, no output". `cargo xtask run` (`-serial stdio`, default `-smp 4`) is the ready-made standalone repro: `dlopen_test` writes sentinels directly to fd 1, so a live run streams the progress the gate hides.

## How This Builds on Earlier Phases

- **Extends Phase 95b** by auditing the demand-paged-loader path it introduced — and finds that, for this gate, the demand fills are ramdisk-synchronous (not the `vfs_server` path 95b added for ext2 `/usr` files), redirecting the investigation to the `dlclose` `munmap` shootdown instead.
- **Reuses the 2026-06-14 SMP hardening** (the `block_current_until`/`wake_task_v2` lost-wakeup guard and the `smp::tlb` re-NMI/mark-offline degrade) as the verified baseline, and treats the shootdown handshake under TCG oversubscription as the prime surviving suspect — the same family `smp-smoke` guards.
- **Reuses the Phase 95c reframe** that the slow-VFS under TCG is an artifact (fast under KVM), which is why the slow-PASS shape responds to a timeout widen while a true wedge does not.

## Implementation Outline

1. **Stand up observability (Area A) first, change nothing in the kernel yet.** Build the looped standalone `cargo xtask run` repro at `-smp 4`, bucket runs, and measure the unmodified gate's **base failure rate** under controlled host oversubscription.
2. **Instrument the suspects (Area A).** Add rate-limited one-shot logs: backend resolution in `kernel_read_fd_at` (Ramdisk vs VfsService) for each fill; `tlb_shootdown_range` target mask + `wait_for_shootdown_acks` spin-time / any offline-mark for the `dlopen_test` child's munmaps; and capture `(no waker registered)` / `[deadlock-guard] demand-fault under lock` presence.
3. **Run the decisive `-smp 1` vs `-smp 4` comparison.** If the stall vanishes single-core, the cause is cross-core (shootdown / oversubscription); if it persists, it is same-core logic/latency. Record the verdict.
4. **Confirm the cause, then fix only that (Area B)**, plus the two always-safe hardenings (fatal-pattern fail-fast in `run_smoke_script`; the lazy-file-fault-under-lock prevention + the `vfs_server` self-read guard as latent-bug hardening).
5. **Replace the blind gate (Area C)** with a serial-direct, FAIL-detecting, ordering-asserting step + COM1-RX integrity guard, and decide the TCG posture.
6. **Soak on the original execution path** under controlled load, powered by the measured base rate, then **bump the kernel version** (Area D) and finalize docs.

## Acceptance Criteria

- A standalone repro procedure exists and, run ≥50× at `-smp 4` TCG under controlled host oversubscription, classifies every run as *slow-PASS* / *true-stall* / *fault* with the bucket counts recorded in this doc; the **unmodified** gate's base failure rate is measured first (≥200 runs) so the post-fix soak is statistically powered (rule-of-three: N clean runs only bounds the rate at ~3/N).
- Runtime instrumentation prints which backend (Ramdisk vs VfsService) serves each `dlopen_test` demand fill, and the captured log shows **100% Ramdisk** for the `dlopen_test` dependency graph — empirically confirming the blocking-`vfs` path is off this gate's hot path (or surfacing a surprise `VfsService` fd if not).
- For ≥1 reproduced stall, `tlb_shootdown_range` is instrumented for the child's `dlclose` munmaps and the result is recorded: it either targets **zero** remote cores **or** completes its ack handshake within a bound, with `wait_for_shootdown_acks` spin time / any offline-mark logged — confirming or excluding the leading hypothesis with data, not narrative.
- The `M3OS_SMP=1` vs `-smp 4` comparison is recorded and treated as a **gate on the root-cause conclusion**: any cross-core hypothesis (incl. the munmap shootdown, whose remote mask collapses to 0 single-core) requires the stall to vanish at `-smp 1`.
- For any reproduced stall, the serial capture is searched and the result recorded for **both** `(no waker registered)` `BlockedOnReply` (`scheduler.rs:6602`) and `[deadlock-guard] demand-fault under lock` (`interrupts.rs:925`); presence/absence is documented and mapped to a confirmed/refuted hypothesis.
- The `dlopen-test-smoke` gate gains a **FAIL pattern** (not PASS-or-SKIP only): an injected fault (a deliberately broken `dlopen_test`) makes the gate **FAIL fast** naming the failing sentinel, not time out at 120s — and this is proven, not assumed.
- `run_smoke_script` fail-fasts on the shared fatal patterns (`KERNEL PANIC` / `no waker registered` / `RECURSIVE KERNEL PAGE FAULT` / `process killed`), hoisted from `cmd_smp_smoke`, so every always-on step reports a *named* cause.
- Any new injected-command step carries a **COM1-RX integrity check** (echo-back / verbatim-receipt assertion) so a serial byte-drop under SMP cannot masquerade as a kernel stall.
- On the confirmed manifestation, the chosen posture (the confirmed fix, and/or KVM-gate or relaxed timeout for a residual latency tail) yields **20/20** consecutive green TCG `smoke-test` runs on a deliberately loaded host with no `dlopen`-step timeout; if the gate is redesigned, the bug is additionally shown to be fixed on the **original** `fork()`+`dup2(capture)`+`waitpid()` path (a green redesigned gate alone does not prove the old path is fixed, since the redesign can remove the very trigger).
- This design doc and the companion task doc pass the template-conformance rules (all required sections populated; each task has File/Symbol/Why it matters/Acceptance; Track Layout table present), and the roadmap README row is repointed (Status **In Progress**; Milestone + Tasks links live).
- The doc record explicitly states, with the `readelf` evidence, that the blocking-`vfs_server` demand-read lost-wakeup hypothesis is **falsified for this gate**, so no fix is merged claiming to fix `dlopen-test-smoke` via the demand-read / `vfs` reply path.

## Companion Task List

- [Phase 97 Task List](./tasks/97-dlopen-smoke-tcg-stall-tasks.md)

## How Real OS Implementations Differ

- **Deferred / batched TLB invalidation.** Linux uses lazy-TLB, per-mm `cpumask` tracking, and batched flushes (`tlb_gather`), and on AMD `INVLPGB` does a broadcast invalidation in hardware with no IPI at all — so `munmap` rarely pays a synchronous cross-core round-trip. m3OS shoots down synchronously per range and spins for acks.
- **IPI vs NMI, and real cores vs TCG.** Production kernels deliver shootdowns as ordinary fixed-mode IPIs to physically-distinct cores that take them in ~1 µs. m3OS delivers them as **NMIs** (for halt-resilience) and, under TCG, "remote cores" are time-sliced on one host thread — so an oversubscribed AP can be wall-clock-late by orders of magnitude, the precise condition this phase targets.
- **Hung-task policy.** Linux's hung-task detector can `panic` (a recoverable, debuggable signal) on configurable timeout; m3OS's watchdog is **log-only** and exempts idle servers, so a wedge is silent — which is why observability, not just a watchdog, is the lever here.
- **Test-harness honesty.** Mature CI distinguishes timeout / crash / assertion-failure as separate verdicts with per-step diagnostics; the current gate collapses all non-PASS into one opaque timeout, which this phase fixes.

## Deferred Until Later

- The heavier **kernel demand-read timeout / watchdog-kill** work — bounding the `vfs_service_read_kernel` `call_msg` and making the watchdog kill-capable — targets the genuinely `vfs`-backed gates (`rustc`/`clang`/install), **not** this gate's ramdisk-synchronous path, and is explicitly out of Phase 97's critical path.
- The full **page cache for file-backed pages** (Phase 95c Track B) that would change the demand-fill cost profile for `vfs`-backed gates.
- **Folding into Phase 98** — Phase 98's roadmap audit re-charters the next arc and folds in Phase 97's *deferred* items; Phase 97 is authored and delivered standalone first.
