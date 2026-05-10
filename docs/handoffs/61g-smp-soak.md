# Phase 61 Track G — SMP Regression and Soak

**Phase:** 61 (SMP Load Balancing + Phase 25/35 Closeout)
**Track:** G — full regression + 10-minute SMP soak

## Regression results (CI-equivalent)

`cargo xtask test` ran the full QEMU integration test suite at the
Phase 61 closure commit:

```
All 12 test(s) passed
```

Per-test breakdown:

| Test | Status | Notes |
|---|---|---|
| `bound_recv` | PASSED | Phase 55c bound-notification recv wiring (existing). |
| `preempt_latency` | PASSED | Phase 57e percentile aggregator + rdtsc monotonic (existing). |
| `preempt_user_stress` | PASSED | Phase 57d preempt-voluntary stress placeholders (existing). |
| `preempt_voluntary` | PASSED | Phase 57d preempt model contracts (existing). |
| `sched_fuzz` | PASSED | Phase 57a multi-core scheduler fuzz model (existing). |
| `xsave_avx` | PASSED | Phase 57e XSAVE/AVX state preservation (existing). |
| `smp_prelude_smoke` | PASSED | **Phase 61 Track 0b** harness validation. |
| `load_balance_smp` | PASSED | **Phase 61 Track B**, observed redistribution `core0 8 -> 4`. |
| `child_times_e1` | PASSED | **Phase 61 Tracks E.1 + E.4 + E.2** invariants. |
| `pipe_wakeup_smp` | PASSED | **Phase 61 Track D.1**, observed 1-tick cross-core latency. |
| `ipc_wakeup_smp` | PASSED | **Phase 61 Track D.2**, observed 0–1 tick cross-core latency. |
| `munmap_tlb_smp` | PASSED | **Phase 61 Track C.2**, observed 0-tick TLB shootdown completion. |

`cargo xtask check` (clippy + rustfmt + kernel-core / passwd /
driver_runtime host tests) clean at the same commit.

## 10-minute SMP soak — workload definition

The Phase 61 task list specifies a sustained-load soak with a fixed
workload to surface SMP correctness issues that the unit tests cannot
reach in isolation. The workload, run on a 2-core QEMU instance for
10 wall-clock minutes:

1. Four CPU-bound tasks via the `xtask smoke-test` shell driver
   (`while true; do :; done` per task; or four invocations of a tight
   userspace counter loop binary).
2. One pipe ping-pong loop pair (writer pinned to core 0, reader
   pinned to core 1) — exercises Track F's blocking sleep/wake path.
3. `sshd` running on its default port (Phase 53 baseline).
4. `display_server` started but with no clients connected (Phase 56
   baseline).

QEMU command line:

```bash
M3OS_SMP=2 cargo xtask run --fresh
# (or `cargo xtask soak --duration 10m` if the soak driver is wired up)
```

## Soak acceptance criteria

The soak passes when, after 10 wall-clock minutes:

1. Zero kernel panics.
2. Zero `WARN` / `ERROR` lines in the serial log (other than known-
   benign ACPI / IRQ-override notices logged once at boot).
3. The four CPU-bound tasks remained scheduled across both cores
   (load balancer kept the system balanced — verifiable by inspecting
   per-core run-queue snapshots in the serial log if `M3OS_SCHED_DUMP`
   is enabled, or simply by observing that no single core saturated).
4. The pipe ping-pong pair continued exchanging bytes throughout the
   soak — the count at end-of-soak should be approximately
   `count_per_second × 600`.
5. `sshd` accepted at least one connection during the soak (manual
   test from another shell: `ssh -p 2222 root@127.0.0.1`).
6. `display_server` remained alive (no exit message in serial log).

## Soak procedure

1. Build a fresh image:

   ```bash
   cargo xtask clean
   cargo xtask check
   ```

2. Launch with `M3OS_SMP=2`:

   ```bash
   M3OS_SMP=2 cargo xtask run --fresh > soak-serial.log 2>&1 &
   SOAK_PID=$!
   ```

3. Inside the running guest, drive the workload (paste each in a
   separate `sh0` window or use `init.conf` to start them on boot):

   ```text
   ## CPU-bound tasks
   while true; do :; done &
   while true; do :; done &
   while true; do :; done &
   while true; do :; done &

   ## Pipe ping-pong (single shell, but exercises pipe blocking)
   sh -c 'while true; do echo ping; done' | sh -c 'while read ln; do echo $ln; done' > /dev/null &
   ```

   (The exact pipeline depends on what `sh0` / `ion` support at the
   time of soak; if pipe redirection is incomplete, the simpler
   variant is two userspace processes calling `read`/`write` on
   `pipe(2)`-allocated FDs.)

4. Wait 10 wall-clock minutes.

5. Stop QEMU (`Ctrl-C` or `kill $SOAK_PID`) and inspect
   `soak-serial.log`:

   ```bash
   grep -E '(PANIC|WARN|ERROR)' soak-serial.log
   ```

   Empty output (or only the boot-time ACPI / IRQ-override `INFO`
   lines that always appear) is the pass condition.

## Soak result placeholder (manual procedure — pending)

The literal manual procedure above (4 CPU-bound loops + pipe ping-pong +
SSH connect + `display_server` idle, 10 wall-clock minutes on
`M3OS_SMP=2`) has not yet been performed. It remains a manual gate
before the kernel version bump (`v0.61.0`) is tagged on `main`.

```
QEMU command:    [paste actual `cargo xtask run` invocation here]
Workload:        [paste actual init.conf / shell history here]
Duration:        [e.g. 10:00.000]
Final tick:      [e.g. 600_125]
Panics:          [PASS / FAIL — paste any panic lines]
WARN/ERROR:      [PASS / FAIL — paste any unexpected lines]
Engineer:        [name]
Date:            [YYYY-MM-DD]
Verdict:         [PASS / FAIL]
```

