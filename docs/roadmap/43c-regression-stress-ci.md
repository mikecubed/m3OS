# Phase 43c - Regression, Stress, and CI Harness

**Status:** Complete
**Source Ref:** phase-43c
**Depends on:** Phase 43a (Crash Diagnostics) ✅, Phase 43b (Kernel Trace Ring) ✅
**Builds on:** Extends the xtask build system from Phase 01; uses crash diagnostics from Phase 43a and trace ring dumps from Phase 43b for meaningful failure artifacts; exercises fork/IPC/PTY paths from Phases 11/06/29
**Primary Components:** xtask regression/stress commands, CI workflows, host-side proptest/loom integration, artifact capture

## Milestone Goal

A layered test infrastructure that catches SMP race conditions through focused
QEMU regression tests, looped stress tests with automatic artifact capture,
seed-based timing randomization for reproducibility, host-side property and
concurrency testing for extracted scheduler/fork/IPC models, and CI tier mapping
that runs regressions on every PR and stress tests nightly.

## Why This Phase Exists

Phases 43a and 43b added crash diagnostics and trace rings, but without
automated testing infrastructure these tools only help during manual debugging.
SMP race conditions are timing-dependent and may not reproduce on every run.
A systematic test infrastructure that runs focused regression tests on every PR
and long-running stress tests nightly catches intermittent bugs before they
reach production.

## Learning Goals

- Understand tiered testing strategies (host tests, regression, smoke, stress)
- Understand property-based testing with proptest for state machine invariants
- Understand concurrency model checking with loom for IPC protocols
- Understand CI pipeline design with artifact capture for post-mortem analysis
- Understand seed-based reproducibility for timing-dependent failures

## Feature Scope

### Regression Test Framework

`cargo xtask regression` runs focused QEMU-based tests that exercise specific
SMP-sensitive code paths. Each test boots the OS, logs in via the serial
expect engine, runs a guest-side binary, and checks for pass/fail markers.

### Stress Test Framework

`cargo xtask stress` repeats regression scenarios with configurable iteration
count and seed-based timing randomization. Each iteration launches a fresh
QEMU instance for clean state.

### Artifact Capture

On failure, serial logs are saved to `target/regression/<name>/serial.log`
or `target/stress/<name>/<iteration>/serial.log`. Trace ring dumps from
Phase 43b are extracted into separate `trace.log` files.

### Host-Side Property Testing

Proptest-based property tests verify scheduler and fork state machine
invariants: RSP nonzero on dispatch, no dual-enqueue, wake-on-running is
a no-op, block-then-wake produces Ready state.

### Host-Side Concurrency Testing

Loom-based concurrency tests verify IPC send/recv and call/reply protocols
under all possible thread interleavings: no lost messages, no missed wakeups.

### CI Integration

Regression tests run on every PR (< 120s added). Nightly stress tests run
50 iterations of the SSH overlap scenario with artifact upload on failure.

## Important Components and How They Work

### `cargo xtask regression` (`xtask/src/main.rs`)

Dispatches to `cmd_regression()` which builds the kernel, creates a UEFI image,
and runs each registered test. Tests use the smoke-script expect engine
(`run_smoke_steps_with_capture`) to interact with the guest via serial. The
engine detects kernel panics/faults in serial output as immediate failures.

Registered regressions: `fork-overlap` (runs fork-test twice), `ipc-wake`
(runs unix-socket-test), `pty-overlap` (runs pty-test).

### `cargo xtask stress` (`xtask/src/main.rs`)

Repeats a scenario N times with seed-based iteration. Each iteration launches
a fresh QEMU instance. Stops on first failure by default (`--continue-on-failure`
to keep going). Prints seed at start for reproducibility.

### Proptest Integration (`kernel-core/tests/scheduler_props.rs`, `fork_props.rs`)

Property tests use extracted state machine models (not the full kernel). The
scheduler model has 2 cores and configurable tasks. The fork model uses a
VecDeque matching the kernel's queue.

### Loom Integration (`kernel-core/tests/ipc_loom.rs`)

Loom tests are gated behind `#[cfg(loom)]`. Run with:
`RUSTFLAGS="--cfg loom" cargo test -p kernel-core --test ipc_loom`

## How This Builds on Earlier Phases

- Extends the xtask build system from Phase 01 with `regression` and `stress` subcommands
- Reuses the smoke-test serial expect engine for guest interaction
- Uses crash diagnostics from Phase 43a to detect kernel panics in serial output
- Extracts trace ring dumps from Phase 43b into artifact files
- Exercises fork paths from Phase 11, IPC from Phase 06, PTY from Phase 29

## Implementation Outline

1. Add `regression` and `stress` subcommands to xtask
2. Implement regression test framework with pass/fail detection
3. Register fork-overlap, ipc-wake, and pty-overlap regressions
4. Implement stress test loop with seed and artifact capture
5. Add proptest dev-dependency and scheduler/fork property tests
6. Add loom dev-dependency and IPC concurrency tests
7. Add regression step to PR and build CI workflows
8. Create nightly stress workflow with artifact upload
9. Create documentation

## Acceptance Criteria

- `cargo xtask check` passes
- `cargo xtask test` passes
- `cargo test -p kernel-core` passes including proptest tests
- `cargo xtask regression` is a valid subcommand
- `cargo xtask stress --test fork-overlap --iterations 1` is a valid subcommand
- CI workflows updated with regression step and nightly stress schedule

## Companion Task List

- [Phase 43c Task List](./tasks/43c-regression-stress-ci-tasks.md)

## How Real OS Implementations Differ

- Linux uses KernelCI, syzkaller (coverage-guided syscall fuzzing), and kselftest
  for automated testing at scale
- Production stress testing uses thousands of iterations across diverse hardware
- CI-integrated fuzz testing (OSS-Fuzz) continuously explores syscall paths
- Lock ordering validators (lockdep) run in-kernel rather than as host models
- Hardware-specific timing variation (NUMA, different CPU vendors) affects
  which interleavings are exercised

## Deferred Until Later

- Full syzkaller-style syscall fuzzing
- QEMU gdbstub integration in xtask
- lockdep-lite integration with stress tests
- Performance regression benchmarks
- Coverage-guided fuzzing for kernel syscall paths
- Kernel watchdog timer (per-core, dumps on stuck scheduler)
