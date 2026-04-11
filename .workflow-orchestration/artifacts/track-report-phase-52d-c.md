Track: phase52d-track-c
Tasks: C.1, C.2
Files: userspace/stdin_feeder/src/main.rs, userspace/syscall-lib/src/lib.rs, kernel/src/arch/x86_64/syscall/mod.rs, docs/appendix/copy-to-user-reliability-bug.md
Dependencies: Track B merged
Validation: cargo xtask check; cargo xtask smoke-test --timeout 180
Work surface: /home/mikecubed/projects/wt-phase-52d-c (branch feat/phase-52d-track-c)
State: merged
Validation outcome: `cargo xtask check` pass; no Track C-specific smoke regression observed, but `cargo xtask smoke-test --timeout 180` still times out before the login prompt on both feat/phase-52d-track-c and the unmerged feat/phase-52d baseline
Unresolved issues:
- none
Rescue history:
- initial implementation simplified `stdin_feeder` to a raw-input bridge and documented GET_TERMIOS_* as compatibility-only, but review found canonical-mode applications could still receive literal VT100 escape bytes | targeted resend moved escape-sequence filtering into the kernel-side `LineDiscipline`, added host-side regression coverage, confirmed no userspace termios policy returned, and proved the smoke-test timeout matches the feat/phase-52d baseline | merged into feat/phase-52d | attempt 1
Next action: Launch Track E on the merged feat/phase-52d head.
Revision rounds: 1
Summary: Track C is complete and merged into feat/phase-52d. `stdin_feeder` is now a pure scancode-to-byte bridge, the kernel line discipline owns canonical/raw escape handling, and the register-return termios workaround syscalls remain documented as deprecated compatibility interfaces with no in-tree callers.
Follow-ups: Track E now owns the still-failing pre-login smoke-test baseline and the broader release-gate repair work.
