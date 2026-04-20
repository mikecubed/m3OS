# PR #116 review findings

**Review surface:** PR #116 (`feat/phase-55b): ring-3 driver host — NVMe+e1000 extracted, supervised, IOMMU-isolated, v0.55.2`) against `main`

**Current state:** done

**Structured checker:** code-review agent (`pr116-review`) + manual whole-diff review

**File summary:** 57 code, 6 test, 11 config, 12 docs, 0 binary, 4 other

**Validation context:**
- PR checks are all in a terminal state and passing (`check`, `CodeQL`, `devskim`, `Analyze (rust)`, `Analyze (c-cpp)`, `Analyze (actions)`).
- `cargo xtask check` passed locally.
- `cargo test -p kernel-core --target x86_64-unknown-linux-gnu --quiet` passed locally.

## Blockers

1. **`sys_device_claim` is still permissionless**
   - `kernel/src/syscall/device_host.rs:546-557`
   - The new device-host boundary is documented as capability-gated, but the syscall currently allows every userspace task through because the credential gate is a hardcoded `if false`.
   - That means any process that can invoke the syscall can claim PCI devices directly, which defeats the intended ring-3 driver isolation and turns the new syscall surface into a global privilege escalation path.

2. **Capability-table insertion failure drops every device claim for the PID**
   - `kernel/src/syscall/device_host.rs:155-165`
   - `kernel/src/syscall/device_host.rs:603-620`
   - If `scheduler::insert_cap()` fails for one new claim, the unwind path calls `release_for_pid(pid)`, which tears down **all** claims held by that process, not just the one being created.
   - Existing `Capability::Device` handles in the task’s cap table are left behind while their backing registry slots, IOMMU domains, and PCI claims are removed. That creates stale live caps and can revoke unrelated devices from an otherwise healthy driver process.

3. **Block write grant is consumed before restart wait/retry logic**
   - `kernel/src/blk/remote.rs:245-275`
   - `kernel/src/blk/remote.rs:281-290`
   - `write_sectors()` consumes the Phase 50 payload grant before checking whether the driver is already restarting, then retries with the same grant after a restart-related failure.
   - On the timeout path the grant is burned even though no write happened; on the retry path the same grant is reused after it has already been marked consumed. That breaks the single-use grant contract and can turn a legitimate restart retry into a replay failure.

## Fix-now

1. **Driver runtime drops the real bulk length on receive**
   - `kernel/src/ipc/mod.rs:527-529`
   - `userspace/lib/driver_runtime/src/ipc/mod.rs:86-100`
   - `userspace/lib/driver_runtime/src/ipc/mod.rs:180-195`
   - `kernel/src/ipc/mod.rs:589-603`
   - The kernel IPC layer publishes the actual bulk length in `msg.data[1]`, but `RecvFrame` keeps only `data0`, and `SyscallBackend::recv()` always returns a full zero-initialized receive buffer instead of the copied byte count.
   - That means the driver-side block/net helpers cannot distinguish real bytes from zero padding. A truncated or intentionally short direct IPC request to a public driver endpoint can be treated as a full-sized payload, which is especially dangerous on the block-write path because the missing tail becomes zeros rather than an `InvalidRequest`.

## Follow-ups

1. **`driver_runtime::Mmio` advertises an MMIO capability handle it never receives**
   - `userspace/lib/driver_runtime/src/mmio.rs:63-76`
   - `userspace/lib/driver_runtime/src/mmio.rs:98-111`
   - `kernel/src/syscall/device_host.rs:751-807`
   - The kernel returns only `user_va` from `sys_device_mmio_map`, but `Mmio<T>` stores `handle.cap()` (the device cap) in its `cap` field and documents it as the underlying MMIO capability.
   - Nothing in this PR seems to consume that field yet, so I do not think this is an immediate breakage, but the abstraction is already lying about what it owns and will become a real bug as soon as a later track tries to forward or revoke MMIO caps from userspace.

## Existing review comments

- None unresolved.

## Skipped checks

- Prior-learnings lookup: skipped — no repository-local knowledge sink was present.
- Codex/conductor review: not run; used code-review agent plus manual review instead.

## Unresolved questions

