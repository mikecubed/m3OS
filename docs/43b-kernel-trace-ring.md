# Phase 43b — Kernel Trace Ring

**Aligned Roadmap Phase:** Phase 43b
**Status:** Complete
**Source Ref:** phase-43b
**Depends on:** Phase 43a (Crash Diagnostics & Assertions)

## Overview

Phase 43b adds a per-core lockless trace ring that records scheduler, fork, and
IPC events with timestamps. When the kernel crashes, the trace ring is
automatically dumped alongside the crash diagnostics from Phase 43a, giving
developers a structured timeline of the kernel transitions leading into the
fault. The ring is host-testable in kernel-core, accessible from userspace via
`sys_ktrace`, and feature-gated so it compiles out for release builds.

## What This Doc Covers

- How the per-core trace ring data structure works
- What events are recorded and their field meanings
- How to read a trace dump in crash output
- How the feature gate enables/disables tracing
- The `sys_ktrace` syscall for live userspace access

## Core Implementation

### Trace Ring Data Structure (`kernel-core/src/trace_ring.rs`)

`TraceRing<N>` is a fixed-size circular buffer of `TraceEntry` values. It uses
no mutex and no atomics for the write path -- single-writer semantics are
guaranteed by the per-core ownership model (only the owning core writes to its
ring).

- **Push**: overwrites the oldest entry when the ring is full
- **Snapshot**: returns all entries in chronological order (oldest first)
- **Size**: 256 entries per core (covers ~4 full fork+dispatch+fault cycles)
- **Entry size**: `TraceEntry { tick: u64, core: u8, event: TraceEvent }`

The ring is host-testable via `cargo test -p kernel-core`.

### Trace Events (`kernel-core/src/trace_ring.rs`)

The `TraceEvent` enum has three families:

**Scheduler events:**

| Variant | Fields | Emitted at |
|---|---|---|
| `Dispatch` | task_idx, core, rsp | After `pick_next` returns and state is set to Running |
| `SwitchOut` | task_idx, core, saved_rsp | After `switch_context` returns in the dispatch loop |
| `YieldNow` | task_idx, core | Before `switch_context` in `yield_now` |
| `BlockCurrent` | task_idx, core, new_state | Before `switch_context` in `block_current` |
| `WakeTask` | task_idx, state_before, core | After state set to Ready in `wake_task` |
| `RunQueueEnqueue` | task_idx, core | After `push_back` in `enqueue_to_core` |

**Fork events:**

| Variant | Fields | Emitted at |
|---|---|---|
| `ForkCtxPublish` | pid, rip, rsp | After fork context is stored in `spawn_fork_task` |
| `ForkTaskSpawned` | pid, task_idx, core | After the child task is pushed to the task vec |
| `ForkTrampolineEnter` | pid, task_idx | At entry to `fork_child_trampoline` |
| `ForkTrampolineExit` | pid, rip, rsp | Immediately before `enter_userspace_fork` |

**IPC events:**

| Variant | Fields | Emitted at |
|---|---|---|
| `RecvBlock` | task_idx, ep | Before `block_current_on_recv` |
| `RecvWake` | task_idx, ep | After `take_message` returns successfully |
| `SendBlock` | task_idx, ep | Before `block_current_on_send` |
| `SendWake` | task_idx, ep | Before `wake_task(receiver)` in send |
| `CallBlock` | task_idx, ep | Before `block_current_on_reply` |
| `ReplyDeliver` | caller_idx, ep | Before `wake_task(caller)` in reply |
| `MessageDelivered` | task_idx, ep | When a pending sender is matched in recv |

### Trace Emit Helper (`kernel/src/trace.rs`)

A single `trace_event(event)` call at each instrumentation site handles
timestamping (via `tick_count()`) and core-ID tagging automatically. When the
`trace` feature is off, the function compiles to a no-op.

### Panic/Fault Auto-Dump (`kernel/src/trace.rs`)

`dump_trace_rings()` is called after `dump_crash_context()` in:
- The panic handler (`kernel/src/main.rs`)
- Page fault handler (both ring-0 and ring-3 paths)
- GPF handler (both ring-0 and ring-3 paths)
- Double fault handler

It iterates all online cores and prints each core's ring independently
(oldest to newest) via `_panic_print`. No heap allocation is performed —
the non-allocating `for_each_chronological()` API is used instead of
`snapshot()`. Entries are not merged or sorted across cores; each core's
events appear as a contiguous block.

### `sys_ktrace` Syscall (`kernel/src/arch/x86_64/syscall.rs`)

Syscall `0x1002`: `sys_ktrace(core_id, buf_ptr, buf_len) -> entries_written`

Copies raw `TraceEntry` bytes from the specified core's trace ring into a
userspace buffer. Returns the number of entries written, or `u64::MAX` on
invalid core ID or bad pointer.

