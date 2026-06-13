# Memory Protection Keys (PKU) and JIT-Capable V8

**Aligned Roadmap Phase:** Phase 90a
**Status:** Complete
**Source Ref:** phase-90a
**Supersedes Legacy Doc:** (none — new capability; extends the Phase 75 W^X invariant)

## Overview

Phase 90a adds x86 **Memory Protection Keys (PKU/PKE)** to m3OS and uses them to
evolve the Phase 75 W^X invariant from a page-table-only rule into a hardware-
enforced, *per-thread* one. The motivating consumer is V8: Phase 89 had to ship
Node.js **jitless** because modern V8's JIT is "PKU-or-RWX" and m3OS forbids
RWX. Phase 90a is the kernel substrate that lets a JIT-enabled V8 generate
machine code at runtime — and run WebAssembly — without ever holding an
*unguarded* writable+executable mapping, which is exactly what Phase 90b's
Claude Code TUI (its `yoga.wasm` layout engine) needs.

The central lesson is about evolving a security invariant instead of abandoning
it. The naive "fix" for Node-with-JIT is to relax W^X. That is the wrong move:
W^X is one of the few hardening properties m3OS actually demonstrates *and*
regression-gates (`wx-violation`). Phase 90a strengthens it into **W^X v2**:
unguarded W+X stays rejected everywhere, and the *only* path to a W+X mapping is
the one real OSes and V8 converged on — a page tagged with a non-default
protection key whose default per-thread rights deny write, so hardware (not the
page table alone) gates every store. The compiling thread opens a brief write
window with `WRPKRU`; every other thread, and that same thread the rest of the
time, still cannot write the page.

The second lesson is that the substrate is mostly *reuse*. PKU's per-thread
register, PKRU, is XSAVE state component 9 — so it rides the per-task XSAVE
save/restore (Phase 57e/60) and signal frames (Phase 86f) that already exist;
the syscalls (`pkey_alloc`/`pkey_free`/`pkey_mprotect`) take Linux numbers so
musl and V8 work unmodified. Almost no bespoke context-switch machinery is
added — the work is *enabling* the component, *auditing* every PTE-writing path,
and *proving* PKRU never leaks across threads.

## What This Doc Covers

- The x86 PKU hardware model: PTE protection-key bits 59–62, the PKRU register,
  `RDPKRU`/`WRPKRU`, `CR4.PKE`, and **why key rights are per-thread while page
  tags are per-mapping** — the central mental model.
- W^X v1 → v2 as a case study in evolving an invariant without abandoning it,
  with the **verbatim v2 rule** and the enforcement-point audit (every path that
  could produce a W+X PTE, and where it is gated — including the `mmap(W+X)` hole
  that was closed).
- PKRU on XSAVE (component 9): how thread-local PKRU rides the existing per-task
  XSAVE + signal-frame machinery, the runtime RFBM (`0x207`), the Linux-default
  init value `0x55555554`, and the xsaveopt init-state / fork-inherit subtleties.
- V8's runtime PKU-adoption mechanics, the three musl-static blockers + their
  port-side remedies, and the no-PKU fallback.
- How real OSes solve the same JIT problem (Linux pkeys, OpenBSD `wxallowed`,
  Apple `MAP_JIT`, Windows ACG) — a convergent per-thread-write-window idea.
- The deferred items and the SMP follow-up D.1 surfaced.

## Core Implementation

### The PKU hardware model (the central mental model)

x86 Memory Protection Keys split memory-access control into **two independent
pieces**, and understanding *which piece lives where* is the whole mental model:

- **Page tags are per-mapping.** Each user page-table entry carries a **4-bit
  protection key** in bits **59–62** (just below the NX bit 63). The key is a
  property of *the page* — it is identical no matter which thread touches it.
- **Key rights are per-thread.** A per-CPU register, **PKRU** (Protection-Key
  Rights for User pages), holds **two bits per key** — *access-disable* (AD) and
  *write-disable* (WD) — for all 16 keys, in one 32-bit word. Because PKRU is a
  register, its value is whatever the *currently running thread* last loaded.

With `CR4.PKE` set, every user-mode access consults PKRU using the accessed
page's key: if that key's WD bit is set, a store faults (`#PF` with the PK bit);
if AD is set, even a load faults. So the same physical page is writable by a
thread whose PKRU clears WD for that key, and *non*-writable by a thread whose
PKRU sets it — **same page, different thread, different outcome**. That
asymmetry is the entire point: it is what lets one compiling thread write JIT
code while no other thread (and the same thread outside its write window) can.

