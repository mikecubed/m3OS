# Phase 90a - Memory Protection Keys (PKU) and JIT-Capable V8

**Status:** ✅ Complete — `pku-smoke` + `node-jit-smoke` PASS under KVM (PR #246); V8 JIT + WASM run under the W^X v2 invariant on the guarded PKU path
**Source Ref:** phase-90a
**Depends on:** Phase 57e/60 (per-task XSAVE save/restore) ✅, Phase 75 (W^X enforcement + the `wx-violation` gate) ✅, Phase 84 (mitigations policy + `m3ctl mitigations status` reporting surface) ✅, Phase 89 (Node.js — the jitless baseline this phase upgrades) ✅
**Builds on:** Extends the Phase 75 W^X invariant to a hardware-enforced per-thread "W^X v2" using x86 Memory Protection Keys, so V8's PKU-guarded JIT can run without ever granting an unguarded writable+executable mapping
**Primary Components:** kernel paging (PTE protection-key bits), syscall layer (`pkey_alloc`/`pkey_free`/`pkey_mprotect`), per-task PKRU state (XSAVE component 9), `kernel-core` pkey accounting, `build_node` JIT variant, `pku-smoke` + `node-jit-smoke` gates, `m3ctl mitigations status`

## Milestone Goal

m3OS supports x86 Memory Protection Keys end-to-end — allocation, page tagging, per-thread PKRU access control — and uses them to evolve the W^X policy from "no mapping may be writable and executable" to "no mapping may be writable and executable *unless guarded by a non-default protection key whose default policy denies write*." On that substrate, a JIT-enabled Node.js variant runs with full V8 code generation and working WebAssembly, unblocking the Phase 90b Claude Code TUI without weakening the security story.

## Why This Phase Exists

Phase 89 shipped Node.js jitless because m3OS forbids RWX mappings and modern V8 removed its `mprotect` RW↔RX write-protected-code-memory path — today V8's JIT is PKU-or-RWX. Jitless was the right bring-up call, but it permanently rules out runtime WebAssembly (Claude Code's TUI layout engine is `yoga.wasm`) and leaves interpreter-only performance on the table.

The wrong fix is relaxing W^X — it is one of the few hardening invariants m3OS actually demonstrates and regression-gates. The right fix is the one real OSes and V8 converged on: Memory Protection Keys, where JIT code pages carry a protection key, the default per-thread PKRU policy denies write access to that key, and only the compiling thread temporarily enables write via `WRPKRU` around a code patch. The invariant survives — it just moves from the page table alone to page table + per-thread key rights, enforced by hardware on every access.

This was already a tracked follow-up: Phase 89 A.3 recorded "PKU-backed JIT is a tracked follow-up (needs a kernel MPK/`pkey_mprotect` story)." This phase is that story.

## Learning Goals

- Understand x86 Memory Protection Keys: PTE key bits 59–62, the PKRU register, `RDPKRU`/`WRPKRU`, and why key rights are per-thread while page tags are per-mapping.
- See how a security invariant evolves without being abandoned: W^X v1 (page-table-only) to W^X v2 (page-table + hardware key rights), and what the kernel must check at each enforcement point.
- Learn why modern JIT compilers need this model — the writer/executor split, and why per-thread write windows beat process-wide `mprotect` flips both for security and for performance.
- Understand how thread-local register state (PKRU) rides the existing XSAVE save/restore machinery and what signal frames must preserve.
- See how a userspace runtime (V8) detects and adopts a kernel feature at runtime, and what happens on hardware or kernels without it.

## Feature Scope

### Kernel PKU substrate

CPUID detection (PKU/OSPKE), CR4.PKE enablement on every core, the PTE protection-key bits, the three Linux-compatible syscalls (`pkey_alloc`, `pkey_free`, `pkey_mprotect`), per-process key accounting, and per-task PKRU via the existing XSAVE area (component 9) including signal-frame preservation.

### W^X policy evolution (v2)

