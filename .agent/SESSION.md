---
current-task: "PR #114 review resolution — 8 unresolved copilot-reviewer threads on feat/55a-iommu-substrate"
current-phase: "triage-complete"
next-action: "begin fix batch 1 (IOMMU correctness group)"
workspace: "feat/55a-iommu-substrate (PR #114)"
last-updated: "2026-04-19T00:00:00Z"
---

## Review surface

PR #114: feat/55a-iommu-substrate → main, 9 review threads from
copilot-pull-request-reviewer + github-advanced-security (devskim).
1 thread already resolved (devskim TODO). 8 unresolved — all triaged below.

## Decisions

| # | Thread ID | File:Line | Verdict | Action | Notes |
|---|---|---|---|---|---|
| 1 | PRRT_kwDORTRVIM57-9y8 | kernel-core/src/iommu/identity.rs:94 | valid | fix | Gate create_domain + other methods on brought_up; make install_identity_fallback call bring_up first (and the per-slot identity fallback in init()). |
| 2 | PRRT_kwDORTRVIM57_Ggh | kernel/src/iommu/mod.rs | (already resolved/outdated) | skip | devskim TODO warning — already closed. |
| 3 | PRRT_kwDORTRVIM57_P1l | kernel/src/iommu/registry.rs:237 | valid | fix | destroy_domain error path drops DmaDomain without release → debug_assert panic. |
| 4 | PRRT_kwDORTRVIM57_P1t | kernel/src/iommu/fault.rs:59 | valid | fix | Replace spin::Mutex with AtomicPtr — IRQ-path must be lock-free per module contract. |
| 5 | PRRT_kwDORTRVIM57_P10 | kernel/src/net/virtio_net.rs:238 | partially valid | fix scoped | Rename phys_base → bus_base + log field. buf_phys Vec stays (out of scope). |
| 6 | PRRT_kwDORTRVIM57_P12 | kernel/src/blk/virtio_blk.rs:189 | partially valid | fix scoped | Rename phys_base → bus_base + log field. |
| 7 | PRRT_kwDORTRVIM57_P15 | kernel/src/net/e1000.rs:126 | valid | fix | Rename ring_phys → ring_bus (struct field + all uses). |
| 8 | PRRT_kwDORTRVIM57_P17 | xtask/src/main.rs:1691 | valid | fix | --gui + --iommu emits two -machine args → last one wins → pcspk setting lost. Consolidate into one -machine. |
| 9 | PRRT_kwDORTRVIM57_P1- | xtask/src/main.rs:7742 | valid | fix | Test assertion mismatches IOMMU_QEMU_ARGS constant (confirmed failing via cargo test). |

## Files Touched

(read so far)
- kernel-core/src/iommu/identity.rs, kernel-core/src/iommu/contract.rs
- kernel/src/iommu/registry.rs, kernel/src/iommu/mod.rs, kernel/src/iommu/fault.rs, kernel/src/iommu/intel.rs
- kernel/src/net/virtio_net.rs, kernel/src/net/e1000.rs, kernel/src/blk/virtio_blk.rs
- xtask/src/main.rs

## Open Questions

(none — fixes derive from code evidence)

## Blockers

(none)

## Failed Hypotheses

(none)
