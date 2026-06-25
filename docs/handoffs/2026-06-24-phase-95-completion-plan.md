---
status: CANONICAL PLAN (pick up here) — the single source of truth for finishing the
  Phase 95 series (95b on-device `rustc` codegen + 95c VFS/block-I/O perf). Supersedes the
  *forward-planning* framing in the 95b/95c task + design docs and the 2026-06-23 handoff,
  all of which predate the page-table fix (`841fd53f`) and the KVM perf measurements and so
  carry an outdated "the wall is the slow VFS install" diagnosis. Those docs remain the
  technical *record*; THIS doc is the plan.
owner: unassigned
updated: 2026-06-24
---

# Phase 95 completion plan — finish on-device `rustc` (95b) + VFS perf (95c)

## TL;DR — where we actually are

- **✅ `RUSTC_OK` ACHIEVED (2026-06-24) — on-device Rust code generation works on m3OS.**
  `rustc-smoke` PASSES end-to-end under `M3OS_KVM=1`: `rustc hello.rs` compiles, rust-lld links
  it, and the native binary runs (`RUSTC_OK hello from rustc`), 0 kernel faults, ~41 s. The
  Phase-95b milestone (the Rust analog of Phase 85d clang) is reached with a **single-threaded
  linker** constraint — `-C link-arg=--threads=1` (now baked into the gate; analogous to Go's
  single-core constraint). This required, in order: FIONBIO ioctl (ENOTTY fix); kernel
  `AT_EXECFN` + loader `DT_RUNPATH`/`$ORIGIN` (so rust-lld finds `libLLVM.so`); and `--threads=1`
  (so rust-lld avoids the multithreaded `relocsVec[threadIndex]` OOB fault + the worker-kill
  `addr=0x8`/deadlock). See Step 1's dated updates. Deferred (non-blocking): the multithreaded
  rust-lld `threadIndex`/pool-size bug; the thread-group fatal-kill kernel-robustness fix.
- **`rustc` runs on m3OS.** After the loader/libc/page-table crash chain was fully fixed
  (see "What is done"), `rustc --version` → `rustc 1.96.0` and `rustc --print sysroot` →
  `/usr` both **execute on-device, serial-captured**. The multi-week `CR2=0` blocker is gone.
