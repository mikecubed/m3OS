current-task: Resolve the live PR #105 musl placeholder review thread on feature branch feat/53-headless-hardening
current-phase: fix-batch-1-complete
next-action: run post-fix validation and close the GitHub thread
workspace: PR #105 / feat/53-headless-hardening
last-updated: 2026-04-14T06:28:00+00:00

## Decisions

- `discussion_r3077430924` | evidence verdict: valid | concern: correctness + contract mismatch | action: fix. Current `build_musl_rust_bins()` only creates zero-length placeholders when the staged file does not already exist, so a missing musl target can leave stale cached binaries in `target/generated-initrd/` while logging that placeholders are being left in place.
- Discovery brief skipped because the live review batch is already narrow and fully scoped: one open thread on one file (`xtask/src/main.rs`) with no scope ambiguity.
- Fix batch 1 implemented by resetting musl Rust staged initrd files to zero-length placeholders before availability checks/build attempts, plus xtask unit coverage for both create and truncate paths.
- Post-fix validation passed: `cargo test -p xtask --target x86_64-unknown-linux-gnu --quiet` and `cargo xtask check`.
- Independent fix review reported no substantive remaining issues after the warning text was aligned with the new placeholder behavior.

## Files Touched

- .agent/SESSION.md
- xtask/src/main.rs

## Open Questions

- none

## Blockers

- none

## Failed Hypotheses

- none
