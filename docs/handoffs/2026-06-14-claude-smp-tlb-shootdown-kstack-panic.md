---
status: SUBSTANTIALLY FIXED — root-caused; Tracks A+B+C landed 2026-06-14. Track A
  (NMI-on-IST) removed the whole-machine KERNEL PANIC (a wedged core now ACKs TLB
  shootdowns on a clean per-core IST stack). Track B (mark a core offline when it
  halts — done in `hlt_loop`, the single chokepoint all dead-end paths funnel through)
  makes future shootdowns exclude a dead core for free. Track C (degrade the shootdown
  ack-timeout instead of `panic!` — re-NMI the laggards, then mark-offline + continue;
  ack tracking converted from a decrementing `SHOOTDOWN_PENDING` count to a per-round
  idempotent `SHOOTDOWN_ACK` bitmap so the degrade path has no underflow / double-
  decrement / cross-round-corruption hazard) makes the timeout non-fatal even for a
  genuinely-dark core. A+C together make the `tlb.rs` timeout both effectively
  unreachable AND non-fatal. Validated: `cargo xtask check` + `smoke-test` (boots clean
  on the default SMP=4; all fork/exec/mmap/dynlink shootdown traffic acks correctly
  through the new bitmap) + `regression` (11/11, incl. `fork-overlap` CoW shootdowns).
  Only Track D (root-cause the originating kstack overflow / recover the wedged task
  instead of halting it) remains OPEN — deeper origin work, not a machine-kill. The
  `M3OS_SMP=1` workaround is no longer required for survivability; it remains the
  fastest path for single-core determinism.
branch: feat/phase-90b-claude-code
repro-commit: bb2ee97f  # bug predates this; present since the SMP+PKU era. NOT caused by recent work.
date: 2026-06-14
component: kernel/arch/x86_64 (IDT/IST exception plumbing) + kernel/smp (TLB shootdown
  liveness model) + kernel/task (kstack overflow / fault recursion). Workload trigger:
  multi-core Node/V8 (heavy mprotect/pkey_mprotect/demand-fault churn under PKU).
related:
  - docs/handoffs/2026-06-05-4gib-smp-panic-corrupted-output.md   # SAME crash family (4 GiB + --kvm + SMP=4 panic); this doc root-causes it
  - docs/handoffs/2026-05-24-4gib-pci-hole-vga-mapping.md         # introduced NMI-delivered shootdown + the spin-timeout-panic; left a residual SMP hang open
  - docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md   # scheduler-lock-held-across-a-fault class
  - docs/roadmap/tasks/90b-claude-code-tasks.md                   # the pre-flagged multi-core recursive-kernel-fault race the gates dodge via -smp 1
  - docs/roadmap/90a-memory-protection-keys.md                    # the SMP-PKU gap; pkey_mprotect is a top shootdown generator
  - kernel/src/arch/x86_64/interrupts.rs:685                      # page_fault: NO IST (linchpin)
  - kernel/src/arch/x86_64/interrupts.rs:699                      # NMI (TLB-shootdown delivery): NO IST (linchpin)
  - kernel/src/arch/x86_64/interrupts.rs:688-692                  # double_fault: the ONLY handler with an IST
  - kernel/src/arch/x86_64/gdt.rs:51                              # DOUBLE_FAULT_STACK is a single GLOBAL static (not per-core)
  - kernel/src/smp/tlb.rs                                         # was the fatal panic (old line 176); Track C replaced it with re-NMI + degrade on the SHOOTDOWN_ACK bitmap
  - kernel/src/smp/boot.rs:545                                    # is_online: was the ONLY write site (→ true). Track B added `false` writes in hlt_loop (lib.rs)
artifact: m3os.log (project root, ~172 KB; control-byte corrupted — read with `grep -a` / `sed`, plain grep treats it as binary)
---

## TL;DR

A developer ran `cargo xtask run-gui --kvm --fresh` with `M3OS_WITH_CLAUDE` (default
**4 cores**) and launched `claude`. The QEMU framebuffer vanished — the kernel
**panicked**. Root cause is **not** Claude/Node: it is an SMP-robustness gap that
Node/V8's heavy address-space churn reliably tickles. The causal chain is fully
root-caused below; the one-line summary:

> One core overflows its kernel stack and re-faults (because **#PF has no IST**) into
> a `hlt_loop` with interrupts off and the scheduler lock held → it can no longer
> service the **NMI** that delivers cross-core TLB shootdowns (**NMI also has no
> IST**) → a sibling core doing a routine `mprotect`/`mmap` spins 500 ms waiting for
> the dead core's ACK and **`panic!`s the whole machine** at `tlb.rs:176`, because the
> shootdown protocol **has no way to detect or exclude a wedged core** (`is_online`
> is write-once).

**Workaround now:** `M3OS_SMP=1 M3OS_WITH_CLAUDE=1 cargo xtask run-gui --kvm --fresh`.
Single-core removes both the cross-core shootdown (the fatal panic path can't fire)
and the concurrent address-space mutation that triggers the cascade. This is why all
heavy-Node gates pin `-smp 1` (see `claude-smoke`, `main.rs:16203`).

**Highest-leverage fix:** give `#PF`, `#NMI` (and `#GP`) their own **per-core** IST
stacks. That alone converts the wedge into a serviceable state so the ACK still
fires and the panic never happens.

## Symptom / evidence (from `m3os.log`, 2026-06-14)

Read with `grep -a` / `sed` — the multi-core panic dump interleaves serial bytes and
plain `grep` silently treats the file as binary.

- Launch (`m3os.log:5`): `xtask run-gui --kvm --fresh`; `M3OS_WITH_CLAUDE` bundled
  (`:979` `bundled claude-code.m3pkg … into /usr/pkg`). SMP=**4** (`:1101`
  `[smp] 4 core(s) discovered`; APs 1/2/3 online `:1131/:1137/:1143`). PKU enabled on
  every AP (`[sec] AP … CR4.PKE enabled`).
- Claude ran: `/root/.claude.json` config writes (`:1593-1600`), `rg` mapped as
  pid 51 (`:1606-1611`, a static-PIE `ET_DYN`), SIGCHLD (signal 17) delivered to the
  node process pid 40 (`:1612`).
- **Benign noise** — repeated `[user_mem] copy_from_user/copy_to_user: address-space
  generation divergence … concurrent or untracked mapping mutation detected`
  (`:1613-1616`). See "Layer 0" — this is a log-only correctness guard firing under
  V8's mmap/mprotect churn; it is **not** the cause.
- **Originating fault** (core 1, the node process pid 40 / task_idx 28):
  `[int] KERNEL STACK OVERFLOW: kstack slot 10 guard page hit at 0xffff8080000aa000`
  → `[int] kernel page fault: addr=0xffff8080000aa000 err=0x0`. The PTE walk confirms
  a real guard-page hit: `[pf-diag] … PT[170] flags=PageTableFlags(0x0)` (unmapped).
- **Cascade**: `[int] RECURSIVE KERNEL PAGE FAULT on core 1 — cascade halted
  (cr2=0x0 …)`. Crash dump: `task_idx=28 on core 1 / scheduler lock held -- skipping
  task dump`. Core 1 is now wedged with the scheduler lock held and IF=0.
- **Fatal blow** (core 2): `[tlb] tlb_shootdown_range stuck >500ms:
  SHOOTDOWN_PENDING=1 (of 3 targets), my_core=2, range=0x20173b9000..0x20173bb000,
  … recipients=[remote_mask=0x…b]` then `KERNEL PANIC at kernel/src/smp/tlb.rs:176`.
  The per-core liveness dump is decisive:

  | core | tlb-ipi serviced Δ | LAPIC timer Δ | reading |
  |---|---|---|---|
  | 0 | 0 | **+498** | alive, just wasn't a stale-TLB target needing action |
  | **1** | **0** | **0** | **FROZEN — IF=0 in hlt_loop; never serviced the NMI** |
  | 2 | 0 | +50 | the waiter (issuing the shootdown) |
  | 3 | 0 | +6 | alive |

  Core 1's timer **and** ipi counters are both Δ0 → it is the wedged core that never
  ACKed. Machine halts → framebuffer disappears.

> Note: the saved `InterruptStackFrame` reports `code_segment rpl:Ring0` with
> `instruction_pointer=0x10000bb7d41` (a userspace PIE address). A ring-0 CS with a
> userspace RIP is impossible in a clean frame — it confirms the exception frame was
> **stomped by the stack overflow itself**, so individual register/RIP values in the
> dump are unreliable. The *mechanism* (verified below) is reliable; the exact first
> faulting instruction is not recoverable from this log.

## Root-cause analysis (three layers; all code facts verified)

### Layer 0 — the `copy_*_user` divergence warnings are benign (ruled out)

`copy_from_user`/`copy_to_user` (`kernel/src/mm/user_mem.rs:41-198`) snapshot the
per-address-space generation counter (`mm/mod.rs:45-50`, `AtomicU64`), copy per 4 KiB
chunk under a *per-chunk* `lock_page_tables()`, then post-hoc compare the generation.
On mismatch they **only `log::warn!`** (`user_mem.rs:307-334`) — no retry, no loop,
no re-fault, no abort; the copy already succeeded via per-page validation. It is a
bounded, fire-and-forget diagnostic. It **cannot** recurse or spin and did not cause
the crash; it is correlated only because the same V8 churn that floods the warnings
also maximizes the TLB-shootdown traffic that Layer 2 fails on. *Do not "fix" it by
widening the lock — holding `lock_page_tables()` across a whole copy would serialize
all address-space mutation and lengthen shootdown windows, worsening Layer 2.*

### Layer 1 — a kernel-stack overflow becomes an unrecoverable cascade because #PF has no IST

- kstacks are 64 KiB usable + a 4 KiB unmapped guard, stacks grow down so overflow
  hits the guard first (`kernel/src/task/kstack.rs:45-58`). 64 KiB was sized for the
  *AP-boot* worst case (~33 KiB, `kstack.rs:45-51`), **not** for worst-case fault
  handling depth. Slot area is PML4[257] @ `0xFFFF_8080_0000_0000` — matches the
  fault VA `0xffff8080000aa000` (p4=257).
- **The page-fault handler runs on the current kernel stack — it has no IST.**
  Verified at `interrupts.rs:685` (`idt.page_fault.set_handler_fn(...)`, no
  `.set_stack_index`). `#GP` likewise (`:686-687`). **Only `#DF` gets an IST**
  (`:688-692`, `gdt::DOUBLE_FAULT_IST_INDEX`). So when the kstack overflows, the #PF
  taken on the guard page tries to push its frame onto the *same* exhausted stack →
  it re-faults. The recursion latch `IN_KERNEL_PAGE_FAULT[core]` (`interrupts.rs:144`,
  checked `:1288`) catches the second fault, prints `RECURSIVE KERNEL PAGE FAULT …
  cascade halted`, and `hlt_loop()`s the core (`:1300`). The non-recursive
  stack-overflow arm also ends in `hlt_loop()` (`:1337`).
- **The wedge holds the scheduler lock and disables interrupts.** The fault is
  entered through an interrupt gate (IF=0) and `hlt_loop()` (`lib.rs:1075`) never
  re-enables it — consistent with core 1's frozen LAPIC timer. The crash dump's
  `scheduler lock held -- skipping task dump` shows the lock was held into the fault
  (the handler also *acquires* `PROCESS_TABLE`/`try_lock_scheduler` on the dying
  path, `interrupts.rs:1190/1228`). A survivable per-task event becomes a permanently
  dead core.
- **Originating trigger (strongly suspected, not 100% pinned):** the pre-flagged
  multi-core recursive-kernel-fault race (`docs/roadmap/tasks/90b-claude-code-tasks.md`,
  the `cr2=0x8` core-1 race the gates avoid via `-smp 1`) under heavy PKU/demand-fault
  churn — the Phase 90b cross-thread PKU read-recovery path (`interrupts.rs:1174-1202`)
  is the hot path this exact `cli.js` workload hammers. The corrupted frame prevents
  pinning the first faulting instruction; the cascade mechanism above is what makes it
  fatal regardless of origin.

### Layer 2 — the wedge escalates to a whole-machine panic because the shootdown has no liveness model

- TLB shootdowns are delivered by **NMI** (`tlb.rs:64-71`, `ipi::send_nmi`,
  `interrupts.rs:693-699`) specifically so an IF=0 recipient still services them. **But
  the NMI handler also has no IST** (`interrupts.rs:699`, no `.set_stack_index`). On a
  core whose kstack already overflowed (or is spinning in the recursive-fault
  `hlt_loop`), the NMI cannot run on the dead stack — and a faulting/halted NMI never
  `IRETQ`s, which architecturally latches NMI-blocked. Either way
  `handle_tlb_shootdown_ipi`'s `SHOOTDOWN_PENDING.fetch_sub` (`tlb.rs:527`) never runs.
  → core 1 `ipi Δ0`.
- The sender, `wait_for_shootdown_acks_or_panic` (`tlb.rs:83-180`), spins 500 ms and
  `panic!`s at **`tlb.rs:176`** when any target hasn't ACKed.
- **No core is ever marked offline.** `is_online` has exactly one write site —
  `boot.rs:545` (→ `true`); the `AtomicBool::new(...)` at `smp/mod.rs:670/789` are
  constructors. The shootdown target loops *do* filter on `is_online`, but since it is
  write-once, a wedged core is never excluded → `remote_mask` still includes the dead
  core → guaranteed timeout → panic.
- **The waiter spins holding the global `SHOOTDOWN_LOCK`** (`tlb.rs:210` acquired,
  spin at `:372` before the guard drops at `:379`). So during the 500 ms one wedged
  core also stalls *every other core's* shootdowns — the failure is machine-wide even
  before the panic.

### Why V8/Node is the trigger

`sys_mprotect` and `sys_pkey_mprotect` both batch a `tlb_shootdown_range`
(`syscall/mod.rs:11962 mprotect_worker`, shootdown `:12191`); CoW (`fork`/spawn),
`mmap`/`munmap`, shm, brk/heap-grow, and the demand-fault path itself all issue
shootdowns. V8's JIT RW↔RX code-page flips + the Phase 90a PKU code-space commits make
this a continuous multi-thousand-per-second cross-core handshake (the dump shows
~3,600–4,000 lifetime IPIs/core at crash). Every one of those handshakes is a
500 ms-armed `panic!` waiting on any momentarily-unresponsive core. Failure
probability scales with V8's mprotect rate — which is why this is reliable on 4 cores
and invisible single-core.

## Remediation plan (ranked; suggested as a small SMP-hardening track)

### Track A — NMI-on-IST  [PRIMARY; IMPLEMENTED 2026-06-14]

**Implemented (this is the fix for the reported machine-kill):** a per-core **NMI**
IST stack (`gdt::NMI_IST_INDEX = 1`) wired into the BSP global TSS (`gdt.rs`, static
`NMI_STACK`) and each AP's per-core TSS (`smp/mod.rs` `init_ap_per_core`, a
`kstack::alloc_leaked_top()` slot), plus `.set_stack_index(gdt::NMI_IST_INDEX)` on the
`non_maskable_interrupt` IDT entry (`interrupts.rs`). Validated: `cargo xtask check` +
`smoke-test` (boots clean on the default SMP=4; fork/mmap/dynlink shootdown traffic
unaffected) + `regression`.

NMI is the cross-core TLB-shootdown delivery vector and fires regardless of IF, but it
still needs a usable stack. A core whose kstack overflowed (or is wedged in the
recursive-#PF `hlt_loop`) could not push the NMI frame onto its dead stack → never
reached the ACK (`tlb.rs:527`) → a sibling `panic!`d at `tlb.rs:176`. With an IST stack
the wedged core services the shootdown NMI on a clean per-core stack and ACKs, so **one
wedged core no longer kills the machine** — the box and the other cores survive (the
wedged core's task stays stuck; recovering it is Track D). Safe to IST because the NMI
handler is TLB-shootdown-only (`invlpg`/CR3-reload + atomic decrement — fault-free), so
it never re-enables NMI mid-handler and cannot nest on the shared per-core stack.

**Deliberately NOT done — #PF / #GP on IST (contraindicated):**
- **#PF is the hot demand-paging path that returns** (thousands/sec under Node). An IST
  is reused on every entry, so a nested #PF resets RSP to the IST top and corrupts the
  outer frame. Linux keeps #PF off IST for exactly this reason; today a nested #PF safely
  continues on the kernel stack.
- **#PF and #GP both use the `fault_kill_trampoline` redirect** (`interrupts.rs:1250-
  1261`), which captures the *current RSP* and resumes execution on it. On an IST that
  RSP is the IST stack, which a later fault/interrupt would reset out from under the
  running trampoline → corruption. IST and the kill-trampoline are incompatible.
- Consequence for Layer 1: the kstack overflow still wedges its core (cascade →
  `hlt_loop`), but the machine survives. **Recovering** that core (kill the task instead
  of halting) moves to Track D — it needs an off-IST mechanism (#DF-based stack-overflow
  detection, or a trampoline on a fresh normal kstack), not #PF-on-IST.

<details><summary>Original plan (superseded by the analysis above)</summary>

- **Where:** add IST indices + stacks in `gdt.rs` (mirror `DOUBLE_FAULT_STACK`,
  `gdt.rs:13/16/51/62`); `.set_stack_index(...)` on `idt.page_fault`
  (`interrupts.rs:685`), `idt.non_maskable_interrupt` (`:699`), and
  `idt.general_protection_fault` (`:686-687`).
- **Why:** directly fixes Layer 1+2. An IST-backed NMI runs on a clean stack even when
  the recipient's kstack is overflowed → the shootdown ACK fires → the `tlb.rs:176`
  panic never happens. An IST-backed #PF lets a kstack-overflow report cleanly instead
  of cascading. Standard practice (Linux/seL4 put #PF/#DF/#NMI on IST).
- **Critical caveats (must be in the implementation):**
  1. **Per-core** IST stacks. `DOUBLE_FAULT_STACK` is a single *global* static
     (`gdt.rs:51`) shared by all cores — a latent SMP corruption bug today; the new
     IST stacks (and ideally #DF's) must be per-core (the kstack pool already budgets
     per-AP double-fault stacks, `kstack.rs:78-84`).
  2. An IST stack is **reused on every entry** — a handler that re-faults onto its own
     IST overwrites its frame. Pair the #PF IST with the existing
     `IN_KERNEL_PAGE_FAULT` latch (check *before* any deep work / the diagnostics dump,
     which currently runs on the faulting stack, `interrupts.rs:1326-1336`).
  3. NMI is not re-entrant on a shared IST. m3OS uses NMI only for TLB shootdown
     (single source), so nesting risk is low, but there is **no NMI nesting guard**
     today — add one or document the invariant.

</details>

### Track B — mark a core offline when it halts  [IMPLEMENTED 2026-06-14]
**Implemented** in `hlt_loop` (`lib.rs`). All five dead-end paths funnel through
`hlt_loop` — the panic handler (`handle_panic`), the recursive-#PF cascade
(`interrupts.rs`), the kstack-overflow / kernel-#PF arm, #GP, and #DF — so a single
`if let Some(pc) = smp::try_per_core() { pc.is_online.store(false, Release) }` at the
top of `hlt_loop` covers every one of them with no per-site edits. `try_per_core()` is
ISR-safe and a no-op if per-core data isn't up yet (earliest boot). The shootdown
target loops already filter `is_online` (which was otherwise write-once → `true`), so a
halted core is now excluded **for free, with no protocol change**. A halted core never
runs userspace again, so abandoning its stale TLB is correct.

### Track C — degrade the shootdown ack-timeout instead of `panic!`  [IMPLEMENTED 2026-06-14]
**Implemented** in `smp/tlb.rs`. Two parts:
1. **Ack protocol: count → idempotent per-round bitmap.** Replaced the decrementing
   `SHOOTDOWN_PENDING: AtomicU8` with `SHOOTDOWN_ACK: AtomicU64` (bit `i` = core `i`
   flushed). The sender builds a `target_mask` in the same single snapshot walk it
   already did, resets `SHOOTDOWN_ACK=0` under `SHOOTDOWN_LOCK` before sending NMIs, and
   waits on `(SHOOTDOWN_ACK & target_mask) == target_mask`. The handler does
   `fetch_or(1<<core_id, Release)` *after* the flush (was `fetch_sub`). This makes the
   degrade path provably safe: a stale NMI latched from an abandoned earlier round can
   only set a bit *outside* the current round's `target_mask` (the abandoned core was
   marked offline, so it is excluded here) → ignored. The old count could underflow,
   double-decrement on re-NMI, or have a late ACK corrupt a *subsequent* round's count
   (silent stale TLB). The bitmap also subsumes the prior count-vs-send TOCTTOU
   hardening: a core flipping online/offline post-snapshot simply never sets its bit and
   is handled by the re-NMI + degrade path.
2. **Timeout: panic → re-NMI → degrade.** `wait_for_shootdown_acks` now: (phase 1) spins
   the 500 ms window; (phase 2) on timeout dumps the existing per-core
   serviced/timer diagnostic, re-NMIs the still-outstanding cores once, and waits a
   shorter (~100 ms) grace window; (phase 3) if any core *still* hasn't acked, marks it
   offline (Track B) and **returns** instead of `panic!`ing. With A+C the timeout is
   both effectively unreachable (Track A lets a wedged core ack on the IST stack) *and*
   non-fatal if ever reached.
- **Tradeoff (unchanged, now safe):** continuing past a lost shootdown risks a stale TLB
  on the target — safe because the core is being taken offline anyway (it will never run
  userspace again).
- **Not done:** the `SHOOTDOWN_LOCK`-across-the-full-spin idea (don't stall *other*
  cores' shootdowns during one degrade) was left out — with the degrade path now
  bounded to ~600 ms total and effectively unreachable post-Track-A, the added lock
  complexity wasn't worth the risk in this pass. Revisit only if the degrade path is
  observed firing in practice.

### Track D — root-cause the originating overflow  [deeper; fixes the origin, not just the blast radius]
- Confirm whether the cross-thread PKU read-recovery (`interrupts.rs:1174-1202`) +
  `demand_map_*` under multi-core contention can re-fault/loop, and audit kernel paths
  that hold `SCHEDULER`/`PROCESS_TABLE` across fault-prone work (so a fault never wedges
  with a lock held — cf. `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`).
  Pairs with the pre-flagged race in `docs/roadmap/tasks/90b-claude-code-tasks.md`.
- Consider redirecting a task-attributable kstack overflow to a controlled
  process-kill (off the IST stack) instead of `hlt_loop` (`interrupts.rs:1337`).

### Non-fixes / explicitly out of scope
- Do **not** widen `copy_*_user` locking (Layer 0) — it would worsen Layer 2 contention.
- Optionally demote `report_generation_divergence` (`user_mem.rs:322`) to debug level to
  cut serial spam (cosmetic; spam itself slows a core and widens shootdown windows).

**Suggested order:** A → B → C (A+C together make the timeout unreachable and
non-fatal; B is the cheap safety net), then D for the origin. **A, B, and C all landed
2026-06-14.** Only D (root-cause the originating overflow) remains.

## Reproduction

```bash
# REPRO (panics): default 4 cores
M3OS_WITH_CLAUDE=1 cargo xtask run-gui --kvm --fresh
# inside m3OS: pkg install claude-code ; claude   → interactive use → kernel panic

# WORKAROUND (stable): pin to one core (what the gates do)
M3OS_SMP=1 M3OS_WITH_CLAUDE=1 cargo xtask run-gui --kvm --fresh
```

`--kvm` is required for the interactive JIT-node TUI (real PKU). The crash is an SMP
property; `M3OS_SMP=1` makes it disappear. Default core count is 4
(`qemu_smp_count()`, `main.rs:4761`).

## Open questions

1. The exact first faulting instruction / call chain that overflows the kstack is not
   recoverable from this log (the overflow stomped the frame). A Track-A IST-backed #PF
   handler would capture a clean frame on the next repro — land A first, then re-run the
   repro to pin Track D's origin.
2. Is core 1's NMI lost because it (a) faulted on the overflowed stack, or (b) was
   NMI-blocked from an earlier un-`IRETQ`'d NMI? Track A fixes both; distinguishing them
   is academic unless A proves insufficient.
3. Does this also explain the OPEN `2026-06-05-4gib-smp-panic` (no Node, graphical
   boot)? Same family (SMP shootdown + no readable banner); the panic-path AP-quiesce
   ask there is complementary to Tracks A–C.

## Cross-references

- `docs/handoffs/2026-06-05-4gib-smp-panic-corrupted-output.md` — same family; this doc
  is its root cause. Its "quiesce APs before printing the panic banner" ask is still
  valid and complementary.
- `docs/handoffs/2026-05-24-4gib-pci-hole-vga-mapping.md` — introduced NMI shootdown +
  `wait_for_shootdown_acks_or_panic`; noted a residual SMP hang (this).
- `docs/roadmap/90a-memory-protection-keys.md`, `docs/roadmap/90b-claude-code.md` — the
  SMP-PKU gap and the cross-thread read-recovery; pkey_mprotect is a top shootdown
  generator.
- `docs/roadmap/tasks/90b-claude-code-tasks.md` — the pre-flagged multi-core
  recursive-kernel-fault race the gates avoid with `-smp 1`.
- `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md` — scheduler-lock /
  ISR-liveness invariants (Track D).
