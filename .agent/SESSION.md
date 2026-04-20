---
current-task: "PR #116 review resolution — 9 new unresolved copilot threads at commit 7813757 (round 2)"
current-phase: "triage-complete"
next-action: "begin fix batch 1 (e1000+nvme endpoint truncation)"
workspace: "feat/phase-55b-ring-3-driver-host (PR #116)"
last-updated: "2026-04-20T03:50:00Z"
---

## Review surface

PR #116 — round-2 re-review. After the 7813757 fix commit, copilot generated
9 new review threads. Round-1 (11 threads) is already resolved. All 9 new
threads are from `copilot-pull-request-reviewer`; no new devskim items.

## Decisions (round 2 — 9 new unresolved threads)

| Thread ID | File:Line | Verdict | Action |
|---|---|---|---|
| PRRT_kwDORTRVIM58E_aq | userspace/drivers/e1000/src/main.rs:120 | valid — `(ep & u32::MAX as u64) as u32` silently truncates; `u32::try_from` + error is the right guard (same bug in nvme driver too) | fix (both e1000 + nvme) |
| PRRT_kwDORTRVIM58E_a8 | kernel-core/src/device_host/syscalls.rs:34 | valid — doc shows `(dev_cap, vector_hint)` but dispatcher passes 3 args | fix (doc only) |
| PRRT_kwDORTRVIM58E_bG | kernel/src/syscall/mod.rs:9 | valid — module doc says only SYS_DEVICE_CLAIM is routed; dispatcher now routes all 5 | fix (doc only) |
| PRRT_kwDORTRVIM58E_bK | kernel/src/net/remote.rs:259 | valid — drain-before-task-check path silently drops frames if `current_task_id()` returns None | fix (move check above drain) |
| PRRT_kwDORTRVIM58E_bR | kernel/src/net/remote.rs:290 | duplicate of E_bK — same function, same concern | fix (covered by one change) |
| PRRT_kwDORTRVIM58E_bU | kernel/src/pci/bar.rs:532 | partially valid — raw `0x3` and `1` are undocumented magic; the existing comment already concedes a future central constant; local named constants are a low-risk readability win | fix (local named constants only; defer cross-crate centralization per existing comment) |
| PRRT_kwDORTRVIM58E_bZ | kernel/src/net/tcp.rs:346 | valid — one-line doc says "resets retransmit timers" but the body transitions connections to Closed | fix (doc only) |
| PRRT_kwDORTRVIM58E_bf | kernel/src/net/tcp.rs:353 | duplicate of E_bZ — same doc comment block | fix (covered by one change) |
| PRRT_kwDORTRVIM58E_bi | kernel/src/net/tcp.rs:372 | duplicate of E_bZ — same doc comment block | fix (covered by one change) |

## Files Touched

(round-2 fix scope)
- userspace/drivers/e1000/src/main.rs
- userspace/drivers/nvme/src/main.rs
- kernel-core/src/device_host/syscalls.rs
- kernel/src/syscall/mod.rs
- kernel/src/net/remote.rs
- kernel/src/pci/bar.rs
- kernel/src/net/tcp.rs

## Open Questions

(none — all 9 triaged with evidence verdicts)

## Blockers

(none)

## Failed Hypotheses

(none)