PKRU is read with **`RDPKRU`** and written with **`WRPKRU`** — note these are
*not* syscalls. `WRPKRU` is a plain user instruction (a few cycles, serialising,
no kernel entry), which is what makes a per-thread write window cheap enough to
open and close around each individual code patch. The cost of a process-wide
`mprotect` flip — a syscall, a TLB shootdown across every core — is exactly what
PKU avoids, and is also why the old V8 `mprotect` RW↔RX model was both slower and
broader (every thread saw the page go writable) than the PKU model that replaced
it.

`m3OS`'s probe (`kernel/src/arch/x86_64/cpuid.rs`) reads CPUID.7.0:ECX for PKU
(the feature) and OSPKE (the OS has set `CR4.PKE`), exposing `pku_supported()` /
`pku_usable()`. Because PKU is per-core state, `CR4.PKE` is set on the BSP **and
every AP** — an AP without it would silently ignore key bits, a per-core hole —
through the single `enable_xsave_state()` each core runs, which also enables
XSAVE component 9 in XCR0 and re-validates the larger XSAVE area
(832→2752 bytes).

### W^X v1 → v2: evolving an invariant without abandoning it

**W^X v1** (Phase 75) is a page-table-only rule: `sys_mprotect`/`sys_mmap`
reject any `PROT_WRITE|PROT_EXEC` request outright (`EINVAL`). It is simple and
absolute — *no* mapping may be both writable and executable — and the
`wx-violation` gate enforces it.

**W^X v2** keeps that rule and adds exactly *one* principled exception: a W+X
mapping is permitted **iff** it is tagged with a non-default protection key whose
*default* PKRU policy denies write. The invariant did not weaken — it moved from
"the page table alone forbids W+X" to "the page table + per-thread hardware key
rights forbid *unguarded* W+X." A v2-guarded page is writable+executable in the
PTE, but **not writable by any thread** until that thread explicitly opens a
write window via `WRPKRU` — so at any instant, from the CPU's point of view, the
page is either writable *or* executable to the running thread, never both.

The exact rule could not be written until a host-side scout (Track A.1) pinned
the syscall sequence V8 emits. The result is the **verbatim kernel contract
note** the implementation (`wx_decision` in
`kernel/src/arch/x86_64/syscall/mod.rs`) follows:

> 1. `sys_mmap` with `PROT_WRITE|PROT_EXEC` → rejected, unchanged from Phase 75.
> 2. `sys_mprotect` with `PROT_WRITE|PROT_EXEC` → rejected, unchanged from Phase 75.
> 3. `sys_pkey_mprotect(addr, len, prot, pkey)`:
>    a. with `pkey == 0` (or `pkey == -1`, the untag-Linux-alias) it behaves exactly like `sys_mprotect` — W+X rejected;
>    b. with a non-default `pkey` that is **allocated** and whose **alloc-time `init_access_rights` include `PKEY_DISABLE_WRITE` or `PKEY_DISABLE_ACCESS`**, and PKU active (CR4.PKE set), `PROT_READ|PROT_WRITE|PROT_EXEC` is **permitted** and the range's PTEs are tagged with the key; the kernel logs one positive `[wx] v2-guarded W+X mapping (pkey=N)` line per grant;
>    c. any other W+X request through `sys_pkey_mprotect` (unallocated key, key allocated with permissive init rights, PKU absent) → rejected with the same errno as (1)/(2).
> 4. No other syscall or fault path may produce a W+X PTE; the C.1 enforcement-point audit enumerates them.

The pure accept/reject predicate is host-tested as
`kernel_core::pkey::wx_v2_permits(table, key_decision, pku_active)`: it grants a
W+X request *only* when all of — it is a `pkey_mprotect` with a non-default
allocated key, the key's `init_access_rights` deny write
(`PkeyTable::denies_write`), and `pku_usable()` — hold.

**The enforcement-point audit is the heart of "the invariant survives."** A rule
that is checked at one syscall but bypassable at another is theater; v2 had to
enumerate *every* path that can introduce a W+X PTE and show where it is gated:

| Path | Composes W+X from | Gate |
|---|---|---|
| `sys_mprotect` | `mprotect_worker(Preserve)` | `wx_decision` → `Rejected` for W+X (clause 2). `Preserve` never reaches `Tag`, so v1 unchanged. |
| `sys_pkey_mprotect` | `mprotect_worker(classified key)` | `wx_decision` (clauses 3.a/3.b/3.c). The **only** `GuardedV2` producer. |
| `sys_mmap` (anon + file-backed) | VMA recorded with `PKEY_DEFAULT`; PTEs composed key-0 | **W+X rejected at mmap entry (contract clause 1), same errno** — `mmap` carries no pkey argument, so it is the key-0 case of the `wx_decision` rule, applied eagerly in both the anon and file-backed entry points. |
| Demand-fault PTE composition (`compose_user_pte_flags`) | VMA `prot` + VMA `pkey` | A VMA only carries a non-default `pkey` after a `pkey_mprotect(Tag(k))` `wx_decision` **already permitted**; a rejected request never tags the VMA, so a fault cannot resurrect a refused mapping. |
| ELF segment load / eager file-backed mmap | `p_flags`/`prot`, key 0 only | Key 0 is never write-deny (`denies_write(0) == false`), so even a W+X request there falls to `Rejected`. |
| CoW fork copy / CoW fault resolution | parent PTE's whole flag word | Carries the key field verbatim and only toggles `WRITABLE`/`BIT_9`; never *introduces* a new W+X combination. |

The teaching moment is the `mmap` row. The original audit reasoned "`mmap`
cannot express a v2 grant, so it's fine" — and left `mmap(W+X)` *unguarded*. That
was a latent Phase 75 hole that had survived precisely because the `wx-violation`
gate only ever exercised `mmap(RW)` → `mprotect(WX)`, never `mmap(W+X)` directly.
A W+X `mmap` would have eager-mapped (or demand-faulted into) a *live* unguarded
W+X PTE tagged key 0 that never touched the guard. The fix rejects W+X at both
`mmap` entry points and adds an `mmap(PROT_READ|PROT_WRITE|PROT_EXEC)` → `EINVAL`
arm to `userspace/wx-violation/src/main.rs`. **The lesson: "every enforcement
point" is a literal requirement, and the cheap regression gate is only as strong
as the cases it actually drives.** The `wx-violation` gate staying green —
including the new arm — is the unchanged-behavior proof that v2 strengthened
rather than relaxed v1.

### PKRU on XSAVE (component 9)

PKRU is **XSAVE state component 9**, so m3OS does not add bespoke context-switch
state for it. The per-task `xsaveopt64`/`xsave64` around `switch_context`
(Phase 57e/60) and the signal-frame FPU save/restore (Phase 86f) already move
the XSAVE area; Phase 90a just enables component 9 in XCR0 and makes the
**RFBM (requested-feature bitmap) runtime-computed**: `xsave_rfbm()` returns
`0x207` (x87 `0x1` + SSE `0x2` + AVX `0x4` + PKRU `0x200`) when PKU is usable,
and `0x7` otherwise. That single derivation flows through `save_fpu_state`,
`restore_fpu_state`, and `sanitize_xsave_header`, so PKRU rides every context
switch and signal frame automatically.

Three subtleties matter, each a potential silent security hole:

- **Init value.** New tasks/threads seed PKRU to the **Linux default
  `0x55555554`** at execve: every *non-zero* key is access-denied, key 0 is
  unrestricted. So a freshly allocated key starts denying everything until the
  owning thread explicitly relaxes it — the safe default.
- **The xsaveopt init-state skip.** `xsaveopt` *omits* a component from the saved
  area when it is in its hardware init state (an optimisation), clearing that
  component's bit in the saved `XSTATE_BV` rather than writing zeros. So a
  fork-inherit that copies the parent's PKRU must **honor `XSTATE_BV[9]`**: if
  the bit is clear, the parent's PKRU is the init value, not whatever stale bytes
  sit in the area. Phase 90a fixes this stale-read so a permissive parent
  (XSTATE_BV[9] clear) correctly inherits `0`, not garbage.
- **Forged-bit hardening.** `sanitize_xsave_header` masks any user-supplied
  `XSTATE_BV` down to the runtime RFBM, so a forged bit-9 in a signal frame can't
  `#GP` `xrstor64` on a no-PKU boot.

The falsifiable proof that PKRU does *not* leak across threads is the
`pku-smoke` gate's per-context asymmetry arm: two `fork`ed contexts with opposite
PKRU values over the *same* tagged virtual address, asserting opposite
allow/deny outcomes — plus a signal-window arm holding a write window open across
handler entry and `sigreturn`. A missed save/restore is a silent cross-thread
hole, not a crash, so the gate Waits on the real-hardware sentinels (a silent
SKIP-fallback fails the gate).

### V8's runtime adoption + the no-PKU fallback

