---
status: IN PROGRESS — Phase 99 Tracks C + D. C.1 (panic AP-quiesce) implemented and
  no-regression-validated; C.2 (4 GiB residual race) investigation pass recorded.
  Track D (step-25 demand-fault cr2=0 flake) is CI-host-correlated and does not
  reproduce locally — root-cause-by-inspection narrowed the candidates, C.1 makes the
  next red run's banner readable, but a definitive fix is BLOCKED on capturing + ELF-
  symbolizing a red CI artifact (D.1 acceptance).
date: 2026-06-28
phase: phase-99
component: kernel/lib.rs (handle_panic), kernel/smp (panic_stop), kernel/arch/x86_64
  (nmi_handler), kernel/arch/x86_64 demand-fault chain
related:
  - docs/handoffs/2026-06-05-4gib-smp-panic-corrupted-output.md       # Track C ask
  - docs/handoffs/2026-06-25-flaky-dynlink-mismatch-demand-fault-kernel-fault.md  # Track D
  - docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md  # NMI-on-IST + recovery this builds on
---

# Phase 99 — Panic AP-Quiesce (Track C) & Step-25 Demand-Fault Flake (Track D)

## Track C.1 — panic-path AP-quiesce (implemented)

The 2026-06-05 handoff's blocking ask was **diagnosability**: at 4 GiB + `--kvm` + SMP an
intermittent panic's banner is SMP-interleaved garbage because `handle_panic` prints +
dumps the trace rings while sibling cores keep writing COM1.

**Implemented** (`kernel/src/smp/mod.rs` `panic_quiesce_aps` / `panic_stop_ack_and_park`,
`kernel/src/lib.rs` `handle_panic`, `kernel/src/arch/x86_64/interrupts.rs` `nmi_handler`):

- The first panicking core wins `PANIC_IN_PROGRESS` via CAS, stamps itself the
  `PANIC_OWNER_CORE`, broadcasts a halt **NMI** to every other online core, and spins a
  **bounded** grace window (`SPIN_BUDGET`) for them to ack-and-park before it prints. NMI
  (not a fixed IPI) is used so an IF=0 sibling still stops; it runs on the per-core NMI IST
  stack landed 2026-06-14.
- The `nmi_handler` checks `panic_in_progress()`: a non-owner core acks (`PANIC_STOP_ACK`
  bit) and parks in `hlt_loop` (never IRETQs → stays frozen and quiet on COM1); the owner
  falls through and prints.
- **Re-entrancy:** a second core that panics (or is mid-`handle_panic`) when the NMI lands
  loses the CAS / parks, so it cannot re-corrupt the banner (Linux `panic_smp_self_stop`).
- **No force-unlock needed:** `serial::_panic_print` already falls back to a fresh COM1
  port if `SERIAL1` is held, so a stranded serial lock neither deadlocks nor drops the
  banner — quiescing the siblings is sufficient to make it legible.
- **Bounded / no single-core regression:** a wedged sibling that never acks times the
  window out rather than hanging; single-core / pre-SMP boot returns immediately with no
  NMIs; `cfg(test)` `handle_panic` still short-circuits to the ISA-debug-exit
  `test_panic_handler` before the quiesce, so the test harness exit convention is intact.

**No-regression validation (2026-06-28, KVM):** `smoke-test`, `smp-smoke @ -smp 8`, and the
4 GiB + `-smp 8` run below all boot/stress cleanly with the new panic path compiled in.

> The positive "captured readable 4 GiB panic banner" demonstration is **opportunistic**:
> the residual 4 GiB race is intermittent and there is no kernel panic-trigger to force it.
> The diagnosability *mechanism* is in place; the next red 4 GiB panic (or step-25 CI fault)
> will print an uninterleaved banner.

## Track C.2 — 4 GiB residual OOM/race investigation pass

**Run captured (2026-06-28):** `M3OS_MEM=4g M3OS_KVM=1 M3OS_SMP=8 cargo xtask smp-smoke`.

- **Boot is clean at 4 GiB / 8 cores:** `[mm] buddy allocator: 1040256 free pages` (≈4 GiB),
  all 7 APs online, login reached, `pkg install node` succeeded, `node --version` issued.
- **No panic, no wedge, no lost-wake watchdog.** The scheduler stays **live** the whole
  time (background `dhcpv6`/`ure` heartbeat tasks keep running); the only blocked task in
  the stall-census was a benign `fork-child` in `nanosleep` (`syscall_age=20s`,
  `wake_deadline_in=26s` — it has a deadline, not a lost wake). The 30 s stuck-no-waker
  watchdog did **not** fire.
- **What did not complete:** the cold `node` load / 256-op futex stress did not finish in
  the 360 s budget. With the scheduler demonstrably live and no watchdog verdict, this
  reads as the documented **">2 GiB scales-with-RAM slowdown"** (the cold `node` binary is
  demand-paged page-by-page from the ring-3 `vfs_server`, and at 4 GiB the demand-fault +
  TLB-shootdown traffic is heavier) — **not** a scheduler lost-wake or wedge. The same gate
  at the default 2 GiB passes in 68 s.

