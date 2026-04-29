# Phase 57a — Parallel Implementation Discovery Brief

**Status:** Active
**Source Ref:** `docs/roadmap/tasks/57a-scheduler-rewrite-tasks.md`
**Integration branch:** `feat/57a-scheduler-rewrite`

## Task summary

Rewrite m3OS's task-blocking primitive to a Linux-style single-state-word + condition-recheck protocol with a per-task spinlock. Delete `switching_out` / `wake_after_switch` / `PENDING_SWITCH_OUT[core]`. Restore Phase 56/57 graphical stack on real hardware.

## Task shape

multi-track-batch — 9 tracks (A–I), ~30 sub-tasks, hard dependency graph. Concurrency cap = 2.

## Relevant files

- `kernel/src/task/scheduler.rs` — primary surgery target (~2200 lines, all v1 primitives live here)
- `kernel/src/task/mod.rs` — `Task` struct, where `switching_out`, `wake_after_switch` fields live; new `pi_lock`, `on_cpu`, `TaskBlockState` land here
- `kernel/src/task/wait_queue.rs:56` — kernel-internal v1 caller
- `kernel/src/main.rs:486, :648` — `serial_stdin_feeder_task`, `net_task` v1 callers
- `kernel/src/arch/x86_64/syscall/mod.rs` — IPC/notif/futex/poll/select/epoll/nanosleep syscalls (v1 callers around `:14763`, `:15019`, `:15432`, plus deadline arithmetic at `:14647`, `:14894`, `:15304`)
- `kernel-core/src/sched_model.rs` — new pure-logic state machine (Track A.4)
- `kernel-core/tests/sched_property.rs` — property fuzz harness (Track A.6)
- `kernel-core/Cargo.toml` — already has `proptest` and `loom` dev-deps available
- `userspace/syslogd/src/main.rs:141-216`, `userspace/audio_server/src/main.rs:67` — Track H bug fixes

## Task boundaries

**In scope:** all of Phase 57a Tracks A–H, plus I.2 (SSH soak in QEMU), I.3 (in-QEMU fuzz), I.4 (long soak in QEMU), I.5 (docs).
**Out of scope (cannot run from this session):** I.1 (real-hardware regression test on the user's box). Documented explicitly in the I.1 task report; user runs locally.

## Validation commands

- Host (kernel-core model): `cargo test -p kernel-core`
- Workspace check: `cargo xtask check`
- QEMU integration: `cargo xtask test`
- Headless smoke: `cargo xtask run`
- GUI smoke: `cargo xtask run-gui --fresh`

## Dependencies (track ordering)

Wave 1 (parallel): A.1 (audit) ‖ A.2+A.3 (transition tables, doc-only)
Wave 2 (parallel): A.4+A.5+A.6+A.7 (kernel-core model+tests) ‖ G.3 (multiplier sweep, doc-touching only)
Wave 3 (sequential): B.1→B.2→B.3→B.4 (per-task pi_lock infra)
Wave 4 (parallel): C.1+C.2+C.3 (block primitive, gated) ‖ E.1 (on_cpu)
Wave 5: C.4 (first IPC migration), D.1+D.2 (wake primitive)
Wave 6: D.3+D.4 (notif/scan migrations), G.1+G.2 (diagnostics)
Wave 7 (parallel batches of 2): F.1, F.2, F.3, F.4, F.5, F.6 (call-site migrations)
Wave 8: E.2, E.3, E.4, E.5 (delete v1 fields)
Wave 9: F.7 (delete v1 functions), G.4 (userspace timeout regression)
Wave 10: H.1, H.2, H.3 (secondary bug fixes)
Wave 11: I.2, I.3, I.4 (validation), I.5 (docs)

## Comparison baseline

`main` at `449fc05` (Phase 57a plan). The implementation must not regress the Phase 56 graphical stack or the Phase 57 audio path.

## Open questions

- None at start; questions surface per-track and are resolved in track reports.

## Skip reason for scout agent

Scope is fully enumerated in the source task list (`docs/roadmap/tasks/57a-scheduler-rewrite-tasks.md`) with file paths, symbol names, and acceptance criteria already specified. A separate scout pass would duplicate that artifact. This brief points back to the source.
