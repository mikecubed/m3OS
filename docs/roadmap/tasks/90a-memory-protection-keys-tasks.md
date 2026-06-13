# Phase 90a — Memory Protection Keys (PKU) and JIT-Capable V8: Task List

**Status:** Planned
**Source Ref:** phase-90a
**Depends on:** Phase 57e/60 (per-task XSAVE save/restore) ✅, Phase 75 (W^X enforcement + the `wx-violation` gate) ✅, Phase 84 (mitigations policy + `m3ctl mitigations status`) ✅, Phase 86f (signal-frame FPU save/restore) ✅, Phase 89 (Node.js — the jitless baseline + `build_node`) ✅
**Goal:** Implement x86 Memory Protection Keys end-to-end (CPUID/CR4.PKE, PTE key bits, `pkey_alloc`/`pkey_free`/`pkey_mprotect`, per-task PKRU via XSAVE component 9 incl. signal frames), evolve the Phase 75 W^X invariant to "W^X v2" (unguarded W+X stays rejected; a W+X mapping is permitted only under a non-default protection key whose default PKRU policy denies write), and on that substrate ship a JIT-enabled `build_node` variant (own content key; the Phase 89 jitless artifact stays the default and fallback) proven by `pku-smoke` + `node-jit-smoke` — V8 generates code at runtime and `WebAssembly.instantiate` succeeds, unblocking the Phase 90b Claude Code TUI. Bump the kernel to `0.90.0` and ship the learning doc.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirrors the [Phase 89](./89-nodejs-tasks.md) / [Phase 90b](./90b-claude-code-tasks.md) style.)
>
> **Track A gates everything.** The kernel W^X v2 acceptance rule cannot be written until a host-side scout pins the exact syscall sequence V8 emits when PKU is available (mmap-RWX-then-tag vs. `pkey_mprotect` after RW), and confirms V8 in a **static-musl** build engages PKU at all rather than silently requesting unguarded RWX (which the kernel will rightly reject). Do not start Track B's W^X-touching work until A.1's contract note exists.
>
> **Hardware note.** The primary dev host (Ryzen 5 7600, Zen 4) advertises `pku` + `ospke`, so `M3OS_KVM=1` exposes real PKU to the guest. Gates must still handle the no-PKU case (TCG without the feature, other hosts) with skip-with-reason, and the jitless artifact remains the documented default everywhere.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Host-side scout: V8/PKU ground truth + the kernel contract note | 89 | Planned |
| B | Kernel PKU substrate (CPUID/CR4.PKE, PTE key bits, `pkey_*` syscalls, PKRU via XSAVE + signal frames) | A | Planned |
| C | W^X v2 policy + reporting (`sys_mprotect`/`sys_mmap` exception rule, `wx-violation` preservation, `m3ctl`) | A, B | Planned |
| D | `pku-smoke` kernel gate + the JIT `build_node` variant + `node-jit-smoke` | B, C | Planned |
| E | Docs + release closeout (learning doc, README rows, AGENTS.md, `0.90.0` bump) | A–D | Planned |

---

## Track A — Host-Side Scout: V8/PKU Ground Truth

### A.1 — Pin V8's PKU syscall contract (strace on PKU hardware) + the musl-static adoption question

**Files:**
- a scratch JIT build of Node 22 via `build_node` minus `--jitless` (host-runnable, like the 89 B.2 host-validation trick)
- `docs/roadmap/tasks/90a-memory-protection-keys-tasks.md` (this doc — the contract note lands here and in the design doc)

**Symbol:** `strace -f -e trace=mmap,mprotect,pkey_alloc,pkey_free,pkey_mprotect` over `node -e` JIT + WASM workloads; V8's runtime PKU detection path
**Why it matters:** everything downstream hangs on two facts only an experiment settles. (1) **The kernel contract:** does V8 `mmap` code space `PROT_NONE`/RW and then `pkey_mprotect` it with W+X under a key, or request RWX up front and rely on the key to mask write? The answer decides exactly which enforcement point in `sys_mmap`/`sys_mprotect`/`sys_pkey_mprotect` carries the v2 exception. (2) **Adoption:** V8's PKU support is runtime-detected; if the detection path doesn't engage in a **static musl** build (wrapper availability, glibc-specific assumptions), V8 falls back to plain RWX requests the kernel must keep rejecting — and the phase needs a V8 build-flag or patch answer before any kernel work is worth doing.