- None. The PR title/body and the actual diff match: this branch is clearly the Phase 55b ring-3 driver-host extraction and support plumbing.

## Next action

- Fix the three blockers first, then rerun the final readiness pass. The bulk-length issue should be fixed in the same cycle because it affects the new public driver IPC seam.

## Verdict

- **not ready**

## Verification checklist

- Diff surface is non-empty and validated — **PASS**
- Binary files excluded and noted — **N/A**
- Prior-learnings lookup completed or skipped with reason — **PASS**
- Existing review comments addressed or noted — **PASS**
- Structured review completed — **PASS**
- Final whole-diff review completed — **PASS**
- Output delivered as a durable report — **PASS**

## Readiness gates

- CI state gate — **PASS**
- Review state gate — **PASS**
- Diff integrity gate — **PASS**

## Outcome measures

- `discovery-reuse`: `no`
- `prior-learnings`: `skipped`
- `rescue-attempts`: `0`
- `codex-available`: `no`
- `final-gate-result`: `not-ready`

## Resolution (2026-04-20)

Every item above was triaged as valid and either fixed or addressed as a
follow-up on this branch, bringing the verdict to **resolved**. Fixes were
made together with the Phase 55b adversarial review findings (see
`docs/appendix/phase-55b-adversarial-review.md`); where the two reports
overlapped, a single change resolves both.

### Blockers

1. **`sys_device_claim` is still permissionless — fixed**

   `kernel/src/syscall/device_host.rs`

   Replaced `if false` with a real authorization gate
   (`is_authorized_driver_process`). Only processes whose `exec_path`
   starts with `/drivers/` may claim devices; everything else returns
   `-EACCES`. Same fix closes the adversarial review's critical finding.

2. **Capability-table insertion failure drops every device claim — fixed**

   `kernel/src/syscall/device_host.rs`

   Rollback now calls `DeviceHostRegistry::release_single(pid, key)`,
   which removes only the just-inserted claim. Unrelated claims the
   same PID already holds — and their derived MMIO/DMA/IRQ state —
   survive an ENOMEM on the new claim.

3. **Block write grant consumed before restart wait/retry — fixed**

   `kernel/src/blk/remote.rs`

   `write_sectors` no longer consumes the grant upfront. Instead, the
   consume is done inside `do_write_ipc` on the first attempt; the
   restart-retry path calls `do_write_ipc(..., consume_grant = false)`
   so the same logical write is retried without a spurious
   `GrantReplayed`. A timeout at the entry-level restart wait no
   longer burns the caller's grant.

### Fix-now

1. **Driver runtime drops the real bulk length on receive — fixed**

   `userspace/lib/driver_runtime/src/ipc/mod.rs`

   `SyscallBackend::recv` now truncates the recv buffer to the length
   the kernel published in `msg.data[1]`. `RecvFrame.bulk` therefore
   carries exactly the bytes the sender wrote — short direct IPC
   requests to a public driver endpoint can no longer be misread as
   full-sized zero-padded payloads, which would turn a truncated block
   write into zero bytes on disk.

### Follow-ups

1. **`driver_runtime::Mmio` spurious cap-handle field — addressed**

   `userspace/lib/driver_runtime/src/mmio.rs`

   Renamed the internal field from `cap` to `device_cap`, updated the
   docstring to reflect that it is the `Capability::Device` used at map
   time (the kernel does not issue a separate MMIO cap), and added a
   `device_cap()` accessor. The existing `cap()` method is retained as
   a deprecated alias so in-tree callers still compile. The abstraction
   no longer lies about what it owns.

### Validation

- `cargo xtask check` — clippy clean, rustfmt clean, kernel-core +
  passwd + driver_runtime host tests pass.
- `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` — all
  suites pass.
- `cargo xtask test` (QEMU harness) — in-kernel integration tests pass,
  including the device-host cascade and post-crash restart coverage.
- `cargo xtask image` — release image builds cleanly.

### Final outcome measures

- `final-gate-result`: `resolved`
- `re-review-loops`:
  - blocker-1 (`sys_device_claim`): 0
  - blocker-2 (cap rollback): 0
  - blocker-3 (write grant): 0
  - fix-now (bulk length): 0
  - follow-up (Mmio cap): 0