Userspace wrapper: `syscall_lib::ktrace(core_id, buf)`.

## Reading a Trace Dump

Example crash output showing the trace ring dump after the crash diagnostics:

```
=== CRASH DIAGNOSTICS ===
--- CPU Registers ---
...
--- Current Task ---
  task_idx=5 on core 0
--- Per-Core State ---
>>> core 0 | online=true task_idx=5 resched=false run_queue=1
    core 1 | online=true task_idx=-1 resched=true run_queue=0
=== END CRASH DIAGNOSTICS ===
=== TRACE RING DUMP ===
  [1042] core=0 Dispatch { task_idx: 3, core: 0, rsp: 0xffff80000002f800 }
  [1042] core=0 RunQueueEnqueue { task_idx: 5, core: 0 }
  [1043] core=0 YieldNow { task_idx: 3, core: 0 }
  [1043] core=0 SwitchOut { task_idx: 3, core: 0, saved_rsp: 0xffff80000002f780 }
  [1043] core=0 Dispatch { task_idx: 5, core: 0, rsp: 0xffff800000031800 }
  [1044] core=0 ForkCtxPublish { pid: 7, rip: 0x401234, rsp: 0x7fffff000 }
  [1044] core=0 ForkTaskSpawned { pid: 7, task_idx: 8, core: 0 }
  [1044] core=0 RunQueueEnqueue { task_idx: 8, core: 0 }
=== END TRACE RING DUMP ===
```

**How to read it:**
- **`[tick]`** -- monotonic tick counter (10ms per tick)
- **`core=N`** -- which CPU core emitted this event
- Events appear in chronological order across all cores
- Look for the last few events before the crash to understand what transition
  was in progress

## Feature Gate

The trace feature is controlled by the `trace` cargo feature on the `kernel`
crate:

```toml
# kernel/Cargo.toml
[features]
default = ["trace"]
trace = []
```

When enabled (default):
- `PerCoreData` includes a `TraceRing<256>` field
- `trace_event()` records events into the per-core ring
- `dump_trace_rings()` prints the merged timeline on crash
- `sys_ktrace` syscall is available

When disabled:
- `trace_event()` and `dump_trace_rings()` compile to no-ops
- `PerCoreData` does not include the ring buffer
- `sys_ktrace` syscall is not registered

The `TraceEvent` and `TraceEntry` types in `kernel-core` are always compiled
(they are zero-cost types). Only the storage and recording in the kernel is
gated.

## Performance Characteristics

- **Lockless**: no mutex acquisition on the hot path
- **Per-core**: no contention between cores (critical for SMP race debugging)
- **Fixed overhead**: one ring buffer write per instrumented path (~256 entries
  per core, wrapping)
- **Zero cost when disabled**: compiles out completely with `--no-default-features`

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/trace_ring.rs` | `TraceRing<N>`, `TraceEvent`, `TraceEntry` -- host-testable |
| `kernel/src/trace.rs` | `trace_event()`, `dump_trace_rings()` |
| `kernel/src/smp/mod.rs` | `PerCoreData.trace_ring` field |
| `kernel/src/task/scheduler.rs` | Scheduler instrumentation (6 sites) |
| `kernel/src/process/mod.rs` | Fork instrumentation (2 sites) |
| `kernel/src/ipc/endpoint.rs` | IPC instrumentation (7 sites) |
| `kernel/src/arch/x86_64/syscall.rs` | `sys_ktrace` syscall |
| `userspace/syscall-lib/src/lib.rs` | `ktrace()` userspace wrapper |

## How This Phase Differs From Later Debugging Work

- This phase records structured events in a ring buffer for crash-time replay.
  It does not provide filtering, live streaming, or binary export.
- Phase 43c will add regression and stress testing infrastructure that can
  exercise the trace ring under concurrent load.
- Binary trace format for off-target analysis tooling is deferred.
- A userspace `ktrace` command-line tool can be added as a coreutils extension.
- Trace filtering (per-subsystem enable/disable) is deferred.
- Timer ISR / reschedule signal events are deferred.

## Related Roadmap Docs

- [Phase 43b task doc](./roadmap/tasks/43b-kernel-trace-ring-tasks.md)
- [Phase 43a learning doc](./43a-crash-diagnostics.md)

## Known Limitations

- **Tick resolution is 10ms** -- events within the same 10ms window share the
  same tick value. TSC-based sub-tick ordering is deferred.
- **No filtering** -- all instrumented events are always recorded. High-frequency
  scheduler events may dominate the ring on busy systems.
- **256 entries per core** -- at high event rates, the ring may wrap before a
  crash dump. Increasing the ring size trades memory for history depth.
- **`sys_ktrace` copies raw bytes** -- the userspace caller must know the exact
  `TraceEntry` layout. A structured serialization format is deferred.