**Acceptance:**
- [x] A JIT-enabled static-musl node binary is built host-side and strace'd on the PKU host running (a) a JIT-hot JS loop and (b) `WebAssembly.instantiate` — the full ordered syscall sequence for code-space setup, code patching, and key usage is recorded verbatim in this doc. *(No rebuild was needed: Phase 89's `--v8-options=--jitless` is a runtime-flag default, and `node --no-jitless --turbofan --maglev --sparkplug` restores full JIT+WASM on the existing binary — verified via `%GetOptimizationStatus` = `kTurboFanned` and a working `WebAssembly.Instance`.)*
- [x] The musl-static adoption question is answered falsifiably: V8 does **not** call `pkey_alloc`/`pkey_mprotect` (zero pkey syscalls under strace with full JIT), for the three pinned reasons in the findings below, each with a validated port-side remedy.
- [x] The **kernel contract note** is written below — concrete enough that C.1's acceptance quotes it.
- [x] The fallback decision is recorded: on a no-PKU machine `pkey_alloc` returns `ENOSPC` → V8's `ThreadIsolation` stays disabled → V8 falls back to plain-RWX commits, which the kernel rejects → the JIT variant **aborts at first code-space commit** (it does not degrade). The jitless artifact remains the documented default; the JIT binary can still be run manually with `--jitless`. D.3's no-PKU arm is therefore skip-with-reason.

#### A.1 Findings (recorded 2026-06-12, Ryzen 5 7600 / Zen 4, `pku`+`ospke`, Linux 6.8)

**Observed syscall contract** (strace `-f -e trace=mmap,mprotect,munmap,pkey_*` of the Phase 89 static-musl node v22.22.3 with `--no-jitless --turbofan --maglev --sparkplug`, JIT-hot loop + `WebAssembly.instantiate`; V8 12.4.254.21):

```
mmap(hint, size, PROT_NONE, MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE, -1, 0)   # code-space reserve
mprotect(page, len, PROT_READ|PROT_WRITE|PROT_EXEC)                            # code page commit (PKU-disabled fallback!)
mprotect(range, len, PROT_NONE) / munmap(...)                                  # decommit / teardown
```

Zero `pkey_*` syscalls in the musl-static build — **and zero in a host glibc Node v24 either**. V8's PKU JIT write-protection is compiled in and on by default in this tree (`V8_HAS_PKU_JIT_WRITE_PROTECT=1`, `src/base/build_config.h:38` — Linux+x64 only, no GN arg; `V8_HEAP_USE_PKU_JIT_WRITE_PROTECT=true` since Node disables pointer compression; runtime flag `--memory-protection-keys` defaults **true**, `flag-definitions.h:759`), but is neutralized by three independent blockers:

1. **musl link gap.** V8 declares `pkey_alloc/free/mprotect/get/set` as `extern __attribute__((weak))` (`src/base/platform/memory-protection-key.cc:17`, `src/libplatform/default-thread-isolated-allocator.cc:21`) and null-checks them at runtime. musl defines none of them (and not the `PKEY_DISABLE_ACCESS`/`PKEY_DISABLE_WRITE` macros — `default-thread-isolated-allocator.cc:47` compiles to a `return -1` stub without them), so every guard short-circuits. **Remedy (D.2):** a small strong-symbol shim TU (`pkey_alloc/free/mprotect` via `syscall(2)`, `pkey_get/set` via `RDPKRU`/`WRPKRU` — they are not syscalls) linked into node, plus `-DPKEY_DISABLE_ACCESS=0x1 -DPKEY_DISABLE_WRITE=0x2` at V8 compile time.
2. **NodePlatform never provides the allocator.** `ThreadIsolation::Initialize()` requires `platform->GetThreadIsolatedAllocator()`; the `v8::Platform` default returns `nullptr` (`deps/v8/include/v8-platform.h:1068`) and Node's `NodePlatform` (`src/node_platform.{h,cc}`) never overrides it — this is why even glibc Node emits no pkey syscalls upstream. **Remedy (D.2):** port patch overriding `NodePlatform::GetThreadIsolatedAllocator()` to return V8 libplatform's `DefaultThreadIsolatedAllocator`.
3. **Kernel-version gate.** `KernelHasPkruFix()` (`default-thread-isolated-allocator.cc:26`) parses `uname()` release and requires ≥ 5.13 (a Linux PKRU-across-fork fix); m3OS reports `0.90.0` (`sys_linux_uname`, `kernel/src/arch/x86_64/syscall/mod.rs:14070`) and would be rejected. **Remedy (D.2):** port patch accepting the m3OS release string — justified because B.4 implements (and D.1 proves) correct PKRU inherit-on-clone/reset-on-exec semantics, which is what the Linux check guards.

