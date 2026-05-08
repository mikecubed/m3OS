# Findings: Code-Side Stub and Shortcut Scan

> **Validation pass 2026-05-08.** Re-checked against the post-`ad7d9b2` (PR #136) tree. Changes since this finding's original snapshot (2026-05-07):
> - **`TODO(57a-C/D)` pi_lock markers** — still present, 4 sites, line numbers shifted: now `kernel/src/task/scheduler.rs:829, 3649, 3656, 3855` (were 829, 3782, 3789, 3988).
> - **Tick-multiplier bug callouts not in this finding** but flagged in `02-deferred-and-shortcuts.md` and `06-pre-1.0-blocker-list.md` C8 — closed by PR #136 Track G.3.
> - **`TODO(subscription-push)` in display server** — still present at lines 670, 690, 696, 703.
> - **`fat_server` ENOSYS stub** — still present (`userspace/fat_server/src/main.rs:67`).
> - **NVMe isolation `todo!()` scaffolds** — still present at lines 85, 112, 139, 171.
> - **AMD-Vi fault ISR TODO**, VT-d scalable mode false, queued invalidation deferred, multi-BDF AMD-Vi domains — all unchanged.


## Summary counts

- TODO markers: 22 (kernel: 8, userspace: 9, kernel-core: 5, xtask: 0)
  - Note: counts exclude false positives (`todo-rust` app, `SYS_DEBUG_PRINT`, `M3OS_DISPLAY_SERVER_DEBUG_CRASH` env markers)
- FIXME markers: 0
- HACK/XXX markers: 1 (XXX in coreutils-tests hexdump format string — benign naming)
- `unimplemented!()` macros: 0
- `todo!()` macros: 4 (all in `userspace/drivers/nvme/tests/isolation.rs`)
- `#[ignore]` test annotations: 47
- Workaround comments: 6 substantive (see below)
- Deprecated API wrappers: 3 (marked `#[deprecated]` in `syscall-lib`)
- Total `unsafe { }` blocks: 881 (kernel/src: 526, userspace: 328, kernel-core/src: 25, xtask: 2)

---

## High-severity findings (likely real gaps)

### Kernel ring-0 (`kernel/src/`)

- `kernel/src/task/scheduler.rs:829,3782,3789,3988` — `// TODO(57a-C/D): route through pi_lock + with_block_state`
  Four sites in scheduler that directly mutate `task.state` (to Dead/Ready/Running) bypassing the `pi_lock` + `with_block_state` abstraction that Phase 57a Tracks C/D were supposed to deliver. The pi_lock wiring is missing; these are bare `task.state = ...` stores without the atomic block-state protocol.

- `kernel/src/task/scheduler.rs:28-30` (module doc) — `// True per-core scheduling (where the dispatch hot path never acquires a global lock) is deferred to a future phase.`
  SMP scheduler still acquires `SCHEDULER` global lock on every dispatch. Per-core lock-free dispatch is explicitly deferred with no assigned phase.

- `kernel/src/mm/user_space.rs:135,143` — `// W^X enforcement is deferred to Phase 6+`
  Code pages are mapped WRITABLE | USER_ACCESSIBLE with no NO_EXECUTE enforcement. All userspace code pages are currently writable, eliminating W^X protection for the entire ring-3 address space.

- `kernel/src/fs/ext2.rs:355` — `// Triple-indirect — deferred; files this large shouldn't exist on our 64MB filesystem.`
  Triple-indirect block reads return `Err(Ext2Error::CorruptedEntry)`. Files requiring triple-indirect blocks (>~8 MB on a typical ext2) silently fail with a corrupt-entry error rather than an explicit "not supported" path.

- `kernel/src/iommu/amd.rs:938` — `// AMD-Vi fault-dispatch ISR path is currently a Track E TODO; no ISR handler is installed today`
  AMD IOMMU has no interrupt handler for fault records. Hardware IOMMU faults on AMD platforms are silently dropped.

- `kernel/src/iommu/intel.rs:722` — `// Queued-invalidation is deferred — register-based path is sufficient for Phase 55a.`
  Intel VT-d uses only the legacy register-based invalidation path. Queued invalidation (required for performance and required by later VT-d revisions) is unimplemented.

- `kernel/src/iommu/intel.rs:178` — `scalable_mode: false, // Phase 55a — deferred.`
  VT-d scalable mode (required for SR-IOV and multi-level page tables) is hardcoded disabled.

- `kernel/src/iommu/amd.rs:143` — `// per claimed BDF; multi-BDF domains are deferred.`
  AMD-Vi domain management is one-domain-per-BDF only. Multi-BDF (device group) domains are unimplemented.

- `kernel/src/pci/bar.rs:428` — `// Without PAT slots (deferred) this is the best approximation`
  BAR MMIO mappings use `NO_CACHE | WRITE_THROUGH` as a blanket fallback because PAT (Page Attribute Table) slot management is unimplemented. Correct UC- / WC / WB mappings per BAR type cannot be expressed.

- `kernel/src/arch/x86_64/syscall/mod.rs:1422-1427` — `// Temporary compatibility: direct register-return termios field reads. Introduced as a copy_to_user reliability workaround (Phase 52).`
  Three termios register-return syscalls (`GET_TERMIOS_LFLAG`, `GET_TERMIOS_IFLAG`, `GET_TERMIOS_OFLAG`) remain live in the kernel's syscall dispatch table as a workaround introduced in Phase 52, even though no in-tree binary calls them after Phase 52d. Retained dead kernel ABI surface.

- `kernel/src/ipc/mod.rs:34-35` — `// Deferred to Phase 7+: capability grants via IPC, page-capability bulk transfers, IPC timeouts.`
  Core IPC features (capability grants via IPC messages, bulk page-capability transfer, IPC timeouts) are explicitly deferred to "Phase 7+". These are fundamental to a capability-based microkernel and are currently absent.

### Kernel core (`kernel-core/src/`)

No `TODO`/`FIXME`/`unimplemented!` hits in `kernel-core/src/`. The sched_loom test has a pending pi_lock wiring note (see Medium/Low).

### Userspace (`userspace/`)

- `userspace/fat_server/src/main.rs:67` — Entire `fat_server` service replies ENOSYS to every request.
  The Phase 54 userspace FAT storage server is a supervised empty socket: it registers as the "fat" IPC service, then replies `-ENOSYS` to all incoming messages in a loop. No FAT32 file operations are implemented. VFS callers hitting the fat service get a clean errno but no data.

- `userspace/display_server/src/control.rs:670,690,696,703` — `// TODO(subscription-push): server-initiated push of subscribed events`
  Four `publish_*` functions in the control socket protocol (for `SurfaceCreated`, `SurfaceDestroyed`, `FocusChanged`, `BindTriggered`) queue events into the subscription registry but never push them to subscribers. The module doc confirms: "only the wire transmission remains." `m3ctl`-style consumers subscribed to these events will never receive them.

- `userspace/drivers/nvme/tests/isolation.rs:85,112,139,171` — Four `todo!()` macros in isolation test bodies.
  All four end-to-end IOMMU isolation tests (`cross_device_mmio_denied_end_to_end`, `cross_device_dma_denied_end_to_end`, `capability_forge_denied_end_to_end`, `post_crash_handles_invalid_end_to_end`) have `todo!()` bodies. They are `#[ignore]`d (so they don't fail CI), but the tests exist only as scaffolding; no code validates the privilege-isolation paths they are named for.

- `userspace/display_server/src/compose.rs:173-175` — `// damage tracking of regions deferred to a Phase 56 follow-up; today every mouse move triggers a full repaint of every mapped surface.`
  Compositor has no damage-region tracking. Every cursor motion or surface update forces a full composite of all surfaces. This is a correctness shortcut (no differential repaint) documented as a Phase 56 follow-up.

- `userspace/term/src/syscall_pty.rs:127-129` — `// HOME is hard-coded to /root because Phase 57 term inherits init's uid (root) — the graphical-login story is a future-phase concern.`
  The terminal emulator hardcodes `HOME=/root`. Multi-user support in the graphical session is explicitly deferred.

- `userspace/syscall-lib/src/lib.rs:957-991` — Three `#[deprecated]` functions retained after Phase 52d.
  `get_termios_lflag()`, `get_termios_iflag()`, `get_termios_oflag()` are marked deprecated with note "temporary copy_to_user workaround; use tcgetattr or push_raw_input." The matching kernel syscalls are also still live. Dead API surface on both sides of the ABI boundary.

---

## Medium/low-severity findings

- **Debug logging left in production paths**: Several Phase 57/57a follow-up `// Phase 57 DEBUG:` and `// Phase 57a follow-up DEBUG:` comments in `kernel/src/arch/x86_64/syscall/mod.rs` (lines 198, 1572, 1603, 2025) and `kernel/src/smp/mod.rs:180` indicate per-pid syscall tracing and reschedule-IPI countdown logging that may still be in the binary.

- **`sys_clone` unsupported flags return ENOSYS**: `kernel/src/arch/x86_64/syscall/mod.rs:12854` — clone with unsupported flags logs a warning and returns ENOSYS, limiting POSIX threading compatibility.

- **`sys_prlimit64` stub**: `kernel/src/arch/x86_64/syscall/mod.rs:1877` — `PRLIMIT64 => NEG_ENOSYS` with no implementation.

- **tmpfs timestamps unimplemented**: `kernel/src/arch/x86_64/syscall/mod.rs:11686-11687` — `sys_utimensat` returns ENOSYS for tmpfs entries; timestamps are unsupported.

- **`diff` coreutils is non-functional**: `userspace/coreutils-rs/src/diff.rs:3-5` — the `diff` binary produces only "all removed, all added" output; no LCS algorithm is implemented.

- **`sched_loom` test does not test pi_lock**: `kernel-core/tests/sched_loom.rs:166` — the concurrent Wake race model uses `AtomicU8` as a stand-in because the real pi_lock CAS primitive does not exist yet. The loom model tests a simplified proxy.

- **`logger.rs` priority simplified**: `userspace/coreutils-rs/src/logger.rs:23` — syslog priority field hardcoded to 14 (`user.info`) instead of being computed from facility+severity.

---

## Phase-tagged deferrals in code

Organized by target phase:

### Phase 55a (IOMMU substrate — claimed Complete)
- `kernel/src/iommu/intel.rs:178` — scalable_mode hardcoded false
- `kernel/src/iommu/intel.rs:722` — queued-invalidation deferred, register-based only
- `kernel/src/iommu/amd.rs:938` — AMD-Vi fault ISR Track E TODO, no handler installed
- `kernel/src/iommu/amd.rs:143` — multi-BDF domains deferred
- `kernel-core/tests/iommu_parity.rs:262` — ISR bring-up deferred in Phase 55a (parity test only checks struct shape)

### Phase 55c (ring-3 driver correctness closure — claimed Complete)
- `userspace/drivers/nvme/tests/isolation.rs:47,73,100,126,155` — 4 end-to-end negative-path isolation tests deferred to phase-55c; all bodies are `todo!()` and `#[ignore]`d

### Phase 56 (ring-3 display server — claimed Complete)
- `userspace/display_server/src/control.rs:670,690,696,703` — subscription-push wire transmission not implemented (4 publish_ functions)
- `userspace/display_server/src/compose.rs:173` — damage tracking deferred to Phase 56 follow-up
- `kernel-core/tests/phase56_g1_multi_client_coexistence.rs:99` — G.1 multi-client coexistence test deferred to QEMU integration
- `kernel-core/tests/phase56_g2_keybind_grab_hook.rs:146` — G.2 synthetic-key-injection regression deferred to QEMU smoke
- `kernel-core/tests/phase56_g4_control_socket_roundtrip.rs:165` — G.4 live control-socket round-trip deferred to QEMU smoke

### Phase 57a (scheduler correctness — claimed Complete)
- `kernel/src/task/scheduler.rs:829,3782,3789,3988` — 4 sites with `TODO(57a-C/D): route through pi_lock + with_block_state`; pi_lock wiring never landed

### Phase 57 / 57e (audio + local session — current)
- `kernel/src/smp/mod.rs:180` — Phase 57 DEBUG countdown for reschedule IPI
- `kernel/src/arch/x86_64/syscall/mod.rs:198` — Phase 57 DEBUG execve surfacing

### Future phases (Phase 6+, Phase 7+)
- `kernel/src/mm/user_space.rs:135,143` — W^X enforcement deferred to Phase 6+
- `kernel/src/ipc/mod.rs:34-35` — IPC capability grants, bulk transfers, timeouts deferred to Phase 7+
- `kernel/src/task/scheduler.rs:28-30` — per-core lock-free dispatch deferred to an unspecified future phase
- `kernel/src/pci/bar.rs:664,796` — PTE cleanup and PAT slot management deferred to a later phase
- `kernel/src/fs/ext2.rs:355` — triple-indirect ext2 block reads deferred (no assigned phase)

---

## Disabled or ignored tests

| File | Test name | Reason |
|---|---|---|
| `kernel/tests/preempt_latency.rs:153` | `bench_cross_core_ipi_wakeup` | Track G activation pending — needs smp::boot::boot_aps + futex syscalls |
| `kernel/tests/preempt_latency.rs:169` | `bench_same_core_wakeup` | Track G activation pending — needs futex syscalls + scheduler dispatch |
| `kernel/tests/preempt_latency.rs:184` | `bench_kernel_timer_preempt` | Track G activation pending — needs kernel task spawn + scheduler dispatch |
| `kernel/tests/preempt_latency.rs:201` | `bench_preempt_enable_zero_crossing` | Track G activation pending — needs scheduler + lock-release instrumentation |
| `kernel/tests/preempt_user_stress.rs:125` | `multicore_preempt_stress_5min` | requires 4-core QEMU + preempt-voluntary feature + full userspace |
| `kernel/tests/preempt_user_stress.rs:139` | `real_hardware_acceptance_gate` | procedural — requires real hardware, not a QEMU harness test |
| `kernel/tests/preempt_user_stress.rs:150` | `soak_30_plus_30_min` | procedural — requires 60-min QEMU soak, not a harness test |
| `kernel/tests/preempt_voluntary.rs:307` | `peek_preempt_count_matches_task_count` | requires QEMU + full scheduler init |
| `kernel/tests/preempt_voluntary.rs:324` | `preempt_to_scheduler_saves_frame_correctly` | requires QEMU + full scheduler init |
| `kernel/tests/preempt_voluntary.rs:335` | `preempt_resume_restores_rip_and_registers` | requires QEMU + full scheduler init |
| `kernel/tests/preempt_voluntary.rs:345` | `cooperative_yield_still_uses_switch_context` | requires QEMU + full scheduler init |
| `kernel/tests/preempt_voluntary.rs:365` | `timer_handler_preempts_user_within_1ms` | requires QEMU + preempt-voluntary feature + userspace tight loop |
| `kernel/tests/preempt_voluntary.rs:377` | `reschedule_ipi_preempts_user_within_1ms` | requires QEMU + SMP + preempt-voluntary feature |
| `kernel/tests/preempt_voluntary.rs:389` | `preempt_count_nonzero_suppresses_preemption` | requires QEMU + preempt-voluntary feature |
| `kernel/tests/xsave_avx.rs:137` | `ymm_upper_halves_survive_context_switch` | Track G activation pending — needs smp::init_bsp_per_core + scheduler dispatch |
| `kernel/tests/xsave_avx.rs:144` | `ymm_upper_halves_survive_1000_iterations` | Track G activation pending — needs scheduler + iterated yield harness |
| `kernel-core/tests/driver_restart.rs:790` | `qemu_nvme_kill_mid_write_returns_driver_restarting` | QEMU-only: pure-logic coverage is in other tests; QEMU infrastructure not available in kernel-core |
| `kernel-core/tests/driver_restart.rs:828` | `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds` | QEMU-only: authoritative check is the e1000-restart-crash xtask smoke |
| `kernel-core/tests/driver_restart.rs:870` | `qemu_max_restart_exceeded_service_status_returns_failed` | QEMU-only: authoritative check is the max-restart-exceeded xtask smoke |
| `kernel-core/tests/phase56_g1_multi_client_coexistence.rs:99` | `multi_client_coexistence_deferred` | Phase 56 G.1 deferred to QEMU integration; bulk-drain gap closed but cross-process out of scope |
| `kernel-core/tests/phase56_g2_keybind_grab_hook.rs:146` | `runtime_grab_hook_synthetic_injection_belongs_in_qemu_smoke` | Phase 56 G.2 runtime synthetic-key-injection regression belongs in QEMU smoke |
| `kernel-core/tests/phase56_g4_control_socket_roundtrip.rs:165` | `runtime_list_surfaces_subscribe_and_frame_stats_belongs_in_qemu_smoke` | Phase 56 G.4 live control-socket round-trip belongs in QEMU smoke |
| `userspace/drivers/nvme/tests/isolation.rs:75` | `cross_device_mmio_denied_end_to_end` | phase-55c deferred: blocked on supervised spawn + CapHandle injection harness |
| `userspace/drivers/nvme/tests/isolation.rs:102` | `cross_device_dma_denied_end_to_end` | phase-55c deferred: blocked on supervised spawn + CapHandle injection harness |
| `userspace/drivers/nvme/tests/isolation.rs:128` | `capability_forge_denied_end_to_end` | phase-55c deferred: requires negative-path driver binary |
| `userspace/drivers/nvme/tests/isolation.rs:157` | `post_crash_handles_invalid_end_to_end` | phase-55c deferred: requires cap-handle injection harness |

Total: 26 named `#[ignore]` test functions. The remaining ~21 `#[ignore]` count entries are annotation occurrences in comments/docs rather than distinct test bodies.

---

## `unsafe` block density

| Crate | Files with unsafe | Total `unsafe { }` blocks |
|---|---|---|
| `kernel/src/` | 44 | 526 |
| `userspace/` (all crates) | 34 | 328 |
| `kernel-core/src/` | 2 | 25 |
| `xtask/src/` | 1 | 2 |
| **Total** | **81** | **881** |

Safety comment coverage: `kernel/src/` has ~309 `// SAFETY:` comments against 526 `unsafe {}` blocks — roughly 59% coverage. The gap (~217 unsafe blocks with no adjacent SAFETY rationale) is concentrated in `syscall/mod.rs` (57 blocks), `interrupts.rs` (45 blocks), `scheduler.rs` (58 blocks), and `syscall-lib/src/lib.rs` (137 blocks in userspace).

---

## Concerns surfaced from the code that the roadmap docs may not capture

- **Phase 55c isolation tests are entirely scaffolding.** All four end-to-end IOMMU isolation tests have `todo!()` bodies and `#[ignore]` annotations. Phase 55c is claimed complete, but the negative-path privilege-isolation tests (cross-device MMIO denial, DMA denial, forged capability denial, post-crash handle invalidation) have zero executable coverage.

- **`fat_server` is a permanent ENOSYS stub.** It is supervised, deployed, and registered as the "fat" IPC service, but replies `-ENOSYS` to all requests. If any VFS path calls the fat service, callers get a live errno but no data. Phase 54 is claimed complete, but FAT32 I/O was never migrated to ring-3.

- **Display server subscription-push is structurally incomplete.** The control socket's four `publish_*` functions queue events but never transmit them. Clients that subscribe to surface lifecycle or focus-change events receive nothing. The module doc acknowledges "only the wire transmission remains" — this is a Phase 56 close-out gap.

- **Scheduler pi_lock wiring is missing.** Phase 57a Tracks C/D were supposed to route task state mutations through `pi_lock + with_block_state`. Four scheduler sites still do bare `task.state = ...` stores. The pi_lock abstraction does not exist at these call sites.

- **AMD IOMMU has no fault ISR.** Phase 55a is claimed complete, but the AMD-Vi fault dispatch ISR is a Track E TODO with no handler installed. Hardware IOMMU faults on AMD platforms produce no kernel response.

- **W^X is absent.** Code pages are mapped writable with no execute-disable separation. This is documented as a "Phase 6+" deferral but represents a fundamental security property gap that the roadmap summary does not call out.

- **IPC capability grants, bulk transfers, and timeouts are entirely absent** (Phase 7+). The IPC engine lacks the features that would make it a true seL4-style capability IPC: you cannot pass capabilities in messages, cannot do zero-copy bulk page transfers, and there are no timeouts on synchronous calls.

- **14 preemption and XSAVE tests are all `#[ignore]`d** waiting for "Track G activation." Track G is not assigned to a completed or planned phase in the evidence gathered here. These include latency benchmarks, stress tests, and context-switch correctness tests for the voluntary-preemption subsystem.

- **VT-d scalable mode and queued invalidation are disabled.** The IOMMU substrate's Intel implementation hard-codes `scalable_mode: false` and uses only the legacy register-based invalidation path. These are permanent limitations of Phase 55a that affect correctness on modern VT-d hardware.
