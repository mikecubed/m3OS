# Handoff — `dlopen-test-smoke` intermittent TCG stall (Phase 97 debugging)

**Status:** Superseded by the Phase 97 design + task docs — see
[`docs/roadmap/97-dlopen-smoke-tcg-stall.md`](../roadmap/97-dlopen-smoke-tcg-stall.md)
and [`docs/roadmap/tasks/97-dlopen-smoke-tcg-stall-tasks.md`](../roadmap/tasks/97-dlopen-smoke-tcg-stall-tasks.md).
Tracked as **Phase 97** in the [roadmap README](../roadmap/README.md).
**Created:** 2026-06-26.
**Owner area:** kernel demand-paging / `vfs_server` blocking-read path (Phase 95b `MAP_LAZY_FILE`), the dynamic linker (`ld-musl-x86_64.so.1`), and the `smoke-test` harness.

> **⚠️ Correction (Phase 97 investigation).** The "Leading hypothesis" below — a
> blocking-`vfs_server` demand-read lost-wakeup — is **falsified for this gate**.
> `readelf -d` shows `dlopen_test`'s whole dependency graph (`ld-musl`, `libdl.so`,
> `libhello.so`, `libhello_fini.so`) is **ramdisk-embedded**, so every demand fill is a
> **synchronous in-kernel `copy_from_slice`** (`kernel_read_fd_at`'s `FdBackend::Ramdisk`
> arm, `syscall/mod.rs:12098`) — there is no `call_msg`, no parking, and no `vfs_server`
> reply to lose on this gate's hot path (the blocking `vfs` path is only exercised by
> `/usr` ext2 files: the `rustc`/`clang`/install reads). The leading **surviving** suspect
> is the cross-core **TLB shootdown** that `dlclose`'s `munmap` runs **twice** on the
> `FINI_PENDING`→`PASS` critical path (`unmap_dso` → `sys_munmap` → `sys_linux_munmap` →
> `crate::smp::tlb::tlb_shootdown_range`, `dl.rs:670` / `mod.rs:12820,13006`) under `-smp 4`
> TCG host oversubscription. Read the Phase 97 design doc, not this hypothesis, before fixing.

## Symptom

The always-on `smoke-test` gate (also run by the **pre-push** hook) intermittently
fails at **step 26 — `guest/dlopen-test-smoke`**:

```
step 26 timed out: guest/dlopen-test-smoke: libdl runtime + DT_FINI_ARRAY destructors
expected pattern_a: "SMOKE:dlopen-test-smoke:PASS"
```

It is **intermittent** under plain **TCG**:
- Some runs emit `SMOKE:dlopen-test-smoke:PASS` but **after** the wait window (slow).
- Other runs emit **no dlopen sentinel at all** within the window (stall/no-output) — observed
  even on a **lightly-loaded** host (load ~1.1, 0 other QEMU procs).

It is a **pre-existing** flake (fails on a clean `origin/main` per prior sessions) and is
**unrelated to the stat-identity fix** (`ae01ed4`) that surfaced it: with that fix, smoke steps
1–25 all pass and the runner reaches step 26 — the only remaining failure. Per the Phase 95c
reframe, the underlying slow-VFS is largely a **TCG artifact** (fast under KVM).

## What was ruled out (investigation 2026-06-26)

- **Not the destructor.** `libhello_fini`'s `DT_FINI_ARRAY` entry (`userspace/lib/libhello_fini/hello_fini.c`)
  is a single `write(2)` syscall — there is no slow code in the destructor itself.
- **Not the `dlclose` logic.** `ld-musl-x86_64.so.1/src/dl.rs::dlclose` (line ~606) is clean:
  resolve handle → refcount → `run_destructors_for` → `unmap_dso`. No re-read of the DSO, no
  O(n²), no obvious retry loop.
- **Not a kernel panic.** No `KERNEL PANIC` / `RECURSIVE` / `#PF` / `#DF` lines appear around the
  step in the captured logs.

## Leading hypothesis

`dlopen_test` (`userspace/dlopen_test/dlopen_test.c`) performs **~5 `dlopen`/`dlclose`** ops
(libhello ×2 refcount, libhello_fini, plus negative paths). Each `.so` is mmap'd **file-backed**
and **demand-paged** (Phase 95b `MAP_LAZY_FILE`): every touched code page faults in via a
**blocking `vfs_server` read issued from the page-fault handler**.

Pure per-fault I/O latency does **not** explain a >120 s wait or zero output — the DSOs are tiny
(a few pages each). That points to an **intermittent stall in the blocking page-fault → `vfs_server`
read path** under TCG — most likely a **scheduler/futex lost-wakeup-class** race (the same family
the 95c completion plan flags as the real `RUSTC_OK` blocker, and that `smp-smoke` /
`block_current_until` guard), **not** a logic bug in `dlopen`/the linker.

## Mitigation already applied (`ae01ed4`)

`xtask/src/main.rs`: the `dlopen-test-smoke` `WaitEither` timeout was widened **30 s → 120 s**.
This addresses the *slow-PASS* manifestation but **cannot** fix the *no-output/stall* one.

## Repro

```bash
cargo xtask clean && cargo xtask smoke-test        # TCG; step 26 fails intermittently
# passes far more reliably under KVM (the slow-VFS is a TCG artifact):
M3OS_KVM=1 cargo xtask smoke-test                  # note: KVM has its own pre-existing SMP-race flake
```

The `smoke-test` harness **consumes** matched guest serial via its `wait` matchers, so the
dlopen sentinels are NOT echoed to the captured log on a pass — only build output and the
timed-out step survive. **To see the actual behavior you must run `dlopen_test` in isolation
with full serial capture** (see next steps).

## Next steps (for whoever picks up Phase 97)

1. **Get observability.** Run `/dlopen_test` standalone over `cargo xtask run` (or a dedicated
   QEMU invocation with raw serial passthrough) and capture full serial — determine how far it
   gets: does it print `DLOPEN_TEST:FINI_PENDING`? `LIBHELLO_FINI:RAN`? Then stall, or get
   `fault_kill`'d? This single data point splits "slow" vs "stall/deadlock" vs "userspace fault".
2. **Instrument the blocking demand-read.** Add timing + a lost-wakeup watchdog to the
   page-fault → `vfs_server` blocking read (`kernel/src/arch/x86_64/syscall/mod.rs`
   `demand_read_file_page` / `vfs_service_read`, Phase 95b). Confirm whether a fault thread parks
   `BlockedOnFutex`/blocked and never wakes (cross-reference `block_current_until` / `wake_task_v2`
   and the `smp-smoke` lost-wakeup fixes).
3. **Decide the gate posture.** If it is genuinely a TCG-only performance/stall artifact, consider
   making `dlopen-test-smoke` KVM-gated (skip-with-reason under TCG, like `tls-smoke`/`pku-smoke`)
   rather than always-on under TCG — or keep it always-on once the stall is fixed.

## Cross-references

- Phase 95b — `MAP_LAZY_FILE` + blocking vfs-IPC read from the page-fault handler ([95b](../roadmap/95b-on-device-rustc.md)).
- Phase 95c — VFS/Block-I/O perf; the slow-VFS-is-a-TCG-artifact reframe ([completion plan](2026-06-24-phase-95-completion-plan.md)).
- The `smp-smoke` lost-wakeup / `block_current_until` work ([SMP handoff](2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md)).