**The PKU-engaged sequence** (source-pinned; what the kernel will see from the patched JIT variant):

```
uname(...)                                                          # KernelHasPkruFix (patched to accept m3OS)
pkey_alloc(flags=0, init_access_rights=PKEY_DISABLE_WRITE) = 1     # default-thread-isolated-allocator.cc:53
pkey_mprotect(trusted_data, len, PROT_READ, pkey=0)                # ThreadIsolation metadata, code-memory-access.cc:118
mmap(hint, size, PROT_NONE, MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE, -1, 0)    # code-space reserve (unchanged)
pkey_mprotect(page, len, PROT_READ|PROT_WRITE|PROT_EXEC, pkey=1)   # JIT/wasm code page commit+tag
                                                                    #   (wasm-code-manager.cc:1925, memory-allocator.cc:596→code-memory-access.cc:464)
WRPKRU (pkey_set, no syscall) per write window                      # code-memory-access-inl.h:255
mprotect(range, len, PROT_NONE) / munmap(...)                       # decommit / teardown (unchanged)
```

**Kernel contract note — the W^X v2 rule Track C implements verbatim:**

> 1. `sys_mmap` with `PROT_WRITE|PROT_EXEC` → rejected, unchanged from Phase 75.
> 2. `sys_mprotect` with `PROT_WRITE|PROT_EXEC` → rejected, unchanged from Phase 75.
> 3. `sys_pkey_mprotect(addr, len, prot, pkey)`:
>    a. with `pkey == 0` (or `pkey == -1`, the untag-Linux-alias) it behaves exactly like `sys_mprotect` — W+X rejected;
>    b. with a non-default `pkey` that is **allocated** and whose **alloc-time `init_access_rights` include `PKEY_DISABLE_WRITE` or `PKEY_DISABLE_ACCESS`**, and PKU active (CR4.PKE set), `PROT_READ|PROT_WRITE|PROT_EXEC` is **permitted** and the range's PTEs are tagged with the key; the kernel logs one positive `[wx] v2-guarded W+X mapping (pkey=N)` line per grant;
>    c. any other W+X request through `sys_pkey_mprotect` (unallocated key, key allocated with permissive init rights, PKU absent) → rejected with the same errno as (1)/(2).
> 4. No other syscall or fault path may produce a W+X PTE; the C.1 enforcement-point audit enumerates them.

**V8 quirk recorded for D.2/D.3:** because the Phase 89 binary bakes `--jitless` as an embedded default, V8's one-shot flag-implication pass latches `--no-opt`; plain `--no-jitless` re-enables WASM but *not* TurboFan (`%GetOptimizationStatus` = `kNeverOptimize`). The JIT variant must therefore drop `--v8-options=--jitless` at configure time (not rely on runtime `--no-jitless`); `node-jit-smoke`'s JIT-proof arm must assert actual optimization, not just WASM.

---

## Track B — Kernel PKU Substrate

### B.1 — CPUID detection + CR4.PKE on every core + PKRU in XCR0

**Files:**
- `kernel/src/arch/x86_64/cpuid.rs` (the Phase 57e XSAVE feature probe — extend with CPUID.7.0:ECX.PKU/OSPKE and the XSAVE component-9 surface)
- the BSP/AP CR4 init path (wherever CR4.OSXSAVE is set per-core today — CR4.PKE joins it)

