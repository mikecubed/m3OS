# Phase 43c — Regression, Stress, and CI Harness

**Aligned Roadmap Phase:** Phase 43c
**Status:** Complete
**Source Ref:** phase-43c
**Depends on:** Phase 43a (Crash Diagnostics), Phase 43b (Kernel Trace Ring)

## Overview

Phase 43c adds a layered test infrastructure for catching SMP race conditions.
The system has four tiers: host-side property/concurrency tests (proptest, loom),
QEMU regression tests for specific SMP-sensitive paths, smoke tests for full-boot
validation, and looped stress tests for timing-dependent failures. CI runs
regressions on every PR and stress tests nightly.

## What This Doc Covers

- How to run regression and stress tests
- How to add a new regression test
- How to reproduce a stress failure using seed replay
- How the property and concurrency tests work
- How CI tiers are mapped
- How to read failure artifacts

## Core Implementation

### Test Tier Architecture

```
Host tests (fast, no QEMU):
  cargo test -p kernel-core           # 200+ unit + property tests
  RUSTFLAGS="--cfg loom" cargo test   # loom concurrency tests

Regression tests (QEMU, ~30s each):
  cargo xtask regression              # all registered regressions
  cargo xtask regression --test fork-overlap

Smoke test (QEMU, full boot validation):
  cargo xtask smoke-test              # boot + login + auth + service + storage + log + TCC compile

Stress tests (QEMU, repeated iterations):
  cargo xtask stress --test ssh-overlap --iterations 50
```

### Regression Tests

Each regression test boots the OS in QEMU, logs in via serial, runs a
guest-side binary, and checks for pass/fail. The expect engine detects
kernel panics/faults in serial output as immediate failures.

| Test | Binary | What it exercises |
|---|---|---|
| `fork-overlap` | `/bin/fork-test` | Concurrent fork() from multiple parents |
| `ipc-wake` | `/bin/unix-socket-test` | Overlapping IPC send/recv/reply |
| `pty-overlap` | `/bin/pty-test` | PTY allocation + shell spawning |
| `signal-reset` | `/bin/signal-test` | Exec-time signal disposition reset |
| `exit-group-teardown` | `/bin/thread-test` | exit_group() reaps live spinning sibling |
| `kbd-echo` | shell echo | Keyboard input via serial→TTY→stdin |
| `service-lifecycle` | `/bin/service` | Service list/status in headless workflow |
| `storage-roundtrip` | shell commands | Ext2 write/read/delete round-trip |
| `log-pipeline` | `/bin/logger`, `/bin/grep` | Logger → syslogd → /var/log/messages |
| `security-floor` | `/bin/id`, `/bin/whoami`, `/bin/grep` | Phase 48 shadow auth, credential transition, hash format |

### Stress Tests

`cargo xtask stress` repeats a scenario N times. Each iteration launches a
fresh QEMU instance. Stops on first failure unless `--continue-on-failure`.

| Test | What it exercises |
|---|---|
| `fork-overlap` | Repeated fork-test runs |
| `pty-overlap` | Repeated PTY test runs |
| `ssh-overlap` | Fork + PTY tests back-to-back |

### Seed-Based Reproducibility

Every stress run prints its seed at startup:

```
stress: test=ssh-overlap iterations=50 seed=1712345678 timeout=90s
```

To reproduce a failure:

```
cargo xtask stress --test ssh-overlap --seed 1712345678 --iterations 1
```

### Artifact Capture

Serial logs are saved on every run (pass or fail). Trace ring dumps
are extracted into separate files when a kernel crash is detected:

```
target/regression/<test-name>/serial.log    # Full serial output
target/regression/<test-name>/trace.log     # Trace ring dump (if present)
target/stress/<test-name>/<iter>/serial.log # Per-iteration serial
target/stress/<test-name>/<iter>/trace.log  # Per-iteration trace dump
```

### Property Tests (proptest)

Scheduler model tests in `kernel-core/tests/scheduler_props.rs`:

| Property | What it verifies |
|---|---|
| `saved_rsp_nonzero_on_ready` | RSP is never zero when task becomes Ready |
| `no_dual_enqueue` | Task never in two cores' run queues simultaneously |
| `wake_running_is_noop` | wake_task on Running is a no-op |
| `block_then_wake_is_ready` | Block + wake produces Ready state |
| `yield_returns_to_queue` | Yield returns task to run queue |

