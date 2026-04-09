# Worker findings and synthesis

This summary condenses the parallel GPT and Opus investigations that were run
against the dedicated `feat/cow-bug-investigation` worktree.

## Worker summary

| Worker | Focus | Main takeaway |
|---|---|---|
| GPT-5.4 | User-memory path audit | `copy_to_user` is the riskier path than `copy_from_user` because it keeps a live mapper across page-table mutation, writes through the direct map after a one-shot translation, and sits next to multiple local-only invalidation sites. |
| Claude Opus 4.6 | SMP/TLB audit | The strongest TLB-specific clue is the still-present "we're on a single CPU" comment above CoW fault handling, combined with local-only invalidation in `resolve_cow_fault()` and `cow_clone_user_pages()`. |
| GPT-5.3-Codex | Validation audit | The bug is historically validated with high confidence from repo evidence; the current tree's default login flow works after image priming, which points to active symptom masking rather than a proven fix. |
| Claude Opus 4.6 fast | Adversarial review | A pure stale-TLB story does not fully explain the primary `stdin_feeder` case by itself. Frame ABA/reuse and mapper aliasing should stay on the board until single-core or frame-identity evidence narrows it further. |

## Where the workers agree

1. **The bug was real.** Nobody found evidence that the document was fabricated,
   stale, or based on a layout/ABI misunderstanding.
2. **The current branch is masked, not closed.** The active workarounds in
   `stdin_feeder` and `login` are treated as symptom controls rather than a
   root-cause fix.
3. **TLB invalidation is the strongest code-level suspect.** The kernel clearly
   uses SMP shootdown in some user-PTE mutation sites and local-only invalidation
   in others.

## Where the workers disagree or add caution

| Topic | TLB-heavy reading | Counterweight |
|---|---|---|
| Primary explanation | Missing cross-core invalidation after CoW or other user-PTE mutation is the best fit. | The main `stdin_feeder` victim is single-threaded and post-`execve`, so stale-TLB alone is not a slam dunk for every observed failure. |
| Best alternative | Secondary concern is `cow_clone_user_pages()` leaving other threads with stale writable TLB entries. | Frame ABA/reuse and `get_mapper()` aliasing are still credible and explain some symptoms without leaning entirely on TLB state. |
| Most decisive next test | Single-core QEMU or post-write frame readback. | Also log `(vaddr, phys)` pairs and frame reuse to separate stale mapping from stale frame identity. |

## Synthesis

The parallel audits converge on a practical framing:

- Treat the issue as **validated and still open**.
- Keep **TLB invalidation** as the lead hypothesis because the code contains a
  clear single-core assumption and inconsistent SMP handling.
- Do **not** collapse the investigation into TLB alone until one of the following
  lands:
  1. a single-core run that changes the symptom profile,
  2. direct evidence that `copy_to_user` wrote one frame while userspace later
     read another,
  3. direct evidence of frame reuse or mapper-staleness in a failing case.
