current-task: Resolve the remaining PR #105 review and readiness findings on feature branch feat/53-headless-hardening
current-phase: fix-batch-3-complete
next-action: publish the remaining account-parser hardening follow-up, rerun final readiness on the new head, and close the loop
workspace: PR #105 / feat/53-headless-hardening
last-updated: 2026-04-14T06:44:27+00:00

## Decisions

- `discussion_r3077430924` | evidence verdict: valid | concern: correctness + contract mismatch | action: fix. Current `build_musl_rust_bins()` only creates zero-length placeholders when the staged file does not already exist, so a missing musl target can leave stale cached binaries in `target/generated-initrd/` while logging that placeholders are being left in place.
- Discovery brief skipped because the live review batch is already narrow and fully scoped: one open thread on one file (`xtask/src/main.rs`) with no scope ambiguity.
- Fix batch 1 implemented by resetting musl Rust staged initrd files to zero-length placeholders before availability checks/build attempts, plus xtask unit coverage for both create and truncate paths.
- Post-fix validation passed: `cargo test -p xtask --target x86_64-unknown-linux-gnu --quiet` and `cargo xtask check`.
- Independent fix review reported no substantive remaining issues after the warning text was aligned with the new placeholder behavior.
- Final readiness structured review surfaced a valid follow-up fix-now item: passwd/su account parsing used wrapping arithmetic for UID/GID fields, allowing malformed overflowed numeric values to alias low u32 IDs.
- Fix batch 2 hardened numeric parsing in `userspace/passwd/src/lib.rs` and `userspace/su/src/main.rs` to reject malformed or overflowed UID/GID fields via checked arithmetic, and added a host regression test proving an overflowed UID cannot shadow root in `find_username_by_uid`.
- Post-fix validation for batch 2 passed: `cargo test -p passwd --target x86_64-unknown-linux-gnu --no-default-features --features host-tests --test passwd_host --quiet` and `cargo xtask check`.
- Independent fix review reported no substantive remaining issues in the UID/GID parser hardening diff.
- The rerun structured review surfaced the same wrapping UID parser still present in `userspace/login/src/main.rs` and `userspace/id/src/main.rs`; local audit found matching `/etc/passwd` parsing in `userspace/whoami/src/main.rs` and `userspace/adduser/src/main.rs`, so the follow-up was widened to the full remaining account-parser family.
- Fix batch 3 hardened `login`, `id`, `whoami`, and `adduser` to reject malformed or overflowed UID/GID fields via checked arithmetic, and made `adduser` fail cleanly when `max_uid.checked_add(1)` overflows instead of wrapping a new account to UID 0.
- Post-fix validation for batch 3 passed: `cargo xtask check` and the existing passwd host regression remained green.
- Independent review on the final five-file parser batch reported no substantive remaining issues after the `adduser` overflow follow-up fix.

## Files Touched

- .agent/SESSION.md
- userspace/passwd/host-tests/passwd_host.rs
- userspace/passwd/src/lib.rs
- userspace/adduser/src/main.rs
- userspace/id/src/main.rs
- userspace/login/src/main.rs
- userspace/su/src/main.rs
- userspace/whoami/src/main.rs
- xtask/src/main.rs

## Open Questions

- none

## Blockers

- none

## Failed Hypotheses

- none
