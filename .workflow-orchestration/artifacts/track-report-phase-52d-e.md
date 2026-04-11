Track: phase52d-track-e
Tasks: E.1, E.2
Files: xtask/src/main.rs, userspace/fork-test/src/main.rs, userspace/pty-test/src/main.rs, userspace/stdin_feeder/src/main.rs, .github/workflows/pr.yml, .github/workflows/build.yml, docs/roadmap/52d-kernel-completion-and-roadmap-alignment.md
Dependencies: Tracks B, C, and D merged
Validation: cargo xtask check; cargo xtask smoke-test --timeout 180; cargo xtask regression --timeout 90
Work surface: /home/mikecubed/projects/wt-phase-52d-e (branch feat/phase-52d-track-e)
State: active
Validation outcome: pending
Unresolved issues:
- `cargo xtask smoke-test --timeout 180` still times out before the login prompt on the merged feat/phase-52d baseline, so the release-gate harness/runtime path is not yet trustworthy evidence for closing Phase 52d.
Rescue history:
- none
Next action: Diagnose and repair the current pre-login smoke-test stall, then align smoke/regression/CI closure evidence with the 52d acceptance criteria.
Revision rounds: 0
Summary: Track E owns the remaining Phase 52d gate-repair work: make the smoke/regression harness discriminate real kernel/runtime failures from stale expectations, and close the phase against the same gates used in CI.
Follow-ups: Final readiness depends on Track E completion and stable release-gate evidence.
