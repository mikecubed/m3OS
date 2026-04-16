---
current-task: "Resolve PR #108 fourth-round Copilot follow-ups (3 new threads)"
current-phase: "fix-batch-4-complete"
next-action: "push, reply + resolve threads"
workspace: "PR #108 / feat/54-deep-serverization-closure"
last-updated: "2026-04-16T23:10:00Z"
---

## Decisions

Three new review threads triaged as VALID; all fixed on top of commit 1930213.

- PRRT_kwDORTRVIM57lbD7 / endpoint.rs:547 — VALID (correctness, signal
  interaction). `endpoint::reply()` could deliver a real reply into the
  pending_msg slot of a caller that had already been pulled out of its IPC
  wait by `interrupt_ipc_waits`; a later `ipc_call` from the same task would
  consume the stale reply. Fix: during `interrupt_ipc_waits`, revoke any
  `Capability::Reply(target)` caps held by other tasks. The server's
  subsequent `sys_ipc_reply` then fails with `u64::MAX` instead of calling
  `endpoint::reply()`. Added `CapabilityTable::revoke_matching` and
  `scheduler::revoke_reply_caps_for` helpers. Did **not** state-gate
  `endpoint::reply()` itself — that would break the legitimate SMP race
  where the server replies on another CPU before the caller reaches
  `block_current_on_reply_unless_message`.
- PRRT_kwDORTRVIM57lbEG / syscall/mod.rs:516 — VALID (comment drift). The
  comment said Phase 54 routes "read-only /etc/... file opens", but
  `vfs_service_should_route` has no `/etc/` scope check — it accepts any
  ext2-backed regular file when no write / create / exclusive flags are
  set. Fix: rewrite the comment to describe the actual (broader) scope.
- PRRT_kwDORTRVIM57lbEL / udp_protocol.rs:155 — VALID (naming). Test was
  named `max_payload_is_page_aligned` but only asserted 512-byte (block)
  alignment. `NET_UDP_MAX_PAYLOAD` is 4096, so both bounds pass today, but
  the name and the assertion disagreed. Fix: renamed to
  `max_payload_is_block_aligned` to match the enforced invariant (didn't
  strengthen to `% 4096` because the existing 512-alignment is the
  intentional block-size contract).

## Files Touched

- kernel-core/src/ipc/capability.rs
- kernel-core/src/net/udp_protocol.rs
- kernel/src/task/scheduler.rs
- kernel/src/process/mod.rs
- kernel/src/arch/x86_64/syscall/mod.rs
- .agent/SESSION.md

Validation: `cargo xtask check` passes.

## Open Questions

## Blockers

## Failed Hypotheses

- Initial attempt at fix 1 tried to state-gate `endpoint::reply()` on
  `state == BlockedOnReply`. That dropped legitimate replies during the
  SMP race where the server replies on another CPU between the caller's
  `wake_task(receiver)` and `block_current_on_reply_unless_message()`. The
  original code intentionally handled that race by setting pending_msg
  before the caller reaches the block_current check. Reverted that attempt
  and switched to reply-cap revocation instead.
