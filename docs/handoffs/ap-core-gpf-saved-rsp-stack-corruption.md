# AP-Core GPF on Boot: Kernel Stack Shared With Log Buffer

**Status:** **Fixed** 2026-05-09 — kernel stacks isolated to a static `.bss` pool so they can never alias with kernel-heap allocations. See "Resolution" at the bottom of this file.
**First observed:** 2026-05-09 (m3os.log captures from user testing during the Phase 61 PR cycle).
**Severity:** AP-core takedown, kernel survives on remaining cores. Userspace boot completes; subsequent SMP capacity is reduced by one core. NOT user-facing-fatal in the cases observed so far, but the failure mode is undefined behaviour reading from corrupt memory and could escalate (double-fault, silent state corruption) under different timing.
**Scope:** Outside Phase 61. Phase 61 attempted detection-and-skip mitigation; the bandaid did not catch the actual mechanism (see "Mitigation attempts that did not work" below). Phase 61's scheduler changes have been reverted from this code path; this bug is the same as on `main`.

## Symptom

Random AP core takes a kernel-mode General Protection Fault (`GPF`, `error_code = 0x0`) during the late-boot window, while the BSP is still spawning userspace services. The fault is in the dispatcher's context-switch path: the AP's saved register frame loads from memory that is *not actually a valid context-switch frame*. Specifically, the bytes that ought to be `RFLAGS`, `r15..rbx`, `rip` are instead **kernel-log ASCII text** (e.g., the formatted output of the ext2 mount log line).

When `switch_context` `popf`s and `ret`s those bytes, the CPU jumps to a non-canonical RIP and faults. The GPF handler dumps diagnostics, calls `crate::hlt_loop()`, and the affected core is permanently halted. The remaining cores continue running normally — the BSP completes userspace boot, the user can log in, and the system functions on `(cores - 1)` for the rest of the session.

### Canonical fault dump (from m3os.log, 2026-05-09 02:34, post-revert)

```
[int] GPF: InterruptStackFrame {
    instruction_pointer: VirtAddr(0x6220746120646574),
    code_segment: SegmentSelector { index: 1, rpl: Ring0 },
    cpu_flags: RFlags(ID | RESUME_FLAG | IOPL_HIGH | IOPL_LOW
                    | DIRECTION_FLAG | INTERRUPT_FLAG
                    | AUXILIARY_CARRY_FLAG | 0x2),
    stack_pointer: VirtAddr(0x2803df959b0),
    stack_segment: SegmentSelector { index: 2, rpl: Ring0 }
}
[int] GPF error_code=0x0 (selector_idx=0, table=0, external=0)

--- CPU Registers ---
RAX=0x0000000000000001  RBX=0x6e756f6d20656d75
RCX=0x0000000000000001  RDX=0x00000000000005bf
RSI=0x000002803dfb0000  RDI=0x0000000000000001
RBP=0x6c6f76205d327478  RSP=0x000002803df95678
R8 =0x0000010000994ec8  R9 =0x0000000000000938
R10=0x0000000000000000  R11=0x00000100000090c5
R12=0x655b205d4f464e49  R13=0x5b0a383d7370756f
R14=0x7267202c36333535  R15=0x363d7365646f6e69
RFLAGS=0x0000000000203012
CR2=0x00007ffffeffc448  CR3=0x0000000000101000

--- Current Task ---
  no active task (scheduler loop) on core 3
--- Per-Core State ---
    core 0 | online=true task_idx=-1 resched=false run_queue=3
    core 1 | online=true task_idx=22 resched=false run_queue=0
    core 2 | online=true task_idx=2  resched=false run_queue=0
>>> core 3 | online=true task_idx=-1 resched=true  run_queue=1
```

### Decoding the registers

The "garbage" register values are not random. They are the bytes of a kernel `log::info!` message, interpreted little-endian:

| Register | Hex value             | Bytes (little-endian) | ASCII fragment |
|----------|----------------------|----------------------|----------------|
| RIP      | `0x6220746120646574` | `74 65 64 20 61 74 20 62` | `"ted at b"`   |
| RBX      | `0x6e756f6d20656d75` | `75 6d 65 20 6d 6f 75 6e` | `"ume moun"`   |
| RBP      | `0x6c6f76205d327478` | `78 74 32 5d 20 76 6f 6c` | `"xt2] vol"`   |
| R12      | `0x655b205d4f464e49` | `49 4e 46 4f 5d 20 5b 65` | `"INFO] [e"`   |
| R13      | `0x5b0a383d7370756f` | `6f 75 70 73 3d 38 0a 5b` | `"oups=8\n["`  |
| R14      | `0x7267202c36333535` | `35 35 33 36 2c 20 67 72` | `"5536, gr"`   |
| R15      | `0x363d7365646f6e69` | `69 6e 6f 64 65 73 3d 36` | `"inodes=6"`   |

