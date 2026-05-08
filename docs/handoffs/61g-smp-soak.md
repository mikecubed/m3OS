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

## Soak result placeholder

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

## Related artefacts

- `kernel/tests/load_balance_smp.rs` — automated load-balance
  correctness regression that complements the manual soak.
- `kernel/tests/pipe_wakeup_smp.rs` — automated cross-core pipe
  wakeup regression that complements the manual pipe-ping-pong.
- `docs/61-smp-load-balancing-closeout.md` — Phase 61 aligned legacy
  doc.
- `docs/roadmap/tasks/61-smp-load-balancing-closeout-tasks.md`
  Track G section.
