current-task: Resolve the open PR #105 smoke-test ordering review comment on feature branch feat/53-headless-hardening
current-phase: fix-batch-1-complete
next-action: run post-fix validation and publish the branch update
workspace: PR #105 / feat/53-headless-hardening
last-updated: 2026-04-14T04:48:34+00:00

## Decisions

- discussion_r3077159872 fixed in `docs/roadmap/53-headless-hardening.md` by updating the published smoke-test sequence to match the actual `xtask` smoke-test order.
- Related Phase 53 gate docs in `docs/43c-regression-stress-ci.md` were updated so the published `cargo xtask check` contract matches the current CI entrypoint, including the `passwd_host` host-test regression.
- `xtask/src/main.rs` now runs `cargo test -p passwd --target x86_64-unknown-linux-gnu --no-default-features --features host-tests --test passwd_host` inside `cargo xtask check`.

## Files Touched

- .agent/SESSION.md
- docs/43c-regression-stress-ci.md
- docs/roadmap/53-headless-hardening.md
- xtask/src/main.rs

## Open Questions

- none

## Blockers

- none

## Failed Hypotheses

- none