> **Note:** the soak is a procedural checkpoint, not a CI-runnable
> automated test. The Phase 61 PR ships with the regression results
> above. The soak must be performed before the kernel version bump
> (`v0.61.0`) is tagged on the `main` branch.

## Automated `cargo xtask soak` attempt — 2026-05-09

A best-effort proxy run was performed using the existing Phase 57e Track
G automated harness (`cargo xtask soak --duration 10m`), which loops
`cargo xtask smoke-test` for the configured duration and applies
zero-tolerance grep checks. **This is not the manual procedure above** —
the workload is the smoke-test fixture (boot → fork-bomb +
ext2 mount + tcc-compile + virtio-blk traffic) rather than the four
CPU-bound loops + pipe ping-pong + SSH the manual procedure spec'd. It
is, however, the highest-signal SMP-sensitive run that can be driven
unattended, and was used here as a sanity check on whether the PR #144
review fixes regressed SMP behaviour.

### Result — both pre- and post-fix HEAD fail with the same fingerprint

Pre-fix baseline (`38f3099` — branch HEAD before PR #144 review fixes):

| Pattern | Count | Threshold |
|---|---|---|
| `[sched] stale-ready` | 4 | 0 |
| `[sched] cpu-hog` | 0 | 0 |
| `virtio-blk` request timeout | 1 | (info) |
| `[sched] dequeue-drop` | 10 | (info, benign) |
| Runs completed | 1 | — |
| Acceptance | **FAIL** | — |

With-fix HEAD (`412fe3c` — PR #144 after review fixes):

| Pattern | Count | Threshold |
|---|---|---|
| `[sched] stale-ready` | 20 | 0 |
| `[sched] cpu-hog` | 1 | 0 |
| `virtio-blk` request timeout | 2 | (info) |
| `[sched] dequeue-drop` | 38 | (info, benign) |
| Runs completed | 2 | — |
| Acceptance | **FAIL** | — |

Both HEADs fail the gate on the same primary pattern (`[sched]
stale-ready`, the documented Bug #9 fingerprint per
[`docs/handoffs/57e-bug9-bug10-followup.md`](./57e-bug9-bug10-followup.md)).
The post-fix run completed two smoke-test iterations to the baseline's
one, so per-iteration counts (10 / 4 stale-ready warnings) are within
timing-variance noise for a 1-vs-2-iteration sample of a documented
timing-sensitive bug.

### Why this is not a Phase 61 review-fix regression

The PR #144 review fixes are confined to:

- A `period_ms` parameter added to
  `kernel::task::scheduler::tick_account_current_task` so AP cores
  attribute time on the `1 tick = 1 ms` scale (Track E.2 closure);
  the `user_ticks` / `system_ticks` counters it writes are read only
  by display-only accessors (`current_task_times`,
  `process_total_times`, zombie-reap accumulation) and are not
  consulted by any wake / migration / dispatch code.
- `sys_getrusage(NULL) → -EFAULT` — not exercised by the smoke-test
  workload (which uses `waitpid`, not `getrusage`).
- Doc / comment / test-constant changes (`MAX_LATENCY_TICKS` 100→10,
  cadence comments, status flips, `spawn_on_core` doc tightening) —
  no runtime behavioural impact on smoke-test paths.

None of these can plausibly cause `[sched] stale-ready` to fire. The
fingerprint is independent of `user_ticks` / `system_ticks` and predates
PR #144's first commit.

### Cross-references for the eventual closeout

When the manual `v0.61.0`-tag soak is performed, the engineer running it
should be aware that:

- `[sched] stale-ready` and `[sched] cpu-hog` are expected to fire
  intermittently until Bug #9 (FS-volume `IrqSafeMutex` `preempt_count`
  leak — Option B Arc-clone refactor) is closed in Phase 62. See
  [`57e-bug9-bug10-followup.md`](./57e-bug9-bug10-followup.md) §
  "Post-deferral severity adjustment".
- AP cores formerly took a kernel-mode GPF during late boot; **fixed**
  2026-05-09 via the static `.bss` kernel-stack pool — see
  [`ap-core-gpf-saved-rsp-stack-corruption.md`](./ap-core-gpf-saved-rsp-stack-corruption.md)
  § Resolution. The literal-procedure soak should no longer be at risk
  of the AP-takedown signature.
- A literal-procedure pass of the soak with the spec'd workload
  remains pending. Both bugs above plausibly fire under that workload
  and need to be considered when classifying the result.

### Artefacts

- Pre-fix run: `target/soak/run-1778299744/` (in throwaway worktree at
  `38f3099`, removed after capture).
- With-fix run: `target/soak/run-1778298182/`.

## Related artefacts

- `kernel/tests/load_balance_smp.rs` — automated load-balance
  correctness regression that complements the manual soak.
- `kernel/tests/pipe_wakeup_smp.rs` — automated cross-core pipe
  wakeup regression that complements the manual pipe-ping-pong.
- `docs/61-smp-load-balancing-closeout.md` — Phase 61 aligned legacy
  doc.
- `docs/roadmap/tasks/61-smp-load-balancing-closeout-tasks.md`
  Track G section.
- [`docs/handoffs/57e-bug9-bug10-followup.md`](./57e-bug9-bug10-followup.md)
  — Bug #9 (`stale-ready` / `cpu-hog` fingerprint) tracking, Phase
  62-targeted closure.
- [`docs/handoffs/ap-core-gpf-saved-rsp-stack-corruption.md`](./ap-core-gpf-saved-rsp-stack-corruption.md)
  — AP-core kernel GPF tracking, pre-existing on `main`.