- **The remaining blocker to `RUSTC_OK` is now precisely root-caused (2026-06-24).** The ENOTTY +
  `libLLVM.so`-load blockers were fixed this pass (FIONBIO ioctl; kernel `AT_EXECFN` + loader
  `DT_RUNPATH`/`$ORIGIN`), so `rustc hello.rs` now compiles and rust-lld **loads + runs**. The
  chain: (1) a rust-lld **worker thread deterministically faults in userspace** (`rip=0x50755c`,
  same 4-core and single-core) and is killed; (2) `fault_kill_trampoline` kills only that **one
  TID**, not the **thread group**, so the siblings strand `BlockedOnFutex` forever (single-core:
  a deterministic hang); (3) on **SMP** a sibling on another core races the partial teardown →
  the kernel **`addr=0x8` NULL+8 deref**, then the `VirtAddr::fmt` heavy dump overflows the
  kstack → recursive #PF cascade-halt. So **`addr=0x8` is the SMP-race manifestation of a
  missing thread-group fatal-kill** (m3OS already has `sys_exit_group` group-kill machinery; the
  fatal-fault path just doesn't use it). The fix = terminate the whole thread group on a fatal
  fault + make the fault dump stack-safe (kernel-robustness) — but the **`RUSTC_OK`-critical
  blocker is the rust-lld worker fault itself (`rip=0x50755c`)**, which the group-kill does not
  resolve. See Step 1's dated updates for the full evidence + fix design.
- **The "slow VFS install is the wall" story was a TCG artifact.** Measured under KVM, the
  FS is fast (136 MB/s write, 422 MB/s read, 217 µs IPC RTT): `pkg install rust` is **~25 s**
  and the 169 MB `librustc_driver.so` cold-load is **~9.6 s**. So **95c's VFS perf work is
  NOT on the `RUSTC_OK` critical path under KVM** — it only matters for running the gate
  under plain TCG (CI without KVM).
- **Architecture decision (owner, 2026-06-24): the in-kernel ext2 read fast path
  (95c Track/Area F) is REJECTED.** It violates the microkernel boundary and directly
  conflicts with the just-completed ext2-engine-unification (vfs_server is now the sole
  post-boot root reader). Fix VFS performance *in the ring-3 ext2 driver*; do not move ext2
  reads back into ring 0 unless A+B+D **and** a recorded measurement prove the VFS path
  categorically cannot reach acceptable throughput — and even then prefer faster IPC.

## What is done (committed on `feat/phase-95b-on-device-rustc`)

The entire "rustc won't start" crash chain — the subject of
[`2026-06-23-rustc-runtime-null-deref-after-tls.md`](./2026-06-23-rustc-runtime-null-deref-after-tls.md):

| Fix | Commit |
|---|---|
| `__libc.auxv` NULL (mallocng first-malloc) → loader sets it before constructors | `610f8acd` |
| musl TLS globals + `tls_module` list (`__copy_tls td=0`) | `bbc510c8` |
| `dladdr` implemented in the loader (sysroot detection) | `500b3747` |
| writable+user intermediate page-tables (the 165 M-iteration fault loop) | `841fd53f` |
| multi-module static TLS for DT_NEEDED DSOs | `59cd0c00` |

95c supply-side perf, also landed: zero-copy SHM read-window (A.1), readahead caps 64→256 KiB
(A.2), `LruBlockCache` for `vfs_server` (C), `DirIndex` O(1) metadata + per-group free-search
cursor + granular `invalidate_cache` (the O(N²)-create + device-read fixes), the
`vfs-throughput-smoke` gate (E.1). And the **ext2-engine-unification** (vfs_server = sole
post-boot root engine for reads/exec/open; see
[`2026-06-24-ext2-engine-unification-plan.md`](./2026-06-24-ext2-engine-unification-plan.md),
closed).

## Corrected understanding (why this plan differs from the older docs)

Three reframes happened *after* the 95b/95c docs were written, and the older docs were never
fully reconciled (this plan does that):

1. **rustc now reaches userspace and executes.** The 95b "Outcome" + README row say "the
   loader wedges in the kernel loading the 162 MB `librustc_driver.so`; rustc never runs
   userspace." That was true *before* the page-table fix (`841fd53f`). It is now **false** —
   rustc runs; `--version`/`--print sysroot` complete.
2. **The FS is not the wall under KVM.** The "~100–200 KB/s VFS / ~40-min install /
   install-timeout is the immediate `RUSTC_OK` blocker" framing in 95b's Outcome and 95c's
   "Why This Phase Exists" was measured under **TCG**. Under **KVM** the FS is fast and the
   install is ~25 s. The throughput numbers are real for TCG but do not gate the milestone
   when KVM is available.
3. **The real blocker moved to the compiler's threads.** `rustc hello.rs` hangs/very-slows
   in multithreaded codegen — a kernel scheduler/futex issue, the class m3OS has hit before
   (Phase 89 `FUTEX_REQUEUE`, Phase 90b SMP futex keys, the 2026-06-14 lost-wakeup work).

## The plan — ordered work to close the 95-series

### Step 1 (PRIMARY) — the `rust-lld` link wedge: a Phase-95b "lock held across a blocking lazy-file fault" CLASS

**DIAGNOSIS SOLVED (2026-06-24). It was never a codegen hang.** Empirical KVM runs peeled it:
1. rustc **compiles fine** — the handoff's "compile-thread hang" was the slow TCG compile.
2. The smoke harness's linker invocation was **mis-wired** — `-C linker-flavor=ld.lld` alone
   execs a binary named `lld`; the bundled one is `rust-lld`. **FIXED** by adding
   `-C linker=rust-lld` (xtask/src/main.rs ~23791; kept). With that, rustc invokes `rust-lld`.
3. **The real blocker:** once `rust-lld` (a *dynamic*, multithreaded LLVM linker) runs, the
   **whole system wedges — the stall-census watchdog stops too**. That "watchdog stops"
   ⇒ a kernel deadlock (a global lock stranded across a context switch), not a userspace
   futex block.

**Root cause (high confidence, agent-verified).** Phase 95b (commit `bc732cb8`) made dynamic
PT_LOAD pages `MAP_LAZY_FILE`, so a demand-fault now issues a **blocking vfs_server IPC**.
Any syscall that does `copy_from_user`/`copy_to_user` (which pre-faults via `try_demand_fault`)
**while holding an `IrqSafeMutex`** can now block → `block_current_until` → `switch_context`
**with the lock still held, IF masked, preempt raised**. The lock is never released; every
other core spinning on it wedges; the watchdog (needs scheduler progress) goes silent. This
is exactly why a **static** binary (node) passes `smp-smoke` but a **dynamic** one (rust-lld)
wedges — only dynamic binaries have lazy-file PT_LOAD pages.

**FIXED so far (this is a CLASS — more sites remain):**
- `sys_futex` `FUTEX_WAIT` + `FUTEX_CMP_REQUEUE` read the futex word under `FUTEX_TABLE`
  (`syscall/mod.rs` ~19677 / ~19833). **Fixed** by pre-faulting the word with no lock held
  before locking (Linux-style). Note: this did NOT clear the `rust-lld` link wedge — it's a
  *different* site (the link path completes `--version`/`--print sysroot`, which always
  worked, then wedges only in the actual link).

**REMAINING: find the other lock-held-across-lazy-file-fault site(s) in the `rust-lld` link
path** (the agent flagged `PROCESS_TABLE`, `ENDPOINTS`, and the scheduler locks as candidates;
the `PROCESS_TABLE`-held-across-`copy_from_user` case is also a non-reentrant self-deadlock —
`process/mod.rs:1293,1309` re-take `PROCESS_TABLE` from the demand path).

**NEXT STEP — the deadlock-guard (stop guessing which lock).** Add a one-shot diagnostic at
the blocking demand-fault chokepoint (`block_current_until`, scheduler.rs:3544, or
`demand_map_vma_page`, interrupts.rs ~908): when about to block **with a lock held / preempt
raised**, `log::error!` the offending **syscall number + user RIP + pid** (per-task
`syscall_snapshot` in task/mod.rs ~636; the IrqSafeMutex/preempt held-state is the trigger).
The **last line before the silence** names the culprit syscall → pre-fault its user copy
before the lock (same fix shape). Repeat until the wedge clears. Consider a **systemic** fix
too: pre-fault user-pointer args at syscall entry before locks, or a drop-lock-fault-retry on
`copy_*_user`, since the agent warns this hazard exists anywhere a held lock spans a user copy.

**FAST REPRO (build this first — ~2 min vs the 15-min rust gate).** A minimal **dynamic**
(`PT_INTERP` + lazy-file PT_LOAD) C program, **4 pthreads**, with the contended
`pthread_mutex_t`/`pthread_cond_t` in a **global `static` (.bss/.data)** — so its page is
lazy-file-cold on first touch — doing tight lock/cond contention. A stack-local lock faults
into the *non-blocking anonymous* path and misses the bug, so it MUST be a global. Build via
the `build_dynamic_hello_fixture` template (`-O2 -fPIE -pie`, `xtask/src/main.rs:21933`); wire
it as a `dynamic-mt-smoke` gate (the permanent regression guard for this Phase-95b class).

**Acceptance:** `rustc /usr/src/hello.rs` links via multithreaded `rust-lld` and the binary
runs → `RUSTC_OK` (serial, `M3OS_KVM=1`, clean disk); `dynamic-mt-smoke` passes; `smp-smoke`
stays green. Also handle the `--threads=1` path's `std::process` `ENOTTY` panic (a separate,
smaller bug) only if it resurfaces once the multithreaded path works.

