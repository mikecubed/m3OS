# Handoff: IPC notification-wake race + enabling QEMU gates in CI

**Status:** Open — investigation complete, fix not started
**Date:** 2026-06-03
**Author:** handoff from a CI-reliability investigation (see PR #215 trial)
**Related:** PR #215 (trial measurement workflow), PR #214 (Phase 82 — adds the
new `ahci-smoke` / `ahci-root-smoke` gates), `.github/workflows/build.yml`,
`.github/workflows/pr.yml`, `.github/workflows/nightly-stress.yml`

---

## TL;DR

We want the headless QEMU smoke gates to run reliably enough to **gate PRs**
(today `pr.yml` runs only `cargo xtask check`; the QEMU gates run post-merge in
`build.yml` and nightly).

The base gates are now reliable (the latest nightly on `main` passes), **but one
real bug blocks the rest**: an IPC notification-wake race that hangs the **NVMe
`device-smoke`** path and intermittently the **`ipc-wake`** regression test under
slow **TCG** timing (GitHub runners have no KVM). Signature:

```
[WARN] [sched] task pid=13 name=fork-child state=BlockedOnNotif stuck-since=119443ms (no waker registered)
```

Two work items:

- **Part A (PRIMARY): fix the notification-wake race.** It is *not* a timeout
  problem and *not* IOMMU-specific (see data). Fixing it should make both the
  NVMe `device-smoke` and the `ipc-wake` regression test reliable.
- **Part B (NOTES, do after A): enable the gates on the PR check.** The reliable
  subset can be enabled immediately; the full suite after Part A lands.

---

## Evidence (the measurement behind this handoff)