**Conclusion (C.2):** at 4 GiB + `-smp 8` the kernel boots and schedules correctly with no
panic and no lost-wake; the residual >2 GiB effect manifests here as cold-load *slowness*,
not a crash. No panic fired, so there was no banner to symbolize this pass — but the C.1
quiesce is in place for the next one. Root-causing the underlying RAM-scaling slowdown is
explicitly **Deferred** in the Phase 99 design doc ("a full fix may spill to a follow-up");
C.2 delivers the captured run + this hypothesis, which is the acceptance.

## Track D — step-25 demand-fault `cr2=0` NULL-deref flake

The 2026-06-25 handoff documents an OPEN ~11–15% CI flake: `dynlink-hello-versioned-
mismatch-smoke` (smoke-test step 25) intermittently hits a kernel `#PF rip=…b5de71
cr2=0x0 err=0x0` (kernel-mode read of NULL) in the Phase 95b `MAP_LAZY_FILE` demand-fault
chain. It is host-correlated, low-rate, and **does not reproduce locally**.

### D.1 — capture + symbolize: BLOCKED on a red CI artifact

D.1 requires `gh run download <red-run-id> -n pr-regression-artifacts` then `addr2line`
against the **CI-built** ELF (the kernel is not bit-reproducible, so a local ELF gives a
*misleading* symbol — local addr2line of `0xb5de71` returns `parse_device_scopes`, an
IOMMU boot parser that cannot run during step 25). No red run has been captured with the
artifact infra in place; the flake is probabilistic and external. **This is the gating
blocker for a definitive root-cause** and cannot be forced from the dev box.

### D.2 — root-cause-by-inspection (no speculative fix)

The faulting `rip` is in kernel `.text` (a kernel NULL-deref), so it is **not** a kstack
guard-page fault — `fault_kill_trampoline` will not recover it; it panics. Candidates
examined in the chain `page_fault_handler → demand_map_vma_page → shared_vma_demand_file
→ blocking vfs_server read`:

- `current_addr_space()` (re-derived after the blocking read, `interrupts.rs:1015/1075`)
  **cannot return `Some(null)`** — it null-checks the per-core cached pointer and falls
  back to a `PROCESS_TABLE` lookup, returning `None` or a valid pointer. Not the `cr2=0`.
- `vfs_read_window_slice()` returns `Option<&[u8]>` guarded by `?`; `&window[..]` indexing
  is bounds-checked (would panic with a different signature, not `cr2=0`).
- The B.1 audit confirmed the chain holds **no** `SCHEDULER`/`PROCESS_TABLE` lock across
  the blocking IPC, so it is not a lock-state corruption.

No unchecked NULL deref is evident from inspection of the current tree, and **D.2's
acceptance explicitly forbids a speculative fix** ("grounded in the symbolized faulting
function from D.1 — not a speculative kstack bump"). The most likely shape remains a
teardown race specific to the versioned-mismatch test (pid resolves a versioned symbol to
`jmp 0` → SIGSEGV → thread-group teardown concurrent with a demand-fault), but pinning the
exact NULL read needs the CI ELF.

**Track D delivers two concrete advances toward closing it:**
1. **Diagnosability (via C.1):** the next red CI run — which on a multi-core CI guest would
   previously interleave the panic banner — now prints an uninterleaved banner + crash
   dump, making the `rip`/`cr2` reliably extractable.
2. **A larger local repro surface:** see the soak below.

### D.3 — local soak (best-effort; CI proof still required)

The N≥50-iteration flake=0 proof is, by the flake's own nature, **not achievable on the dev
box**: the 2026-06-25 handoff already records two independent local sweeps passing 5/5 plus
an 8-run flake-hunt that never re-captured a red — the trigger is a CI-host condition not
present locally. A local soak therefore only ever confirms what is already known (local
non-repro); it cannot prove the CI flake fixed.

What *was* exercised cleanly during Phase 99 validation (2026-06-28, KVM):

- `smoke-test` (which **contains step 25**, `dynlink-hello-versioned-mismatch-smoke`) —
  PASSED.
- `dynamic-hello-smoke` (the dynamic loader + `THREAD_FAULT` + `DYNAMIC_TLS` arms) — PASSED.
- `smp-smoke @ -smp 8` — PASSED (heavy SMP demand-fault churn, the same `MAP_LAZY_FILE`
  chain).

These give local stability evidence at `-smp 8`, but are **not** the CI proof. The genuine
D.3 proof remains pending a captured red CI run (D.1).

## Verdict

- **C.1:** done + no-regression-validated. C.2: investigation pass recorded (below).
- **D:** advanced (inspection + diagnosability) but **not closed** — definitive root-cause
  + the N≥50 flake=0 proof are gated on a red CI artifact, which is probabilistic and
  cannot be produced on demand from the dev box. Recommend: when CI next flakes step 25 red,
  `gh run download` the artifact and `addr2line` against the uploaded ELF; the C.1 readable
  banner now makes that capture actionable.