The Phase 75 guard in `sys_mprotect`/`sys_mmap` rejects `PROT_WRITE|PROT_EXEC` outright. v2 keeps that rejection for unguarded mappings and adds the one principled exception: a W+X mapping is permitted if and only if it is tagged with a non-default protection key whose default PKRU policy denies write. The exact contract (which syscall sequence V8 emits, and therefore where the kernel must check) is pinned by a host-side scout before implementation. `m3ctl mitigations status` reports the policy and whether PKU is active.

### JIT-enabled Node.js variant

A second `build_node` configuration without `--jitless`, sealed under its own content key. The jitless `.m3pkg` from Phase 89 remains the default artifact and the documented fallback on hardware or configurations without PKU; the JIT variant is what Phase 90b's Claude Code TUI depends on.

### Validation gates

A kernel-level `pku-smoke` (key allocation, page tagging, PKRU write-denial faulting, per-thread asymmetry) and a `node-jit-smoke` (V8 actually JITs, `WebAssembly.instantiate` succeeds, and no unguarded RWX mapping is ever granted). The existing `wx-violation` gate must stay green unchanged — v1 behavior for non-pkey mappings is preserved exactly.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Host-side V8/PKU scout before kernel work | The kernel W^X v2 acceptance rule cannot be written without knowing the exact mmap/mprotect/pkey sequence V8 emits — and whether V8-on-static-musl engages PKU at all |
| W^X v1 preserved for unguarded mappings | The phase exists to strengthen the invariant; any regression in the existing `wx-violation` gate defeats its purpose |
| PKRU in context switch and signal frames | A missed PKRU save/restore is a silent cross-thread security hole, not a crash |
| Graceful no-PKU fallback | The jitless artifact must remain fully supported; PKU absence (TCG without the feature, older hardware) must degrade, not break |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| XSAVE baseline | Phase 57e/60 per-task XSAVE covers enabling and context-switching component 9 (PKRU) | Add the XCR0/XSAVE-area work here |
| W^X baseline | Phase 75's enforcement points are the complete set of places a W+X mapping could be created | Audit and close any unchecked path in this phase |
| V8 adoption | V8 in the static-musl Node build actually selects PKU at runtime (not silently falling back to RWX requests the kernel rejects) | Add the V8 build/flag work or patch here |
| Reporting baseline | `m3ctl mitigations status` (Phase 84) can carry the W^X v2 / PKU line | Add the reporting plumbing here |

## Important Components and How They Work

### PTE protection keys + CR4.PKE

Each user page-table entry carries a 4-bit key (bits 59–62). With CR4.PKE set, every user-mode access consults PKRU: two bits per key (access-disable, write-disable). The kernel tags JIT code pages with an allocated key; PKRU then makes those pages non-writable for every thread except one that has explicitly opened a write window.

### PKRU as per-task state

PKRU is XSAVE state component 9. m3OS already does per-task `xsaveopt64`/`xsave64` around `switch_context` (Phase 57e/60), so enabling the component in XCR0 makes save/restore ride the existing machinery; the signal-frame FPU path (Phase 86f) must preserve it the same way.

### The syscall triple

`pkey_alloc` (allocate a key + initial rights), `pkey_free` (release), `pkey_mprotect` (mprotect + tag the range with a key) — Linux numbers 330/331/329, so musl's wrappers and V8's runtime detection work unmodified.

### W^X v2 enforcement

`sys_mprotect`/`sys_mmap` keep rejecting unguarded `PROT_WRITE|PROT_EXEC`. The pkey-guarded exception is granted only where the scout-pinned V8 contract requires it, and only with a non-default key. The decision and its enforcement points are recorded in the learning doc.

### The JIT node variant

`build_node` gains a JIT configuration (drop `--jitless`) sealed under a distinct content key. Default images and `node-smoke` keep the jitless artifact; `node-jit-smoke` and Phase 90b consume the JIT variant.

## How This Builds on Earlier Phases