Fork model tests in `kernel-core/tests/fork_props.rs`:

| Property | What it verifies |
|---|---|
| `fifo_ordering` | N push + N pop returns PIDs in FIFO order |
| `no_zero_pid` | Interleaved push/pop never returns PID=0 |
| `context_integrity` | Push/pop round-trip preserves all fields |

### Loom Concurrency Tests

IPC model tests in `kernel-core/tests/ipc_loom.rs` (run with `--cfg loom`):

| Test | What it verifies |
|---|---|
| `send_recv_no_lost_message` | Concurrent send+recv never loses a message |
| `call_reply_always_delivers` | Call+reply always delivers the reply |

### CI Tiers

| Tier | Trigger | Tests | Time |
|---|---|---|---|
| PR | Every pull request | `cargo xtask check` + loom + `smoke-test` + `regression` | ~5-8 min |
| Build | Push to main | Same as PR | ~5-8 min |
| Nightly | 3 AM UTC daily | `cargo xtask stress --test ssh-overlap --iterations 50` | ~60 min |

### Gate Artifact Locations

All automated gate artifacts are produced under `target/` and uploaded as CI
artifacts on failure. This is the single reference for artifact paths:

| Artifact | Path | When produced |
|---|---|---|
| Regression serial logs | `target/regression/<test-name>/serial.log` | Every regression run |
| Regression trace dumps | `target/regression/<test-name>/trace.log` | On kernel crash |
| Stress serial logs | `target/stress/<test-name>/<iter>/serial.log` | Every stress iteration |
| Stress trace dumps | `target/stress/<test-name>/<iter>/trace.log` | On kernel crash |
| CI regression bundle | `regression-artifacts` (GitHub Actions) | On PR/build failure |
| CI stress bundle | `stress-artifacts` (GitHub Actions) | On nightly failure |

## Adding a New Regression Test

1. Create a guest binary (Rust in `userspace/` or C compiled with musl-gcc)
   that prints a clear pass/fail marker to stdout
2. Add a `fn my_test_steps() -> Vec<SmokeStep>` function in `xtask/src/main.rs`
   that boots, logs in, and runs the binary
3. Register it in `regression_tests()` with a name and timeout
4. Verify: `cargo xtask regression --test my-test`

## Key Files

| File | Purpose |
|---|---|
| `xtask/src/main.rs` | `cmd_regression`, `cmd_stress`, test registries |
| `kernel-core/tests/scheduler_props.rs` | Proptest scheduler invariants |
| `kernel-core/tests/fork_props.rs` | Proptest fork queue invariants |
| `kernel-core/tests/ipc_loom.rs` | Loom IPC concurrency tests |
| `.github/workflows/pr.yml` | PR CI with regression step |
| `.github/workflows/build.yml` | Build CI with regression step |
| `.github/workflows/nightly-stress.yml` | Nightly stress workflow |

## How This Phase Differs From Later Testing Work

- This phase adds focused regression and stress tests. It does not add
  coverage-guided fuzzing (syzkaller-style) or hardware diversity testing.
- Property tests use extracted models, not the full kernel. Models must be
  kept in sync with kernel logic manually.
- Stress tests use QEMU with fixed SMP count (4 cores). Real hardware
  introduces additional timing variation.
- Nightly stress is time-bounded (50 iterations). Production testing would
  run thousands of iterations across diverse configurations.

## Related Roadmap Docs

- [Phase 43c task doc](./roadmap/tasks/43c-regression-stress-ci-tasks.md)
- [Phase 43a learning doc](./43a-crash-diagnostics.md)
- [Phase 43b learning doc](./43b-kernel-trace-ring.md)

## Known Limitations

- **Guest-side tests reuse existing binaries** — fork-test, unix-socket-test,
  and pty-test were not designed specifically for regression testing. Dedicated
  regression binaries with tighter concurrency patterns are deferred.
- **No SSH-level stress** — the ssh-overlap stress test exercises fork+PTY paths
  but does not establish actual SSH connections (that would require host-side
  SSH client orchestration).
- **Loom tests are opt-in** — must be run with `RUSTFLAGS="--cfg loom"` since
  loom replaces std threading primitives.
- **No QEMU monitor integration** — crash state (registers, memory) is not
  captured from the QEMU monitor side yet.
