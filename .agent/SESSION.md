---
current-task: "PR #113 review resolution — 10 copilot-reviewer threads on feat/55-hardware-substrate"
current-phase: "triage-complete"
next-action: "begin fix batch 1 (ISR/task deadlocks + scheduler race)"
workspace: "feat/55-hardware-substrate (PR #113)"
last-updated: "2026-04-18T16:00:00Z"
---

## Decisions

Triage of 10 copilot-pull-request-reviewer threads on PR #113
(feat/55-hardware-substrate → main).

| # | Thread ID                 | Location                                     | Evidence         | Action   |
|---|---------------------------|----------------------------------------------|------------------|----------|
| 1 | PRRT_kwDORTRVIM578h34     | kernel/src/net/virtio_net.rs:557             | partially valid  | fix      |
| 2 | PRRT_kwDORTRVIM578h4C     | kernel/src/net/e1000.rs:448                  | false positive   | decline  |
| 3 | PRRT_kwDORTRVIM578h4J     | kernel/src/net/virtio_net.rs:454             | valid            | fix      |
| 4 | PRRT_kwDORTRVIM578h4W     | kernel/src/arch/x86_64/interrupts.rs:1045    | valid            | fix      |
| 5 | PRRT_kwDORTRVIM578h4i     | kernel/src/arch/x86_64/interrupts.rs:1061    | valid            | fix      |
| 6 | PRRT_kwDORTRVIM578h42     | xtask/src/main.rs:7494                       | valid            | fix      |
| 7 | PRRT_kwDORTRVIM578h5J     | kernel/src/main.rs:576                       | valid            | fix      |
| 8 | PRRT_kwDORTRVIM578h5T     | kernel/src/net/e1000.rs:309                  | valid            | fix      |
| 9 | PRRT_kwDORTRVIM578h5Y     | kernel/src/pci/bar.rs:499                    | valid            | fix      |
| 10| PRRT_kwDORTRVIM578h5c     | kernel/src/net/virtio_net.rs:843             | valid            | fix      |

Triage notes:
- #1 partial: `read_buffer` alloc must stay inside the IF-off region (copy
  required before descriptor repost), but the redundant header-strip
  `.to_vec()` can move out. Minor fix only.
- #2 decline: `data.to_vec()` at e1000.rs:423 is the one required copy
  before the descriptor is recycled for hardware reuse. Matches the Linux
  e1000 softirq `alloc_skb` pattern. No safe way to move the alloc
  outside the critical section without per-ring pre-allocated swap
  buffers (design change, not a minor fix).

Grouped fix batches (all independent; plan to apply serially in one
branch commit, no parallel-impl needed):
- Batch 1: Comments 3, 4, 5, 8 — AtomicU64 TaskId + without_interrupts.
- Batch 2: Comment 7 — unified any-NIC woken flag.
- Batch 3: Comment 9 — preserve existing PTE flags in ensure_uncacheable.
- Batch 4: Comment 6 — inject NVMe image path into qemu_args_with_devices.
- Batch 5: Comment 10 — remove frame_allocator keep-alive dead code.
- Batch 6: Comment 1 — move redundant to_vec() out of IF-off region.

## Files Touched

Read so far:
- kernel/src/net/virtio_net.rs (lines 1-60, 440-560, 820-845)
- kernel/src/net/e1000.rs (lines 290-449)
- kernel/src/arch/x86_64/interrupts.rs (lines 1000-1100)
- kernel/src/main.rs (lines 540-620)
- kernel/src/pci/bar.rs (lines 460-540)
- xtask/src/main.rs (lines 85-135, 7470-7550)

## Open Questions

- none (all 10 comments triaged)

## Blockers

- none

## Failed Hypotheses

- none
