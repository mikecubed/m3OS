---
status: OPEN — flaky, not yet root-caused. CI-only kernel fault in the
  `dynlink-hello-versioned-mismatch-smoke` smoke step (step 25). Does NOT
  reproduce locally. Symbolization infra is deployed (pr.yml uploads the kernel
  ELF + serial log on failure) but a red run has not yet been captured WITH that
  infra in place. CI is green on HEAD; the flake is low-rate (~11–15%) and
  host-correlated, so the gate passes most of the time.
branch: feat/phase-95b-on-device-rustc (PR #265)
surfaced-by: PR #265 review-resolution (commit 9bdbf879's `checked_add` hardening)
---

# Flaky CI kernel fault — `dynlink-hello-versioned-mismatch-smoke` (Phase 95b demand-fault path)

## Symptom

The QEMU smoke gate's **step 25** (`dynlink-hello-versioned-mismatch-smoke`,
D2.2 fallback + D2.3 strict mode) intermittently **times out** waiting for
`SMOKE:dynlink-hello-versioned-mismatch-smoke:PASS`. The test runs the ld-musl
loader's versioned-symbol **mismatch** path (pid ~71 runs
`/bin/dynlink_hello_versioned_mismatch`). On a failing run the guest hits a
**kernel fault** servicing that process, the PASS sentinel never prints, and the
step times out (all 3 internal smoke-test retries fail on a "bad" runner).

Two different manifestations were observed, both at kernel `.text` file-vaddr
**~0x b5de71** (PIE; `rip − load_base = file vaddr`), both attributable to pid ~71:

- **Run A (commit 9bdbf879):** `[int] KERNEL STACK OVERFLOW … rip=0x10000b5de71`,
  then `kstack-bt: verdict=LARGE-FRAME chain — only 1 .text frame`. **The bt
  walker self-reported it could not reconstruct the call chain**, so the single
  printed address is unreliable — treat the "stack overflow" label with
  suspicion.
- **Run B (commit d7f46e27):** a clean Ring0 page fault —
  `[int] KERNEL #PF rip=0x8000b5de71 cr2=0x0 err=0x0` =
  **kernel-mode read of address 0x0** (a kernel NULL-pointer dereference). This
  is the more reliable signal (the CPU's faulting `rip`, not a walked stack
  value).

Working hypothesis: a single primary fault in the deep demand-fault path whose
**secondary** manifestation (the crash-diagnostics dumper / fault-kill teardown)
varies — sometimes presenting as a near-guard stack overflow, sometimes as a
NULL deref. Unconfirmed.

## Why it's hard

- **Does NOT reproduce locally.** Two independent local sweeps passed every time
  (5/5 each, plus the always-green local `cargo xtask smoke-test`). The
  triggering host condition is not present on the dev machines tried.
- **Host-correlated, low-rate (~11–15%).** A "bad" CI runner fails all 3 internal
  smoke retries; a "good" runner passes immediately. Re-running on fresh runners
  is the only way to vary it. An 8-run flake-hunt (1 red, then 8 green on the
  SAME kernel binary) did not re-capture a red.
- **Non-reproducible PIE build.** The kernel ELF is not bit-reproducible across
  machines (local `double_fault_handler` file-vaddr ≠ CI's), so the CI crash
  **cannot be symbolized with a locally-built kernel**. Local addr2line of
  0xb5de71 returns `parse_device_scopes` (an IOMMU boot-time parser that cannot
  run during step 25) — i.e. a wrong/misleading symbol. Symbolize ONLY against
  CI's own uploaded ELF.

## Suspected area (unproven)

The deep Phase 95b lazy file-backed demand-fault chain, exercised by the
strict-mode `LD_BIND_NOW` loader eagerly relocating the versioned-mismatch DSO:

```
page_fault_handler  (kernel/src/arch/x86_64/interrupts.rs, ~L890)
  → process::shared_vma_demand_file  (kernel/src/process/mod.rs)
  → blocking vfs_server read / map readahead cluster
```

…plus pid ~71's *expected* `jmp 0` (unresolved versioned symbol) SIGSEGV and the
fault-kill / thread-group teardown that runs afterward. The source itself flags
this path as "already deep on the per-task kernel stack (Area C)".

## How to capture it (infra already in place)

`.github/workflows/pr.yml` (commit 56782c6b) now, on smoke-test **failure**:
- tees the full serial output (incl. `=== CRASH DIAGNOSTICS ===` + trace ring) to
  `target/ci-crash/smoke-test.log`, and
- uploads it **plus the exact CI-built kernel ELF**
  (`target/x86_64-unknown-none/release/kernel`) as the `pr-regression-artifacts`
  artifact.

When step 25 next flakes red:
1. `gh run download <run-id> -n pr-regression-artifacts`.
2. From `smoke-test.log`, take the crash `rip` and the PIE load base (the high
   bits — e.g. `0x8000000000`); `file_vaddr = rip − base`.
3. `addr2line -fiCe kernel <file_vaddr>` (and the trace-ring addresses) against
   **that uploaded ELF** → the real faulting function.
4. Root-cause from there (NULL deref source vs. stack-depth).

## What was tried (and the verdict)

- **`checked_add` mmap-offset hardening (commit 9bdbf879, KEPT):** PR #265 review
  comment A. A behavioral **no-op** for normal offsets (`checked_add ≡
  wrapping_add` with no overflow) — does not change runtime behavior, only kernel
  code layout. Retained (it is a legitimate, if low-severity, hardening).
- **kstack 64→96 KiB bump (commit d7f46e27, REVERTED):** based on the Run A
  "stack overflow" label, which Run B showed to be a misdiagnosis. **Did not
  demonstrably help** — d7f46e27 (96 KiB) itself went red on its first CI run.
  Reverted to avoid carrying +17 MiB of committed kstack RAM on a false premise.

## Open / deferred

- Root-cause the NULL deref (needs a captured red run — see "How to capture it").
- Decide whether the demand-fault path needs genuine depth reduction or whether
  the fault is unrelated to stack depth (Run B suggests the latter).
- Determine whether commit 9bdbf879's layout shift *increased* the flake
  probability vs. a pre-existing flake in the Phase 95b demand-fault code (the
  parent's single green pass is only n=1; the 2 reds clustered at the two new
  kernel binaries is suggestive but not conclusive).
