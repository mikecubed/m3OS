current-task: Resolve the final PR #105 readiness findings on feature branch feat/53-headless-hardening
current-phase: fix-batch-2-validated
next-action: collect final integrated review results, then publish the branch update
workspace: PR #105 / feat/53-headless-hardening
last-updated: 2026-04-14T06:10:00+00:00

## Decisions

- `userspace/login/src/main.rs` now reuses the shared `passwd` shadow rewrite helper so first-boot password setup preserves any existing shadow suffix metadata instead of hardcoding `::::::`.
- `xtask/src/main.rs` now reads `/var/log/messages` directly for smoke/regression log-pipeline checks so the awaited marker cannot be satisfied by echoed `grep` command text.
- `userspace/init/src/main.rs` now records disabled services discovered from dynamic `/etc/services.d/*.conf` scans and appends them to `/var/run/services.status`, not just `KNOWN_CONFIGS`.
- Local validation for this batch passed: `cargo xtask check`, `cargo xtask smoke-test --timeout 300`, and `cargo xtask regression --timeout 90`.

## Files Touched

- .agent/SESSION.md
- Cargo.lock
- userspace/init/src/main.rs
- userspace/login/Cargo.toml
- userspace/login/src/main.rs
- xtask/src/main.rs

## Open Questions

- none

## Blockers

- none

## Failed Hypotheses

- none
