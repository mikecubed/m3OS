Task summary: Start Phase 52d on a dedicated branch based on `feat/phase-52`, using parallel execution only for the dependency-ready tracks: Track A for roadmap audit alignment plus exec-time signal-reset regression coverage, and Track B for task-owned syscall return-state completion plus address-space generation diagnostics.
Task shape: multi-track-batch
Relevant files: docs/roadmap/52d-kernel-completion-and-roadmap-alignment.md, docs/roadmap/tasks/52d-kernel-completion-and-roadmap-alignment-tasks.md, docs/roadmap/52a-kernel-reliability-fixes.md, docs/roadmap/52b-kernel-structural-hardening.md, docs/roadmap/52c-kernel-architecture-evolution.md, docs/roadmap/README.md, userspace/signal-test/signal-test.c, xtask/src/main.rs, kernel/src/task/mod.rs, kernel/src/task/scheduler.rs, kernel/src/arch/x86_64/syscall/mod.rs, kernel/src/mm/mod.rs, kernel/src/mm/user_mem.rs, kernel/src/process/mod.rs, kernel/src/smp/mod.rs
Task boundaries: Track A owns roadmap/docs updates and signal-reset regression coverage; Track B owns syscall/scheduler/memory return-state and generation-tracking work. Track C depends on B, Track D depends on A, and Track E depends on B/C/D. No two active tracks should modify the same worktree or broaden scope beyond those owned files.
Validation commands: cargo xtask check, cargo xtask smoke-test --timeout 180, cargo xtask regression --timeout 90
Dependencies: Track A none; Track B none; Track C -> B; Track D -> A; Track E -> B,C,D
Comparison baseline: branch `feat/phase-52` at commit `ce2a892`; integration branch `feat/phase-52d`; final PR target `feat/phase-52`
Prior-learnings consulted:
- skipped
Open questions: none; the referenced `docs/appendix/roadmap-phase-implement-prompt.md` is absent, so the 52d roadmap and task docs are the active brief
Skip reason: none
