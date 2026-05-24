---
status: fix applied — awaiting host verification on user's Zen 5 / Linux 7.0 box
branch: feat/phase-73-compositor-polish (PR not yet open)
last-known-good-commit: 7b4b65b  # ShootdownIrqWindow lands the root-cause fix
fix-commits:
  # === Session 1 (2026-05-24 AM) — diagnostic / xtask scaffolding ===
  - 352e04c  # xtask: add -m / --memory flag (override QEMU guest RAM)
  - 1784c45  # compositor: invalidate cached arrangement per frame during workspace slide
  - 3b22620  # sched: surface real caller of deep IrqSafeMutex nesting in [preempt-depth]
  - 316d351  # xtask: M3OS_GUI_BACKEND / M3OS_GUI_VGA escape hatches
  # === Session 2 (2026-05-24 PM) — investigation that landed the fix ===
  - 66b1896  # xtask: -m / --memory on smoke-test + regression; centralize M3OS_MEM env
  - b36bcaa  # smp: bounded-spin diagnostics on wait_icr_idle + tlb_shootdown_pending
  - 9bffc1b  # smp: per-core TLB-IPI-serviced counter (recipient-side IPI delivery probe)
  - 1401239  # smp: per-core LAPIC-timer-tick counter (IRQ-delivery vs IPI-dispatch bisector)
  - 595951f  # smp: enable interrupts on APs before spawn_idle_for_core (necessary precondition)
  - 7b4b65b  # smp/tlb: ShootdownIrqWindow — IF=1 across SHOOTDOWN_PENDING spin (ROOT-CAUSE FIX)
date: 2026-05-24
component: kernel/smp (TLB shootdown IF=1 invariant + AP IRQ-enable ordering); secondary xtask
  -m flag plumbing; tertiary userspace/display_server slide rendering (unrelated, fixed in
  session 1)
related:
  - docs/handoffs/2026-05-22-compositor-shm-leak-multi-term-oom.md  # immediate predecessor
  - docs/roadmap/73-compositor-polish.md