**Symbol:** the `enable_xsave_state()`-adjacent per-core init; a new `pku_supported()` probe
**Why it matters:** PKU is per-core state: CR4.PKE must be set on the BSP **and every AP** (an AP without it ignores key bits — a silent per-core security hole), and PKRU only participates in XSAVE if component 9 is enabled in XCR0 and fits the sized XSAVE area the Phase 57e probe validates.

**Acceptance:**
- [ ] CPUID detection distinguishes PKU-absent / PKU-present and is exposed via a `pku_supported()` probe; all downstream paths (syscalls, W^X v2, gates) consult it rather than assuming.
- [ ] CR4.PKE is set on the BSP and every AP when supported (the same per-core pattern as SMEP/SMAP in the CPU-hardening work), and XCR0 enables component 9 with the XSAVE-area size re-validated against the probe.
- [ ] On a no-PKU CPU nothing changes: CR4.PKE stays clear, `pkey_alloc` returns `ENOSPC`/`EINVAL` per Linux semantics, and every existing gate is unaffected.
- [ ] `cargo xtask check` stays green.

### B.2 — PTE protection-key bits + page-table plumbing

**Files:**
- the kernel page-table manager (PTE flag composition — bits 59–62)
- `kernel-core` (host-testable key-bit encode/decode + per-process key accounting — new module, modeled on how `kernel_core::timerfd` factored the Phase 89 math)

**Symbol:** PTE bits 59–62 (`PKEY` field) in the page-table flag composition; `kernel_core::pkey` accounting
**Why it matters:** the key tag lives in each user PTE; mapping/protection changes must preserve or set it correctly, and remap paths (demand faults, COW, `mprotect` splits) must not silently drop a tag — a dropped tag on a JIT page is an unguarded W+X page.

**Acceptance:**
- [ ] PTE composition supports a 4-bit key field; default key 0 everywhere preserves existing behavior bit-for-bit.
- [ ] Every path that rewrites a tagged PTE (demand fault, COW, `mprotect` range split, fork copy) preserves the tag — enumerated and asserted, not assumed.
- [ ] The encode/decode + per-process allocation accounting (16 keys, key 0 reserved) lives in `kernel-core` with host unit tests passing under `cargo xtask check`.

### B.3 — `pkey_alloc` / `pkey_free` / `pkey_mprotect` syscalls

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (`nr` constants + dispatch + handlers; `pkey_mprotect` factored against `sys_mprotect` at `:11754`)
**Symbol:** `sys_pkey_alloc` (`PKEY_ALLOC=330`), `sys_pkey_free` (`PKEY_FREE=331`), `sys_pkey_mprotect` (`PKEY_MPROTECT=329`)
**Why it matters:** Linux-compatible numbers and semantics are what let musl's wrappers and V8's runtime detection work unmodified — the same compatibility bet every prior runtime phase made (`timerfd`, `eventfd2`, `epoll_pwait`).

**Acceptance:**
- [ ] All three syscalls are dispatched with the Linux numbers; `pkey_alloc` honors the `init_access_rights` argument (initial PKRU rights for the new key), rejects unknown flags, and returns `ENOSPC` when keys are exhausted.
- [ ] `pkey_mprotect` applies protection + tags the range's PTEs with the key (sharing the `sys_mprotect` VMA/permission logic, plus the W^X v2 rule from C.1); `pkey_free` rejects freeing key 0 and in-use semantics match Linux.
- [ ] TLB shootdown covers tagged-PTE updates on SMP (the existing IPI path), since a stale TLB entry with an old tag is an access-control bypass.
- [ ] Host-tested argument validation lives in `kernel-core`; `cargo xtask check` green.

### B.4 — PKRU across context switch and signal delivery

**Files:**
- the `switch_context` XSAVE save/restore path (Phase 57e/60 — validation + component-9 coverage)
- `kernel/src/signal.rs` (the Phase 86f signal-frame FPU save/restore — PKRU must ride it)

**Symbol:** the per-task XSAVE area (component 9); the signal-frame xstate save/restore
**Why it matters:** PKRU is *the* security control — a missed save/restore means thread A's open write-window leaks to thread B, silently. Because m3OS already XSAVEs around `switch_context` and signal frames, this is mostly enabling + falsifiably proving coverage, not new machinery; the proof must be a test that fails if PKRU leaks.

