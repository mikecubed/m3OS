Track: phase52d-track-b
Tasks: B.1, B.2, B.3
Files: kernel/src/task/mod.rs, kernel/src/task/scheduler.rs, kernel/src/arch/x86_64/syscall/mod.rs, kernel/src/mm/user_mem.rs, kernel/src/arch/x86_64/interrupts.rs, kernel/src/mm/paging.rs
Dependencies: none
Validation: cargo xtask check; cargo xtask test --timeout 120
Work surface: /home/mikecubed/projects/wt-phase-52d-b (branch feat/phase-52d-track-b)
State: blocked
Validation outcome: pass
Unresolved issues:
- `CLONE_THREAD` shares CR3 but still copies `brk_current`, `mmap_next`, and `vma_tree` by value, so sibling threads can observe stale mapping metadata for the same address space.
- `map_current_user_page()` publishes new hierarchy levels without shared-address-space serialization, so concurrent faults in one CR3 can race on SMP.
- file-backed `mmap` rollback still frees frames without unmapping already-installed PTEs on partial failure.
Rescue history:
- initial implementation completed and passed validation | review/fix loops resolved generation coverage, scheduler lock ordering, partial `brk` growth, and `PROT_NONE` guard-page lifecycle | final review exposed broader shared-address-space and rollback boundary violations beyond the original Track B scope | blocked pending re-scope | attempt 1
Next action: Re-scope Track B (or split a follow-on track) for shared-address-space metadata ownership, synchronized current-CR3 page-table publication, and transactional file-backed `mmap` rollback before merging.
Revision rounds: 8
Summary: The current Track B diff passes `cargo xtask check` and `cargo xtask test --timeout 120`, and it fixes the single-threaded/current-owner return-state and generation-tracking gaps. It is not merge-ready because final review found cross-thread address-space ownership bugs and a file-backed rollback hole outside the original boundary.
Follow-ups: Track C and Track E remain gated by Track B. If Track B is left blocked, Track D is the only next-ready implementation track.
