# Phase 57c — Kernel Busy-Wait Audit Catalogue

**Status:** Complete (Phase 57c Track A)
**Source Ref:** phase-57c
**Generated from:** `git grep -En 'core::hint::spin_loop|while !.*\.load\(|while .*\.try_lock\(\)' kernel/src/`

This is the durable source of truth for the Phase 57c busy-wait audit.
Every `core::hint::spin_loop()`, `while !X.load(...)`, and `while X.try_lock().is_none()` invocation
in `kernel/src/` is listed below with its classification and rationale.
Each entry maps to a Track B (convert), Track C (annotate), or Track D/leave (leave) task.

---

## Audit Table

| File:Line | Symbol | Spin Pattern | Holder | Bound | Decision | Rationale |
|---|---|---|---|---|---|---|
| `kernel/src/smp/ipi.rs:46` | `wait_icr_idle` | `while lapic_read(ICR_LOW) & (1<<12) != 0` | LAPIC hardware clears delivery-pending bit | HW-bounded: ~1 µs (Intel SDM Vol 3A §10.6 *Local APIC ICR Delivery*) | **annotate** (C.1) | This is a fundamental SMP primitive; a context switch costs more than the spin. Converting to block+wake would require the LAPIC to generate an interrupt on delivery completion, which it does not. |
| `kernel/src/smp/tlb.rs:120` | `tlb_shootdown` (full-range) | `while SHOOTDOWN_PENDING.load(Acquire) > 0` | Remote CPUs' TLB-shootdown IPI handlers decrement the counter | Bounded by IPI delivery latency + remote IRQ handler runtime; all remote-CPU work is itself bounded and lock-free | **annotate** (C.1) | Classic cross-core synchronisation; IF is enabled so this core services its own IRQs. Converting to block+wake here is unsafe — we are holding a preempt-disabled section protecting the shootdown atomics. |
| `kernel/src/smp/tlb.rs:220` | `tlb_shootdown_range` | `while SHOOTDOWN_PENDING.load(Acquire) > 0` | Same as above | Same as above | **annotate** (C.1) | Same rationale as tlb.rs:120; both shootdown entry points share the same wait pattern. |
| `kernel/src/smp/boot.rs:277` | `lapic_udelay` | LAPIC timer countdown loop | LAPIC hardware decrements its own count register | HW-bounded: ≤ `us` microseconds; used only during AP startup IPI sequence (one-shot, init-time) | **annotate** (C.1) | Init-time only; not on any hot path. AP boot delay is required by the APIC IPI startup protocol (Intel SDM Vol 3A §8.4.4). |
| `kernel/src/iommu/intel.rs:247` | `wait_gsts_bit` | `for _ in 0..GSTS_POLL_LIMIT { if bit set { return } spin_loop() }` | VT-d hardware sets GSTS register after processing a command | HW-bounded: GSTS_POLL_LIMIT iterations × IOMMU register update latency (Intel VT-d spec §11.4.4, typically < 1 µs) | **annotate** (C.2) | Hardware register handshake; no software agent holds the condition. Cannot convert to IRQ-driven wake because the IOMMU does not generate an interrupt on GSTS update. |
| `kernel/src/iommu/intel.rs:368` | `invalidate_context_cache_global` | `for _ in 0..GSTS_POLL_LIMIT { if CCMD_ICC clear { return } spin_loop() }` | VT-d hardware clears ICC bit after context-cache invalidation completes | HW-bounded (Intel VT-d spec §11.4.4) | **annotate** (C.2) | Same as intel.rs:247; different register, same HW-bounded wait pattern. |
| `kernel/src/iommu/intel.rs:390` | `invalidate_iotlb_global` | `for _ in 0..GSTS_POLL_LIMIT { if IOTLB_IVT clear { return } spin_loop() }` | VT-d hardware clears IVT bit after IOTLB invalidation completes | HW-bounded (Intel VT-d spec §11.4.4) | **annotate** (C.2) | Same as intel.rs:247; IOTLB invalidation register. |
| `kernel/src/iommu/amd.rs:339` | `flush_cmd_queue` | `loop { if observed == marker { break } spin_loop() }` | AMD-Vi hardware processes the command ring and writes the completion marker to the store page | HW-bounded: AMD-Vi IOMMU PPR/command queue completion latency (AMD IOMMU spec §3.3.3, typically < 1 µs per command) | **annotate** (C.2) | AMD-Vi does not provide an interrupt-on-completion mechanism for command-queue drain. The completion marker pattern is the canonical AMD-Vi flush protocol. |
| `kernel/src/arch/x86_64/ps2.rs:239` | `wait_input_clear` | `for _ in 0..POLL_BUDGET { if status clear { return } spin_loop() }` | PS/2 controller hardware clears STATUS_INPUT_FULL when it is ready to accept another byte | HW-bounded: POLL_BUDGET iterations × status-port read latency; PS/2 controller response is in the microsecond range (IBM PC/AT Technical Reference Manual §A-2) | **annotate** (C.3) | PS/2 init-path operation during keyboard setup; runs only during init. |
| `kernel/src/arch/x86_64/ps2.rs:252` | `wait_output_full` | `for _ in 0..POLL_BUDGET { if output full { return } spin_loop() }` | PS/2 controller hardware sets STATUS_OUTPUT_FULL when a byte is ready | HW-bounded (same as ps2.rs:239) | **annotate** (C.3) | Same as ps2.rs:239; paired read counterpart. |
| `kernel/src/arch/x86_64/apic.rs:436` | `calibrate_lapic_timer` (PIT spin) | `while pit_gate.read() & 0x20 == 0` | 8254 PIT channel 2 hardware sets bit 5 of port 0x61 when 10 ms countdown completes | HW-bounded: exactly 10 ms (the PIT 10 ms calibration window); runs once during LAPIC initialisation at boot | **annotate** (C.3) | One-time boot-path PIT→LAPIC calibration; the 10 ms window is deterministic and not attributable to any user workload. |
| `kernel/src/main.rs:185` | `kernel_main` (timer-IRQ detection, debug builds only) | `for _ in 0..10_000_000u32 { spin_loop(); if tick_count advanced { break } }` | Timer IRQ handler advances tick_count | `debug_assertions` only; bounded by 10 M iterations × spin_loop hint (≈ 200 ms at 50 ns/iter worst case); runs once during init | **annotate** (C.6) | Debug diagnostic only, never compiled in release builds. Not on any user-attributable path. |
| `kernel/src/mm/frame_allocator.rs:942` | `drain_per_cpu_page_caches` | `while DRAIN_PENDING.load(Acquire) != 0` | Remote CPUs' IPI handlers decrement DRAIN_PENDING after draining their per-CPU magazine | Bounded by IPI delivery latency + per-CPU drain runtime; analogous to TLB shootdown wait; preempt-disabled section, IF enabled | **annotate** (C.4) | Cross-core IPI ack wait identical in structure to TLB shootdown. Preempt-disabled + IF-enabled guard makes it safe. Cannot yield here. |
| `kernel/src/mm/slab.rs:495` | `collect_remote_frees` (slab reclaim) | `while SLAB_RECLAIM_PENDING.load(Acquire) != 0` | Remote CPUs' IPI handlers decrement SLAB_RECLAIM_PENDING after completing their magazine flush | Bounded by IPI delivery latency + remote magazine flush runtime (both are bounded and lock-free) | **annotate** (C.4) | Identical structure to frame_allocator drain spin; same rationale. |
| `kernel/src/rtc.rs:90` | `read_rtc` (update-in-progress wait) | `while update_in_progress() && spins < MAX_UIP_SPINS` | RTC chip (MC146818) hardware clears UIP bit when its internal update cycle completes | HW-bounded: MC146818 update window is ~244–248 µs max (published figures vary slightly by rounding; Motorola MC146818A datasheet §2.3); MAX_UIP_SPINS provides a hard upper limit | **annotate** (C.5) | RTC hardware update window; already has a spin-count guard. Not on a user-visible hot path. |
| `kernel/src/task/scheduler.rs:2699` | `wake_task` (`on_cpu` wait) | `while on_cpu_ref.load(Acquire)` | The remote core's scheduler clears `on_cpu` after completing context-switch-out | Bounded by cross-core context-switch completion time; documented in Phase 57a (`SCHEDULER.lock` is not held during this spin — see 57a Track B.3 notes). IF stays enabled. | **leave** (C.7) | Documented in Phase 57a Track B.3 as a deliberate bounded spin. The `on_cpu` flag is cleared with memory ordering guarantees on the next context switch out; converting to block+wake here would require holding the scheduler lock longer and risks deadlock with the woken task's pi_lock. |
| `kernel/src/task/scheduler.rs:2910` | `test_task_entry` (`#[cfg(test)]`) | `loop { spin_loop() }` | — (never executed — this function is a test scaffolding entry point, never dispatched by the scheduler) | N/A — function is never called in any running path | **leave** (C.7) | Test-only stub. The function exists to satisfy `Task::new`'s function-pointer requirement in host-side unit tests; the scheduler never enqueues tasks with this entry. |
| `kernel/src/task/mod.rs:667` | `dummy_task_entry` (`#[cfg(test)]`) | `loop { spin_loop() }` | — (same as scheduler.rs test_task_entry above) | N/A | **leave** (C.7) | Test-only stub. Never dispatched; exists only to construct `Task` objects in host-side unit tests. |
| `kernel/src/arch/x86_64/syscall/mod.rs:3302` | `sys_nanosleep` (< 1 ms TSC busy-spin) | `while rdtsc() - start < sleep_tsc` | TSC hardware advances monotonically | HW-bounded: ≤ 1 ms by construction (caller path requires `sleep_us < 1_000`); a context switch at 10 ms AP timer granularity would overshoot the sleep target by 10× | **leave** (C.7) | Already documented in the syscall implementation. Sub-millisecond spin is correct: a yield costs ~10 ms which is 10× the sleep duration. The 1 ms upper bound is enforced by the caller's branch condition. |

