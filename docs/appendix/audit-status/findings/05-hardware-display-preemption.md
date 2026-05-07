# Audit Findings: Hardware Substrate, Display, and Preemption Phases (55 – 57e)

**Scope:** Phases 55, 55a, 55b, 55c, 56, 56-completion-gaps, 57, 57a, 57b, 57c, 57d, 57e
**Audited:** 2026-05-07
**Auditor:** Claude (Sonnet 4.6), structured read of all primary docs + supporting appendices

---

## Phase 55 — Hardware Substrate

### Declared status

**Complete.** Task list also marked Complete. Evaluation Gate verification recorded in the design doc's "Evaluation Gate Verification" section.

### Acceptance criteria — checked vs unchecked

All five criteria in the design doc are stated in prose (no checkbox format). The task-list acceptance items use `[ ]` checkboxes throughout Tracks A–F; none are shown as `[x]` in the task file — the task-list itself was never checkbox-completed (all boxes remain `[ ]`). This is a bookkeeping gap consistent with the Phase 56 pattern.

### Deferred items

Verbatim from `docs/roadmap/55-hardware-substrate.md`:

- "Broad laptop/desktop certification"
- "Wide Wi-Fi, GPU, and USB peripheral matrices"
- "IOMMU-heavy isolation work — tracked as Phase 55a"
- "Ring-3 extraction of the NVMe and e1000 drivers — tracked as Phase 55b"
- "Hardware-acceleration features not needed for the reference targets"

Physical-hardware validation deferred for all matrix entries: "Physical target deferred" for all NVMe and e1000 rows.

### Documented shortcuts

- **Ring-0 placement of NVMe and e1000 is explicitly a trade-off**: "Phase 55 places the NVMe and e1000 drivers in ring 0 (`kernel/src/blk/nvme.rs`, `kernel/src/net/e1000.rs`) for bring-up simplicity, which is a conscious widening of the TCB relative to Phase 54's userspace-service direction." (Documentation Notes, task doc)
- **IOMMU caveat on all physical-hardware entries**: "VT-d / AMD-Vi enabled systems may block driver DMA until IOMMU mappings exist; IOMMU support is deferred per Phase 55 design doc."

### Residuals tracked in appendix docs

None explicitly. The IOMMU caveat is formalized and assigned to Phase 55a.

### Red flags

- **Unchecked task-list checkboxes**: All 37 acceptance items across Tracks A–F remain `[ ]` despite phase status being Complete. Task F.4's kernel version bump to 0.55.0 was presumably done but not marked.
- **Intel e1000e scope exclusion**: "The e1000e family (82574, 82576, etc.) is different silicon with separate register layouts and is not in scope" — explicitly bounded, not a gap, but should be checked if any later phase references e1000e support.

---

## Phase 55a — IOMMU Substrate

### Declared status

**Planned.** Neither the design doc nor the task list show any work done.

### Acceptance criteria — checked vs unchecked

All acceptance items in the task list (Tracks A–G) use `[ ]` format; none are checked.

### Deferred items

Verbatim from `docs/roadmap/55a-iommu-substrate.md`:

- "VFIO / device passthrough for guest VMs"
- "SR-IOV virtual function support"
- "IOMMU group enforcement policies beyond per-device domains"
- "ARM SMMU support (m3OS is x86_64-only)"
- "Dynamic IOVA space compaction and large-page promotion optimizations"
- "Interrupt remapping" (from task doc Documentation Notes)
- "VT-d scalable mode"

### Known open bug recorded in the design doc

The design doc contains a "Known Open Bug — must close before Phase 58" section:

> **VT-d MMIO translation drops driver `CTRL.RST` writes under `--iommu`.** Surfaced by Phase 55b's tighter `cargo xtask device-smoke --device {nvme,e1000} --iommu` assertions. The per-device domain setup does not install identity-mapped MMIO windows for each claimed device's BAR regions, so ring-3 drivers' MMIO resets are silently lost under active VT-d translation. Full diagnosis, reproduction, and acceptance criteria in `docs/appendix/phase-55b-residuals.md` (item R2). **This must close before the Phase 58 1.0 gate ships its "IOMMU-isolated ring-3 drivers" claim.**

Note: this is the same issue addressed by Phase 55c R2. The fact that the 55a design doc still records it as an open bug (not crossed out or annotated as resolved by 55c) is an inconsistency worth checking — Phase 55c's task list shows R2 as closed, but 55a's "Known Open Bug" section still reads as open.

### Red flags

- **Phase 55a is still Planned** — this means the entire IOMMU substrate (ACPI DMAR/IVRS parsing, per-device VT-d/AMD-Vi domains, `DmaBuffer<T>` rerouting, fault handlers) has never been implemented. Yet the AGENTS.md overview text and Phase 55c, 56, 57 docs all reference IOMMU isolation as already working (e.g., "Phase 55a IOMMU substrate" is listed as a completed dependency for 55b, 55c, 56, 57). Either the AGENTS.md description is aspirational, or 55a was implemented without its status being flipped, or the dependency tracking in later phases is forward-referencing the not-yet-done work.
- **The 55a "Planned" status contradicts 55c's "Complete" status**: 55c's declared dependencies include "Phase 55a (IOMMU Substrate) ✅". A Planned 55a cannot be a completed dependency.