**Acceptance:**
- [ ] PKRU is included in the per-task XSAVE state (component 9 enabled end-to-end) and a two-thread test proves isolation: thread A opens a write window (`WRPKRU`), thread B concurrently faults writing the same key's page — the per-thread asymmetry that *is* the PKU security model.
- [ ] A signal delivered while a write window is open preserves PKRU across handler entry/return (the Phase 86f frame carries it), proven by a test.
- [ ] New tasks/threads start with the Linux-default PKRU (all non-zero keys access-denied... verified against Linux's documented init value) rather than inheriting a stale register.

---

## Track C — W^X v2 Policy + Reporting

### C.1 — The pkey-guarded exception in the W^X enforcement points

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_mprotect` W^X guard at `:11769`; the `sys_mmap` prot path at `:11135`+; the new `sys_pkey_mprotect`)

**Symbol:** the `prot & PROT_WRITE != 0 && prot & PROT_EXEC != 0` rejection (`mod.rs:11769`) — extended per the A.1 contract note
**Why it matters:** this is the phase's core security decision made code: unguarded W+X stays rejected exactly as Phase 75 shipped it, and the only path to a W+X mapping is the A.1-pinned V8 contract under a non-default key whose default rights deny write. The rule must be enforced at *every* point a W+X mapping could arise (mmap, mprotect, pkey_mprotect, remap), or the invariant is theater.

**Acceptance:**
- [x] The v2 rule implements the A.1 contract note verbatim (quoted in the code comment on `wx_decision`), permitting W+X only with a non-default key allocated with write-deny default rights; plain `mprotect`/`mmap` W+X requests still return the Phase 75 rejection.
- [x] The enforcement-point audit is recorded (see **C.1 implementation note** below; the same audit is the doc comment on `wx_decision`): every syscall/fault path that can produce a W+X PTE is enumerated with where the v2 check sits.
- [ ] The `wx-violation` gate (`SMOKE:wx-violation:PASS`) passes — the v1 binary's expectations (W+X via `mprotect` → EINVAL) hold on both PKU and no-PKU configurations, and the revision-round-1 arm (`mmap(PROT_READ|PROT_WRITE|PROT_EXEC)` → EINVAL) confirms the mmap(W+X) bypass is closed. *(Plain `mprotect`/`mmap` W+X never satisfies the v2 exception — `mprotect` carries the `Preserve` decision; `mmap` carries no pkey, so it is rejected at mmap entry by the same `wx_decision` rule; coordinator runs the QEMU gate.)*

**C.1 implementation note (recorded 2026-06-13):**

The W^X v2 rule is the single decision point `wx_decision(prot, key_decision, table)` in `kernel/src/arch/x86_64/syscall/mod.rs`, called once from `mprotect_worker` (the shared body of both `sys_mprotect` and `sys_pkey_mprotect`) before any page-alignment validation, VMA walk, or PTE mutation. The pure accept/reject predicate is `kernel_core::pkey::wx_v2_permits(table, key_decision, pku_active)` (host-tested — `wx_v2_*` tests in `kernel-core/src/pkey.rs`). It permits a W+X request iff **all** of: the request is a `pkey_mprotect` with a non-default, currently-allocated key (`PkeyMprotectKey::Tag(k)`); the key's alloc-time `init_access_rights` deny write (`PkeyTable::denies_write` — `PKEY_DISABLE_WRITE` or `PKEY_DISABLE_ACCESS`); and `pku_usable()` is true (CR4.PKE + XSAVE component 9 live). On a grant it logs exactly one `[wx] v2-guarded W+X mapping (pkey=N)` line; the v1 rejection path returns `NEG_EINVAL` (Phase 75 errno) with no log.

**Enforcement-point audit — every path that can introduce a W+X PTE, and where it is gated:**

| Path | Composes W+X from | Gate |
|---|---|---|
| `sys_mprotect` | `mprotect_worker(Preserve)` | `wx_decision` → `Rejected` for W+X (clause 2). `Preserve` never reaches `Tag`, so v1 unchanged. |
| `sys_pkey_mprotect` | `mprotect_worker(classified key)` | `wx_decision` (clauses 3.a/3.b/3.c). The **only** `GuardedV2` producer. |
| `sys_mmap` (anon + file-backed) | VMA recorded with `PKEY_DEFAULT`; PTEs composed key-0 | **W+X rejected at mmap entry (contract clause 1), same errno as `mprotect` (`NEG_EINVAL`, -22)** — `mmap` carries no pkey argument, so it is the key-0 / non-`Tag` case of the `wx_decision` rule, applied eagerly in both `sys_linux_mmap` (anon, before file-backed dispatch / MAP_FIXED / VMA recording) and `sys_mmap_file_backed` (before any allocation/mapping). Without this, a W+X prot would demand-fault / eager-map into a LIVE unguarded W+X PTE tagged key 0 that never touches the guard. `mmap` cannot express a v2 grant; accepted (non-W+X) ranges still record `PKEY_DEFAULT`. |
| Demand-fault PTE composition (`compose_user_pte_flags`) | VMA `prot` + VMA `pkey` | A VMA only carries a non-default `pkey` after a `pkey_mprotect(Tag(k))` that `wx_decision` **already permitted**; a rejected `pkey_mprotect` never tags the VMA, so a fault cannot resurrect a refused W+X mapping. |
| ELF segment load / eager file-backed mmap | `p_flags`/`prot`, key 0 only (`mm/pkey.rs` audit rows) | Key 0 is never write-deny (`denies_write(0) == false`), so even a W+X request there falls to `Rejected`. No way to express a v2 key. |
| CoW fork copy / CoW fault resolution | parent PTE's whole flag word | Carries the key field verbatim and only toggles `WRITABLE`/`BIT_9`; never *introduces* a new W+X combination (an R-X page stays non-writable; a writable page stays non-executable). |

So a W+X PTE can only be *introduced* through `mprotect_worker` (gated by `wx_decision`); the `mmap` entry points reject W+X up front with the same `wx_decision` rule and the same errno (`NEG_EINVAL`, -22), so no `mmap` path can install an unguarded W+X PTE either. The eager paths compose key 0 exclusively and therefore cannot smuggle a write-deny key past the guard. *(Revision round 1: the original audit incorrectly claimed `mmap` "cannot smuggle a v2 grant so it's fine" and left the W+X-at-mmap-entry reject unimplemented — a latent Phase 75 hole, since the `wx-violation` gate only ever did `mmap(RW)` then `mprotect(WX)`. Closed by rejecting W+X at both mmap entry points + a new `mmap(W+X)` arm in `userspace/wx-violation/src/main.rs`.)*

### C.2 — `m3ctl mitigations status` reports W^X v2 + PKU state

**Files:**
- `kernel/src/mitigations.rs` (the Phase 84 policy/report surface)
- `userspace/m3ctl/src/main.rs` (`dispatch_mitigations_status` at `:132` + `format_mitigations`)

**Symbol:** a new W^X/PKU line in the `SYS_MITIGATIONS_STATUS` report
**Why it matters:** Phase 84 established that security posture must be *reportable*, not implicit; an operator must be able to see whether W^X is v1 or v2 and whether PKU is active on this boot — and the `mitigations-status-smoke` gate is the cheap regression hook.

**Acceptance:**
- [ ] `m3ctl mitigations status` prints the W^X policy line (v1/v2, PKU present/active, keys in use) on both PKU and no-PKU boots.
- [ ] The `mitigations-status-smoke` gate's expectations are extended to match (and stay green).

---

## Track D — Gates + the JIT Node Variant

### D.1 — `pku-smoke`: kernel-level PKU regression gate

**Files:**
- a new userspace test binary (ramdisk-embedded, following the Adding-a-New-Userspace-Binary checklist) exercising the B-track substrate
- `xtask/src/main.rs` (gate wiring + a `SMOKE_EXIT_*` const; AGENTS.md row `M3OS_PKU_REGRESSION=1`; `.githooks/pre-push` block)

**Symbol:** `cmd_pku_smoke`; the test binary's `PKU_SMOKE:*:ok` serial sentinels
**Why it matters:** the substrate needs its own falsifiable gate independent of V8: key alloc/free lifecycle, tag-then-fault, per-thread asymmetry (B.4's test), signal-window preservation, and the W^X v2 accept/reject matrix — each a sentinel, so a regression names the broken layer instead of surfacing as a V8 crash three layers up.

**Acceptance:**
- [ ] Sentinels cover: alloc/free lifecycle (+`ENOSPC` exhaustion), PKRU-denied write faults (and is reported as the right signal), per-thread asymmetry, signal-frame preservation, v2 accept (guarded W+X) and reject (unguarded W+X) both ways.
- [ ] On a no-PKU configuration the gate prints `SKIP (reason: no PKU — …)` for the hardware-dependent arms and still asserts the v1 rejections; wired opt-in via `M3OS_PKU_REGRESSION=1` in AGENTS.md + pre-push.

### D.2 — `build_node` JIT variant under a distinct content key

**Files:**
- `xtask/src/port_build.rs` (`build_node` — a JIT configuration branch; `compute_port_key_inner` folds the variant so jitless/JIT can never serve each other's cache hits)
- `ports/lang/node/Portfile` (variant declaration)

**Symbol:** the `build_node` variant flag (e.g. `M3OS_NODE_JIT=1`) minus `--v8-options=--jitless`, plus whatever A.1 concluded V8 needs to engage PKU
**Why it matters:** the jitless `.m3pkg` is a landed, gated artifact — it must remain byte-identical as the default (Phase 89's `node-smoke` keeps passing untouched), while the JIT variant is a *second* sealed artifact with its own content key that Phase 90b's Claude Code consumes.

**Acceptance:**
- [ ] The variant builds and seals under a distinct content key (the key folds the JIT/jitless choice); a jitless build after a JIT build is still a pure pkgcache hit and vice versa — no cross-contamination.
- [ ] The default `cargo xtask port build node` output and the existing `node-smoke` gate are unchanged (jitless remains the default artifact).
- [ ] The JIT binary is still fully static (no `PT_INTERP`, `assert_node_layout` passes) — only the V8 code-generation model changed.

### D.3 — `node-jit-smoke`: JIT + WASM on m3OS with no unguarded RWX

**Files:**
- `xtask/src/main.rs` (new gate modeled on `cmd_node_smoke` at `:14987`, booting an image bundling the JIT variant; AGENTS.md row `M3OS_NODE_JIT_REGRESSION=1`; pre-push block)

**Symbol:** `cmd_node_jit_smoke`; sentinels `NODE_JIT_OK` (a measured JIT-hot loop — e.g. asserting V8 reports optimized code or a wall-clock delta vs. jitless), `NODE_WASM_OK` (`WebAssembly.instantiate` of a trivial module succeeds and executes — the exact capability the 90b TUI needs)
**Why it matters:** this is the phase's falsifiable payoff: V8 generating real machine code at runtime on m3OS under the v2 invariant, and WASM — the thing jitless permanently ruled out — executing. The negative arm matters as much: the serial log must show **no** unguarded-RWX grant, proving the JIT ran on the guarded path rather than a policy hole.

**Acceptance:**
- [ ] Under `M3OS_KVM=1` on a PKU host: boot → `pkg install` the JIT variant → `NODE_JIT_OK` + `NODE_WASM_OK` over serial.
- [ ] The gate asserts the absence of any unguarded W+X grant in the kernel log (a positive "v2-guarded mapping" log line is present; the v1-rejection error line is absent), and `pku-smoke`'s reject arms stay green in the same boot.
- [ ] No-PKU configurations skip-with-reason per the A.1 fallback decision; the gate is opt-in (`M3OS_NODE_JIT_REGRESSION=1`) with a clang-class timeout.

---

## Track E — Documentation + Release Closeout

### E.1 — Create the Phase 90a learning doc

**Files:**
- `docs/90a-memory-protection-keys.md` (new — aligned learning-doc template at `docs/appendix/doc-templates.md:167`–`214`)
- `docs/README.md` (link in the `### Phase-Aligned Learning Docs` table after the Phase 89 row)

**Symbol:** the aligned learning-doc header block (`**Aligned Roadmap Phase:** Phase 90a`)
**Why it matters:** the phase's learning payload is unusually rich — the PKU hardware model, W^X v1→v2 as a case study in evolving (not abandoning) an invariant, PKRU-on-XSAVE, V8's runtime adoption, and the real-OS comparisons (Linux pkeys, OpenBSD `wxallowed`, Apple `MAP_JIT`, Windows ACG).

**Acceptance:**
- [ ] `docs/90a-memory-protection-keys.md` exists with all aligned-template sections, records the A.1 contract note and the v2 rule verbatim, and explains the per-thread write-window model in learner terms.
- [ ] Linked from `docs/README.md`'s learning-docs table; cross-links the 90a design + task docs and the 90b consumer.

### E.2 — Update the roadmap README rows, the design docs, and AGENTS.md

**Files:**
- `docs/roadmap/README.md` (the 90a row + the 90b row's dependency wording; the Mermaid post-1.0 graph)
- `docs/roadmap/90a-memory-protection-keys.md` + `docs/roadmap/90b-claude-code.md` (Status flips + cross-links on landing)
- `AGENTS.md` (the `pku-smoke`/`node-jit-smoke` regression rows; the CPU-hardening capability bullet gains the W^X v2/PKU clause on landing)

**Symbol:** the README Status/Tasks cells; the AGENTS.md "CPU hardening" bullet
**Why it matters:** the phase index and the always-loaded inventory must reflect the new invariant; per the keep-it-small policy this *rewrites* the existing CPU-hardening bullet (W^X v2 + PKU is the same capability class as SMEP/SMAP/W^X) rather than adding a new one.

**Acceptance:**
- [ ] The 90a README row's Tasks cell links this doc (done at authoring time); Status flips on landing; the Mermaid graph shows 89 → 90a → 90b.
- [ ] AGENTS.md gains the two regression rows; the CPU-hardening bullet folds in W^X v2/PKU on landing; no new capability bullet.

### E.3 — Bump kernel crate `0.89.0` → `0.90.0`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.90.0"` (currently `0.89.0` at `kernel/Cargo.toml:3`)
**Why it matters:** 90a is the kernel-heavy half of the Phase 90 pair and takes the minor bump; 90b (userspace/packaging) takes `0.90.1` — mirroring how the 86a–f sub-phases shared the 0.86.x line.

**Acceptance:**
- [ ] `kernel/Cargo.toml:3` reads `version = "0.90.0"` (+ `Cargo.lock`), `AGENTS.md` line 7 updated; `cargo xtask check` exit 0.
- [ ] The `pku-smoke`/`node-jit-smoke` boot banner reports `0.90.0`.

---

## Documentation Notes

- **What changed relative to the previous phase.** Phase 89 shipped Node jitless because modern V8 is PKU-or-RWX and m3OS had neither; its A.3 explicitly tracked "PKU-backed JIT" as the follow-up. This phase is that follow-up: the W^X invariant is *strengthened into v2*, not relaxed — unguarded RWX remains rejected everywhere, and the `wx-violation` gate is the unchanged-behavior proof.
- **What replaces what.** Nothing is replaced at runtime: the jitless `.m3pkg` and `node-smoke` are untouched defaults; the JIT variant is additive under its own content key. The only modified existing behavior is the W^X guard gaining the pkey-guarded exception, audited at every enforcement point.
- **Honesty / explicit non-goals.** No PKS, no pkeys beyond the V8 JIT use, no ptrace/coredump PKRU surfaces, no ARM POE. The A.1 scout may conclude V8-on-musl-static needs a patch or flag to engage PKU — that finding gates the phase and must be recorded either way. No-PKU hardware keeps the full jitless story.
- **Prefer exact targets.** Reference `sys_pkey_mprotect`, the `mod.rs:11769` W^X guard, XSAVE component 9, PTE bits 59–62, and `M3OS_NODE_JIT` by name.
- **Cross-links.** Companion design doc: [Phase 90a](../90a-memory-protection-keys.md). Consumer: [Phase 90b — Claude Code](./90b-claude-code-tasks.md) (the TUI depends on D.2/D.3). Invariant predecessor: Phase 75 (W^X). State machinery: Phase 57e/60 (XSAVE), 86f (signal frames). Reporting: Phase 84 (`m3ctl mitigations status`).
