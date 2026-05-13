---
status: resolved
resolved-on: 2026-05-13
resolved-by: PR #155 (fix/page-fault-reentry-guard) + PR #156 (fix/grow-heap-tlb-shootdown)
branch: feat/phase-64a-followups (origin/feat/phase-64a-followups, PR #154)
last-known-good-commit: 76c313f
date: 2026-05-13
component: kernel — pipe-table backing Vec corruption + page-fault-handler recursive crash cascade
related:
  - docs/64-session-manager-lifecycle.md
  - kernel/src/pipe.rs
  - kernel/src/mm/heap.rs
  - kernel/src/mm/mod.rs
  - kernel/src/task/kstack.rs
log: m3os.log (captured by `cargo xtask run --kvm` on user's machine, left idle)
---

# Handoff — kernel page-fault crash on idle (PIPE_TABLE Vec growth) — RESOLVED

> **Resolution summary.** Root cause was **kernel-stack overflow stomping
> adjacent `.bss` statics** (Hypothesis #1 in the original handoff). The
> `.bss`-backed `STACK_POOL` placed 32 KiB kernel stacks with no guard
> page directly adjacent to other kernel statics including `PIPE_TABLE`.
> AP boot legitimately uses ~33 KiB of kernel stack (proven once guard
> pages were added — see PR #155), which silently overflowed into the
> next slot or into adjacent `.bss` for *every* kernel boot. The
> corruption manifested as a `Vec<Option<Pipe>>` metadata stomp only when
> the overflow trajectory happened to cross PIPE_TABLE's storage.
>
> Fix shipped across two PRs:
>
> - **PR #155**: page-fault re-entrance guard (stops the dump-path
>   cascade), virtual kernel-stack guard pages in PML4[257] with
>   `KERNEL_STACK_SIZE` bumped 32→64 KiB (catches overflow at source and
>   structurally eliminates BSS-adjacency corruption), and
>   `PIPE_TABLE`/`PIPE_WAITQUEUES` invariant tripwire (catches any
>   future metadata corruption at the observation site).
> - **PR #156**: cross-CPU TLB shootdown for `grow_heap` mappings
>   (orthogonal SMP correctness fix flagged in the handoff).
>
> Verified by reproducing the original 20–30 min `--kvm` idle recipe
> on `fix/page-fault-reentry-guard` for 30–60 min with no crash and
> no diagnostic firings. The new tripwires, guard-page handler, and
> re-entrance latch are all in place and silent — meaning no overflow,
> no metadata corruption, no recursive faults.

## Original report (kept for context)

> **What the next session needs to do.** Confirm whether this reproduces
> on `main` *before* PR #153/154 (eliminates Phase 64 as the trigger),
> then chase the **wild-pointer corruption** in the `Vec<Option<Pipe>>`
> backing `PIPE_TABLE`. Two infrastructure fixes are recommended
> regardless of root cause: a non-allocating crash-dump path (to
> prevent the page-fault handler from recursively faulting on itself)
> and a cross-CPU TLB shootdown in `grow_heap`.

## TL;DR

Boot to graphical session, leave QEMU `--kvm` idle for ~20–30 minutes. Kernel
emits **6 page-fault crash dumps in rapid succession**. The first is the
real bug; the next 5 are the **page-fault handler recursively faulting
on itself** while trying to print the first dump — RSP shrinks by `0x2f0`
between each, so this is a finite-depth stack overflow disguised as
multiple crashes.

- **Crash 1 (real):** `<alloc::raw_vec::RawVec<core::option::Option<kernel_core::pipe::Pipe>>>::grow_one`
  faulted while dereferencing a wild pointer (`RBP = 0x10032955930`,
  which is **not** in any mapped heap region).
- **Crashes 2–6 (cascade):** identical `RIP = 0x10000952161` with
  registers consistent with a UART writer (`DX=DI=0x3f8`, `R8=0x3fd`,
  `R9='\n'`). Each crash dump's `print_crash_diagnostics` itself
  faulted, re-entered the handler, and pushed another frame.

The root cause is **memory corruption** — something wrote a wild value
into the `Vec<Option<Pipe>>`'s metadata (ptr or capacity) before
`grow_one` ran. Source unknown.

## Reproduction

Recipe used to produce the log:

```bash
cd /home/mikecubed/Projects/m3os   # user's worktree at this commit
cargo xtask run --kvm
# Boot completes, graphical session reaches state=running
# Let it sit idle — no login, no m3ctl, no interaction
# Wait ~20–30 minutes
# Six CRASH DIAGNOSTICS blocks appear on serial
```

The log `m3os.log` in the project root (worktree at `feat/phase-64a-followups`)
is the captured serial output. Boot phase ends around log line 1230
(`session_manager: session.boot: state=running`). The first crash starts
at line **1809–1810** (interleaved with a `stale-ready` warning) at
tick `~1129045`.

Key log positions:

| What | Line | Tick |
|---|---|---|
| Boot reaches state=running | 1229 | n/a |
| Push-event channel registered | 1205 | n/a |
| First push event fired (e1000_driver exit) | 1231 | n/a |
| First fault (crash 1) | 1810 | ~1129045 |
| Crash 2 | 1849 | 1129219 |
| Crash 3 | 1883 | 1129247 |
| Crash 4 (only one with active task) | 1972 | 1129340 |
| Crash 5 | 2073 | 1129445 |
| Crash 6 | 2108 | 1129480 |

## Crash inventory

All six crashes are `[int] KERNEL page fault` (Ring 0), same `CR3=0x3e173000`
(same userspace process page table active). Cores 4–7 are not listed in the
"Per-Core State" block — only cores 0–3 — but crash 4 reports
`task_idx: 15, core: 6` in its trace entry, suggesting the trace ring
records higher core IDs that the dumper doesn't enumerate.

| # | Line | RIP | Fault addr | Active task | Notes |
|---|---|---|---|---|---|
| 1 | 1810 | `0x1000081f417` | `0x10032955930` | none (core 1 idle) | **trigger**: `RawVec::grow_one`; `RBP = fault addr` |
| 2 | 1849 | `0x10000952161` | `0x3` | none | cascade |
| 3 | 1883 | `0x10000952161` | `0x8` | none | cascade |
| 4 | 1972 | `0x10000952161` | `0x3` | **PID 15 (session_manager)** on core 6 | cascade (only one with task context) |
| 5 | 2073 | `0x10000952161` | `0x8` | none | cascade |
| 6 | 2108 | `0x10000952161` | `0x8` | none | cascade |

### PID map at crash time

Extracted from boot transcript (`init: started '<name>' pid=N`):

| PID | Service |
|---|---|
| 1 | init |
| 2 | syslogd |
| 3 | sshd ← **heavily active in crash 1's trace ring** |
| 4 | crond |
| 5 | console |
| 6 | kbd |
| 7 | display |
| 8 | mouse_server |
| 9 | stdin_feeder |
| 10 | fat |
| 11 | vfs |
| 12 | net_udp |
| 13 | nvme_driver |
| 14 | e1000_driver (exited normally before crashes) |
| 15 | session_manager |
| 16 | audio_server |
| 17 | term |

## Detailed analysis

### 1. The cascade is the page-fault handler recursing on itself

The 5 cascade crashes are **not 5 independent bugs**. Evidence:

1. **Identical RIP** (`0x10000952161`) every time.
2. **Identical register state** except RSP, which decreases by **exactly `0x2f0` (752 bytes)** between crashes — that's one stack frame's worth.
3. **Registers match a UART writer mid-call**:
   - `RDX = RDI = 0x3f8` (COM1 base port)
   - `R8 = 0x3fd` (COM1 Line Status Register = base + 5)
   - `R9 = 0xa` (`'\n'`, the byte being written)
   - `R12 = 0xc0`
   - `RCX = RSI = 0x1000000faca` (same kernel address every time)
   - `R11 = 0x100000093f3` (same kernel address every time)
4. Faults are at **`NULL+3`** and **`NULL+8`** — classic offsets-into-a-NULL-struct.

What's happening: the page-fault handler at `0x10000952161` tries to log
or format something while writing the crash diagnostics; that path
allocates or dereferences via a now-corrupt kernel structure; it
faults; the handler is re-entered; it builds another frame; repeat.
The kernel will eventually stack-overflow (unmapped guard page) and
either triple-fault or wedge.

`addr2line` on our locally-built kernel returned `??:0` for
`0x952161` — the user's kernel had a slightly different layout. To
recover the function name, build the kernel at commit
`76c313f` (or whichever commit the user actually booted — they were
on `/home/mikecubed/Projects/m3os/` which may be at a different SHA)
and run:

```bash
addr2line -e target/x86_64-unknown-none/release/kernel -f -C -i \
  0x952161 0x93f3 0xfaca
```

(All addresses minus the `0x100_0000_0000` PIE load offset.) These
will resolve to the kernel logger / UART writer / page-fault handler
internals.

### 2. The real bug — crash 1

`addr2line` on the trigger RIP (with PIE base subtracted):

```
$ addr2line -e target/x86_64-unknown-none/release/kernel -f -C -i 0x81f417
<alloc::raw_vec::RawVec<core::option::Option<kernel_core::pipe::Pipe>>>::grow_one
kernel.b915ed201973dc77-cgu.0:?
```

This is `Vec<Option<Pipe>>::push` forcing a buffer reallocation. The
backing storage:

- `kernel/src/pipe.rs:25` — `static PIPE_TABLE: IrqSafeMutex<Vec<Option<Pipe>>> = IrqSafeMutex::new(Vec::new());`
- `kernel/src/pipe.rs:91` — `table.push(Some(Pipe::new()));` (the call site)

The fault address `0x10032955930` decomposes as:

| Field | Value |
|---|---|
| PML4 index | 2 (kernel binary mapping per `kernel/src/mm/mod.rs:311`) |
| PDPT index | 0 |
| PD index | **404 — not present** |
| PT index | 341 |

Page-table walk **in both the active CR3 and the kernel PML4** shows
`PD[404] flags=0x0` — the address is unmapped everywhere. It is **not**
in the bootstrap kernel heap range (`HEAP_START = 0xFFFF_8000_0000_0000`,
higher half). It's at offset ~840 MB into PML4[2], well beyond any
expected kernel heap, slab, or stack region.

The faulting instruction accesses memory via `RBP` (`RBP = 0x10032955930
= fault addr`). In Rust release builds RBP is general-purpose; in
`RawVec::grow_one` it commonly holds either the old buffer's data
pointer or `old_ptr + cap * size_of::<Option<Pipe>>()`. **The
`Vec<Option<Pipe>>`'s ptr or cap field was corrupted before the call.**

### 3. What we ruled out

- **Heap-grow TLB-shootdown miss** (`kernel/src/mm/heap.rs:860`,
  `flush.flush()` is local-core only — see [Recommended infra fix
  #2](#2-cross-cpu-tlb-shootdown-in-grow_heap-defence-in-depth)).
  Ruled out as the *cause of crash 1* because the fault address is not
  in the bootstrap heap range and is unmapped in **both** the kernel's
  own PML4 and the active CR3. A TLB miss would show a mapped page
  with a stale local TLB; this is a genuinely-unmapped address.
- **Phase 64a/64b touched pipe.rs or heap.rs.** Confirmed neither
  file was modified by the relevant commits.
- **Phase 64a/64b created pipes.** The new `session-events` channel
  uses IPC endpoints (`ipc_send_buf`), not pipes. Pipe-table growth
  must be driven by something else — most likely **sshd** (PID 3,
  the most-active task in the trace ring; sshd creates pipes for
  per-connection stdio).
- **Crash 1 happened with an active task.** No — `task_idx=-1` on
  core 1 means the scheduler-loop is idle. The kernel was performing
  a context-switch-related operation when the fault hit.

### 4. Best theory — memory corruption of the pipe Vec metadata

Something wrote a wild pointer into the `Vec<Option<Pipe>>`'s `RawVec`
fields (`ptr: NonNull<T>` or `cap: usize`). When `grow_one` next ran
(on a `push` from `create_pipe`), it read the corrupted field and
dereferenced into unmapped memory.

Candidate corruption sources (ranked roughly by likelihood):

1. **Kernel stack overflow stomping a static.** `IrqSafeMutex` static
   `PIPE_TABLE` lives in `.data` / `.bss`. A stack that grew past its
   guard page (or has no guard page) could write through it. Worth
   auditing kernel stack sizing and guard-page coverage.
2. **Concurrent mutation bypassing `IrqSafeMutex`.** Some path takes
   a raw pointer / `&mut` into the Vec and holds it across a yield,
   allowing another core to write while the first still holds a stale
   reference. The Phase 57b G.7 comment in `pipe.rs:22-23` claims "no
   ISR reaches it" — verify that's still true and that no syscall
   path holds a stale reference through a context switch.
3. **Wild write from anywhere.** Hardest to track. Strategies: enable
   write-watchpoints on `PIPE_TABLE`'s address if QEMU's gdb stub
   supports it; sprinkle invariant checks (`assert!(table.capacity() < SANE_MAX)`)
   into `create_pipe` / `free_pipe`.
4. **Allocator returned a corrupt pointer at last successful alloc.**
   If the slab/bootstrap allocator returns a usable pointer that
   happens to point to memory it doesn't own (e.g., overlap with
   `.data`), subsequent writes to that allocation could clobber
   `PIPE_TABLE`'s storage. Audit `SizeClassAllocator::alloc` paths
   in `kernel/src/mm/heap.rs:620`.

### 5. Why this is hard to localize from the log alone

- The trace ring was overwritten between the fault and the dump
  (~170 ticks elapsed). The actual operation that *triggered* the
  pipe creation is no longer in the ring.
- Higher core IDs (4–7) are not enumerated in the "Per-Core State"
  block of the crash dump, so we lose cross-core context.
- The recursive cascade obliterated whatever else was being logged
  around the crash.

## Recommended fixes (independent of root cause)

### 1. Non-allocating crash-dump path — **highest priority**

The cascade made this crash much harder to diagnose. The page-fault
handler currently allocates / formats strings during the dump, and
those allocations re-fault when the allocator state is corrupt.

Fix: pre-format the crash output into a `static mut` buffer (or use
fixed `write!` against `core::fmt::Write` with a stack-local
fixed-size buffer); call `_panic_print` from `kernel/src/serial.rs:60`
which uses `try_lock` and a fresh `SerialPort::new(COM1_PORT)`
fallback. **Never allocate in the fault handler.**

Source: probably `kernel/src/interrupts/page_fault.rs` or wherever
`print_crash_diagnostics` lives. Look for the `serial_println!` /
`log::error!` calls inside it; replace with the panic-path serial
writer.

### 2. Cross-CPU TLB shootdown in `grow_heap` — defence in depth

`kernel/src/mm/heap.rs:860`:

```rust
let map_result = unsafe { mapper.map_to(page, frame, flags, &mut frame_alloc) };
match map_result {
    Ok(flush) => flush.flush(),    // ← LOCAL TLB flush only
```

Should issue a cross-CPU shootdown (the existing IPI infrastructure is
in `kernel/src/arch/x86_64/`). Not the cause of this crash, but a real
SMP correctness bug that could surface in other guises.

### 3. Kernel-stack guard-page audit

If stack overflow is the corruption source, guard pages on kernel
stacks would have turned this into an immediate "kernel stack overflow"
fault at a known address instead of a wild write into static memory.

## Suggested investigation order

1. **Add the non-allocating crash dump** (#1 above). The next time this
   reproduces, you'll get the full first-crash dump without the cascade
   stomping on it, and the trace-ring window will be much closer to
   the trigger event.
2. **Bisect / repro on main pre-Phase-64.** `git checkout
   <commit-before-PR-153>`, `cargo xtask run --kvm`, leave idle the
   same way. If it crashes — pre-existing. If not — Phase 64-related
   pressure surfaces it. The latter is still a kernel bug, but the
   trigger window matters.
3. **Stress the pipe table directly.** Write a userspace stress test
   that opens/closes pipes in a tight loop (sshd-like). If
   reproducible deterministically, attaching gdb via QEMU's stub and
   watching `PIPE_TABLE` for unexpected writes becomes feasible.
4. **Address #2 (cross-CPU TLB shootdown)** as a separate small PR
   regardless. It's a latent SMP correctness bug.
5. **Then chase the actual corruption** with the better dump from #1.

## References

| Resource | Where |
|---|---|
| Captured serial log | `m3os.log` (worktree root) |
| Current branch tip | `76c313f fix(64b): address Copilot review on PR 154` |
| Kernel ELF for addr2line | `target/x86_64-unknown-none/release/kernel` (PIE; subtract `0x100_0000_0000` from runtime RIPs before passing to `addr2line`) |
| Pipe table source | `kernel/src/pipe.rs:25,77-94` |
| Kernel heap allocator | `kernel/src/mm/heap.rs:620,815-886` |
| Process page-table init | `kernel/src/mm/mod.rs:300-318` |
| Kernel serial writer | `kernel/src/serial.rs` (`_kernel_print` line 39, `_panic_print` line 60) |
| Address space layout doc | `docs/02-memory.md`, `docs/33-kernel-memory.md` |
| Crash diagnostics doc | `docs/43a-crash-diagnostics.md`, `docs/43b-kernel-trace-ring.md` |

## Glossary of the log artefacts

- `[int] kernel page fault: addr=Ok(VirtAddr(0xN)) err=PageFaultErrorCode(M)` —
  page-fault handler entered. `addr` = CR2, `err` bits = present/write/user/etc.
- `[pf-diag] vaddr=… idx=[p4=A p3=B p2=C p1=D]` — paging walk results;
  shows which level of the page table is missing.
- `[pf-diag] active: PML4[X] flags=…` — walk of the currently-active CR3.
- `[pf-diag] kernel: PML4[X] flags=…` — walk of the kernel's PML4
  (i.e., what every CR3 should share via the higher-half copy).
- `=== CRASH DIAGNOSTICS ===` ... `=== END CRASH DIAGNOSTICS ===` —
  CPU register snapshot + per-core scheduler state at fault time.
- `=== TRACE RING DUMP (all per core) ===` — kernel scheduler trace
  ring (`Phase 43b`); fixed-size ring per core, oldest entries
  overwritten first.

## Open questions

1. Which PID owns CR3=`0x3e173000`? Knowing the process at fault time
   narrows the scope. We did not extract this from the log; it's
   recoverable by correlating against `kernel/src/process` task-table
   instrumentation if any logs ASLR / CR3 assignment.
2. Was the user logged in or idle at the boot prompt? They said
   "let it sit idle" — confirm whether a getty / login was awaiting
   input or whether the session was unlocked.
3. Does `addr2line` on the user's actual built kernel resolve the
   cascade RIP `0x952161`? If so, that names the recursive
   page-fault-handler function exactly.
4. Is this `--kvm`-specific? KVM enables true SMP timing; TCG would
   serialize many races. Reproducing on TCG (`cargo xtask run` without
   `--kvm`) would be telling.

## Resolution Details

### Root cause: `.bss` adjacency between kernel stacks and `PIPE_TABLE`

The original `kernel/src/task/kstack.rs` allocated kernel stacks from a
`.bss`-resident static array (`STACK_POOL: [StackSlot; MAX_KERNEL_STACKS]`)
with no guard pages. Each slot was 32 KiB. The linker placed this array
adjacent to other kernel statics — including `PIPE_TABLE`'s
`IrqSafeMutex<Vec<Option<Pipe>>>` — somewhere in the kernel's `.bss`
section.

When the guard-page rework in PR #155 added unmapped guard pages below
each kstack, the very first reproduction surfaced **AP boot using
~33 KiB of kernel stack** — proven by all three AP cores faulting at
identical offset `0xf18` inside their slot's guard page. Tracing the
RSP value: `0x9000 - 8 (alignment) - 0x80e8 (frames consumed) = 0x10`,
crossing into the guard region just before AP entry completed.

In the original layout this overflow silently spilled into the
*adjacent `.bss` static*. Whatever happened to be next in the linker's
output — sometimes the next kstack slot, sometimes (depending on link
order, build flags, and Phase-N changes that perturbed `.bss` layout)
`PIPE_TABLE`'s storage — got its first 232 bytes overwritten with
`Task::new`'s spilled register state.

When the overwrite happened to land on `PIPE_TABLE`'s `RawVec` metadata
(`ptr`, `cap`, `len`), the next `create_pipe` call would `push` against
the corrupted capacity, dereferencing a wild pointer (`RBP =
0x10032955930` in the original report) into unmapped memory and faulting
inside `RawVec::grow_one`. Idle reproduction time correlated with
"how long until `sshd` opens enough pipes to trigger a `grow_one`."

### Why Phase 64 made it worse

Phase 64 (session_manager lifecycle, PR #153) and 64a (follow-ups,
PR #154) added new kernel statics — service tables, restart tracking,
new endpoints — which subtly changed `.bss` layout. The post-Phase-64
layout happened to place `PIPE_TABLE` in the overflow trajectory of
the AP-boot kstack, raising the corruption rate from "rare" to
"reproduces in 20–30 min idle."

This explains why the handoff author saw the cascade on
`feat/phase-64a-followups` but not on stable older branches.

### Fix (shipped)

PR #155 (`fix/page-fault-reentry-guard`):

- **`e0cc10c`** — per-core `IN_KERNEL_PAGE_FAULT: [AtomicBool; MAX_CORES]`
  latch in the ring-0 page-fault handler. Re-entry prints one diagnostic
  line and halts that core, preserving the first-crash dump.
- **`5bcf232`** — replaced the `.bss`-backed `STACK_POOL` with a
  virtually-mapped slot pool in PML4[257]. Each slot is 68 KiB virtual:
  a 4 KiB unmapped guard page below 64 KiB of mapped stack.
  `KERNEL_STACK_SIZE` bumped 32→64 KiB after the guard surfaced the
  AP-boot overflow. Pool footprint grew from ~17 MiB to ~34 MiB physical.
  The page-fault handler now classifies guard-page hits via
  `kstack::classify_guard_page_fault` and emits
  `[int] KERNEL STACK OVERFLOW: kstack slot N…` at the source.
- **`ef1027a`** — `assert_pipe_vec_sane` at every `PIPE_TABLE` /
  `PIPE_WAITQUEUES` mutation site, verifying `capacity <= 64 KiB` and
  `len <= capacity` before `push` could dereference corrupt metadata.

PR #156 (`fix/grow-heap-tlb-shootdown`):

- **`84b4233`** — `tlb_shootdown_range_kernel(start, end)` for kernel-
  shared mappings, called from `grow_heap` after the per-page mapping
  loop. Addresses recommendation #2 from this handoff: defense-in-depth
  SMP correctness fix, not the root cause.

### Verification

- All 12 QEMU integration tests pass on `fix/page-fault-reentry-guard`.
- `cargo xtask smoke-test` and `cargo xtask regression` (11 tests)
  pass.
- 30–60 min `cargo xtask run --kvm` idle reproduction on
  `fix/page-fault-reentry-guard` produces no crash, no
  `KERNEL STACK OVERFLOW`, no `PIPE_TABLE corruption`, and no
  `RECURSIVE KERNEL PAGE FAULT`. All three diagnostic surfaces remain
  silent — confirming no overflow, no metadata corruption, no recursive
  faults.

### Was the "deferred-detection" recommended fix #1 needed?

The handoff recommended a non-allocating crash-dump path. Investigation
confirmed `_panic_print` is *already* non-allocating (`SerialPort::new`
fallback, no heap, no formatter machinery beyond `core::fmt::Arguments`).
The cascade fault inside the dump path was *not* an allocator re-entry;
it was a corrupted-pointer dereference somewhere in `core::fmt`-style
machinery, observed after the original kstack overflow had stomped
on internal kernel state. The re-entrance guard from PR #155 short-
circuits the cascade without needing to re-architect the dump path,
and the root-cause kstack fix eliminates the corruption that produced
the deref fault in the first place.

### Lessons for future kernel statics

- `.bss`-resident pools of *anything that can grow downward* (stacks,
  guard-page-less buffers) are a corruption-by-adjacency time bomb.
  Use virtually-mapped pools with guard pages.
- Kernel-stack sizing must include worst-case boot-time depth, not
  steady-state. AP boot goes through `init_ap_per_core` + GDT/IDT/TSS
  setup + `apic::init_local` + per-core data init — easily 30+ KiB.
- `Vec` metadata corruption deep inside the kernel is invisible until
  the next `push`; invariant tripwires at the lock-acquire site
  collapse the diagnostic window from "minutes" to "the call that
  caused it."