- Extends Phase 75's W^X from a page-table-only invariant to page-table + per-thread hardware key rights, without changing behavior for any existing binary.
- Reuses Phase 57e/60's XSAVE save/restore for PKRU (component 9) rather than adding bespoke context-switch state.
- Reuses Phase 84's mitigations reporting surface (`m3ctl mitigations status`) for the W^X v2 / PKU policy line.
- Upgrades Phase 89's Node port with a second build configuration; the jitless artifact and its gates remain untouched as the fallback.

## Implementation Outline

1. Host-side scout: strace a JIT-enabled static-musl Node on PKU hardware; pin the exact syscall contract and confirm V8 engages PKU under musl.
2. Kernel substrate: CPUID/CR4.PKE per-core, PTE key bits, the three syscalls, per-process key accounting (host-tested in `kernel-core`), PKRU via XSAVE + signal frames.
3. W^X v2: implement the pkey-guarded exception at the scout-pinned enforcement points; keep `wx-violation` green unchanged; add the `m3ctl` reporting line.
4. `pku-smoke`: kernel-level gate proving tagging, faulting, and per-thread asymmetry.
5. `build_node` JIT variant under a distinct content key; `node-jit-smoke` proving JIT + WASM with no unguarded RWX.
6. Learning doc, README rows, AGENTS.md gate rows, kernel `0.89.0` → `0.90.0`.

## Learning Documentation Requirement

- Create `docs/90a-memory-protection-keys.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the PKU hardware model, the W^X v1 → v2 evolution, the PKRU/XSAVE integration, the V8 adoption mechanics, and the no-PKU fallback story.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/roadmap/README.md` (this phase's row + the Phase 90b dependency), `docs/README.md`, and `docs/claude-code-roadmap.md` when revived (Phase 90b E.2).
- Update `docs/security-and-mitigations`-class docs and the Phase 75 W^X description wherever the v1 invariant is stated as absolute.
- When the phase lands, bump `kernel/Cargo.toml` to `0.90.0` (Phase 90b then takes `0.90.1`).

## Acceptance Criteria

- `pkey_alloc`/`pkey_free`/`pkey_mprotect` are implemented with Linux-compatible numbers and semantics; a userspace test demonstrates a PKRU-denied write faulting while another thread with write rights succeeds on the same page.
- The existing `wx-violation` gate passes unchanged: unguarded `PROT_WRITE|PROT_EXEC` is still rejected everywhere.
- PKRU is correctly saved/restored across context switches and signal delivery (a falsifiable test, not an assumption).
- The JIT node variant runs on m3OS: V8 generates code at runtime, `WebAssembly.instantiate` succeeds, and the serial log shows no unguarded-RWX grant.
- PKU absence degrades gracefully: on a no-PKU configuration the JIT variant refuses cleanly (or V8 falls back) and the jitless artifact remains the documented default.
- `m3ctl mitigations status` reports the W^X v2 policy and PKU state.

## Companion Task List

- [Phase 90a Task List](./tasks/90a-memory-protection-keys-tasks.md)

## How Real OS Implementations Differ

- Linux has shipped `pkey_*` since 4.9 with arch-generic plumbing, key inheritance semantics across fork/exec/signal that took years to settle, and PKS (supervisor keys) for kernel self-protection — m3OS scopes to user keys for the V8 use case only.
- OpenBSD enforces W^X without PKU, using per-binary opt-out (`wxallowed` + an ELF marker) instead; Apple Silicon solves the same JIT problem with `MAP_JIT` + per-thread `pthread_jit_write_protect_np`; Windows uses ACG. The per-thread-write-window idea is convergent across all of them.
- Real kernels must handle PKRU interactions with ptrace, coredumps, and userfaultfd; m3OS has none of those surfaces yet.

## Deferred Until Later

- PKS (supervisor protection keys) for kernel self-protection
- Protection-key uses beyond V8 (e.g., guarding crypto key material pages)
- Multi-key policy management beyond the single JIT key V8 needs
- PKU-aware `ptrace`/debugger integration
- Non-x86 equivalents (ARM POE)