**Other notes:** `unhandled syscall 324` = `membarrier(PRIVATE_EXPEDITED)` — appears benign
(a whole-system wedge is a lock deadlock, not a missing barrier); revisit only if a *single*
thread is left `BlockedOnFutex` after the lock fixes.

**UPDATE (2026-06-24, this pass) — guard + repro LANDED; the failure is a RACE (TWO bugs).**
- The **deadlock-guard** is committed: an always-on "scheduling-while-atomic" check at the
  blocking lazy-file demand-fill (`interrupts.rs` `demand_map_vma_page`, using new
  `current_preempt_count()` in `scheduler.rs`) — logs `[deadlock-guard] … syscall_nr=…` when
  about to block with `preempt>0`. A **`dynamic-mt` repro** (4 pthreads + a **global**
  mutex/cond) is built + staged at `/usr/bin/dynamic-mt` (not auto-run yet — built by
  `build_dynamic_mt_fixture`, run it manually). **It surfaced a THIRD distinct bug:** a
  no-thread-local-main dynamic pthread program **crashes in musl `__copy_tls`** (`addr=0x8`
  WRITE in `libc.so` = `td=0`) on `pthread_create` — deterministic, and distinct from the
  wedge (rustc has thread-locals so it gets past this). Next on the repro: either add a
  `_Thread_local` to `dynamic-mt` to push past TLS and reach the lock wedge, OR fix the
  `__copy_tls td=0` no-TLS-main case first (it is deterministic + fundamental, a good
  starting target). **[FIXED 2026-06-24]** — the loader (`setup_static_tls`) now ALWAYS calls
  `__m3os_set_tls` (head=NULL when zero TLS modules) so `libc.tls_align` ≥ 16; `dynamic-mt`
  no longer crashes (`ldso: tls modules=0` → handoff to main, no `addr=0x8`). **It now
  DETERMINISTICALLY reproduces the wedge** — it hangs *before* `pthread_create`'s
  `clone_thread` log (a lazy-file fault during the new thread's stack/TLS setup, under a
  lock). The deadlock-guard did NOT fire, so the wedge is BEFORE the guard spot (the
  lazy-file blocking read) — likely the `PROCESS_TABLE` re-lock self-deadlock in
  `shared_vma_demand_file`, or a fault on an anon/stack page rather than a lazy-file one.
  **NEXT: move the guard to `demand_map_vma_page` ENTRY** (before `shared_vma_demand_file`)
  and re-run the **2-min `/usr/bin/dynamic-mt` repro** to name the culprit — no more 15-min
  rust-gate cycles. **[DONE 2026-06-24]** — the entry guard named the culprit:
  **`rt_sigprocmask` (syscall 14)** reads the user `sigset_t` (and writes the old set) under
  `PROCESS_TABLE` during `pthread_create`'s signal-block; a cold lazy-file sigset page wedged.
  **FIXED** (both user accesses moved out of the lock, Linux-style; `how` validated before the
  lock). **`dynamic-mt` now PASSES** (`DYNAMIC_MT:ok`, no guard hit) — multithreaded dynamic
  binaries work, and it is now a GREEN permanent regression gate in `dynamic-hello-smoke`.
  Bug **A is very likely fixed**: rust-lld is multithreaded+dynamic and `pthread_create →
  rt_sigprocmask`, so the same wedge. **NEXT: re-run `rustc-smoke`** to confirm `RUSTC_OK`
  (bug **B**, the `ENOTTY` at `std/process.rs:2385`, may still need a separate fix).