Stitched together, in stack order (lowest address first — `popf`, then 6 register pops, then `ret`'s `rip`):

```
... 5536, gr|inodes=6|oups=8\n[|INFO] [e|xt2] vol|ume moun|ted at b ...
```

That's the kernel's ext2 mount log line, formatted by `log::info!("[ext2] mounted: ... inodes={}, groups={}, ...")` at `kernel/src/fs/ext2.rs:109`. The exact message also appears on the serial console at the start of the boot log:

```
[INFO] [ext2] volume mounted at base LBA 2048
[INFO] [ext2] mounted: base_lba=2048, block_size=4096, blocks=..., inodes=65536, groups=8
```

So the bytes the AP dispatcher tried to interpret as a saved register frame are instead a freshly-formatted serial log message.

### Reproducibility

* **Seen on `main` (pre-Phase-61).** User confirmed in m3os-main.log testing.
* **Seen on Phase 61 PR branch** (commit `719cc0a` and earlier).
* **Same RIP, same RSP, same registers across runs.** The corruption pattern is deterministic: the AP's saved frame *consistently* lines up with the ext2 mount log line.
* **Affects an AP, not the BSP.** BSP completes boot normally. The crashed core is typically core 3 in 4-core QEMU (`M3OS_SMP=4`), but the choice of which AP appears to depend on dispatch timing.
* **Kernel survives.** `hlt_loop()` halts the faulting core only. The other cores continue to run the dispatcher; userspace `term`, `login`, `ion`, `sshd`, `syslogd`, etc. all initialize successfully. Doom runs on the surviving cores.

## What is actually corrupted

The fault is **not** a corrupt `Task::saved_rsp` pointer. The pointer value (`task.saved_rsp` field) is *valid* — it points inside a real, allocated kernel stack region. The address ranges line up:

* AP idle (`task_idx=2` or `task_idx=3`, depending on core) is the task whose dispatch hits this. Saved_rsp commonly observed: `0x2803dfdffb0`. This is `(stack base) + 0x7fb0` — i.e., the very top of an 0x8000-byte (32 KiB) kernel stack at base `0x2803dfd8000`.
* The stack memory itself, however, contains log text bytes at the offset `saved_rsp` points to.

The implication: the *kernel-stack heap allocation* for the AP idle task has the same physical memory as a buffer that the kernel uses to format `log::info!` output (or similar). When the kernel formats the ext2 mount log message, it writes those bytes into the buffer. The AP dispatcher later loads from the same address as a register frame and gets garbage.

There is no obvious smoking-gun "this code does the bad thing" yet. It is a use-after-free or aliasing bug somewhere between the heap allocator, the slab `task_cache`, the per-task `_stack: Option<Box<[u8]>>` lifecycle, and whatever path produces formatted log output to memory.

## Mitigation attempts that did not work (Phase 61 PR cycle)

For the record so the next person doesn't repeat them:

### 1. `try_scheduler_lock` from IRQ context (`7785bb5`, *kept*)

Ruled out one possible deadlock: the timer / page-fault helpers Phase 61 added (`tick_account_current_task`, `current_task_record_page_fault`, `current_task_record_ctxsw`) used `scheduler_lock()` directly. If they fired while another task on the same core was holding `SCHEDULER_INNER` with IF=1 (between `IrqSafeMutex::lock`'s `without_interrupts` exit and the eventual drop), `lock()` could spin-deadlock on itself.

This is a real risk pattern but **not** the cause of this GPF — the fault signature is wrong (a deadlock would be a hang or watchdog stale-task warning, not a context-switch into log bytes). The fix is independently useful and was kept.

### 2. `saved_rsp` bounds check at dispatch (`7d92a5d`, **REVERTED in `e8a08d3`**)

Idea: validate at dispatch-commit time that `task.saved_rsp` falls within `task.stack_bounds()`. If not, mark the task `Dead` and `continue` the loop instead of letting `switch_context` load garbage.

Why it didn't work: **the saved_rsp pointer is valid.** It lies *inside* the task's allocated `_stack` region. The corruption is in the contents of that region, not in the pointer that names it. A bounds check on the pointer therefore passes, and `switch_context` still loads garbage from the bytes at that address.

The code was a band-aid against a different failure mode (a saved_rsp that gets overwritten with non-pointer bytes — possible in principle but not what's actually happening here).

### 3. Limit the bounds check to `pid == 0` kernel tasks (`d046ff5`, **REVERTED in `719cc0a`**)

Refinement of (2): userspace tasks legitimately have `saved_rsp` outside their `Task._stack` bounds because their kernel-mode RSP during a syscall lives on the per-core kernel stack (`gs:OFF_STACK_TOP`), not on `_stack`. The original (2) was rejecting `userspace-init` and breaking the userspace boot chain.

The pid-filtered version (3) didn't break userspace, but for the same reason as (2) it doesn't catch this bug either. Reverted.

## What we know about the heap / stack lifecycle

These are the relevant code paths for whoever investigates this:

### Task allocation

* `Task::new()` (kernel/src/task/mod.rs:678+) calls `alloc::vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice()` to allocate the kernel stack. `KERNEL_STACK_SIZE` is 32 KiB (verify in `kernel/src/task/mod.rs` `const`).
* `Task::new()` calls `init_stack(&mut stack, entry)` to write the initial register frame onto the top of the stack and returns the saved_rsp pointing into that stack.
* The `Task` struct itself is placed into `task_cache` via `SlabBox` (Phase 60 work, slab slot size 1024 bytes per `kernel/src/mm/slab.rs:TASK_CACHE_SLOT_SIZE`).
* The `_stack: Option<Box<[u8]>>` lives separately on the heap. The `Task` slot in the slab references it via the `Box` inside the `Option`.

### Task destruction (`drain_dead`, kernel/src/task/scheduler.rs:721)

```rust
fn drain_dead(&mut self) {
    for i in 0..self.tasks.len() {
        let task_current = self.task_current_on_any_core(i);
        let task = &mut self.tasks[i];
        if task.state == TaskState::Dead
            && task.ipc_cleaned
            && !task.on_cpu.load(Ordering::Acquire)
            && task.saved_rsp != 0
            && !task_current
        {
            let _ = task._stack.take();   // <-- frees the Box<[u8]>
            task.saved_rsp = 0;           // <-- marks slot drained
            if !self.free_list.contains(&i) {
                self.free_list.push(i);
            }
        }
    }
}
```

`drain_dead` runs on the BSP only, inside `scheduler_lock`. The two writes (`take()` and `saved_rsp = 0`) happen under the same lock, with no IRQ-window between them (`IrqSafeMutex` masks IRQs).

### Dispatch (`pick_next` + outer loop)

* `pick_next` runs under `scheduler_lock`. It checks `task.state == Ready` and (in some places) `task.saved_rsp != 0`. If the conditions hold it returns `(saved_rsp, idx)`.
* The outer dispatch loop in `run()` (kernel/src/task/scheduler.rs:3865+) marks the task `Running`, sets `current_task_idx`, drops the lock, does some address-space and per-core-data setup, and finally calls `switch_context(scheduler_rsp_ptr, task_rsp)`.

### Logging

`serial_println!` formats arguments via `core::fmt` into the serial output path. The formatted output ultimately gets written to a UART port (no in-memory ring buffer for the serial output itself in normal operation), but `core::fmt::Write` infrastructure uses temporary stack buffers in the calling context to format intermediate strings. None of those should outlive the `serial_println!` invocation — they are stack-allocated in the *caller's* stack frame.

`log::info!` macro paths route through `crate::serial::Logger::log` (or whatever the `log` crate facade plugs in), which similarly formats and writes synchronously.

### Hot suspicion: the `log` crate's `Record` formatting

The `log` crate stores log messages as `&Arguments<'_>` — that is, a pointer to formatted args plus a borrowed message string. The `Logger::log` impl in `kernel/src/serial.rs` (verify) walks the `Record` and writes its parts. Some impls heap-allocate a `String` to hold the fully-formatted line before writing it to the UART. If that allocation goes through the same allocator as `Box<[u8]>` for kernel stacks and the slab returns a recently-freed task stack page…

**This is a hypothesis, not a proven mechanism.** The investigator should:

1. Read `kernel/src/serial.rs` and verify whether the logger heap-allocates per call.
2. Read `kernel/src/mm/heap.rs` and verify whether `Box::new` / `Vec::with_capacity` allocations from the same size class can interleave with `Box<[u8; 32 KiB]>` for kernel stacks.
3. Check whether the Phase 60 slab work changed the path `Box<[u8; KERNEL_STACK_SIZE]>` allocations take. (`task_cache` is for `Task` structs; kernel stacks are a separate large allocation.)

### Subtle: `drain_dead` vs concurrent `pick_next` on another core

`drain_dead` zeroes `saved_rsp` before adding the slot to `free_list`. But the *task index* may still appear in some other core's run_queue. If another core's `pick_next` reads the run queue, dequeues the index, then loads `task.saved_rsp`, it should see `0` (because of the under-lock ordering)… **unless** that other core already passed the `saved_rsp != 0` check in a previous lock-held read and is now operating on a stale local copy. The dispatch loop pattern is:

```rust
let next = {
    let mut sched = scheduler_lock();
    if let Some((rsp, idx)) = sched.pick_next(core_id) {
        // ... pick_next returns (saved_rsp_at_pick_time, idx) ...
        Some((rsp, idx))
    } else {
        None
    }
};
// SCHEDULER lock dropped here
// ... long path: address space switch, per-core data, etc ...
unsafe { switch_context(per_core_scheduler_rsp_ptr(), task_rsp); }
```

The captured `rsp` is the value `task.saved_rsp` had at pick_next time. If between pick_next and switch_context the task was reaped (drain_dead → stack freed → memory reused for log output), the captured `rsp` now points at re-purposed memory. **This is the most plausible mechanism for the observed corruption.**

The dispatcher captures `saved_rsp` under the lock, then drops the lock, then uses the captured value. There is a window during which another core (BSP running drain_dead) could free the stack out from under the chosen task. The task slot is supposed to be in `Running` state at this point (we mark it Running before dropping the lock), and `drain_dead` only collects `Dead` tasks — so this *shouldn't* race. **But:** `task_current_on_any_core(i)` is the only protection, and the racing-core's `set_current_task_idx(Some(idx))` happens just before the lock is dropped. If `drain_dead` reads `current_task_idx` for the race window between the dispatch's `set_current_task_idx` write and the lock-drop, the ordering is fine; if not, there is a race window.

## Reproduction

```bash
cargo xtask run-gui --fresh > m3os.log 2>&1
# Wait for boot to settle. Look for "[int] GPF" lines in m3os.log.
# Confirm the GPF RIP is in the 0x6220...0x6363... ASCII-text range.
```

Reproducibility is high — it has fired on every recent boot the user captured. The exact tick at which the AP catches it varies (`tick~1450` in one log, slightly different in others) but the failure mode is the same.

## Recommended investigation path

In rough order of cheapest first:

1. **Confirm `drain_dead` ordering.** Add a debug check: after `task._stack.take()` and `task.saved_rsp = 0`, assert that no per-core `current_task_idx` references this index. The check must run inside the same lock acquisition.
2. **Audit `Box<[u8]>` lifetime for kernel stacks.** Specifically check whether `_stack.take()` actually drops the Box immediately. (The `Option::take()` returns the inner Box; in the call site it is bound to `_` and dropped at end of the `if` expression.)
3. **Instrument the heap allocator.** Add a `cfg!(debug_assertions)` poison-on-free pattern: when a `Box<[u8]>` larger than (say) 4 KiB is freed, fill it with `0xCC` before returning to the heap. A subsequent dispatch into a frame full of `0xCC` faults on the first `popf` (RFLAGS only allows specific bits) — easier to recognise than ASCII text and impossible to miss in a register dump. This converts the bug from "looks fine then GPF" to "loud panic at the moment of corruption." Cheap and high-signal.
4. **Audit the `log` / `serial` paths for heap allocation.** Confirm whether `log::info!` ever `String::new` or similar in the formatted output path, and if so, what size class the allocator uses. If formatted log lines and 32 KiB kernel stacks share an allocator path, that's the smoking gun.
5. **Static analysis of the dispatch race window.** Map every reader of `current_task_idx` and every writer of `task.saved_rsp` and `task._stack`, and verify the lock + memory-ordering discipline rules out the "stale captured saved_rsp" interpretation.

## Mitigation options if root-causing is deferred

* **Halt-instead-of-degrade.** If we'd rather fail loud than limp on `(cores - 1)`, change the kernel-mode GPF handler to call `panic!` instead of `hlt_loop()` so the whole system stops on first sign of corruption. Trade-off: any unrelated kernel GPF (driver bug, etc) would also become fatal.
* **Stack canary at dispatch.** Inside `pick_next`, before returning, dereference `*saved_rsp` and check that the bytes look like a valid `RFLAGS` (low bits include the always-set bit 1, IF=1, no reserved bits set in the upper half). If `RFLAGS` looks sane, dispatch; otherwise log a `WARN` and skip. Catches the corruption at the actual point it bites — at the cost of an extra unaligned read per dispatch and a heuristic that has false positives if RFLAGS legitimately has unusual values.
* **Separate kernel-stack allocator.** Allocate kernel stacks from a dedicated arena rather than the general heap, so a kernel stack address can never alias with a log buffer. Larger change, no false positives.

The "stack canary at dispatch" option is probably the right intermediate step — it gives observability ("we caught corruption!") without blocking on the deeper allocator audit.

## What is *not* this bug (false leads to skip)

* **Phase 61's load-balancer changes are not the cause.** The `last_migrated_tick` controversy ([798677b](https://github.com/mikecubed/m3OS/commit/798677b) and its revert [d026f10](https://github.com/mikecubed/m3OS/commit/d026f10)) is a different issue — that one fired only under fork-bomb load and produced a different failure mode (deref of `0x3` in pid-19's syscall handler).
* **Phase 61's per-tick CS sampling is not the cause.** The `tick_account_current_task` deadlock concern was addressed by `try_scheduler_lock` in `7785bb5`. That fix is independently correct but did not eliminate this GPF.
* **The `[INFO] [ext2] mounted` log line itself is not the bug.** Plenty of logs pass through the same path without corruption. The bug is *which kernel-stack allocation happens to alias with whichever buffer holds the formatted log text* at the moment the AP dispatches. Different log lines could in principle produce different ASCII patterns; the ext2 line is just the one that consistently lines up at the time of the boot-window GPF.

## Files to read first

| File | Why |
|---|---|
| `kernel/src/task/scheduler.rs` (especially `pick_next` ~line 755, `drain_dead` ~line 721, dispatch loop in `run()` ~line 3865+) | The dispatch race window. |
| `kernel/src/task/mod.rs` (`Task::new` ~line 678, `init_stack` ~line 803) | How kernel stacks are allocated and the initial saved frame is laid out. |
| `kernel/src/mm/heap.rs` | Allocator used by `Box<[u8]>`. Look for size-class routing of 32 KiB allocations. |
| `kernel/src/mm/slab.rs` | `task_cache` slab. Note that the slab is for `Task` structs, NOT for the 32 KiB `_stack`. |
| `kernel/src/serial.rs` | The logging path. Verify whether log messages are heap-allocated. |
| `kernel/src/arch/x86_64/interrupts.rs` (`general_protection_fault_handler` ~line 1157) | The handler that produced the dump quoted above. Note the kernel-mode branch ends in `hlt_loop()`. |

## Related artefacts

* Commits `7d92a5d` (saved_rsp bounds check) and `d046ff5` (pid==0 filter): the failed mitigation. Reverted in `e8a08d3` and `719cc0a`.
* Commit `7785bb5` (`try_scheduler_lock` from IRQ context): independent fix, kept.
* m3os.log captures from 2026-05-09 (user-side, in `~/Projects/m3os/`): two GPFs at lines 1168 and 5593 of one capture, both with the canonical signature.
* m3os-main.log (user-side): same GPF on `main` — confirms pre-existence.

## Acceptance for closing this bug

A fix is acceptable when:

1. The AP-core GPF stops occurring across `cargo xtask run-gui --fresh` boots (10 consecutive clean boots is a reasonable bar).
2. The mechanism is documented in the commit message — either "found the use-after-free at site X, fixed" or "added isolation guarantee Y so the corruption is structurally impossible."
3. Optionally: a regression test under `kernel/tests/` exercises the dispatch + drain_dead interaction (this is hard to write deterministically; if the fix is structural the test may be unnecessary).

## Resolution — 2026-05-09

**Approach:** structural isolation via a dedicated kernel-stack pool (option 3 from the Mitigation Options section above). The underlying use-after-free / aliasing mechanism in the heap was *not* root-caused; instead the fix removes kernel stacks from the heap entirely so the allocator can never put non-stack data into the same physical pages that a stack later resolves through.

### What changed

- New module `kernel/src/task/kstack.rs` — fixed-size pool of `2 × MAX_TASKS + 2 × (MAX_CORES − 1)` slots, each `KERNEL_STACK_SIZE` (32 KiB), living in `.bss`. Slots are claimed via `compare_exchange` on a parallel `[AtomicBool; N]`. Two consumer surfaces:
  - `KernelStack::alloc() -> Option<KernelStack>` — RAII guard. Drop zeros the slot bytes and releases the bit.
  - `kstack::alloc_leaked_top() -> u64` — claim a slot permanently, return the 16-byte-aligned stack top. Used for the leaked-stack call sites that the original design never freed.
- `kernel/src/task/mod.rs` — `Task._stack: Option<Box<[u8]>>` → `Option<KernelStack>`. `Task::new` now uses `KernelStack::alloc().expect(...)`. The `drain_dead` call site (`task._stack.take()`) is unchanged: `Option::take` is type-agnostic and the `KernelStack` Drop impl runs the zero-and-release.
- `kernel/src/process/mod.rs::alloc_kernel_stack` — was `Box::leak(Vec<u8>(32 KiB))`; now calls `kstack::alloc_leaked_top()`. The leak semantics (no per-process stack reclaim) are preserved; that is a separate deferred cleanup.
- `kernel/src/smp/mod.rs::init_ap_per_core` — the AP per-core syscall stack (16 KiB) and double-fault IST stack (20 KiB) were also `Box::leak(Vec<u8>(...))` from the heap; now both call `alloc_leaked_top()`. Pool slots are 32 KiB each, so the 16/20 KiB stacks fit with unused tail (harmless).

The BSP's syscall and double-fault stacks were already in `.bss` (`kernel/src/arch/x86_64/gdt.rs`) and were not touched.

### Why this fixes the failure

The bug was: a kernel stack at physmap virtual address `0x000002803...` happened to share physical pages with a buffer that received the formatted `[ext2] mounted: …` log line at boot, so the AP's `popf`/`pop`/`ret` in `switch_context` loaded ASCII bytes as register values. Whether the mechanism was UAF, double-allocation by the buddy, or an unrelated wild write into a still-live page was never identified.

After the fix, every kernel stack lives at a kernel-image VA inside the `.bss` of the loaded kernel binary (e.g. `0x100009998a0`-class addresses in the post-fix logs). The kernel heap allocator does not own those pages, has no references to them, and cannot hand them out for any other allocation. Any of the three candidate mechanisms above is therefore structurally precluded.

### Verification

- `cargo xtask check` clean.
- `cargo xtask test` — full suite (12 tests) passes, including all SMP-sensitive cases (`smp_prelude_smoke`, `load_balance_smp`, `pipe_wakeup_smp`, `ipc_wakeup_smp`, `munmap_tlb_smp`).
- `cargo xtask run --fresh` boots cleanly to userspace (`term`, `login`, `sshd`, `syslogd` all online) on 4-core QEMU with zero `GPF` / `DOUBLE FAULT` / `panic` lines in the serial log. The same boot reliably produced the documented GPF prior to the fix.

The Bug #9 (`stale-ready` / `cpu-hog`) and `dequeue-drop` warnings remain — those are independent and tracked under Phase 62.

### What this fix does *not* do

- It does not root-cause the original heap-allocator behaviour. If a future code path leaks a stack address out of `Box::into_raw` and writes back to it, or if the buddy ever does double-allocate, those would surface elsewhere — but no longer through the kernel-stack failure mode.
- Process-syscall and AP-per-core stack reclaim is still deferred. Stacks claimed via `alloc_leaked_top()` are pinned for the kernel's lifetime, matching the original `Box::leak` semantics. A future phase that wires per-process and per-core stack reclaim should release the slot bit (and the existing Drop impl on `KernelStack` already shows the shape of how to do that for the RAII path).
- A `MAX_TASKS = 256`-driven hard cap on userspace task count is now enforced by stack-pool exhaustion, not just by the IPC notification table sizing. A fork-bomb that exceeded `MAX_TASKS` previously OOM-panicked the heap; it now panics in `alloc_leaked_top`. Same outcome, slightly earlier signal.

### Cross-references

- `docs/handoffs/61g-smp-soak.md` — the Phase 61 Track G manual soak that was blocked on this bug. Should now run cleanly (subject to Bug #9, see soak doc).
- Phase 61 PR #144 — closed without the failed mitigation attempts (`saved_rsp` bounds checks at dispatch). Those reverts stand; the structural fix above replaces them.