The Track A.1 scout settled the two facts everything downstream hangs on. The
first surprise: V8 in the Phase 89 static-musl build (and even a host glibc Node)
emits **zero `pkey_*` syscalls** — it falls back to `mprotect(...RWX...)`, which
m3OS rightly rejects. V8's PKU JIT write-protection is *compiled in and default-
on* (`V8_HAS_PKU_JIT_WRITE_PROTECT=1`, runtime flag `--memory-protection-keys`
defaults true), but it is neutralized by **three independent blockers**, each
with a port-side remedy (Track D.2):

1. **musl link gap.** V8 declares `pkey_alloc/free/mprotect/get/set` as
   `extern __attribute__((weak))` and null-checks them at runtime; musl defines
   none of them (nor the `PKEY_DISABLE_ACCESS`/`PKEY_DISABLE_WRITE` macros, so a
   guard compiles to a `return -1` stub). Every guard short-circuits. **Remedy:**
   a small strong-symbol shim TU — `pkey_alloc/free/mprotect` via `syscall(2)`,
   `pkey_get/set` via `RDPKRU`/`WRPKRU` (they are *not* syscalls) — linked into
   node, plus `-DPKEY_DISABLE_ACCESS=0x1 -DPKEY_DISABLE_WRITE=0x2` at compile.
2. **NodePlatform never provides the allocator.** `ThreadIsolation::Initialize()`
   requires `platform->GetThreadIsolatedAllocator()`; the default `v8::Platform`
   returns `nullptr` and Node's `NodePlatform` never overrides it — this is why
   even upstream glibc Node emits no pkey syscalls. **Remedy:** a port patch
   overriding `NodePlatform::GetThreadIsolatedAllocator()` to return V8
   libplatform's `DefaultThreadIsolatedAllocator`.
3. **Kernel-version gate.** `KernelHasPkruFix()` parses `uname()` release and
   requires ≥ 5.13 (a Linux PKRU-across-fork fix); m3OS reports `0.90.0` and
   would be rejected. **Remedy:** a port patch accepting the m3OS release string —
   justified because B.4 implements (and D.1 proves) the correct PKRU
   inherit-on-clone / reset-on-exec semantics that the Linux check guards.

With all three remedied, the kernel sees the **PKU-engaged sequence**: `uname`
(now accepted) → `pkey_alloc(init=PKEY_DISABLE_WRITE) = 1` → `pkey_mprotect`
tagging ThreadIsolation metadata → `mmap(PROT_NONE)` code-space reserve →
`pkey_mprotect(page, PROT_READ|WRITE|EXEC, pkey=1)` for each JIT/wasm code-page
commit (the one v2-guarded grant) → `WRPKRU` per write window (no syscall) →
`mprotect(PROT_NONE)`/`munmap` teardown.

**A.1 also pinned a build quirk** for the JIT variant: because the Phase 89 binary
bakes `--jitless` as an embedded default, V8's one-shot flag-implication pass
latches `--no-opt`, so a runtime `--no-jitless` re-enables WASM but *not*
TurboFan (`%GetOptimizationStatus` = `kNeverOptimize`). The JIT variant must
therefore drop `--v8-options=--jitless` **at configure time**, and
`node-jit-smoke`'s JIT-proof arm must assert *actual optimization*, not merely
that WASM loads.

**The no-PKU fallback** is graceful by construction: on a machine without PKU,
`pkey_alloc` returns `ENOSPC` → V8's `ThreadIsolation` stays disabled → V8 falls
back to plain-RWX commits → the kernel rejects them → the JIT variant **aborts
at its first code-space commit** (it does not silently degrade to a hole). The
jitless `.m3pkg` from Phase 89 remains the documented default everywhere, and the
JIT binary can still be run manually with `--jitless`. The D.3 no-PKU arm is
therefore skip-with-reason.

### Reporting

