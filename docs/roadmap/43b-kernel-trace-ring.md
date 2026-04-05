# Phase 43b - Kernel Trace Ring

**Status:** Complete
**Source Ref:** phase-43b
**Depends on:** Phase 43a (Crash Diagnostics & Assertions) ✅
**Builds on:** Extends the crash diagnostics from Phase 43a by adding a structured event timeline; uses per-core data from Phase 35; instruments scheduler, fork, and IPC paths already hardened with assertions in Phase 43a
**Primary Components:** trace_ring module (kernel-core), trace emit helper (kernel), scheduler/fork/IPC instrumentation, panic/fault auto-dump, sys_ktrace syscall

## Milestone Goal

Every kernel crash includes a structured timeline of recent scheduler, fork, and
IPC events. The per-core lockless trace ring records events with timestamps, and
the auto-dump on panic/fault merges all cores into a single sorted timeline in
the serial output. The ring is host-testable, accessible from userspace, and
feature-gated for zero-cost release builds.

## Why This Phase Exists

Phase 43a made crashes self-diagnosing with register dumps and assertions, but
the diagnostics show only the state at the moment of the crash. SMP race bugs
often require understanding the sequence of events leading into the crash:
which tasks were dispatched, which yielded, which IPC messages were exchanged,
and on which cores. A structured trace ring provides this timeline without
requiring reproduction or manual instrumentation.

## Learning Goals

- Understand lockless per-core ring buffers and why they avoid contention
- Understand how structured event enums enable machine-parseable crash dumps
- Understand feature gating for zero-cost debug infrastructure
- Understand syscall design for exposing kernel state to userspace

## Feature Scope

### Trace Ring Data Structure

A `TraceRing<N>` generic ring buffer in kernel-core, host-testable with
standard `cargo test`. Fixed-size circular buffer that overwrites oldest entries
on wrap. No mutex, no atomics on the write path.

### Trace Event Enum

A closed `TraceEvent` enum with three families: scheduler (6 variants), fork
(4 variants), and IPC (7 variants). Each variant carries the minimum fields
needed to reconstruct the transition.

### Instrumentation

One-line `trace_event(...)` calls at 15 instrumentation sites across the
scheduler, fork, and IPC paths. The emit helper handles timestamping and
core-ID tagging automatically.

### Crash Auto-Dump

`dump_trace_rings()` merges all per-core rings into a sorted timeline and
prints via `_panic_print`. Wired into the panic handler and all fault handlers
after the existing `dump_crash_context()` from Phase 43a.

### Userspace Access

`sys_ktrace` (syscall `0x1002`) copies raw trace entries to a userspace buffer.
Wrapper in syscall-lib.

## Important Components and How They Work

### TraceRing<N> (`kernel-core/src/trace_ring.rs`)

A const-generic ring buffer. `push()` writes at `write_idx` and advances;
`snapshot()` returns entries in chronological order by computing the start
position from `write_idx` and `count`. The ring is initialized with
`TraceEntry::EMPTY` values.

### trace_event() (`kernel/src/trace.rs`)

Reads `tick_count()` and the current core ID, constructs a `TraceEntry`, and
pushes to `per_core().trace_ring`. The function checks `is_per_core_ready()`
first to handle early boot before SMP init. When the `trace` feature is off,
compiles to a no-op.

### dump_trace_rings() (`kernel/src/trace.rs`)

Iterates all online cores via `get_core_data()`, snapshots each ring, merges
into a Vec sorted by tick, and prints each entry. Uses `_panic_print` (the
deadlock-safe serial path) since it runs from panic/fault context.

### PerCoreData.trace_ring (`kernel/src/smp/mod.rs`)

A `TraceRing<256>` field gated behind `#[cfg(feature = "trace")]`. Initialized
to `TraceRing::new()` in both BSP and AP init paths. Each core writes only to
its own ring.

## How This Builds on Earlier Phases

- Extends Phase 43a by adding `dump_trace_rings()` after `dump_crash_context()`
  in the panic handler and all fault handlers
- Uses the per-core data infrastructure from Phase 35 to store per-core rings
- Instruments the same scheduler, fork, and IPC paths that Phase 43a hardened
  with `debug_assert!` boundaries
- Follows the `LogRing` pattern from kernel-core for host-testable ring buffers

## Implementation Outline

1. Implement `TraceRing<N>`, `TraceEvent`, `TraceEntry` in kernel-core
2. Add host-side unit tests for the ring buffer
3. Add `trace_ring` field to `PerCoreData` (BSP and AP init)
4. Implement `trace_event()` and `dump_trace_rings()` in kernel
5. Instrument scheduler paths (6 sites)
6. Instrument fork paths (4 sites)
7. Instrument IPC paths (7 sites)
8. Wire `dump_trace_rings()` into panic/fault handlers
9. Implement `sys_ktrace` syscall and syscall-lib wrapper
10. Add feature gate (`trace` feature on kernel crate)
11. Validate with `cargo xtask check` and `cargo xtask test`

## Acceptance Criteria

- `cargo test -p kernel-core` passes with all trace ring tests
- `cargo xtask check` passes with trace feature on and off
- `cargo xtask test` passes with trace feature on
- `cargo build -p kernel --no-default-features` compiles without trace code
- Trace ring dump appears in serial output after crash diagnostics on panic
- `sys_ktrace` syscall copies trace entries to userspace buffer

## Companion Task List

- [Phase 43b Task List](./tasks/43b-kernel-trace-ring-tasks.md)

## How Real OS Implementations Differ

- Linux uses `ftrace` with per-CPU ring buffers backed by `ring_buffer.c`,
  supporting binary format, filtering, function graph tracing, and live
  streaming via `/sys/kernel/tracing/`
- FreeBSD uses `DTrace` (and now also `ktrace`) with kernel-userspace
  cooperation and sophisticated filtering
- Production systems use binary trace formats (CTF, perf.data) for efficient
  storage and offline analysis tools
- Lock-free ring buffers in production kernels use memory ordering barriers
  and sequence counters for safe concurrent reader/writer access

## Deferred Until Later

- Binary trace format for off-target analysis tooling
- Userspace `ktrace` command-line tool
- Network-accessible trace export
- Trace filtering (per-subsystem enable/disable)
- Trace event for timer ISR / reschedule signal
- TSC-based sub-tick ordering