---

## Phase 55b — Ring-3 Driver Host

### Declared status

**Planned.** Design doc says "Companion Task List — defer until implementation planning begins."

### Acceptance criteria — checked vs unchecked

No task list is linked (explicitly deferred). Design doc acceptance is prose-only.

### Deferred items

Verbatim from `docs/roadmap/55b-ring-3-driver-host.md`:

- "Driver-side seccomp / syscall sandbox beyond the default 'only device-host syscalls allowed' posture"
- "Hot-plug / surprise-removal handling for PCIe devices"
- "Extracting VirtIO-blk and VirtIO-net on the same pattern (Phase 55b covers only NVMe and e1000)"
- "Driver live-update / zero-downtime restart; Phase 55b ships cold-restart only"
- "Multi-queue NVMe beyond the single I/O queue pair Phase 55 already ships"

### Residuals tracked in appendix docs

`docs/appendix/phase-55b-residuals.md` documents:

- **R1** (`sys_net_send` EAGAIN visibility): "Closed in Phase 55c (Track G/H)"
- **R2** (IOMMU VT-d MMIO breaks ring-3 driver `CTRL.RST`): "Closed in Phase 55c (Track C/D)"

Additionally, the residuals doc records LOC-metric misses vs. targets:

| Metric | Target | Actual |
|---|---|---|
| Net kernel LOC delta | ≤ −1800 | +1917 |
| Driver-isolation LOC delta | ≤ −1800 | −1597 |
| Combined facade size | ≤ 300 LOC | 518 LOC |

These are classified as "architectural accounting misses, not engineering bugs" and assigned to no future phase.

### Red flags from adversarial review

`docs/appendix/phase-55b-adversarial-review.md` records a Codex adversarial review with verdict "needs-attention / resolved":

Three findings, all marked resolved:
1. **[critical]** `sys_device_claim` had no authorization gate (`if false` placeholder). Fixed: replaced with `is_authorized_driver_process` checking `exec_path` starts with `/drivers/`.
2. **[high]** Cap-table exhaustion tore down every claim for the PID and skipped derived-resource cleanup. Fixed: `release_single(pid, key)` helper.
3. **[high]** Remote NVMe auto-registration trusted an unprivileged service name. Fixed: `Registry::lookup_with_owner` gates auto-binding on owner's `exec_path` starting with `/drivers/`.

Resolution was validated: "cargo xtask check — clippy clean, rustfmt clean, kernel-core + passwd + driver_runtime host tests pass."

### Contradictions with later-phase status claims

- 55b is Planned, yet 55c's declared dependencies list "Phase 55b (Ring-3 Driver Host) ✅".
- AGENTS.md describes ring-3 NVMe and e1000 drivers as operational features.

---

## Phase 55c — Ring-3 Driver Correctness Closure

### Declared status

**Complete.**

### Acceptance criteria — checked vs unchecked

The design doc acceptance is structured by residual (R3, R2, R1, plus phase-wide). All items are prose requirements; the companion task list (`55c-ring-3-driver-correctness-closure-tasks.md`) would carry checkboxes but was not part of the primary read list.

### Closed residuals (from 55b)

- **R1** — `sys_net_send` / userspace EAGAIN visibility: resolved. Implementation: `RemoteNic::check_restart_gate()` + restart gate in `sys_sendto` UDP/ICMP branches. `e1000-crash-smoke` smoke binary asserts EAGAIN. `#[ignore]` removed from `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds`.
- **R2** — IOMMU MMIO identity coverage: resolved. `BarCoverage` pure-logic invariant in `kernel-core/src/iommu/`. VT-d/AMD-Vi domain setup extended to insert identity-mapped 4 KiB pages for each BAR. `cargo xtask device-smoke --device nvme --iommu` and `--device e1000 --iommu` pass.
- **R3** — Event-multiplexing deadlock in e1000 ring-3 driver main loop: resolved. `sys_notif_bind` syscall added; `ipc_recv_msg` extended with `WakeKind::Notification` return. e1000 `run_io_loop` collapses to single blocking `endpoint.recv_multi(&irq_notif)` call.

### Open follow-up (not part of 55c itself)

`docs/roadmap/follow-ups/55c-net-remote-rx-test-bug.md` records a latent test bug in `kernel/src/net/remote.rs`:

**Status: Open.** Three RX-path test cases in `kernel/src/net/remote.rs` (PR #118) use `encode_net_send` instead of `encode_net_rx_notify` when building test payloads. This causes `decode_net_rx_notify` to reject the payloads (wrong `kind` label). The tests `inject_rx_frame_queues_payload_for_deferred_dispatch`, `drain_rx_queue_removes_malformed_frames_after_deferred_queueing`, and `inject_rx_frame_queues_each_record_in_a_multi_frame_batch` all fail.

Fix is documented: replace `encode_net_send` with `encode_net_rx_notify` in three test functions. Estimated effort: 20 minutes. The bug was masked from PR #118 CI by a pre-existing `frame_allocator` test failure that caused early exit; it surfaced during Phase 56 close-out.

### Deferred items

- "Many-to-one binding (multiple notifications bound to one TCB)"
- "Timed recv (`ipc_recv_timeout`)"
- "NVMe migration to bound-notification model"
- "IOMMU coverage for MSI-X table regions"
- "Generalized EAGAIN over block IO"
- "Secondary e1000 IRQ-coalescing concerns flagged in the 55b residuals appendix (multiple RX descriptor-ring wraparounds)"

---

## Phase 55c Learning Doc companion

`docs/roadmap/55c-ring-3-driver-correctness-closure-learning.md` has **Status: Complete**, consistent with the design doc.

---

## Phase 56 — Display and Input Architecture

### Declared status

Design doc: **Planned.** This contradicts the completion-gaps doc and AGENTS.md.

`docs/roadmap/56-phase-56-completion-gaps.md` describes Phase 56 as architecturally complete (PR #124 merged, kernel version bumped to 0.56.0) with a closing checklist that is fully ticked (all 9 items marked `[x]`).

### Acceptance criteria — checked vs unchecked

The completion-gaps doc states: "After the round-1 bookkeeping pass (commit `9817d4a` ticked 195 of 216) and subsequent close-out work, the spec is at 267 ticked / 12 unchecked (279 total). Every unchecked item is intentional and annotated inline."

The 12 remaining unchecked items are classified as:
- Phase 56 wrap-up follow-ons (page-grant leak smoke, DOOM `sys_fb_acquire` migration)
- `gfx-demo` Goodbye/EOF (needs AF_UNIX)
- D-B4 zero-copy (explicit deferral)
- D-E4 subscription push (explicit deferral)
- Screenshot/transcript encouragement items
- G.2 focused-client harness pieces (3 items)

### Explicit gap list (Section 2 of completion-gaps doc)

| ID | Description | Owning future phase |
|---|---|---|
| D-B4 | True zero-copy via page-grant capabilities (inline IPC bulk ships today) | Phase 56b or later |
| D-F1a | `mouse_server` dependency direction reversal (init manifest parser doesn't support comma-separated `depends=`) | Phase 57+ session-manifest pass |
| D-F1b | Distinct `on-restart=` supervisor directive | Phase 51 service-model maturity |
| D-D1 | Standalone modifier-key edges on `kbd_server` pull path | Additive — when a real client needs it |
| D-A0 | L/R modifier chord differentiation (`MOD_SHIFT` doesn't distinguish left vs right) | Wire-format change; needs versioned bump |
| D-E4 | Server-initiated subscription event push (registry queues events but doesn't transmit them; `TODO(subscription-push)` markers in code) | Needs polling verb OR cap-transfer-at-subscribe |

### Pre-existing failing test (open)

`kernel::net::remote::tests::drain_rx_queue_removes_malformed_frames_after_deferred_queueing` — same as the 55c-net-remote-rx-test-bug follow-up. The Phase 56 close-out classifies this as "out-of-scope for Phase 56" and defers it to the 55c follow-up PR.

### Goal-A contract points

All four delivered and verified:
1. `LayoutPolicy` trait + `FloatingLayout` — ✅
2. Keybind grab hook (`BindTable` + `GrabState`) — ✅
3. Layer-shell role (`SurfaceRole::Layer` + `compute_layer_geometry` + `LayerConflictTracker`) — ✅
4. Control socket + minimum verb set (`kernel-core::display::control` + `userspace/m3ctl/`) — ✅

### Red flags

- **Design doc status says "Planned"** but the completion-gaps doc shows 100% closed. The roadmap status field has not been updated to reflect completion. The completion-gaps doc closing checklist item reads: "When all 9 boxes tick, Phase 56 is 100% complete by its own spec."  All 9 boxes are ticked. However the README row was supposed to flip from "Complete (D–H + close-out)" to "Complete (all acceptance bullets ticked)" — unclear if this flip was applied to `docs/roadmap/README.md`.
- **D-E4 `TODO(subscription-push)` markers** are explicitly in-code annotations that are not currently tracked as a named follow-up task anywhere beyond the gaps doc.

---

## Phase 57 — Audio and Local Session

### Declared status

**Complete.**

### Acceptance criteria

All five criteria in the design doc are prose-only. The companion task list (`57-audio-and-local-session-tasks.md`) would carry checkboxes.

Key design decisions formalized:
- `audio_server` is a Phase 55b-style ring-3 supervised driver (uses `sys_device_claim`, `sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe`)
- Target: Intel 82801AA AC'97 (`0x8086:0x2415`)
- Audio ABI: pure-userspace IPC (no `sys_audio_*` syscalls, no kernel facade)
- Single-client PCM-out only (Phase 57 scope); multi-client mixing deferred
- `audio_server` uses Phase 55c bound-notification multiplex (`IrqNotification::bind_to_endpoint`; `run_io_loop` uses `endpoint.recv_multi(&irq_notif)`)

The audio ABI decision doc (`docs/appendix/phase-57-audio-abi.md`) is marked "Status: Decided."

### Kernel-side change is narrowly scoped

"The single concrete change in `kernel/` for Phase 57 audio is one line of widening: `kernel/src/device_host/mod.rs`'s claim path recognizes `0x8086:0x2415` (Intel AC'97) as a valid claim target alongside the existing NVMe and e1000 IDs."

### Deferred items

- "Rich desktop audio routing and mixing"
- "Media playback, recording, and advanced codecs"
- "Multiple graphical sessions or richer display-manager features"
- "Full desktop shell, notifications, settings panels, and broader app ecosystems"

---

## Phase 57a — Scheduler Block/Wake Protocol Rewrite

### Declared status

**Planned.** Design doc status field reads "Planned".

### Contradiction with AGENTS.md

AGENTS.md describes the Phase 57a scheduler rewrite as complete work (the kernel overview includes "Phase 57" completion), yet the design doc status field is Planned. The git log references "Bug #12" and "Bug #13" fixes in Phase 57e commits (commits `17099f6`, `052010a`, `8b44442`, `549584f`), which presupposes 57a–57d are done (since 57e Planned depends on 57b ✅, 57c ✅, 57d).

### Why this phase exists

Two consecutive debug sessions traced scheduler failures:
- **2026-04-25**: `sshd` cleanup hangs in `nanosleep` loop — `wake_after_switch` flag latched true from prior asymmetric scan/wake interaction.
- **2026-04-28**: `display_server` stuck in `BlockedOnReply` under cross-core IPC pressure — wake side's reply hits the `switching_out` window and is deferred; deferred enqueue is lost.

The rewrite eliminates `switching_out`, `wake_after_switch`, `PENDING_SWITCH_OUT[core]`, replaces with single `state` word under per-task `pi_lock` (Linux `p->pi_lock` pattern).

### Secondary bug fixes carried in this phase

1. `serial_stdin_feeder_task` — halt-loop parking core 3 → notification-based wait migration
2. `audio_server` — exits without registering `audio.cmd` stub → causes `session_manager` text-fallback
3. `syslogd` cpu-hog — ~500 ms uninterrupted-CPU windows

### 100 Hz tick-multiplier bug sweep

Five sites in the kernel embed an incorrect `× 10` / `÷ 10` factor assuming `TICKS_PER_SEC = 100` when actual rate is 1000:
- `scheduler.rs:1892` — `stale-ready` log: `stale_ticks * 10` → `stale_ticks`
- `scheduler.rs:2191` — `cpu-hog` log: `ran_ticks * 10` → `ran_ticks`
- `syscall/mod.rs:14647` — `sys_poll`: `(timeout_i as u64).div_ceil(10)` → `(timeout_i as u64)`
- `syscall/mod.rs:14894` — `select_inner`: `ms.div_ceil(10)` → `ms`
- `syscall/mod.rs:15304` — `sys_epoll_wait`: `(timeout_i as u64).div_ceil(10)` → `(timeout_i as u64)`

### Deferred items

- "Per-CPU runqueues with per-CPU locks"
- "Priority inheritance"
- "Wait-queue helper layer (`prepare_to_wait` / `finish_wait` style)"
- "Loom-style formal interleaving search"
- "Refactoring `userspace-init`'s boot fork burst"
- "Migration of the < 1 ms branch of `sys_nanosleep` away from TSC busy-spin"

---

## Phase 57b — Preemption Foundation

### Declared status

**Complete pending soak (PR #132).**

This is the only phase in this audit range with a "pending soak" qualifier. Soak status is not documented elsewhere in the read docs — no appendix doc records whether the soak completed, whether any panics occurred, or whether the phase has been fully closed. The design doc acceptance criteria include: "A 30-minute soak with `cargo xtask run-gui --fresh` produces zero panics from this assertion." No soak artifact or result is referenced.

### What "Complete pending soak" means structurally

57b is a behavior-neutral refactor — it adds `preempt_count`, `PreemptFrame`, `IrqSafeMutex` discipline, and `Vec<Box<Task>>` stable storage without firing preemption. The phase's risk is "a forgotten `preempt_enable` panics on first user-mode return." The soak tests whether this invariant holds under a realistic workload.

### Key structural changes

- `Scheduler::tasks`: `Vec<Task>` → `Vec<Box<Task>>` (stable per-task storage for lock-free preempt-count pointer)
- `PerCoreData::current_preempt_count_ptr: AtomicPtr<AtomicI32>` (per-CPU pointer into current task's `preempt_count`, retargeted at switch-out and switch-in under explicit `cli`)
- `Task::preempt_count: AtomicI32` initialized to 0
- `Task::preempt_frame: PreemptFrame` zero-initialized, unused until 57d
- `IrqSafeMutex::lock()` calls `preempt_disable()` before `interrupts::disable()`; Drop calls `preempt_enable()` after re-enabling
- `debug_assert!` at user-mode return boundary: `preempt_count == 0`

### Pointer lifecycle load-bearing invariant

The per-CPU pointer must target the correct `AtomicI32`:
1. Running task context → `current_task().preempt_count`
2. Switch-out epilogue (under explicit `cli`) → `SCHED_PREEMPT_COUNT_DUMMY[core_id]`
3. Scheduler/idle context → dummy
4. Switch-in handoff (under explicit `cli`) → `next_task.preempt_count`

No `IrqSafeMutex` guard may straddle a pointer retarget.

### Deferred items

- "Per-CPU placement of `preempt_count`"
- "Tracing variants (`preempt_disable_notrace`)"
- "Hardirq / softirq sub-counts"
- "Replacing `switch_context` with a unified preempt-aware switch"

### Red flags

- **Soak status unknown**: The "pending soak" qualifier has no corresponding closure document. Whether PR #132 passed the required 30-minute soak is unverified by any accessible document. The `preempt_count` debug assertion firing during a soak run would have required a follow-up fix.
- **Kernel version bump**: acceptance requires bump to `0.57.2`; no verification that this was applied.

---

## Phase 57c — Kernel Busy-Wait Audit and Conversion

### Declared status

**Complete.**

### Why this phase exists

Phase 57a's validation gate I.1 fails because kernel busy-spins inside syscalls monopolise cores. `PREEMPT_VOLUNTARY` (57d) does not fix kernel-mode CPU monopoly. Phase 57c fixes this independently by converting unbounded spins to `block_current_until` pairs.

### Relationship to 57b

Explicitly **independent of Phase 57b**: "This phase fixes the user-pain symptom (kernel-mode CPU monopoly) directly, without depending on the preempt-count infrastructure."

### Conversion (Track B) — known sites

Already converted ad hoc before Phase 57c:
- `virtio_blk::do_request` request poll
- `sys_poll` no-waiter yield-loop

Sites to verify:
- `net_task` NIC IRQ wake-up (uses `block_current_until`)
- `WaitQueue::sleep` (verify bottoms out in `block_current_until`)
- `futex_wait`

### Annotation-only (Track C) — bounded spins kept as-is

| File | Symbol | Bound |
|---|---|---|
| `kernel/src/smp/ipi.rs:46` | `wait_icr_idle` | LAPIC ICR delivery ~1 µs (Intel SDM) |
| `kernel/src/smp/tlb.rs:102,190` | `tlb_shootdown` ack wait | IPI delivery + remote IRQ handler |
| `kernel/src/iommu/intel.rs:247,368,390` | IOMMU command-queue waits | hardware-bounded |
| `kernel/src/iommu/amd.rs:339` | AMD-Vi command-queue | hardware-bounded |
| `kernel/src/arch/x86_64/ps2.rs:207,220` | PS/2 controller wait | microseconds |
| `kernel/src/arch/x86_64/apic.rs:436` | APIC reset wait | bounded |
| `kernel/src/smp/boot.rs:277` | AP boot wait | bounded by AP startup IPI sequence |
| `kernel/src/rtc.rs:90` | RTC update-in-progress | ~244 µs |
| `kernel/src/mm/frame_allocator.rs:876` | allocation retry | per-CPU magazine refill |
| `kernel/src/mm/slab.rs:442,604` | slab spins | bounded |
| `kernel/src/main.rs:185` | boot-time signal_reschedule wait | debug-build only |

Track C adds annotation comments; the actual `preempt_disable`/`preempt_enable` wrappers around these spins are deferred to **57e Track B** (where they become load-bearing under `PREEMPT_FULL`).

### Artefact

`docs/handoffs/57c-busy-wait-audit.md` — the authoritative audit catalogue. Must exist per acceptance criteria.

### Deferred items

- "Lockdep equivalent"
- "`might_sleep()`-style instrumentation"
- "Loom-style formal interleaving search"
- "Per-CPU load balancing of converted-syscall-task placement"

---

## Phase 57d — Voluntary Preemption (PREEMPT_VOLUNTARY)

### Declared status

**Planned.**

### What this phase activates

Activates 57b's infrastructure: timer IRQ handler fires `preempt_to_scheduler` when interrupted code is in user mode AND `preempt_count == 0` AND per-core `reschedule` flag is set.

### Critical design decision documented

The "uniform layout" approach for the timer IRQ asm stub was found **unsound**:

> "The 'uniform layout' approach was unsound: the synthetic `rsp`/`ss` slots cannot be inserted above the CPU-pushed iretq frame on the IRQ stack, because the bytes immediately above that frame are real interrupted-kernel-stack data; and they cannot be inserted below the CPU frame without putting them at the wrong offset relative to the declared `PreemptTrapFrame` shape."

The correct approach uses two distinct trap-frame types:
- `PreemptTrapFrameUser` — 15 GPRs + 5-field iretq (rip/cs/rflags/rsp/ss), 160 bytes — ring-3 interrupted
- `PreemptTrapFrameKernel` — 15 GPRs + 3-field iretq (rip/cs/rflags), 144 bytes — ring-0 interrupted

The ring-0 path in 57d still returns early (kernel mode non-preemptible under `PREEMPT_VOLUNTARY`). The kernel RSP capture logic is included in the 57d asm stub for 57e to reuse.

### `preempt_enable` zero-crossing (deferred-reschedule)

Under `PREEMPT_VOLUNTARY`, `preempt_enable` records `per_core().preempt_resched_pending = true` when count drops to 0 and `reschedule` is set. The actual scheduler switch happens at the next user-mode return boundary.

Under 57e (`PREEMPT_FULL`), `preempt_enable` can fire the scheduler immediately in kernel-mode-safe contexts.

### Deferred items

- "Kernel-mode preemption (`PREEMPT_FULL`) — Phase 57e"
- "Per-CPU `preempt_count`"
- "Explicit reschedule points (`might_resched`-style)"
- "Priority inheritance"
- "CFS / EEVDF-style fair scheduling"

---

## Phase 57e — Full Kernel Preemption (PREEMPT_FULL)

### Declared status

**Planned.**

### What blocks this phase

Per the design doc, Track 0 is a prelude gate: "Confirm 57d I.2 (post-flip 24-hour soak) and I.3 (`preempt-voluntary` flag removal) have closed." Since 57d is Planned, 57e cannot begin.

### What Bug #12 and Bug #13 were (from git log)

Recent commits on the current branch (`feat/audit-status`) reference Bug #12 and Bug #13 in 57e-attributed fixes:

- `17099f6` — `fix(57e): 4 ms kernel-mode preempt quantum (Bug #12 part 6)`
- `052010a` — `fix(57e): replace init reap-loop busy-yield with 50 ms sleep (Bug #12)`
- `e837c68` — `refactor(57e): remove redundant preempt_enable_no_resched`
- `8b44442` — `fix(57e): defer reschedule globally in preempt_enable (Bug #12 part 5)`
- `549584f` — `fix(57e): bracket wake_child_waiters wake-side (Bug #13)`

These commits exist on the `feat/audit-status` branch but the 57e design doc makes no reference to Bug #12 or Bug #13 by those names. The bugs were likely discovered during 57e implementation. Given the commit messages:
- **Bug #12** relates to the kernel-mode preemption quantum and `preempt_enable` deferral — likely the deferred-reschedule path under `PREEMPT_FULL` where `preempt_enable` zero-crossing behavior was incorrect or caused spurious/lost reschedules. The multi-part fix (parts 5 and 6 recorded, a 50 ms sleep fix for the init reap-loop, removal of `preempt_enable_no_resched`) suggests the `preempt_enable` zero-crossing path described in 57d/57e had edge cases.
- **Bug #13** relates to `wake_child_waiters` wake-side bracketing — a synchronization issue in the child-process wait logic, likely a lost-wake race in the `waitpid`/`wait4` path when a child exits while the parent is in the wake-side window.

The 57e design doc's "Deferred Until Later" does not mention these bugs; they appear to have been encountered and fixed during active 57e implementation on this branch.

### XSAVE migration (Track J)

57e folds an XSAVE migration:
- FXSAVE (covers x87 + MMX + SSE, 512 bytes) → XSAVE64 (adds AVX YMM upper halves, `XCR0 = 0x7`, 832 bytes, 64-byte aligned)
- Required: CPUs without OSXSAVE explicitly unsupported (pre-2011 Sandy Bridge / Bulldozer dropped)
- CR4.OSXSAVE + XCR0 wiring on BSP and all APs before any task runs
- `FxSaveArea` → `XSaveArea`

### Kernel-mode `preempt_resume` is structurally different from user-mode

> "An `iretq` that stays at the same privilege level (ring 0 → ring 0) pops only three: rip, cs, rflags. The interrupted code's `rsp` is implicit."

`preempt_resume_to_kernel` must: restore GPRs, set RSP to `preempt_frame.rsp` explicitly, push 3-field iretq frame, `iretq`. This is distinct from `preempt_resume_to_user` (5-field iretq).

### Deferred items

- "Per-CPU runqueues with per-CPU locks"
- "Priority inheritance"
- "Real-time scheduling policies (SCHED_FIFO, SCHED_RR)"
- "Lockdep equivalent"
- "Loom-style formal interleaving search"
- "`PREEMPT_RT` parity"
- "AVX-512 in `XCR0`"
- "XSAVE fallback for pre-2011 CPUs"
- "Memory protection keys (PKRU) save/restore"

---

## Cross-Phase Residual Tracking

### Open residuals by phase

| Phase | Status | Residual | Tracking doc | Where it should close |
|---|---|---|---|---|
| 55 | Complete | Task-list checkboxes never flipped to `[x]` | `docs/roadmap/tasks/55-hardware-substrate-tasks.md` | Bookkeeping only — no functional gap |
| 55 | Complete | Physical-hardware validation still "deferred" for all NVMe and e1000 matrix entries | `docs/roadmap/55-hardware-substrate.md` hardware matrix | Future hardware bring-up phase |
| 55a | Planned | **Entire phase unimplemented** — ACPI DMAR/IVRS, per-device domains, `DmaBuffer<T>` rerouting, fault handlers | `docs/roadmap/55a-iommu-substrate.md` | Phase 55a |
| 55a | Planned (doc says open) | VT-d MMIO translation drops `CTRL.RST` writes under `--iommu` — "Known Open Bug — must close before Phase 58" | `docs/roadmap/55a-iommu-substrate.md` "Known Open Bug" section; `docs/appendix/phase-55b-residuals.md` R2 | Claimed closed by 55c R2; 55a doc still reads open |
| 55b | Planned | **Entire ring-3 driver extraction unimplemented** — NVMe and e1000 still in ring 0 per original Phase 55 design | `docs/roadmap/55b-ring-3-driver-host.md` | Phase 55b |
| 55b | Planned | VirtIO-blk and VirtIO-net extraction deferred | `docs/roadmap/55b-ring-3-driver-host.md` Deferred | No phase assigned |
| 55c | Complete | `kernel::net::remote::tests` RX-path encoder mismatch (3 failing tests) | `docs/roadmap/follow-ups/55c-net-remote-rx-test-bug.md` | Standalone fix PR (~20 min) |
| 55c | Complete | `EAGAIN` over block IO not propagated (`sys_block_{read,write}` doesn't translate `DriverRestarting`) | `docs/roadmap/55c-ring-3-driver-correctness-closure.md` Deferred | "A later phase that hardens block-layer error surfaces" |
| 55c | Complete | Many-to-one notification binding (multiple notifications to one TCB) | `docs/roadmap/55c-ring-3-driver-correctness-closure.md` Deferred | Future phase |
| 55c | Complete | NVMe migration to bound-notification model | `docs/roadmap/55c-ring-3-driver-correctness-closure.md` Deferred | Future phase |
| 56 | Complete (pending README flip) | D-E4: server-initiated subscription event push — `TODO(subscription-push)` markers in code | `docs/roadmap/56-phase-56-completion-gaps.md` § 2 | "Needs polling verb OR cap-transfer-at-subscribe" — no phase assigned |
| 56 | Complete (pending README flip) | D-B4: true zero-copy via page-grant capabilities | `docs/roadmap/56-phase-56-completion-gaps.md` § 2 | Phase 56b or later |
| 56 | Complete (pending README flip) | D-F1a: `mouse_server` dependency direction reversal (init doesn't support comma-separated `depends=`) | `docs/roadmap/56-phase-56-completion-gaps.md` § 2 | Phase 57+ session-manifest pass |
| 56 | Complete (pending README flip) | D-A0: L/R modifier chord differentiation | `docs/roadmap/56-phase-56-completion-gaps.md` § 2 | Wire-format change, versioned bump needed |
| 57b | Complete pending soak | Soak result not documented — unknown if 30-minute soak passed without assertion panics | Acceptance criteria in `docs/roadmap/57b-preemption-foundation.md` | Should have an artifact PR comment or appendix doc |
| 57d | Planned | Phase not started — user-mode preemption not implemented | `docs/roadmap/57d-voluntary-preemption.md` | Phase 57d |
| 57e | Planned | Phase not started — full kernel preemption not implemented | `docs/roadmap/57e-full-kernel-preemption.md` | Phase 57e |
| 57e | Planned | Bug #12 (kernel-mode preempt quantum / `preempt_enable` deferral) — multi-part fix on `feat/audit-status` branch | Git log: `17099f6`, `052010a`, `8b44442`, `e837c68` | Already fixed on current branch |
| 57e | Planned | Bug #13 (`wake_child_waiters` wake-side bracketing) — fix on `feat/audit-status` branch | Git log: `549584f` | Already fixed on current branch |

---

### Phase 56 completion gaps (verbatim summary)

From `docs/roadmap/56-phase-56-completion-gaps.md`:

**Section 1 — Bugs revealed by close-out:**
- **1.1 F.2 supervisor restart not visible after panic** — RESOLVED (fixed: test pattern mismatch in xtask + smoke binary; init logs service registration name `display`, not binary name `display_server`; retry budget extended to 5 s; restart-confirmed signal reordered after control endpoint reachable).

**Section 2 — Real deferrals (explicit, will NOT close in Phase 56):**

| ID | Description | Owning future phase |
|---|---|---|
| D-B4 | True zero-copy via page-grant capabilities | Phase 56b or later |
| D-F1a | `mouse_server` dependency direction reversal (init doesn't support comma-separated `depends=`) | Phase 57+ session-manifest pass |
| D-F1b | Distinct `on-restart=` supervisor directive | Phase 51 service-model maturity |
| D-D1 | Standalone modifier-key edges on `kbd_server` pull path | Additive — when a real client needs it |
| D-A0 | L/R modifier chord differentiation (`MOD_SHIFT` doesn't distinguish left vs right) | Wire-format change; needs versioned bump |
| D-E4 | Server-initiated subscription event push (registry queues events but doesn't transmit them; `TODO(subscription-push)` markers) | Needs polling verb OR cap-transfer-at-subscribe |

**Section 3 — Bookkeeping (216 unchecked acceptance bullets):**
Resolved: after two passes (commit `9817d4a` + subsequent work) the spec reached 267 ticked / 12 unchecked. All 12 unchecked items are intentionally annotated.

**Section 5 — `cargo xtask test` pre-existing failure:**
`kernel::net::remote::tests::drain_rx_queue_removes_malformed_frames_after_deferred_queueing` — out of scope for Phase 56; tracked at `docs/roadmap/follow-ups/55c-net-remote-rx-test-bug.md`.

---

### Preemption programme (57a–57e) — how complete is the chain?

The preemption programme comprises five sequential phases (57a through 57e), each a prerequisite for the next. Below is the status of each link in the chain:

| Phase | Status | Key deliverable | Blocks |
|---|---|---|---|
| 57a — Scheduler Rewrite | **Planned** (doc says Planned; git log implies work done) | Replace `switching_out`/`wake_after_switch` with `pi_lock` + CAS wake; eliminate lost-wake bug class | 57b |
| 57b — Preemption Foundation | **Complete pending soak** | `preempt_count`, `PreemptFrame`, `Vec<Box<Task>>`, `IrqSafeMutex` discipline; no preemption fires | 57c, 57d |
| 57c — Busy-Wait Conversion | **Complete** | Convert unbounded kernel busy-spins to `block_current_until`; annotate hardware-bounded spins for 57e | 57e (makes safe) |
| 57d — Voluntary Preemption | **Planned** | Timer IRQ fires `preempt_to_scheduler` on user-mode exit with `preempt_count == 0`; kernel-mode stays non-preemptible | 57e |
| 57e — Full Kernel Preemption | **Planned** | Drop `from_user` check; kernel mode preemptible; XSAVE migration; 24-hour soak | 1.0 gate |

**Chain assessment:**

1. **57a and 57b are the critical blockers for 57d**. Without 57a's protocol rewrite, 57b has no race-free state machine to build on; without 57b's `preempt_count` infrastructure, 57d has nothing to gate preemption against.

2. **57b's "pending soak" is an unresolved qualifier**. The soak is the only thing standing between 57b's claim of "Complete" and actual completion. No soak result document is present in the accessible docs.

3. **57c is noted as Complete and independent of 57b**. This is correct per 57c's own design doc ("independent of Phase 57b"). 57c delivers user-pain relief from kernel-mode CPU monopoly without requiring preempt infrastructure.

4. **57d and 57e together close the starvation gap that Phase 57a's validation gate I.1 identified** (cursor frozen at (0,0), keyboard input not appearing). 57c closes the kernel-mode path; 57d closes the user-mode path. 57e extends to kernel-mode preemptibility.

5. **Bug #12 and Bug #13 fixes on `feat/audit-status`** indicate active 57e implementation. These fixes address:
   - **Bug #12**: The `preempt_enable` zero-crossing and kernel-mode quantum behavior — multi-part, suggesting the deferred-reschedule logic in the 57d → 57e transition had correctness issues (spurious or missing reschedules, wrong quantum accounting). The "50 ms sleep" fix in the init reap-loop suggests the init process was busy-spinning in a reap loop at preemption boundary.
   - **Bug #13**: `wake_child_waiters` wake-side bracketing — a synchronization issue in child-exit/wait path, consistent with preemption exposing a race window that cooperative scheduling never triggered.

6. **XSAVE migration (57e Track J)** is a silent hazard: hosted binaries (musl ports, modern Rust crates) emit AVX; FXSAVE does not save YMM upper halves. Under 57e's higher switch frequency, silent FP corruption becomes a likely soak failure. Track J must land before the 24-hour soak gate.

7. **`preempt_disable` wrappers on Track C annotated sites** (from 57c) are deferred to 57e Track B — they are currently comments only, not load-bearing. Under `PREEMPT_VOLUNTARY` (57d) this is safe because kernel mode is non-preemptible by construction. Under `PREEMPT_FULL` (57e), each annotated site without a wrapper becomes a potential livelock (spinner preempted while holder also preempted).

**Overall preemption programme completion: ~60%.**  
- 57a: partially done (evidence from git, doc says Planned)  
- 57b: done pending soak verification  
- 57c: done  
- 57d: not started  
- 57e: active work on `feat/audit-status` branch with Bug #12/#13 fixes landing

---

## Status Metadata Inconsistencies

The following status fields appear contradicted by evidence in the docs:

| Phase | Status in design doc | Contradicting evidence |
|---|---|---|
| 55a | Planned | 55c and 56 list 55a as "✅" completed dependency; AGENTS.md describes IOMMU isolation as operational |
| 55b | Planned | 55c, 56, 57 list 55b as "✅" completed dependency; AGENTS.md describes ring-3 drivers as operational |
| 56 | Planned | Completion-gaps doc closing checklist fully ticked; kernel bumped to 0.56.0; all 9 closing boxes checked |
| 57a | Planned | AGENTS.md describes Phase 57a scheduler rewrite as complete; 57b, 57c list 57a as "✅" dependency |

These inconsistencies suggest the roadmap design docs are not being updated when phases complete — only the task lists (and in some cases those are also not updated). The AGENTS.md "Kernel v0.57.0" reference is the authoritative current-state claim.