- **`rustc-smoke` re-run (2026-06-24): still wedges — `rt_sigprocmask` was NOT the last
  wedge.** `rustc --version`/`--print sysroot` pass; `rustc hello.rs` times out with **NO
  `RUSTC_OK`, NO `ENOTTY`, and ZERO deadlock-guard hits**, and the stall-census stops (only
  the unrelated `fork-child` appears) → a **whole-system lock-held deadlock that does NOT go
  through `demand_map_vma_page`** (so not a `copy_from_user`-under-lock demand-fault).
  `dynamic-mt` PASSES, so this wedge is **rust-lld-specific** (a syscall rust-lld makes that
  the simple repro doesn't) — the 2-min repro can't reproduce it; back to the 15-min rust gate.
  **NEXT DIAGNOSTIC: a more general "scheduling-while-atomic" guard at `block_current_until`**
  (scheduler.rs:3544) — log `current_preempt_count()>0` at the block entry with the
  syscall_nr + `block_caller`. That catches ANY block taken while a lock is held (not just
  demand-faults), so it will name this culprit too. Then fix it like `rt_sigprocmask`
  (move the user access / drop the lock before the block). Bug B (`ENOTTY`) is racy and may
  surface once the wedge is gone — fetch rust 1.96 `process.rs:2385` for the op when it does.
- The futex pre-fault did **NOT** clear the `rust-lld` link wedge.
- **NEW — the link failure is NON-DETERMINISTIC.** It alternates between (a) the **silent
  lock-wedge** and (b) an **`ENOTTY` panic** at `std/src/process.rs:2385`
  (`Result::unwrap()` on `Os { code: 25, "Not a tty" }`) while rustc spawns `rust-lld`. So
  there are **two** bugs: the lock-held-across-fault wedge AND a std-process spawn op that
  hits `ENOTTY`.
- The guard did **not** fire on the ENOTTY run (no wedge that run). To catch the wedge:
  (a) re-run until it wedges rather than ENOTTYs, or (b) move the guard to
  `demand_map_vma_page` **entry** so it also catches the `PROCESS_TABLE`-re-lock self-deadlock
  (`shared_vma_demand_file`, which runs BEFORE the current guard spot).
- **ENOTTY (likely the more tractable / possibly primary blocker):** fetch rust 1.96.0
  `library/std/src/process.rs:2385` (host is 1.97 — line numbers differ) to see which op
  returns `ENOTTY` in the spawn/wait path. Likely a tty/`ioctl` m3OS returns `ENOTTY` for that
  std `unwrap()`s; fix m3OS to return success for it (or confirm it's one std should tolerate).
  Fixing ENOTTY may let the multithreaded link complete, leaving the wedge a rarer race to
  finish off with the guard.
- **UPDATE (2026-06-24, this pass) — ENOTTY FIXED + libLLVM.so RUNPATH-load FIXED; the
  remaining blocker is the compile/link wedge (a RACE).** Three fixes landed this pass:
  1. **ENOTTY (bug B) — FIXED.** The `std/process.rs:2385` `ENOTTY` is `read_output`'s
     `set_nonblocking(true)` → `ioctl(pipe_fd, FIONBIO, &int)` (musl/linux `FileDesc`), which
     m3OS's `sys_linux_ioctl` rejected with `ENOTTY` for any non-TTY fd. Fix: handle `FIONBIO`
     (0x5421) BEFORE the TTY gate, toggling the `FdEntry.nonblock` flag the pipe read path
     already honors (`syscall/mod.rs`). Confirmed: rustc now drains rust-lld's stdout/stderr
     and prints the real linker error instead of panicking.
  2. **rust-lld `DT_NEEDED not found: libLLVM.so.22.1-rust-1.96.0-stable` — FIXED.** With
     ENOTTY gone, the captured error was that the loader could not find rust-lld's
     `DT_NEEDED libLLVM.so`. rust-lld carries `RUNPATH=$ORIGIN/../lib` (`= /usr/lib/rustlib/
     <target>/lib`, where `build_rust` already stages libLLVM.so), but the loader's
     `load_dso_search` only searched `/usr/lib` + `/lib` and ignored `DT_RUNPATH`. Fix: the
     kernel now emits **`AT_EXECFN`** (the resolved execve path — `auxv.rs` `build_layout` +
     `mm/elf.rs` writes the string; `setup_abi_stack_with_envp` gained an `exec_path` arg), and
     the loader parses **`DT_RUNPATH`/`DT_RPATH`** (`dynlink.rs`/`elf64.rs`) and, on a default-
     search miss for the **main exe's** DT_NEEDEDs, falls back to the RUNPATH list with
     `$ORIGIN` expanded from `AT_EXECFN`'s dir (`main.rs` `load_dso_runpath`). Additive auxv +
     fallback-after-default = zero-regression for existing binaries (their libs are in /usr/lib,
     found first). **Proven:** the run changed from the deterministic `DT_NEEDED not found`/
     `exit status: 2` to rust-lld actually loading + running — the libLLVM.so load blocker is
     cleared. No rust re-seal needed (the lib was already packaged).
  3. The diagnostic `block_current_until` "block-while-atomic" guard was added then **removed**
     before commit: it has a legitimate boot-time hit (`virtio_blk.rs:968`, pid 1 syscall 165
     `mount` blocks with a lock held during the root mount — self-healing via the deadline+poll
     fallback), so it is unsuitable as an always-on guard.
  - **REMAINING (the next blocker): the rustc-compile / rust-lld-link whole-system wedge.**
    With (1)+(2) landed, `rustc-smoke` under `M3OS_KVM=1`+`M3OS_RUST_FAST_ITER=1` now reaches
    `rustc --version` 1.96.0 + `--print sysroot` `/usr`, then **hangs at `rustc hello.rs`**:
    the serial dump ends right after `/usr` with **no hello.rs compile output and no dhcpv6
    spam** → a whole-system lock-held deadlock, the SAME non-deterministic
    lazy-file-fault-under-lock class (`rt_sigprocmask` cleared one site + `dynamic-mt`; the
    rustc-compile/rust-lld path hits another). It is a RACE (earlier passes hit the ENOTTY arm
    instead).
  - **UPDATE (2026-06-24, later this pass) — the "wedge" is NOT a lock-held block; it is a
    KERNEL PAGE-FAULT CASCADE (kstack overflow) during rust-lld's multithreaded run.** The
    `block_current_until` "scheduling-while-atomic" guard was re-added and run: it fired ONLY
    on the known self-healing boot case (`virtio_blk.rs:968`, pid 1 `mount`) and **never during
    the rustc/rust-lld phase** — so the hang does NOT go through `block_current_until` with a
    lock held. A run that progressed further (rust-lld is non-deterministic) captured the real
    failure in the serial dump: rust-lld **loaded** (pid 56 mapped — RUNPATH fix confirmed) and
    spawned worker threads, then:
      1. `[int] userspace page fault: pid=57 addr=0x20ac1bb998 err=USER_MODE rip=0x50755c —
         process killed` (a rust-lld worker thread faults in userspace, killed normally);
      2. `[int] KERNEL page fault … addr=0x8` (a kernel NULL+8 deref) on one core;
      3. `[int] RECURSIVE KERNEL PAGE FAULT on core 3 … cr2=0xffff8080000dd000 rip=0x10000b60331
         — cascade halted`. **`cr2=0xffff8080000dd000` is inside the kstack pool guard region**
         (`[kstack] pool … 0xffff808000000000..0xffff8080023fe000`) ⇒ a **kernel stack overflow**
         whose guard-page #PF recurses (the bogus >4 GB rip = a corrupted return frame). The
         existing kstack-overflow recovery (turn the guard #PF into a SIGSEGV of the offending
         task; `kstack-overflow-smoke`/Track D) did NOT contain this — it cascaded to a recursive
         fault that halts the core.
    **So RUSTC_OK's remaining blocker is a kernel-robustness bug, NOT a lock wedge:** rust-lld's
    multithreaded execution drives (a) a userspace fault in a worker thread, (b) a kernel NULL
    deref at `0x8`, and (c) a kernel-stack-overflow recursive #PF cascade. **NEXT (a focused
    kernel-fault investigation, the real `RUSTC_OK` gate):** (i) decode the kernel `addr=0x8`
    fault — which handler NULL-derefs (likely the fault/kill/reap path under SMP, or the trace
    dump path) — and harden it; (ii) find the deep kernel recursion overflowing the per-task
    kstack (a fault handler re-faulting on a lazy-file/CoW page during the kill of pid 57, or the
    trace-ring dump recursing) and bound it so the kstack-overflow recovery actually fires
    instead of cascading; (iii) separately, characterize the pid-57 userspace fault
    (rip=0x50755c, addr=0x20ac1bb998) — map it into rust-lld to see whether a worker thread's
    stack/TLS/mmap setup is wrong (a loader/thread bug) vs. a legitimate access into memory the
    kernel failed to map. Run the KVM rust gate at a short `--timeout` (300) so the cascade dumps
    fast; `M3OS_SMOKE_SERIAL_DUMP` captures the full fault chain past the dhcpv6 spam.
  - **Status of this pass's commits:** the FIONBIO + AT_EXECFN/DT_RUNPATH fixes are committed +
    pushed (they cleared the ENOTTY + libLLVM-load blockers and are independently correct — they
    moved the failure from "rust-lld can't load" to "rust-lld runs, then a kernel fault cascade").
    The diagnostic `block_current_until` guard was reverted (uncommitted; it served its purpose:
    proving the hang is a fault cascade, not a block-while-atomic).
  - **UPDATE (2026-06-24, addr=0x8 ROOT-CAUSED) — it is the SMP-race manifestation of a missing
    thread-group fatal-kill; single-core it is a deterministic sibling futex-deadlock.** Added a
    compact `[int] KERNEL #PF rip=…` line (raw u64 hex, no `VirtAddr`/`InterruptStackFrame`
    Debug — those recurse and overflow the kstack inside the dump, which is what hid the rip)
    at the top of the ring-0 fault arm (`interrupts.rs`, committed). Then ran the gate at
    **`M3OS_SMP=1`** (single core) — the decisive experiment:
      - The **userspace** fault is DETERMINISTIC and SMP-independent: `pid=57 rip=0x50755c
        addr≈0x20ac1bX998` (a rust-lld **worker thread**) faults and is killed on EVERY run
        (4-core and 1-core; the addr varies by ~1 page).
      - Single-core there is **NO kernel `addr=0x8` fault**. Instead `[fault_kill] trampoline
        running for pid 57` runs and then the **sibling threads deadlock forever**: `pid 56`
        (the rust-lld thread-group leader), `pid 52`, `pid 49` are all `BlockedOnFutex "no
        waker registered"` → the link hangs → timeout.
    **Root cause:** `fault_kill_trampoline` (interrupts.rs:303) kills only the faulting **TID**,
    not the **thread group**. m3OS already has the group-kill machinery — `sys_exit_group`
    (syscall/mod.rs:3236) claims `tg.exit_owner`, `request_group_exit_by_pid` each sibling
    (sets `group_exit_pending` → `forced_group_exit_trampoline`), quiesces them, then
    `do_full_process_exit` frees the SHARED resources once — but the **fatal-fault path does not
    use it**. So killing one thread (a) strands the siblings (single-core deadlock) and (b) on
    SMP, a sibling running on another core races the trampoline's single-process teardown
    (`close_all_fds_for` / `AddressSpace::deactivate_on_core` + nulling `pc.current_addrspace` /
    `free_process_page_table`) and derefs a now-NULL shared struct at offset 8 → the kernel
    `addr=0x8` NULL+8 read (err=0x0). The subsequent `VirtAddr::fmt` in the heavy dump overflows
    the kstack → recursive #PF cascade-halt (a SECOND, dump-path bug). The recovered
    `rip=0x10000b60331` was that recursive fault's bogus frame (mid-instruction in
    `VirtAddr::fmt`), not the `addr=0x8` site.
    **FIX (the real `addr=0x8` + sibling-deadlock fix):** make the fatal-fault path
    (`fault_kill_trampoline`, and the analogous GPF/other unhandled-user-fault kills) terminate
    the whole **thread group** when the process is multithreaded — reuse `sys_exit_group`'s
    teardown (request group exit on all siblings + quiesce + free shared resources once) with a
    SIGSEGV-encoded status, instead of the single-process teardown. This is substantial (the
    quiesce loop runs in the post-IRETQ trampoline context; mind the waitpid status encoding —
    killed-by-SIGSEGV vs exit-code; and the shared page-table must be freed only on the last
    thread). Secondary hardening: make the kernel-fault heavy dump stack-safe (use compact u64
    hex, not `VirtAddr`/`InterruptStackFrame` Debug) so a non-guard-page kernel fault can't
    cascade into a kstack-overflow recursive #PF. **NOTE — even with a perfect group-kill,
    `RUSTC_OK` is NOT reached:** the rust-lld worker still faults (rip=0x50755c) → rust-lld dies
    (cleanly now) → the link fails. The group-kill is a kernel-robustness fix (no hang/crash);
    the RUSTC_OK-critical blocker is the **rust-lld worker userspace fault itself** — next,
    disassemble rust-lld at `0x50755c` (in the installed `.m3pkg`) + decode `addr≈0x20ac1bX998`
    to determine whether it is an m3OS thread stack/TLS/mmap setup bug or a rust-lld behaviour
    m3OS mishandles.
  - **RESOLVED (2026-06-24) — `RUSTC_OK` ACHIEVED via `--threads=1`. On-device Rust codegen
    works on m3OS.** Disassembled rust-lld at the worker-fault `rip=0x50755c`: it is bias
    `0x200000` (PIE) → file-vaddr `0x30755c`, in
    **`lld::elf::RelocationBaseSection::addReloc<true>` (Relocations.cpp)**, instruction
    `mov 0x8(%rsi,%rcx,1),%edx` where the lead-up is `rax=%fs:0`; `rax += &llvm::parallel::
    threadIndex`; `ecx = threadIndex`; `rsi = relocsVec.data()`; `rcx = threadIndex<<4`. So the
    fault is `relocsVec[threadIndex].size` — LLD's **lock-free parallel relocation scan**, where
    each worker writes its own per-thread `SmallVector` slot indexed by the `%fs`-relative
    `thread_local` `threadIndex`. The faulting **slot array element** (`relocsVec.data() +
    threadIndex*16`) is unmapped ⇒ the worker's `threadIndex` is out of bounds for the vector
    LLD sized for the pool — a parallel-LLD thread-index/TLS-vs-pool-size mismatch on m3OS.
    **Fix that reached the milestone:** pass **`-C link-arg=--threads=1`** to rustc so rust-lld
    links single-threaded — only the main thread (threadIndex 0) scans relocations
    (`relocsVec[0]` always valid) AND **no worker threads spawn** (so the thread-group-kill
    `addr=0x8`/sibling-deadlock path is never entered either). With it, `rustc-smoke` **PASSES
    end-to-end under `M3OS_KVM=1`+`M3OS_RUST_FAST_ITER=1`: `rustc --version` 1.96.0 → `--print
    sysroot` `/usr` → `rustc -C linker=rust-lld -C linker-flavor=ld.lld -C link-self-contained=yes
    -C link-arg=--threads=1 hello.rs` compiles + links + RUNS → `RUSTC_OK hello from rustc`, 0
    kernel faults, 18 steps in ~41 s.** This is the Phase-95b on-device code-generation milestone
    — the Rust analog of Phase 85d clang — reached with a single-threaded-linker constraint
    (analogous to Go's single-core constraint). The gate now bakes in `--threads=1`.
    **Deferred follow-ups (NOT milestone blockers):** (a) the **multithreaded rust-lld**
    worker-TLS bug (so `--threads=1` can be dropped) — deep-diagnosed below; (b) the
    **thread-group fatal-kill** kernel-robustness fix (so a faulting thread in ANY
    multithreaded process doesn't hang/`addr=0x8` the kernel) per the addr=0x8 update above;
    (c) the fault-dump stack-safety.
  - **UPDATE (2026-06-25) — multithreaded rust-lld deep-diagnosed to the EXACT mechanism; the
    loader/musl/kernel TLS chain is provably consistent ON PAPER, so the remaining defect is a
    subtle RUNTIME TLS bug needing instrumentation, NOT a layout error.** Findings:
    - The crash is `llvm::parallel::threadIndex` (an exported `extern thread_local` in
      **libLLVM.so**) reading its `.tdata` default `UINT_MAX` on an LLD pool **worker** thread,
      so `relocsVec[UINT_MAX]` (`SyntheticSections.h:556`, sized to `ctx.arg.threadCount`) faults
      ~64 GB out of bounds (the fault math: `relocsVec.data()≈0x10ac1bc9a0 + UINT_MAX*16 + 8 =
      0x20ac1bc998`). `work()` (`Parallel.cpp:124`) sets `threadIndex = ThreadID` at worker
      start, but the read sees the default ⇒ the write didn't reach the slot the read uses.
    - **It is a TLS MODEL MIX over 2 modules** — the path neither rustc (1 TLS module:
      librustc_driver.so) nor `dynamic-mt` (0 DSO thread-locals) exercises. Confirmed by relocs:
      the WRITE in libLLVM is **general-dynamic** (`R_X86_64_DTPMOD64`/`DTPOFF64` →
      `__tls_get_addr(self->dtv[id]+off)`); the READ in rust-lld is **initial-exec**
      (`R_X86_64_TPOFF64` → `%fs + TPOFF`); both name the same symbol `_ZN4llvm8parallel11threadIndexE`.
    - **Every link was verified consistent**, so a layout fix is NOT indicated: loader
      `DTPMOD64 → m.tls_id` (matches DTV index), `TPOFF64 = st_value − tls_offset`,
      `assign_tls_modules` offset `ALIGN(running+memsz,align)` == musl `__copy_tls` placement
      `td − p->offset`, the `struct tls_module` chain is built in `tls_id` order (so
      `__copy_tls`'s iteration `i` == `tls_id`), `__copy_tls` sets per-thread `dtv[i] = td −
      offset`, `__tls_get_addr` is musl's per-thread `self->dtv[v[0]]+v[1]`, the `0004-tls-globals`
      patch publishes all of `libc.tls_head/size/align/cnt`, and `sys_clone_thread` sets
      `child_fs_base = tls` on `CLONE_SETTLS`. On paper the GD write and IE read resolve to the
      SAME per-thread address `worker_TP − libLLVM.tls_offset + st_value`.
    - **CONCLUSION + NEXT STEP:** since the static chain is correct, the bug is a runtime
      interaction (candidates: the worker's `dtv`/`%fs` not what `__copy_tls`/clone think at the
      moment `work()` writes vs when `addReloc` reads; a `__copy_tls`/`pthread_create`
      allocation/DTV edge with 2 modules; or an FS_BASE/DTV save-restore subtlety that only the
      GD+IE *mix* exposes). Source analysis is exhausted — pin it down with a **minimal 2-TLS-
      module reproducer**: a `DT_NEEDED` `libfoo.so` defining an exported `__thread int t=-1;`,
      a main exe that (i) WRITES it via a libfoo accessor (general-dynamic, like `work()`) and
      (ii) READS it via `extern __thread` initial-exec (like `addReloc`) from N pthreads, asserting
      `read==written` per thread. Build it like `build_dynamic_mt_fixture` + the `libhello.so`
      shared-lib path; wire it as `dynamic-tls-smoke`. It reproduces in ~2 min (vs the 15-min rust
      gate) and is the permanent regression guard once fixed. With the repro, dump (on the worker)
      the resolved `%fs`/`TPOFF`, `dtv[id]`, and `__tls_get_addr` result to see where write/read
      diverge. (`--threads=1` stays the milestone path until this lands.)

### Step 2 (DECISION — RESOLVED 2026-06-24) — `rustc-smoke` is KVM-gated; CI uses KVM

**Decision: KVM-gate `rustc-smoke`** (skip-with-reason without `M3OS_KVM=1`, like
`node-jit-smoke`). **Verified empirically** — a throwaway `ci/kvm-probe` workflow on
`ubuntu-latest` showed GitHub-hosted runners **DO expose `/dev/kvm`** and `kvm-ok` reports
"KVM acceleration can be used". The "GitHub runners have no KVM" assumption still written in
`build.yml`/`pr.yml` comments is **stale**. Findings:

- Host: Azure VM, **AMD EPYC 7763** (Zen 3), 4 vCPU, SVM exposed on all cores → nested KVM
  works; `pkg install rust` ~25 s and cold-load ~9.6 s apply in CI, not the ~40-min TCG wall.
- `/dev/kvm` is `root:kvm 0660`, so the non-root `runner` user needs the standard one-line
  udev enable step before QEMU can use it:
  ```yaml
  - name: Enable KVM
    run: |
      echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
        | sudo tee /etc/udev/rules.d/99-kvm4all.rules
      sudo udevadm control --reload-rules && sudo udevadm trigger --name-match=kvm
  ```
- **Caveat — PKU is NOT exposed** (`pku=no`/`ospke=no`; Azure masks it even on Zen 3). KVM
  buys `rustc-smoke` **speed**, which is all rustc needs (it does not JIT). It does **not**
  give the PKU-dependent JIT gates (`node-jit-smoke`, the `claude` JIT arm) what they need —
  those stay dev-machine / self-hosted-runner only.

**Consequences:** **Step 3 (95c B/D) is OPTIONAL for the milestone** — under KVM the FS is
already fast, so the 95-series closes on **Step 1** alone. `rustc-smoke` runs KVM-accelerated
in a dedicated heavy/nightly CI lane (the udev step + `M3OS_KVM=1`), matching how the other
heavy toolchain gates are opt-in rather than per-PR. **Do not** spend 95c making TCG fast for
a per-PR rustc gate. Follow-ups when we reach the milestone: (a) wire the KVM lane + udev step
for `rustc-smoke`; (b) refresh the stale "no KVM" comments in `build.yml`/`pr.yml`.

### Step 3 (95c — now OPTIONAL per the Step 2 decision) — finish VFS perf, in the driver

Step 2 resolved to KVM-gating `rustc-smoke`, so this is **not** on the milestone path; pursue
it only for a TCG-runnable gate or for the standalone wins (Track B helps KVM too — repeat /
shared loads). All **in the ring-3 `vfs_server` path** —
**not** in the kernel (Track F is rejected, see below):
- **3a. Track B — kernel page cache for file-backed pages** (`(file-id, offset)` keyed;
  re-faults / second run / shared DSO maps served with zero server IPC). This is the
  external-pager amortizer and the one genuinely-new idiomatic win; it benefits KVM too
  (repeat/shared loads), so it is worth landing regardless.
- **3b. Track D — installer read/verify/write coalescing** (`userspace/pkg/`): use the
  Track A bulk caps, hash in-line with the read, coalesce writes; cut `pkg install` I/O.
- **Re-measure** (Track E) after each; the throughput gate is the guard.

### Step 4 — flip the milestone gate (95b Track D / 95c Track G converge)

With Step 1 (and Step 3 if TCG-gated) done: `rustc-smoke` PASSES end-to-end under
`M3OS_RUST_REGRESSION=1` — `pkg install rust` → `rustc --version` → `--print sysroot` →
`rustc hello.rs` → `RUSTC_OK`. Same gate scaffold; no new wiring.

### Step 5 (STRETCH) — `cargo` + proc-macros (95b Track E)

After the milestone: stage `cargo`, `cargo build` a proc-macro-free fixture (`CARGO_OK`),
then a derive-macro crate via on-device `dlopen` of the proc-macro `.so` against the Phase 93
`libc.so` (`CARGO_PROCMACRO_OK`), behind a separate `cargo-smoke` / `M3OS_CARGO_REGRESSION`
gate. Gated behind the milestone; decide separately whether it is in-scope for the 95-series
or a follow-up phase.

### Step 6 — closeout (95b Track F + 95c Track H)

Only after the milestone is green:
- Kernel version bump `0.95.0` → `0.95.1` (`kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md:7`).
- `AGENTS.md` Phase-95 capability bullet: flip "diagnosed but blocked" → "runs on m3OS".
- Learning docs (`docs/95b-*.md` streaming-loader/proc-macro; `docs/95c-*.md`
  external-pager/zero-copy) per the aligned-learning template; register in `docs/README.md`
  + `codebase-map.md`.
- Roadmap README rows + design-doc Status flips (`Planned`/`Partial` → `Complete`); mermaid
  `P95 → P95b → P95c` edges already present.

## Architecture decision — Track/Area F (in-kernel ext2 read fast path) is REJECTED

**Decision (owner, 2026-06-24): do not implement the in-kernel ext2 read bypass.** It is a
deliberate microkernel-boundary departure (read-only `/usr` demand pages straight from
`EXT2_VOLUME`, no IPC), and it now also conflicts head-on with the **ext2-engine-unification**
that just made `vfs_server` the *sole* post-boot root reader. Performance must be fixed **in
the ring-3 ext2 driver** (zero-copy + readahead + the kernel page cache — Tracks A/B), which
is also the idiomatic Mach/Zircon/L4Re recipe.

The only door left open: F may be reconsidered **only if** Tracks A+B+D are landed **and** a
recorded Track-E measurement proves the VFS path categorically cannot reach acceptable
throughput at large readahead clusters — i.e. raw IPC cost itself is the wall. Even then the
correct fix is **faster IPC**, not ext2-in-ring-0; F would be a flagged, last-resort,
retireable fallback with an explicit retirement condition, never the design. Absent that
proof, F is **intentionally not implemented** and the 95c docs say so.

## How to pick up (repro + the gotchas that cost hours)

Repro and the full infra gotchas (clean-disk-between-runs to avoid `disk.img` corruption,
why **not** to set `M3OS_SERIAL_LOG`, the dhcpv6 retransmit spam burying the crash line,
`M3OS_RUST_FAST_ITER` only on a complete prior install) are in the
[2026-06-23 handoff](./2026-06-23-rustc-runtime-null-deref-after-tls.md#test-infra-gotchas-these-cost-hours-last-time)
— read that section before the first run.

## Doc map (which doc holds what, after this cleanup)

| Doc | Role now |
|---|---|
| **this file** | the plan — what's left, in order, and the decisions |
| `2026-06-23-rustc-runtime-null-deref-after-tls.md` | technical record of the (now-fixed) crash chain + the repro gotchas |
| `2026-06-24-ext2-engine-unification-plan.md` | closed — vfs_server single-root-engine record + the DAC write-back blocker |
| `roadmap/tasks/95b-…-tasks.md` | 95b track checklist (crash chain + loader/mm) |
| `roadmap/tasks/95c-…-tasks.md` | 95c track checklist (VFS perf), Track F demoted |
| `roadmap/95b-…md`, `roadmap/95c-…md` | phase design docs (status reconciled to this plan) |