---

## Summary by Decision

| Decision | Count | Sites |
|---|---|---|
| **convert** (Track B) | 0 | — (all known convert sites were already handled in Phase 57a) |
| **annotate** (Track C) | 15 | ipi.rs:46, tlb.rs:120, tlb.rs:220, boot.rs:277, intel.rs:247, intel.rs:368, intel.rs:390, amd.rs:339, ps2.rs:239, ps2.rs:252, apic.rs:436, main.rs:185, frame_allocator.rs:942, slab.rs:495, rtc.rs:90 |
| **leave** (C.7 / documented) | 4 | scheduler.rs:2699, scheduler.rs:2910, task/mod.rs:667, syscall/mod.rs:3302 |

> **Note on Track B:** The audit finds no new unbounded-convert sites. The Phase 57a migration already converted all unbounded busy-spins in `virtio_blk`, `sys_poll`, `net_task`, `WaitQueue::sleep`, `futex_wait`, and the NVMe device-host path. Track B's work is therefore verification that these conversions remain intact under the new audit lens, plus regression tests for the verified paths.

---

## Track B — Verification Map

| Prior conversion | File | Block+wake source | Verification task |
|---|---|---|---|
| `virtio_blk::do_request` | `kernel/src/blk/virtio_blk.rs` | Completion ring IRQ → `DONE` flag | B.1: verify, add regression test |
| `sys_poll` no-waiter loop | `kernel/src/arch/x86_64/syscall/mod.rs` | `WaitQueue::sleep` | B.1: verify |
| `net_task` NIC wake | `kernel/src/main.rs` | `block_current_until(&NIC_WOKEN, None)` | B.2: verify |
| `WaitQueue::sleep` | `kernel/src/task/wait_queue.rs` | `block_current_until` | B.1: verify |
| `futex_wait` | `kernel/src/arch/x86_64/syscall/mod.rs` | `block_current_until` | B.1: verify |
| NVMe device-host | `kernel/src/` (device-host gate) | Userspace driver, no kernel spin | B.1: verify no kernel-side spin |

---

## How to Use This Catalogue

- Future reviewers: look up any `core::hint::spin_loop()` in `kernel/src/` here. If the decision is **annotate**, verify the comment exists in the source. If the decision is **leave**, verify the prior-phase doc comment exists.
- Phase 57e Track B: every **annotate** row is a candidate for a `preempt_disable()` + `preempt_enable()` wrapper when `PREEMPT_FULL` makes them load-bearing. The comment added by 57c names the phase where the wrapper lands.
- Phase authors: when adding a new kernel spin, start with this catalogue as the template and add a new row here.