Trial workflow `QEMU Gate Validation` (PR #215, `.github/workflows/qemu-gate-validation.yml`)
ran each gate as an independent matrix leg (`fail-fast: false`) with **generous**
timeouts, **3× in real CI** (TCG, no KVM):

| Gate | CI pass rate | Notes |
|---|---|---|
| `smoke-test` (600s) | **3/3** ✅ | reliable |
| `device-smoke e1000 --iommu` (180s) | **3/3** ✅ | reliable — **IOMMU itself is fine** |
| `regression` (180s) | **2/3** ⚠️ | the 1 failure was `ipc-wake` (same root cause as A) |
| `device-smoke nvme` (no IOMMU, 150s) | **1/3** ❌ | fails *without* IOMMU too |
| `device-smoke nvme --iommu` (180s) | **0/3** ❌ | always fails |

Corroborated by **4 independent reproductions**, all the same
`BlockedOnNotif … no waker registered` signature, all independent of IOMMU:
CI nvme+iommu (0/3), CI nvme no-iommu (2/3 fail), local nvme+iommu (180s timeout,
hung at ~119s), local nvme no-iommu (hung at ~119s).

Key inferences:
- **IOMMU is not the trigger** (`e1000 --iommu` is 3/3). The NVMe `device-smoke`
  boot path is.
- **Not a timeout-headroom problem**: local runs with a 180s timeout still hung
  at ~119s and never woke. More timeout will not fix it.
- The `regression` flake (`ipc-wake: FAIL: timeout waiting for 'm3OS login:' at
  step 3 (wait for login prompt after boot settle)`) shares the wake signature,
  so A almost certainly fixes both.

### Reproduce locally (no KVM = TCG, like CI)

```bash
# Fails: forked child wedges in BlockedOnNotif, watchdog logs "no waker registered"
cargo xtask device-smoke --device nvme --iommu --timeout 180
cargo xtask device-smoke --device nvme        --timeout 180   # also fails (not IOMMU)

# Passes (control):
cargo xtask device-smoke --device e1000 --iommu --timeout 180
cargo xtask smoke-test --timeout 600

# The shared-root-cause regression test (flaky on the login-prompt wait):
cargo xtask regression --test ipc-wake --timeout 180
```

A fresh worktree needs `cargo xtask fetch-fonts` first (the data-disk build
refuses without the gitignored Nerd Font asset).

---

## Part A — Fix the IPC notification-wake race (PRIMARY)

### Symptom
A forked child task (`pid=13`, name `fork-child`) in the NVMe `device-smoke`
boot parks in `BlockedOnNotif` and is never woken — the scheduler watchdog
escalates to `StuckNoWaker` after the threshold and spams
`(no waker registered)`. The awaited serial sentinel
(`init: driver.registered name=nvme_driver` / `NVME_SMOKE:rw:PASS`, or the login
prompt for `ipc-wake`) never appears, so the gate times out.

### Root-cause hypothesis
A task calls a notification-wait (parks in `BlockedOnNotif`) and the wake/signal
either (a) fires *before* the waiter registers (lost-wakeup window), or (b)
targets the wrong waiter, or (c) the notification's waker bookkeeping is dropped
across `fork`. The "no waker registered" verdict means the watchdog sees a
`Blocked*` task that has **no registered wake source at all** — i.e. nothing will
ever wake it. This is timing-sensitive: it reproduces under slow TCG but is
masked by KVM speed (it does *not* reproduce locally with `--kvm`). Classic
register-then-check vs. check-then-block ordering race in the notification path,
likely interacting with `fork` child setup or driver IRQ-notification wiring on
the NVMe path specifically (e1000 path is clean, so compare the two).

### Code pointers
- **Diagnostic source** (where the WARN fires + the verdict): 
  `kernel/src/task/scheduler.rs:6088` (doc) and `:6210` (the WARN), gated by
  `watchdog_verdict(...)` → `WatchdogVerdict::StuckNoWaker`
  (`kernel-core/src/watchdog_policy.rs:23-34`). Note: the watchdog is the
  *reporter*, not the bug — but its "no waker" branch tells you the waiter has no
  wake source, which is the strongest clue.
- **Notification block/wake path** (the suspect): `kernel/src/ipc/notification.rs`
  — `recv_msg_with_notif` parks waiters in `BlockedOnNotif` (~`:667`), and the
  signal path transitions `BlockedOnNotif → Ready` (`:750`, `:857`). Audit the
  register-waiter vs. check-pending-bits ordering for a lost-wakeup window.
- **State-machine model + host tests:** `kernel-core/src/sched_model.rs`
  (`BlockedOnNotif × wake → Ready`, ~`:670-676`). If the bug is in the model,
  add a failing host test here first (TDD); if it's in the live wiring, the model
  may be correct while the kernel call sites race.
- **Fork child setup** (where `fork-child` comes from): 
  `kernel/src/process/mod.rs` — `fork_child_trampoline` (`:1873`),
  `make_fork_child_context` (`:1748`). Check whether a notification cap / waker
  registered by the parent survives into the child, or whether the child blocks
  on a notification whose waker was never set up for it.
- **History/context:** Phase 57a "scheduler block/wake protocol rewrite" (commit
  `4c72e34`) and 57b "preemption foundation" (`f39ca13`) reworked this exact
  protocol — read those phase docs before touching it.
- **Why NVMe and not e1000:** diff the two `device-smoke` boots — NVMe involves
  an extra forked child + driver self-test round-trip (`NVME_SMOKE:rw:PASS`) that
  e1000's link-only check does not. The race likely lives in that extra
  fork+notification interaction.

### Investigation plan
1. Reproduce under TCG and capture the full serial log + a `dump_dispatch_state()`
   snapshot (the watchdog already calls it; see `scheduler.rs:6200`). Identify
   *which* notification object `pid=13` is parked on and who *should* signal it.
2. Determine if it's a lost-wakeup (signal before register) or a never-registered
   waker (fork drops the cap/waker). Add tracing at the `recv_msg_with_notif`
   park and the signal site.
3. If reproducible in the `kernel-core` `sched_model` (host), write a failing
   property/unit test there first, fix, keep green. Otherwise fix the live call
   site in `kernel/src/ipc/notification.rs` and/or `process/mod.rs`.
4. Re-run the repro matrix below until stable.

### Acceptance criteria
- `cargo xtask device-smoke --device nvme --timeout 120` and
  `--device nvme --iommu --timeout 120` each pass **5/5** consecutive runs under
  TCG (no `--kvm`).
- `cargo xtask regression --test ipc-wake --timeout 120` passes **5/5** under TCG.
- The `(no waker registered)` WARN no longer appears for `fork-child` during the
  NVMe `device-smoke` boot.
- No regression in the e1000 / smoke-test paths, and `cargo xtask check`
  (incl. the `kernel-core` `sched_model` host tests) stays green.

---

## Part B — Enable QEMU gates in CI (NOTES — do after Part A)

### Current CI topology
- `pr.yml` (on `pull_request`): **only `cargo xtask check`** + allocator-loom.
  The QEMU gates were deliberately deferred here ("smoke and regression run on
  developer hardware").
- `build.yml` (on push to `main`): `check` + `smoke-test --timeout 300` +
  `device-smoke nvme/e1000 --iommu --timeout 90` + `regression --timeout 90`.
  **Fails intermittently on the NVMe `device-smoke` lane** (Part A bug); the 90s
  timeout is also tighter than the ~119s hang.
- `nightly-stress.yml`: `smoke-test --timeout 900` + `regression --timeout 360`
  + `stress`. Does **not** run `device-smoke`. Most recent run on `main` passes.

### What to enable, and when
1. **Now (independent of Part A):** add a QEMU job to `pr.yml` that runs only the
   reliable gates: `smoke-test` (use ≥ 400s; CI TCG is slower than dev hardware)
   and optionally `device-smoke --device e1000 --iommu` (3/3 reliable). This gives
   real pre-merge QEMU coverage with negligible flake risk.
2. **`regression` on PRs:** enable it **with the flake-isolation retry** that
   `.githooks/pre-push` already implements (re-run failed tests in isolation;
   treat transient-only failures as flakes). Port that block — see
   `.githooks/pre-push` (the `cargo xtask test` retry ~lines 120-155 and the
   `cargo xtask regression` retry ~lines 172-207). Without the retry it is 2/3.
3. **After Part A lands:** add `device-smoke nvme` (± IOMMU) back to the PR/CI
   gate; bump `build.yml`'s `device-smoke`/`regression` timeouts off 90s.
4. **Phase 82 gates (PR #214):** once #214 merges, add `ahci-smoke` and
   `ahci-root-smoke` to the CI set. They pass locally (8/8 and 6/6) but have not
   yet been measured on CI TCG — give them a generous timeout (≥ 240s) and
   measure a few runs first via the #215-style trial.

### pr.yml change sketch
Mirror `build.yml`'s setup (toolchain, `apt-get install qemu-system-x86 ovmf
musl-tools qemu-utils e2fsprogs`, cargo + TCC caches, `cargo xtask fetch-fonts`),
then a job/steps such as:

```yaml
  qemu-gates:
    runs-on: ubuntu-latest
    steps:
      # ... toolchain + deps + caches + fetch-fonts (copy from build.yml) ...
      - run: |
          rm -f target/x86_64-unknown-none/release/disk.img || true
          cargo xtask smoke-test --timeout 400
      - run: cargo xtask device-smoke --device e1000 --iommu --timeout 180
      # regression: wrap in the pre-push flake-isolation retry, OR defer until Part A
      # device-smoke nvme: ADD ONLY AFTER Part A
```

### Re-running the measurement (PR #215)
The trial workflow supports `workflow_dispatch`:
```bash
gh workflow run qemu-gate-validation.yml --ref ci/qemu-gate-validation
gh run list --workflow=qemu-gate-validation.yml --limit 5
```
PR #215 is **do-not-merge** (measurement harness only). Close it or fold the
reliable-subset job into `pr.yml` once Part A is fixed.

### Gotcha: local git object corruption
This clone is **missing a blob** (`doomgeneric/Makefile`, `503e0dca…`), so
`git push` of a new branch fails with `error: invalid object … Error building
trees`. The #215 workflow file was added via the GitHub Contents API to work
around it. Run `git fsck` / re-fetch (or a fresh clone) to repair before relying
on local pushes for the CI branch.

---

## Appendix — run IDs / logs (2026-06-03)
- PR #215 trial runs: `26872229762` (pull_request), `26872236667`, `26872238438`
  (workflow_dispatch). All under `gh run view <id> --log-failed`.
- `build.yml` failing example (NVMe device-smoke hang): run `26861490792`.
- Most recent nightly (passing, on `main`): `26863778821`.
