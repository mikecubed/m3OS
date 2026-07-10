# Handoff — Phase 110 Track B.3: CET user shadow stacks

**Date:** 2026-07-08
**Branch:** `feat/phase-110-cet-shstk`, stacked on `feat/phase-110-kpti-hardening2`
(continues the KPTI #322 → #324 → #325 chain; shares the mm/exec path). The CET
work itself is independent of KPTI — it is stacked only because it was cut from
that branch tip.
**State:** ✅ **Substrate COMPLETE + green on QEMU (dormant); active path
Dell-pending.** The whole feature is gated on `cet_active` and **byte-for-byte
inert on every QEMU lane** (TCG models no CET). The active behaviour (shadow-stack
pushes, `#CP` on a forged return, the MSR reads/writes) runs only on CET silicon
— the Precision 5560 (Tiger Lake) arm in [`next-dell-session.md`](./next-dell-session.md).

**Charter:** `docs/roadmap/110-real-hardware-security.md` (Track B.3)
**Tasks:** `docs/roadmap/tasks/110-real-hardware-security-tasks.md` (Track B.3 as-built)

> **Why shadow stacks.** CET is a hardware control-flow-integrity layer: every
> `CALL` pushes the return address onto a protected **shadow stack**, every `RET`
> checks the two agree, and a mismatch faults `#CP` — catching a return-address
> overwrite (ROP, a stack overflow past the canary) the CPU, not the compiler.
> Tiger Lake supports it; QEMU TCG does not, so the substrate lands CI-provable
> (posture + no-op) and the active proof is the Dell.

## Design — user shadow stacks only

m3OS enables **user** shadow stacks (`IA32_U_CET.SH_STK_EN`) and leaves
`IA32_S_CET.SH_STK_EN = 0` — no kernel shadow stack, no IST shadow-stack tokens.
Per the SDM (Vol 3A §6.14): on a ring-3 → ring-0 transition the CPU saves the
outgoing user `SSP` into `IA32_PL3_SSP` and loads `SSP = 0`; `IRET` back to ring 3
reloads `SSP` from `IA32_PL3_SSP`. So within one kernel entry/exit the user SSP
is hardware-preserved — the kernel only saves/restores `IA32_PL3_SSP` across
**task switches** and around **signals**.

**Load-bearing subtlety — the shadow-stack PTE encoding.** A page is a shadow
stack when its leaf PTE is **read-only (R/W=0) + Dirty=1** (with CR4.CET set):
ordinary data stores fault, but shadow-stack pushes (`CALL`, `WRUSS`) succeed.
Modeled + host-tested in `kernel_core::cet` (`compose_user_shadow_stack_pte` /
`is_shadow_stack_pte`); the kernel mapper forces intermediates writable+user, as
CET requires (the determination is made at the leaf).

**Per-core `SH_STK_EN` ⇒ every task needs an SSP.** `IA32_U_CET` is a per-core
MSR and m3OS uses XSAVE (not XSAVES), so it is **not** per-task state — once CET
is on, every user task on the core has shadow stacks enabled and MUST have a
valid `IA32_PL3_SSP` or its first `CALL` faults. Hence all three ring-3-entry
paths install one.

## What landed (5 commits)

- **1/n** — `kernel_core::cet`: CPUID decode, MSR/CR4/`U_CET` bit layout, the
  shadow-stack PTE encoding, the `RSTORSSP` restore-token format. 6 host tests.
- **2/n** — `cpuid::probe_cet` (leaf-7 guarded) + `enable_user_cet_if_supported`
  (`CR4.CET` + `IA32_U_CET`, BSP-before-`boot_aps` / per-AP / S3-resume),
  `MitigationState.{cet_present,cet_active}`, the `[sec] cet(active=…
  supported=…)` boot line, the D.3 wire (byte 16, **version 4**) + the `m3ctl`
  `CET:` posture line. `mitigations-status-smoke` asserts the QEMU
  not-supported posture.
- **3a** — `IA32_PL3_SSP` save/restore across context switch, co-located with
  the FPU/XSAVE save/restore (`Task.cet_ssp`; the co-location IS the
  correctness argument — identical per-task-CPU-state lifecycle).
- **3b** — per-task shadow-stack allocation in `PML4[255]` (a `USER_PML4_SLOTS`
  slot, reachable on the KPTI user CR3) via a per-AS bump allocator
  (`AddressSpace.cet_shstk_next`), across **execve** (fresh, fail-closed on
  ENOMEM), **clone** (fresh per-thread, ENOMEM-clean before spawn), and
  **fork** (inherit the parent's SSP).
- **4/n** — the `#CP` (vector 21) naked KPTI stub + body (kill the ring-3
  violator via the fault-kill trampoline; halt a ring-0 CFI bug), and signal
  `IA32_PL3_SSP` save/restore (`Task.cet_signal_ssp`, stash at delivery /
  restore at `sigreturn`).

**Green (inert lane, per commit):** `check` (+ 6 `cet` host tests + the
`spectre`/`m3ctl` CET wire+formatter tests), `mitigations-status-smoke` (the
CET boot line + `CET: not-supported`), `smoke-test`, `termios-smoke`
(signal/sigreturn), `kstack-overflow-smoke` (the fault-kill path #CP reuses),
`cargo xtask test` (13 — fork/exec/context-switch heavy), `kpti-selftest-smoke`.

## Dell-pending (the active arm — see next-dell-session.md)

1. **Boot with CET live** (`cet(active=true supported=true)`, `CET: enabled`) —
   the first proof the enable + per-task SSP + save/restore + PTE encoding are
   all correct (a wrong encoding or stale SSP = an immediate `#CP`/`#PF`).
2. **A ROP/overwrite PoC** faults `#CP` with CET on, returns into the planted
   address with CET off (mask `CET_SS` in `probe_cet`). The CFI analogue of A.6.

## Known Dell-validation risks (documented, not yet resolved)

- **fork CoW-of-shadow-stack.** The child inherits the parent's SSP and its
  copied AS includes the parent's shadow-stack pages (RO+Dirty). A child's first
  shadow-stack push must CoW-duplicate that page; m3OS's generic CoW may need a
  shadow-stack-aware arm (Linux copies the shadow stack explicitly rather than
  CoW). If the Dell shows a `#CP`/`#PF` in a forked child's first `RET`, this is
  the cause.
- **Nested signals.** `Task.cet_signal_ssp` is a single slot — correct for
  non-nested signals, wrong for a signal interrupting a handler. The fix is the
  `RSTORSSP`-token path (`kernel_core::cet::shadow_stack_restore_token` is
  modeled; `WR_SHSTK_EN` is enabled so `WRUSS` can seed the token).
- **Shadow-stack size** is a fixed 16 KiB per thread (`USER_SHADOW_STACK_SIZE`);
  very deep recursion overflows it → `#PF` kill. Revisit if a real workload hits
  it.