ruled-out-hypotheses:
  # All previously-suspected causes that turned out to be wrong:
  - SDL display backend bug (VNC via M3OS_GUI_BACKEND=vnc reproduces the black screen byte-for-byte)
  - AMD AVIC IPI virtualisation regression (disabling avic=0 on user's Zen 5 / Linux 7.0 host had no effect)
  - KVM vs TCG (4 GiB hangs under both)
  - SMP / cross-core IPI delivery in general (M3OS_SMP=1 + KVM + 4 GiB still hangs)
  - Compositor-side animation rendering (workspace slide rendering was a real bug, fixed in 1784c45, but unrelated to the 4 GiB symptom)
  - "preempt-depth 36→260 means deeply nested locks" (false — the warning system's own log path
    recurses through `_kernel_print → DMESG_RING.lock → IrqSafeMutex::lock → preempt_disable_at`,
    inflating depth by ~1 per recursion; real deepest depth is 5)
  - Latent IPI-timing race in wait_icr_idle (the bounded-spin diagnostic in b36bcaa never
    triggered — wait_icr_idle returned cleanly in every reproduction; the IPIs always dispatched
    successfully from the LAPIC)
  - Framebuffer mapped cacheable (sys_framebuffer_mmap missing PCD/PWT) — looked plausible while
    we still thought it was a display-path issue, but `cargo xtask run -m 4g` (no GUI) ALSO hangs
    and `cargo xtask test --kvm -m 4g` mostly fails, so the bug is kernel-side, not display-side.
    Still worth fixing as latent MMIO-correctness hygiene (separate followup, see "Deferred").
  - PCI-hole crossing at 4 GiB triggering kernel paging issues (the bisected threshold was a red
    herring — the true threshold was "enough heap-grow events during AP boot to expose the IF=0
    race", which scales with RAM)
  - "Original author's sandbox passes at 4 GiB so the bug is host-specific" (no — the bug is in
    m3OS; the sandbox just had different host-scheduling that never lined up the IF=0 windows
    long enough to deadlock; the bug was always present, just statistically rare elsewhere)
root-cause:
  # Confirmed by the multi-step diagnostic in commits b36bcaa → 9bffc1b → 1401239:
  - Bug 1 (necessary precondition): APs ran their entire `ap_entry` setup with IF=0. The
    bootloader hands APs to `ap_entry` with IF=0 and nothing in the function explicitly STI'd.
    First STI happened inside `task::run`'s first iteration, which was AFTER
    `spawn_idle_for_core` had already allocated kernel-stack pages and grown the bootstrap
    heap. Fixed in 595951f (`x86_64::instructions::interrupts::enable()` immediately after
    `ap_lapic_init_from` in `kernel/src/smp/boot.rs:447`).
  - Bug 2 (the load-bearing one): `mm::heap::grow_heap` (`kernel/src/mm/heap.rs:817`) takes
    `GROW_HEAP_LOCK` — an `IrqSafeMutex` that CLI's at acquire — and then calls
    `tlb_shootdown_range_kernel` from inside the locked region. The shootdown sender therefore
    spun for acks with IF=0, so it could not service other cores' concurrent shootdown IPIs.
    Once two cores simultaneously grow the bootstrap heap (very common during early SMP init
    at higher guest RAM), each becomes the other's blocked recipient. Decisive evidence: the
    last diagnostic dump showed every recipient core with `tlb-ipi Δ=0` AND `LAPIC-timer Δ=0`
    across the 500 ms sender spin — i.e., every recipient was inside its own IF=0 region for
    the full window. Fixed in 7b4b65b (new `ShootdownIrqWindow` RAII guard in
    `kernel/src/smp/tlb.rs` wraps each of the three shootdown spin paths, forcing IF=1 for
    the spin and restoring the prior state on `Drop`; `preempt_count` is already raised by
    the helpers' own `preempt_disable`, so no task migration possible during the temporary
    IF=1 window).
new-tooling:
  - xtask `-m` / `--memory` on `smoke-test` and `regression` (66b1896). Previously only `run`
    / `run-gui` / `test` accepted the flag. `M3OS_MEM=<spec>` env-var fallback is now
    centralized inside `qemu_args_with_devices_resolved`, so every QEMU launch path
    (audio-smoke, tui-smoke, doom-audio-smoke, etc.) picks it up automatically. Help banner
    updated.
  - Bounded-spin diagnostics in `kernel/src/smp/ipi.rs` (b36bcaa). `wait_icr_idle` panics
    with ICR_LOW / ICR_HIGH / dest-APIC / iteration count if the LAPIC delivery-pending bit
    stays set for >100 ms (100,000× the SDM-spec ~1 µs ceiling). Pre-calibration fallback to
    a ~500M-cycle absolute ceiling.
  - Bounded-spin diagnostics in `kernel/src/smp/tlb.rs` (b36bcaa). All three shootdown
    helpers spin via `wait_for_shootdown_acks_or_panic`, which panics after 500 ms with
    SHOOTDOWN_PENDING vs expected, my_core, range, ICR_LOW, and the recipient set (APIC IDs
    or remote_mask depending on call site). Pre-calibration fallback: 10G cycles.
  - Per-core counter `TLB_IPI_SERVICED[MAX_CORES]` (9bffc1b). Bumped at the very top of
    `handle_tlb_shootdown_ipi`, snapshotted at entry to the shootdown spin and dumped per
    core on timeout — pinpoints whether each recipient's IDT handler fired at all.
  - Per-core counter `TIMER_TICKS_PER_CORE[MAX_CORES]` (1401239). Bumped at the very top of
    both `timer_handler_user` and `timer_handler_kernel`. Snapshotted alongside the IPI
    counter for the timeout dump — discriminates IRQ-delivery-generic blocks (timer Δ=0 too)
    from IPI-vector-specific issues (timer Δ>0 but IPI Δ=0).
---

## Quick-resume checklist (start here tomorrow)

1. **Branch**: `feat/phase-73-compositor-polish`, HEAD = `7b4b65b`. Pushed to origin.
2. **Build state**: `cargo xtask check` clean. `cargo xtask smoke-test` (2 GiB default) passes
   in ~15 s. `cargo xtask smoke-test -m 4g` passes on the sandbox.
3. **What's pending**: **only one thing — verify the fix on the user's Zen 5 / Linux 7.0 host.**
   The sandbox (Zen 4 / Linux 6.8) always passed at 4 GiB, so its passing tells us nothing
   conclusive — the user's box is the definitive test.
4. **Reproducer (run on user's host)**:
   ```bash
   cargo xtask run -m 4g --fresh           # TCG, the simplest reproducer
   cargo xtask run --kvm -m 4g --fresh     # KVM path
   cargo xtask run-gui --kvm -m 4g --fresh # GUI path
   ```
   If the greeter comes up: **done**. If anything still hangs, the bounded-spin diagnostic
   will produce a fresh panic with `[tlb] per-core diagnostics — tlb-ipi serviced … LAPIC-timer
   ticks …` covering every core. The shape of that dump tells you exactly which invariant
   broke (see "How to read a diagnostic dump" below).
5. **If everything works**: open the PR to `main`. Optional cleanup is listed in "Deferred"
   below — none of it blocks ship.

## TL;DR — what the bug actually was

Two-part bug in m3OS's TLB shootdown protocol vs `IrqSafeMutex` discipline:

**Bug 1 (necessary precondition, fixed in 595951f)**: APs ran their entire `ap_entry` setup
with IF=0. The bootloader hands APs to `ap_entry` with IF=0 and nothing in the function
explicitly STI'd until the first iteration of `task::run`'s scheduler loop — *after*
`spawn_idle_for_core` had already allocated kernel-stack pages and grown the bootstrap heap.
So if a heap-grow during AP init fired a kernel-VMA TLB shootdown, the AP could not service
incoming IPIs from concurrent senders.

**Bug 2 (the load-bearing one, fixed in 7b4b65b)**: `mm::heap::grow_heap` takes
`GROW_HEAP_LOCK` — an `IrqSafeMutex` that CLIs at acquire — and then calls
`tlb_shootdown_range_kernel` from inside the locked region. The shootdown sender spins for
acks with IF=0, so it cannot service other cores' concurrent shootdown IPIs. Two cores
simultaneously growing the heap (common during early SMP init) deadlock waiting for each
other's acks.

The fix lands the IF=1-during-spin invariant *inside the shootdown helpers* via a new
`ShootdownIrqWindow` RAII guard (`kernel/src/smp/tlb.rs`). The window snapshots IF on `open`,
forces IF=1, and restores the prior state on `Drop`. Safe because `preempt_count` is already
raised by each helper's own `preempt_disable`, so no task migration happens during the
temporary IF=1 window; the only IRQs that can fire are the LAPIC timer (atomic tick + EOI,
no allocation) and the TLB-IPI handler itself (lock-free atomic decrement + local flush).

## TL;DR — why this was so hard to find

The bug was always present. What changed at 4 GiB on the user's host was just the statistics:

- **More RAM → more bootstrap-heap growth events during AP boot** → more concurrent
  `grow_heap → tlb_shootdown_range_kernel` calls across cores, all with IF=0.
- **Faster CPU + different host kernel scheduling on Zen 5 / Linux 7.0** kept the four-way
  IF=0 windows perfectly overlapped for >500 ms. On the sandbox's Zen 4 / Linux 6.8, the
  same race had different timing and the overlap never lined up long enough to deadlock.

The "PCI hole crossing at 4 GiB" framing in the original handoff was a red herring — the true
threshold was "enough heap growth during AP boot to expose the IF=0 race." The "latent
`wait_icr_idle` race" suspected in session 1 was also a red herring — the bounded-spin
diagnostic added in `b36bcaa` confirmed `wait_icr_idle` returned cleanly in every reproduction.
The IPIs always dispatched fine; recipients just couldn't take them.

## How to read a diagnostic dump (if the fix is incomplete and you get a fresh panic)

The per-core diagnostic table at the end of the panic dump is the bisector:

| ipi-Δ | timer-Δ | Interpretation                                                |
|-------|---------|---------------------------------------------------------------|
| 0     | >0      | Core's IRQ delivery works; vector 0xFD specifically is broken (IDT install race, vector masking, dispatch path) — bug is IPI-specific |
| 0     | 0       | Core has IF=0 for the full 500 ms window OR its LAPIC timer is dead — bug is interrupt-delivery generic; suspect an outer IrqSafeMutex region |
| >0    | >0      | Handler fired but the atomic ack accounting is wrong (race in SHOOTDOWN_PENDING) — unlikely now that the explicit counter rules out double-count |

Last observed dump (m3os.log from the run that informed 7b4b65b):
```
[tlb]   core 0: ipi 1 → 1 (Δ0)  timer 29 → 29 (Δ0)    ← BSP, ipi=Δ0 + timer=Δ0 = IF=0 entire window
[tlb]   core 1: ipi 0 → 0 (Δ0)  timer 0 → 0 (Δ0)      ← AP1 (sender), never serviced anything its whole boot
(cores 2, 3 omitted because all four counters were 0 — they ALSO had IF=0 entire boot)
```

If you see a *different* dump shape after the fix, that's a separate bug — file a new handoff
and link this one.

## What changed where (this session)

### `xtask`

| File | Change | Commit |
|---|---|---|
| `xtask/src/main.rs` | Added `try_take_memory_arg` + `apply_memory_env_fallback` helpers. Threaded `memory_mib` through `SmokeTestArgs` / `RegressionArgs` so `cargo xtask smoke-test -m 4g` and `cargo xtask regression -m 4g` work. Centralized `M3OS_MEM=` env-var fallback inside `qemu_args_with_devices_resolved` so every QEMU launch path picks it up automatically. Help banner updated. | `66b1896` |

### Kernel — diagnostics

| File | Change | Commit |
|---|---|---|
| `kernel/src/smp/ipi.rs` | `wait_icr_idle` bounded to ~100 ms (TSC-based) with pre-calibration fallback; on timeout panics with ICR_LOW / ICR_HIGH / dest-APIC / iterations / tsc_per_ms. | `b36bcaa` |
| `kernel/src/smp/tlb.rs` | Factored `wait_for_shootdown_acks_or_panic` helper with ~500 ms TSC-based timeout. Routes the dump through `_panic_print` directly (not `log::warn!`) so the recursive `_kernel_print → DMESG_RING.lock → IrqSafeMutex` path can't swallow it. | `b36bcaa` |
| `kernel/src/smp/tlb.rs` | `TLB_IPI_SERVICED[MAX_CORES]` per-core counter, bumped at the top of `handle_tlb_shootdown_ipi` (before any TLB work). Snapshotted at wait entry, dumped per-recipient as `(before → after, delta)`. | `9bffc1b` |
| `kernel/src/arch/x86_64/interrupts.rs` | `TIMER_TICKS_PER_CORE[MAX_CORES]` per-core counter, bumped at the very top of both `timer_handler_user` and `timer_handler_kernel`. | `1401239` |
| `kernel/src/smp/tlb.rs` | Wait-helper diagnostic now also snapshots and dumps the per-core timer counter alongside the IPI counter. | `1401239` |

### Kernel — fixes

| File | Change | Commit |
|---|---|---|
| `kernel/src/smp/boot.rs` | After `ap_lapic_init_from`, before `data.is_online.store(true)`, call `x86_64::instructions::interrupts::enable()`. APs now run their post-LAPIC-init work (logging, `spawn_idle_for_core`, `task::run`) with IF=1 — necessary precondition for receiving the BSP's TLB shootdowns during AP-init heap allocations. | `595951f` |
| `kernel/src/smp/tlb.rs` | New `ShootdownIrqWindow` RAII guard. Opens IF=1 for the duration of the SHOOTDOWN_PENDING spin, restores on `Drop`. All three shootdown helpers wrap their `wait_for_shootdown_acks_or_panic` call with one. This makes the IF=1-during-spin invariant a property of the shootdown protocol itself, so future callers can't accidentally re-introduce the bug by holding an outer `IrqSafeMutex`. | `7b4b65b` |
| `kernel/src/mm/heap.rs` | Comment update noting that the shootdown helper now enforces IF=1 itself, so callers from inside an `IrqSafeMutex` region are safe. | `7b4b65b` |

## Open work / next-session targets

1. **Verify on user's host** (Zen 5 / Linux 7.0, QEMU 8.2.2). One of:
   ```bash
   cargo xtask run -m 4g --fresh
   cargo xtask run --kvm -m 4g --fresh
   cargo xtask run-gui --kvm -m 4g --fresh
   ```
   The first one is the simplest reproducer (TCG, no GUI, no SMP-race-handling complexity).
2. **If verified, open the PR to `main`.** The diff is small (six commits, ~100 LoC of fix +
   the diagnostic scaffolding). The diagnostic scaffolding is intentionally left in tree as a
   durable safety net for any future IPI-coherence regression.
3. **If still hung after this**: capture the new dump and triage per the table above. The
   most likely remaining shapes are (a) a different IrqSafeMutex caller of a shootdown
   helper that I missed in the audit, or (b) a vector-specific IDT issue on Zen 5 + Linux 7.0
   that needs vector-level instrumentation.

## Deferred / nice-to-haves (not blocking ship)

These came up during the investigation but aren't on the critical path:

- **Audit other `tlb_shootdown_*` call sites for IrqSafeMutex containers.** I checked the two
  obvious ones (`heap.rs:888` was the bug we fixed; `pci/bar.rs:547,634` and the syscall-path
  call sites all run with IF=1 since they're invoked from syscall context). But a systematic
  audit would be reassuring. Grep target: `tlb_shootdown` in non-`smp/tlb.rs` files, then
  walk up the lock-acquire chain at each callsite. Now that `ShootdownIrqWindow` enforces
  IF=1 internally, this is defense-in-depth rather than correctness-critical.
- **Land the recursive-log re-entry guard** (`PREEMPT_WARN_IN_FLIGHT[MAX_CORES]`) that the
  previous handoff documented as reverted. The original concern was "removing the recursive
  delay exposes a latent wait_icr_idle race", but `b36bcaa`'s bounded-spin diagnostic in
  `wait_icr_idle` would now catch any such race loudly. The guard is a clear quality-of-life
  improvement: it would cap a single warning's stack-eating recursion. Not blocking.
- **Add PCD/PWT (cache-disable) on `sys_framebuffer_mmap`** (`kernel/src/arch/x86_64/syscall/mod.rs:10239`).
  Even though the framebuffer cache hypothesis turned out to be wrong as the cause of the
  4 GiB hang, MMIO is supposed to be uncacheable regardless of MTRR state. The current code
  relies on MTRR defaults to make 0xc0000000 uncacheable, which is brittle. One-line change:
  `| PageTableFlags::NO_CACHE | PageTableFlags::WRITE_THROUGH` on the flags constant. Plus
  the same audit on the bootloader-inherited kernel framebuffer mapping in
  `kernel/src/fb/mod.rs`.
- **xtask GUI escape-hatches `M3OS_GUI_BACKEND` and `M3OS_GUI_VGA`** (landed in 316d351 last
  session) are useful for any future display-path debugging. Worth a sentence in the xtask
  README or `cargo xtask --help` extension that documents them.
- **Drop the workspace-slide cache-invalidation fix from `1784c45`** is unrelated to this
  bug but was bundled in. Already shipped. Mentioning here only so a future investigator
  doesn't get confused why a compositor commit is in this fix series.

## How to actually reproduce (if it does still hang)

User's host:
```bash
# Reliable repro (pre-fix series, before 7b4b65b):
cargo xtask run -m 4g --fresh
M3OS_GUI_BACKEND=vnc cargo xtask run-gui --kvm -m 4g --fresh   # then `vncviewer localhost:5900`
M3OS_SMP=1 cargo xtask run-gui --kvm -m 4g --fresh
cargo xtask run-gui -m 4g --fresh                              # TCG, also black

# Should now succeed (post-7b4b65b):
cargo xtask run --kvm -m 4g --fresh
cargo xtask run-gui --kvm -m 4g --fresh

# Still expected to succeed (regression base):
cargo xtask run --kvm --fresh                                  # 2 GiB default
cargo xtask run --kvm -m 3g --fresh                            # 3 GiB
```

If a hang still happens, capture `serial.log` (xtask writes it to stdout by default; redirect
to a file) and look for the `[tlb] per-core diagnostics` block at the end. That block is
self-explanatory per the table above.

## Hypotheses I burned time on this session (so you don't repeat them)

- **"PCI hole crossing at 4 GiB causes kernel paging issues."** False. The bisected threshold
  was correlation, not causation — heap-grow frequency scales with RAM size, and the IF=0
  deadlock requires concurrent heap-grow events.
- **"Framebuffer is mapped cacheable (missing PCD/PWT) so writes don't reach QEMU's VGA."**
  Real latent issue (worth fixing, see Deferred), but ruled out as the cause of this hang
  because `cargo xtask run -m 4g` (no GUI, no framebuffer) also hangs.
- **"It's the `wait_icr_idle` latent race the previous handoff hinted at."** False. The
  bounded-spin diagnostic added in `b36bcaa` would have panicked from `wait_icr_idle`'s
  100 ms timeout if so. In every reproduction, `wait_icr_idle` returned cleanly — the IPI
  was always dispatched by the LAPIC. The hang was always at the recipient side.
- **"Maybe the AP IDT vector for 0xFD isn't installed correctly on Zen 5."** False. The
  per-core IPI-serviced counter showed cores DID service IPIs (BSP=1 lifetime, from earlier
  smaller-target shootdowns) — they just weren't servicing the current one because they
  were in IF=0 regions.
- **"Adding `interrupts::enable()` to `ap_entry` will fix everything."** Partial — it was a
  necessary precondition (otherwise APs would have IF=0 throughout their init), but
  insufficient: the heap-grow-with-IrqSafeMutex pattern re-CLI's immediately on lock
  acquire. The fix had to land at the shootdown-protocol layer.

## How real OS implementations handle this

- **Linux** uses INVPCID where available and falls back to per-CPU IPI flush, but its IPI
  send paths use a different lock discipline (`smp_call_function_*` doesn't CLI; locks are
  designed to allow IPIs to fire during the wait). Linux also explicitly enables interrupts
  during stop-the-world TLB-shootdown polls for exactly this reason.
- **seL4** avoids the problem entirely by making the kernel non-preemptive within an entry
  and using a single big lock; no concurrent kernel-VMA mutations are possible.
- **FreeBSD** uses a similar IPI handshake to m3OS but its TLB shootdown sender code is
  documented as requiring `IF=1` and the lock primitives it uses respect that. The
  `ShootdownIrqWindow` in `7b4b65b` brings m3OS in line with that discipline.