Per Phase 84's "security posture must be reportable, not implicit" principle,
`m3ctl mitigations status` prints a W^X/PKU line sourced live from the B.1
probes: `W^X: v2 (PKU present, active)` under KVM on a PKU host,
`W^X: v1 (PKU absent)` on no-PKU/TCG (a present-but-inactive form is also
handled). It is encoded into the existing Phase 84 `MitigationReport` (spare flag
bits, wire-version bumped 1→2), not a new channel, and the
`mitigations-status-smoke` gate asserts the no-PKU line on the default TCG lane.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/arch/x86_64/cpuid.rs` | PKU/OSPKE CPUID detection + `pku_supported()`/`pku_usable()`; the XSAVE component-9 surface (extends the Phase 57e probe) |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `sys_pkey_alloc` (330) / `sys_pkey_free` (331) / `sys_pkey_mprotect` (329); the shared `mprotect_worker`; the `wx_decision` W^X v2 guard (the verbatim contract note is its doc comment) |
| `kernel/src/mm/pkey.rs` | The PTE-rewrite enforcement audit table (demand-fault, COW, mprotect splits, eager file-backed/ELF) showing every path preserves/composes the key field correctly |
| `kernel-core/src/pkey.rs` | Host-testable key encode/decode (bits 59–62), per-process accounting (16 keys, key 0 reserved, `init_access_rights`, `denies_write()`), and the `wx_v2_permits` predicate (`wx_v2_*` unit tests) |
| The `switch_context` XSAVE path + `kernel/src/signal.rs` | PKRU rides component 9 via the runtime `xsave_rfbm()` (0x207 / 0x7); signal frames carry it; `sanitize_xsave_header` masks forged bits; execve seeds `0x55555554`, fork honors `XSTATE_BV[9]` |
| `kernel/src/mitigations.rs` + `userspace/m3ctl/src/main.rs` | The W^X v2 / PKU status line in the Phase 84 `MitigationReport` |
| `userspace/wx-violation/src/main.rs` | The v1-preservation gate, including the new `mmap(W+X)` → `EINVAL` arm that closes the mmap hole |
| `docs/roadmap/90a-memory-protection-keys.md` | Phase design doc |
| `docs/roadmap/tasks/90a-memory-protection-keys-tasks.md` | Per-track task list (the A.1 Findings + C.1 audit are the source of this doc's contract note and table) |

## How This Phase Differs From Later Memory Work

- This phase introduces **user** protection keys for the single V8 JIT use case.
  Linux additionally has **PKS** (supervisor protection keys) for kernel
  self-protection; m3OS defers that.
- This phase strengthens **W^X v1** (Phase 75, page-table-only) into **W^X v2**
  (page-table + per-thread hardware key rights) *without changing behavior for
  any existing binary* — the `wx-violation` gate proves it.
- A later phase could use protection keys beyond V8 (e.g. guarding crypto key
  material pages) or add richer multi-key policy management; Phase 90a scopes to
  the single write-deny key V8 needs.
- Phase 90b (Claude Code) is the **consumer**: its TUI's `yoga.wasm` layout
  engine needs runtime WebAssembly, which only the JIT V8 variant this phase
  unblocks can run.

## Related Roadmap Docs

- [Phase 90a design doc](./roadmap/90a-memory-protection-keys.md)
- [Phase 90a task list](./roadmap/tasks/90a-memory-protection-keys-tasks.md) — the A.1 Findings (V8/PKU contract, the three musl-static blockers + remedies, the verbatim v2 rule) and the C.1 enforcement-point audit are the source for this doc
- [Phase 89 — Node.js](./89-nodejs.md) — the jitless baseline this phase upgrades; its A.3 explicitly tracked "PKU-backed JIT" as the follow-up
- [Phase 75 — W^X Enforcement](./75-wx-enforcement.md) — the v1 invariant this phase evolves to v2
- [Phase 90b — Claude Code](./roadmap/tasks/90b-claude-code-tasks.md) — the consumer; the TUI's `yoga.wasm` depends on the JIT variant (D.2/D.3)

## Deferred or Later-Phase Topics

- **PKS (supervisor protection keys)** for kernel self-protection — Linux has it;
  m3OS scopes to user keys for the V8 use case only.
- **Protection-key uses beyond V8** (e.g. guarding crypto key-material pages) and
  multi-key policy management beyond the single JIT key.
- **PKU-aware `ptrace`/coredump integration** — real kernels must handle PKRU
  interactions with ptrace, coredumps, and userfaultfd; m3OS has none of those
  surfaces yet.
- **Non-x86 equivalents (ARM POE)** — the per-thread-write-window idea is
  convergent (OpenBSD `wxallowed` + ELF marker, Apple `MAP_JIT` +
  `pthread_jit_write_protect_np`, Windows ACG, Linux pkeys since 4.9), but only
  x86 PKU is implemented here.
- **The D.1-surfaced SMP follow-up** — under KVM + default 4-core SMP the
  fork+fault PKU sequence triggers a `RECURSIVE KERNEL PAGE FAULT on core 2`; the
  gate is pinned single-core (precedent: `go`/`node` smoke), correct because the
  JIT consumer runs single-core regardless. The hypothesis is a generic cross-core
  CoW-resolve vs. address-space-teardown race exposed by fork-under-SMP — **not** a
  per-AP PKU-coherence bug (a coherence bug would surface as a wrong allow/deny
  `:FAIL`, not a NULL deref). Tracked as a post-90a follow-up.
