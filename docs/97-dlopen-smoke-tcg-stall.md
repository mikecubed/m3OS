# Debugging an Intermittent Flake: the `dlopen-test-smoke` `DT_RELR` Bug

Kernel **v0.97.0**. Status: ✅ landed (PR #268) — the fix is validated by host
tests, a 232-iteration on-device soak, and a full `smoke-test` pass.

## Overview

Phase 97 is a *debugging* phase: there is no new OS capability, only a fix and a
hardened test gate. Its teaching value is the **investigation discipline**, not
the size of the diff (which is tiny).

The always-on `smoke-test` gate intermittently failed at **step 26**
(`dlopen-test-smoke`). The planning docs and the prior handoff confidently
blamed an SMP race — first a blocking-`vfs_server` demand-read lost-wakeup, then
a cross-core TLB shootdown under `-smp 4` TCG oversubscription. **Both were
wrong.** The real cause was a missing relocation type in the userspace dynamic
loader:

> `libhello_fini.so`'s only relocation — its `DT_FINI_ARRAY` destructor pointer —
> is encoded in the modern compact **`DT_RELR`** (`.relr.dyn`) format, and the
> `ld-musl` loader had **no `DT_RELR` support at all**. So the destructor pointer
> was never relocated; `dlclose` → `run_destructors_for` called its unrelocated
> in-file value `0x2a0` → a near-NULL `INSTRUCTION_FETCH` page fault →
> `process killed`.

The "intermittent TCG stall" framing was a triple misdiagnosis: the failure is
not a stall (the process is *killed*, fast), not cross-core (a userspace
relocation bug is core-count-independent), and not even truly intermittent — it
is **toolchain-deterministic**. Modern linkers emit `.relr.dyn`; older ones emit
the equivalent `.rela.dyn` runs that the loader already handled. The gate passed
on hosts with an older linker and failed on hosts with a newer one — and it
*looked* like a flaky timeout only because the gate had **no FAIL pattern** and
collapsed every non-PASS outcome into one opaque 120 s wait.

## What This Doc Covers

- **`DT_RELR`** — the compact relative-relocation format, how it encodes a run of
  `R_X86_64_RELATIVE` writes as address + bitmap words, and why a loader that
  handles `DT_RELA` but not `DT_RELR` silently drops relocations.
- **Observability-first debugging** — how the bug was reproduced and the wrong
  hypotheses *falsified with data* (a serial dump with zero `[tlb]` lines;
  `readelf` showing `RELASZ: 0`) before any code was changed.
- **Why "intermittent" lied** — a deterministic, build-environment-dependent
  failure masquerading as a runtime SMP race, and how a gate with no FAIL pattern
  turns a deterministic crash into an "intermittent stall".
- **The honest gate** — replacing a PASS-or-SKIP `WaitEither` with a
  `WaitPassOrFail`, and hoisting a kernel-fatal scan into every wait so any
  always-on step fails fast with a *named* cause.

## Core Implementation

### The symptom, and three wrong hypotheses

The gate is the last guest step before `SMOKE:PASS`. The `smoke-runner` forks
`dlopen_test`, `dup2`s its stdout to an unlinked tmpfs capture file, and
`waitpid`s it — so the child's own sentinels never reach serial, and on failure
the host saw only "begin, then silence" until the 120 s timeout. That opacity is
what made three different authors reach for an SMP-race story:

1. **Blocking-`vfs_server` lost-wakeup** (the handoff) — falsified at the artifact
   level: every DSO is ramdisk-embedded, so each demand fill is a synchronous
   in-kernel `copy_from_slice` (`kernel_read_fd_at`'s `FdBackend::Ramdisk` arm),
   with no IPC to lose.
2. **Cross-core TLB shootdown** (the design doc's "leading surviving" suspect) —
   falsified by the reproduced serial dump: it contained **zero** `[tlb]` lines.
   The always-on `wait_for_shootdown_acks` degrade/ack-timeout logging would have
   printed them if the shootdown had fired or spun.
3. **A `MAP_LAZY_FILE` page reverting to pristine** (an intermediate hypothesis
   tried *during* this phase) — disproved when eager-mapping the writable
   segments did not change the symptom: the page never reverts; the slot is
   simply never written.

What the dump *did* show was decisive: a userspace
`addr=0x2a0 rip=0x2a0 INSTRUCTION_FETCH … process killed`, with the captured
child output stopping at `DLOPEN_TEST:FINI_PENDING` (printed right before
`dlclose(hf)` runs destructors) and never reaching `LIBHELLO_FINI:RAN`. The
destructor pipeline was calling a bad pointer.

### `DT_RELR`: the compact relative-relocation format

`readelf -d libhello_fini.so` is the whole story:

```
RELA   0x0      RELASZ  0 (bytes)      ← no DT_RELA relocations at all
RELR   0x1070   RELRSZ  8 (bytes)      ← the one relocation lives in .relr.dyn
.fini_array at 0x2ea0 holds file value 0x2a0   (the destructor's in-DSO vaddr)
```

`DT_RELR` packs a stream of `R_X86_64_RELATIVE` writes (`*slot += load_bias`)
into 8-byte words to shrink the relocation table:

- An **address word** (LSB == 0) is the image-relative byte offset of a slot to
  relocate; it also sets a running cursor to the next slot.
- A **bitmap word** (LSB == 1) uses bits `1..=63` to select up to 63 consecutive
  slots starting at the cursor; the cursor then advances 63 slots regardless of
  which bits were set.

The loader applied `DT_RELA` and `DT_JMPREL` and *silently ignored* `DT_RELR`
(the tag wasn't even defined). So `*0x2ea0 += load_bias` never ran, the slot kept
its file value `0x2a0`, and `run_destructors_for` — which reads `fini_array[i]`
and calls it *raw*, assuming it is already relocated — jumped to `0x2a0`.

### Why it looked intermittent

`DT_RELR` is what modern `lld` / recent `binutils` (with
`-z pack-relative-relocs`) emit by default; older toolchains emit the same
relocations as a `DT_RELA` run, which the loader *did* handle. The flake was
therefore **per-build-host**: deterministic-fail with a RELR-emitting linker,
deterministic-pass with a RELA-emitting one. No runtime randomness, no SMP, no
TCG timing — the "intermittency" was variance *across machines and toolchain
versions*, not across runs of one binary.

### The fix: `apply_relr` wired into the relocation engine

The fix is small and entirely in userspace:

- **`reloc::apply_relr`** — a pure, host-tested decoder for the RELR encoding
  (address + bitmap words), with bounds and 8-byte-alignment checks on every
  slot write.
- **`DT_RELR` / `DT_RELRSZ` / `DT_RELRENT`** constants and `DynamicSection.relr` /
  `relrsz` parsed from `PT_DYNAMIC`.
- **`apply_relr_for_dso`** called at all three relocation sites — `dlopen`'d DSOs
  (`apply_relocations_for`), bring-up `DT_NEEDED` DSOs, and the main binary — so
  any DSO whose relative relocations are RELR-packed is fully relocated. It is a
  no-op for a DSO that carries no `DT_RELR`, so the change is invisible to the
  RELA path.

The loader's own startup self-relocation (`dl_relocate_self`) still handles only
`DT_RELA`; the `ld-musl` binary is built `x86_64-unknown-none` and currently
emits `DT_RELA` (`RELASZ: 96`), so this is unreachable today and left as a noted
follow-up.

### The honest gate

A correctness bug is only half the story; the *other* defect was that the gate
hid it. The `dlopen-test-smoke` step was a `WaitEither{PASS, SKIP}` with **no
FAIL pattern**, so the `smoke-runner`'s already-emitted
`SMOKE:dlopen-test-smoke:FAIL` was ignored and the step timed out. Two changes
make the gate honest:

- The step is now `WaitPassOrFail`, matching the runner's FAIL verdict and the
  kernel `process killed` / panic markers — a regression **fails fast with a
  named cause** instead of an opaque 120 s timeout. (The `FINI_PENDING < RAN <
  PASS` ordering assertion is preserved guest-side in
  `run_command_expect_dlopen_order`, so no `smoke-runner` rewrite was needed.)
- `global_fatal_line` (`KERNEL PANIC` / `RECURSIVE KERNEL PAGE FAULT` /
  `no waker registered`) is scanned in **every** `run_smoke_script` wait arm, so
  any always-on step aborts immediately on a kernel-fatal marker. (`process
  killed` is deliberately *not* global — legitimate negative tests fork-and-kill
  children — so it stays in the per-step `fail_prefixes` where a kill is
  expected.)

### The reproduction harness

`cargo xtask dlopen-repro` boots `-smp 4` TCG, logs in, and runs `/bin/dlopen_test`
in a host-driven loop (`M3OS_DLOPEN_ITERS`, default 100), matching
`DLOPEN_TEST:PASS` directly on serial and fail-fasting on the destructor fault.
It is both the confirmation tool (pre-fix: faulted on iteration 1 every run) and
the soak harness (post-fix: 232 consecutive passes with `LIBHELLO_FINI:RAN`
printed every time — the destructor *runs* now — and zero faults).

## Key Files

| File | Purpose |
|---|---|
| `userspace/ld-musl-x86_64.so.1/src/reloc.rs` | `apply_relr` — the host-tested DT_RELR decoder + `relr_*` unit tests |
| `userspace/ld-musl-x86_64.so.1/src/elf64.rs` | `DT_RELR` / `DT_RELRSZ` / `DT_RELRENT` constants |
| `userspace/ld-musl-x86_64.so.1/src/dynlink.rs` | `DynamicSection.relr` / `relrsz` parsed from `PT_DYNAMIC` |
| `userspace/ld-musl-x86_64.so.1/src/main.rs` | `apply_relr_for_dso` + its calls at the dlopen / bring-up / main-binary relocation sites; `run_destructors_for` (the bad-pointer call site) |
| `xtask/src/main.rs` | `cmd_dlopen_repro`; the `WaitPassOrFail` dlopen gate step; `global_fatal_line` + its wiring into `run_smoke_script` |
| `userspace/dlopen_test/dlopen_test.c` | the gate's test binary (positive/negative libdl paths + the `DT_FINI_ARRAY` destructor pipeline) |

## How This Phase Differs From Production OSes

- **`DT_RELR` is table stakes.** glibc's `ld.so` and musl's own dynamic linker
  have handled `DT_RELR` since it was standardized (glibc 2.36, 2021); a
  from-scratch loader simply hadn't grown the arm yet. The relative-relocation
  format itself exists *because* large PIEs have huge `.rela.dyn` tables —
  `DT_RELR` is the compression that makes relative relocs cheap.
- **Mature CI separates verdicts.** Production test harnesses distinguish
  *timeout* vs *crash* vs *assertion failure* as different results with per-step
  diagnostics. The original gate collapsed all of them into one timeout — which
  is precisely how a deterministic crash got mislabeled an "intermittent stall"
  for multiple sessions. The fix (a real FAIL pattern + a global fatal scan) is
  the honest-CI lesson, not a kernel change.
- **Believe the artifact, not the narrative.** The most expensive part of this
  bug was the plausible-but-wrong SMP-race story in the planning docs. `readelf`
  on the failing DSO would have ended the investigation on day one; the
  observability-first discipline (reproduce, then *falsify* each hypothesis with
  data before patching) is the transferable skill.

## Related Roadmap Docs

- [Phase 97 roadmap doc](./roadmap/97-dlopen-smoke-tcg-stall.md) — design + the
  full Investigation Findings verdict.
- [Phase 97 task doc](./roadmap/tasks/97-dlopen-smoke-tcg-stall-tasks.md) — the
  per-task resolution table (which planned tasks were N/A vs done).
- [Phase 95b](./95b-on-device-rustc.md) — the `MAP_LAZY_FILE` demand-paged loader
  the (refuted) hypotheses suspected.
- [SMP TLB-shootdown handoff](./handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md)
  — the `wait_for_shootdown_acks` degrade path whose absence (zero `[tlb]` lines)
  excluded the shootdown hypothesis.

## Deferred or Later-Phase Topics

- **Loader self-relocation `DT_RELR`** — `dl_relocate_self` still handles only
  `DT_RELA`; unreachable today (the `ld-musl` binary emits `DT_RELA`) but it would
  need the same decode if that build ever starts emitting `.relr.dyn`.
- **The planned kernel hardenings (lazy-file-fault-under-lock prevention, the
  `vfs_server` self-read guard)** — they targeted the *refuted* lazy-file/`vfs`
  hypotheses, so they were intentionally **not** landed (minimal confirmed fix);
  they remain a latent-only concern the log-only deadlock-guard still catches.
- **Kernel demand-read timeout / watchdog-kill** — relevant to the genuinely
  `vfs`-backed gates (`rustc`/`clang`/install), not this ramdisk-synchronous path.
