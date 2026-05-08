# 04 — Code-Side Evidence

This document distils what the code says — TODO/FIXME markers, stub implementations, ignored tests, phase-tagged deferrals — into a single reference. It complements the doc-side audit. Numbers and citations come from `findings/06-code-side-scan.md`; per-file breakdowns live there.

This is a catalogue. Not every entry is a problem; some are honestly-deferred work tagged for future phases. The point is to make the in-code deferrals visible to anyone reading just the roadmap.

---

## Summary counts

| Category | Total | Where |
|---|---|---|
| Substantive `TODO` markers | 22 | kernel: 8, userspace: 9, kernel-core: 5 |
| `FIXME` markers | 0 | — |
| `HACK`/`XXX` markers | 1 | benign (coreutils-tests format string) |
| `unimplemented!()` macros | 0 | — |
| `todo!()` macros in production paths | 4 | all in `userspace/drivers/nvme/tests/isolation.rs`, all `#[ignore]`d |
| `#[ignore]`d test bodies | 26 named | preemption/XSAVE: 14; Phase 55c isolation: 4; Phase 56 G-track: 3; QEMU-only driver-restart: 3; xsave context-switch: 2 |
| Workaround comments (substantive) | 6 | mixed |
| `#[deprecated]` API wrappers | 3 | `syscall-lib` termios register-return helpers |
| Total `unsafe { }` blocks | 881 | kernel: 526, userspace: 328, kernel-core: 25, xtask: 2 |
| Kernel `// SAFETY:` comment coverage | ~59% | ~309 SAFETY comments against 526 unsafe blocks; gap concentrated in `syscall/mod.rs`, `interrupts.rs`, `scheduler.rs` |

---

## High-severity findings (likely real gaps)

### Kernel ring-0 (`kernel/src/`)

1. **Scheduler — pi_lock wiring missing.** `kernel/src/task/scheduler.rs:829, 3649, 3656, 3855` (post-rebase line numbers; pre-merge they were 829, 3782, 3789, 3988) carry `// TODO(57a-C/D): route through pi_lock + with_block_state` markers. Four sites mutate `task.state` (Dead/Ready/Running) with bare stores, bypassing the protocol Phase 57a Tracks C/D were supposed to land. The pi_lock primitive does not exist at these callsites. (Validation pass 2026-05-08: still present after Phase 57e deferred.)

2. **Scheduler — global lock on dispatch hot path.** `kernel/src/task/scheduler.rs:28-30` module doc: *"True per-core scheduling (where the dispatch hot path never acquires a global lock) is deferred to a future phase."* No phase number assigned.

3. **W^X enforcement absent.** `kernel/src/mm/user_space.rs:135, 143`: *"W^X enforcement is deferred to Phase 6+"*. Code pages are mapped `WRITABLE | USER_ACCESSIBLE` with no `NO_EXECUTE` separation. Every userspace code page is currently writable.

4. **Triple-indirect ext2 reads broken.** `kernel/src/fs/ext2.rs:355`: *"Triple-indirect — deferred; files this large shouldn't exist on our 64MB filesystem."* Returns `Err(Ext2Error::CorruptedEntry)` rather than `ENOSYS` or `EFBIG`. Files >~8 MB silently appear corrupt rather than unsupported.

5. **AMD-Vi has no fault ISR.** `kernel/src/iommu/amd.rs:938`: *"AMD-Vi fault-dispatch ISR path is currently a Track E TODO; no ISR handler is installed today"*. Hardware IOMMU faults on AMD platforms are silently dropped — no kernel response.

6. **VT-d queued invalidation deferred.** `kernel/src/iommu/intel.rs:722`: *"Queued-invalidation is deferred — register-based path is sufficient for Phase 55a."* Required for performance and required by later VT-d revisions.

7. **VT-d scalable mode hardcoded false.** `kernel/src/iommu/intel.rs:178`: `scalable_mode: false, // Phase 55a — deferred.` Required for SR-IOV and multi-level page tables.

8. **AMD-Vi multi-BDF domains unimplemented.** `kernel/src/iommu/amd.rs:143`: *"per claimed BDF; multi-BDF domains are deferred."*

9. **PCI BAR PAT slot management absent.** `kernel/src/pci/bar.rs:428`: *"Without PAT slots (deferred) this is the best approximation"*. BAR MMIO mappings use `NO_CACHE | WRITE_THROUGH` blanket fallback; correct UC-/WC/WB per BAR type cannot be expressed.

10. **Termios register-return syscalls retained as workaround.** `kernel/src/arch/x86_64/syscall/mod.rs:1422-1427`: *"Temporary compatibility: direct register-return termios field reads. Introduced as a copy_to_user reliability workaround (Phase 52)."* Three syscalls (`GET_TERMIOS_LFLAG`, `GET_TERMIOS_IFLAG`, `GET_TERMIOS_OFLAG`) live in dispatch despite no in-tree caller after Phase 52d.

11. **IPC features deferred to "Phase 7+".** `kernel/src/ipc/mod.rs:34-35`: *"Deferred to Phase 7+: capability grants via IPC, page-capability bulk transfers, IPC timeouts."* These are foundational to a capability microkernel and are absent.

### Userspace (`userspace/`)

1. **`fat_server` is a permanent ENOSYS stub.** `userspace/fat_server/src/main.rs:67`: registers as the "fat" IPC service, replies `-ENOSYS` to every request in a loop. No FAT32 file operations implemented.

2. **Display server subscription push missing.** `userspace/display_server/src/control.rs:670, 690, 696, 703`: four `publish_*` functions (`SurfaceCreated`, `SurfaceDestroyed`, `FocusChanged`, `BindTriggered`) queue events but never push them to subscribers. Module doc: *"only the wire transmission remains."*

3. **Phase 55c isolation tests are pure scaffolding.** `userspace/drivers/nvme/tests/isolation.rs:85, 112, 139, 171`: four `todo!()` macros. Tests `cross_device_mmio_denied_end_to_end`, `cross_device_dma_denied_end_to_end`, `capability_forge_denied_end_to_end`, `post_crash_handles_invalid_end_to_end` exist as named scaffolding only. All `#[ignore]`d so CI doesn't fail.

4. **Compositor has no damage tracking.** `userspace/display_server/src/compose.rs:173-175`: *"damage tracking of regions deferred to a Phase 56 follow-up; today every mouse move triggers a full repaint of every mapped surface."*

5. **Terminal hardcodes `HOME=/root`.** `userspace/term/src/syscall_pty.rs:127-129`: *"HOME is hard-coded to /root because Phase 57 term inherits init's uid (root) — the graphical-login story is a future-phase concern."*

6. **`#[deprecated]` ABI surface retained.** `userspace/syscall-lib/src/lib.rs:957-991`: three deprecated termios register-return helpers paired with the kernel-side syscalls above.

### Kernel-core (`kernel-core/src/`)

No `TODO`/`FIXME`/`unimplemented!()` hits. The `sched_loom` test (`kernel-core/tests/sched_loom.rs:166`) uses `AtomicU8` as a stand-in because the real pi_lock CAS primitive does not exist yet — the loom model tests a simplified proxy.

---

## Medium / low-severity findings

- **Debug logging left in production paths.** Phase 57/57a follow-up `// Phase 57 DEBUG:` and `// Phase 57a follow-up DEBUG:` comments at `kernel/src/arch/x86_64/syscall/mod.rs:198, 1572, 1603, 2025` and `kernel/src/smp/mod.rs:180` indicate per-pid syscall tracing and reschedule-IPI countdown logging that may still be in the binary.
- **`sys_clone` unsupported flags return ENOSYS.** `kernel/src/arch/x86_64/syscall/mod.rs:12854` — clone with unsupported flags logs a warning and returns ENOSYS, limiting POSIX threading compatibility.
- **`sys_prlimit64` is a stub.** `kernel/src/arch/x86_64/syscall/mod.rs:1877`: `PRLIMIT64 => NEG_ENOSYS`.
- **tmpfs timestamps unimplemented.** `kernel/src/arch/x86_64/syscall/mod.rs:11686-11687` — `sys_utimensat` returns ENOSYS for tmpfs.
- **`diff` coreutil is non-functional.** `userspace/coreutils-rs/src/diff.rs:3-5` — produces only "all removed, all added" output; no LCS algorithm.
- **`logger.rs` syslog priority simplified.** `userspace/coreutils-rs/src/logger.rs:23` — hardcoded to 14 (`user.info`) instead of being computed from facility+severity.

---

## Phase-tagged deferrals organised by target phase

### Phase 55a (claimed Complete; design doc says Planned)
- `kernel/src/iommu/intel.rs:178` — scalable_mode hardcoded false
- `kernel/src/iommu/intel.rs:722` — queued-invalidation deferred
- `kernel/src/iommu/amd.rs:938` — AMD-Vi fault ISR Track E TODO, no handler installed
- `kernel/src/iommu/amd.rs:143` — multi-BDF domains deferred
- `kernel-core/tests/iommu_parity.rs:262` — ISR bring-up deferred (parity test only checks struct shape)

### Phase 55c (claimed Complete)
- `userspace/drivers/nvme/tests/isolation.rs:47, 73, 100, 126, 155` — 4 end-to-end negative-path isolation tests deferred; bodies are `todo!()` and `#[ignore]`d

### Phase 56 (claimed Complete)
- `userspace/display_server/src/control.rs:670, 690, 696, 703` — subscription-push wire transmission not implemented (4 publish_ functions)
- `userspace/display_server/src/compose.rs:173` — damage tracking deferred to Phase 56 follow-up
- `kernel-core/tests/phase56_g1_multi_client_coexistence.rs:99` — G.1 multi-client coexistence deferred to QEMU integration
- `kernel-core/tests/phase56_g2_keybind_grab_hook.rs:146` — G.2 synthetic-key-injection regression deferred to QEMU smoke
- `kernel-core/tests/phase56_g4_control_socket_roundtrip.rs:165` — G.4 live control-socket round-trip deferred to QEMU smoke

### Phase 57a (claimed Complete)
- `kernel/src/task/scheduler.rs:829, 3782, 3789, 3988` — 4 sites with `TODO(57a-C/D): route through pi_lock + with_block_state`; pi_lock wiring never landed

### Phase 57 / 57e (current work)
- `kernel/src/smp/mod.rs:180` — Phase 57 DEBUG countdown for reschedule IPI
- `kernel/src/arch/x86_64/syscall/mod.rs:198` — Phase 57 DEBUG execve surfacing

### Future / unassigned
- `kernel/src/mm/user_space.rs:135, 143` — W^X deferred to "Phase 6+"
- `kernel/src/ipc/mod.rs:34-35` — IPC capability grants, bulk transfers, timeouts deferred to "Phase 7+"
- `kernel/src/task/scheduler.rs:28-30` — per-core lock-free dispatch deferred to unspecified future phase
- `kernel/src/pci/bar.rs:664, 796` — PTE cleanup and PAT slot management deferred to a later phase
- `kernel/src/fs/ext2.rs:355` — triple-indirect ext2 block reads deferred (no assigned phase)

---

## `#[ignore]`d test bodies (26 named)

| File | Test | Reason |
|---|---|---|
| `kernel/tests/preempt_latency.rs:153` | `bench_cross_core_ipi_wakeup` | Track G activation pending — needs `smp::boot::boot_aps` + futex |
| `kernel/tests/preempt_latency.rs:169` | `bench_same_core_wakeup` | Track G activation pending — needs futex + scheduler dispatch |
| `kernel/tests/preempt_latency.rs:184` | `bench_kernel_timer_preempt` | Track G activation pending — needs kernel task spawn + scheduler |
| `kernel/tests/preempt_latency.rs:201` | `bench_preempt_enable_zero_crossing` | Track G activation pending |
| `kernel/tests/preempt_user_stress.rs:125` | `multicore_preempt_stress_5min` | requires 4-core QEMU + preempt-voluntary feature |
| `kernel/tests/preempt_user_stress.rs:139` | `real_hardware_acceptance_gate` | procedural — requires real hardware |
| `kernel/tests/preempt_user_stress.rs:150` | `soak_30_plus_30_min` | procedural — requires 60-min QEMU soak |
| `kernel/tests/preempt_voluntary.rs:307` | `peek_preempt_count_matches_task_count` | requires QEMU + full scheduler init |
| `kernel/tests/preempt_voluntary.rs:324` | `preempt_to_scheduler_saves_frame_correctly` | requires QEMU + full scheduler init |
| `kernel/tests/preempt_voluntary.rs:335` | `preempt_resume_restores_rip_and_registers` | requires QEMU + full scheduler init |
| `kernel/tests/preempt_voluntary.rs:345` | `cooperative_yield_still_uses_switch_context` | requires QEMU + full scheduler init |
| `kernel/tests/preempt_voluntary.rs:365` | `timer_handler_preempts_user_within_1ms` | requires QEMU + preempt-voluntary feature |
| `kernel/tests/preempt_voluntary.rs:377` | `reschedule_ipi_preempts_user_within_1ms` | requires QEMU + SMP + preempt-voluntary |
| `kernel/tests/preempt_voluntary.rs:389` | `preempt_count_nonzero_suppresses_preemption` | requires QEMU + preempt-voluntary |
| `kernel/tests/xsave_avx.rs:137` | `ymm_upper_halves_survive_context_switch` | Track G activation pending |
| `kernel/tests/xsave_avx.rs:144` | `ymm_upper_halves_survive_1000_iterations` | Track G activation pending |
| `kernel-core/tests/driver_restart.rs:790` | `qemu_nvme_kill_mid_write_returns_driver_restarting` | QEMU-only |
| `kernel-core/tests/driver_restart.rs:828` | `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds` | QEMU-only |
| `kernel-core/tests/driver_restart.rs:870` | `qemu_max_restart_exceeded_service_status_returns_failed` | QEMU-only |
| `kernel-core/tests/phase56_g1_multi_client_coexistence.rs:99` | `multi_client_coexistence_deferred` | Phase 56 G.1 deferred to QEMU integration |
| `kernel-core/tests/phase56_g2_keybind_grab_hook.rs:146` | `runtime_grab_hook_synthetic_injection_belongs_in_qemu_smoke` | Phase 56 G.2 deferred |
| `kernel-core/tests/phase56_g4_control_socket_roundtrip.rs:165` | `runtime_list_surfaces_subscribe_and_frame_stats_belongs_in_qemu_smoke` | Phase 56 G.4 deferred |
| `userspace/drivers/nvme/tests/isolation.rs:75` | `cross_device_mmio_denied_end_to_end` | phase-55c deferred — `todo!()` body |
| `userspace/drivers/nvme/tests/isolation.rs:102` | `cross_device_dma_denied_end_to_end` | phase-55c deferred — `todo!()` body |
| `userspace/drivers/nvme/tests/isolation.rs:128` | `capability_forge_denied_end_to_end` | phase-55c deferred — `todo!()` body |
| `userspace/drivers/nvme/tests/isolation.rs:157` | `post_crash_handles_invalid_end_to_end` | phase-55c deferred — `todo!()` body |

---

## `unsafe` block density

| Crate | Files with `unsafe` | Total `unsafe { }` blocks |
|---|---|---|
| `kernel/src/` | 44 | 526 |
| `userspace/` (all crates) | 34 | 328 |
| `kernel-core/src/` | 2 | 25 |
| `xtask/src/` | 1 | 2 |
| **Total** | **81** | **881** |

Safety-comment coverage in `kernel/src/`: ~309 `// SAFETY:` comments against 526 `unsafe { }` blocks (~59%). The ~217-block gap is concentrated in:
- `syscall/mod.rs` — 57 unsafe blocks
- `interrupts.rs` — 45 unsafe blocks
- `scheduler.rs` — 58 unsafe blocks

Userspace `syscall-lib/src/lib.rs` alone holds 137 unsafe blocks. These are at the kernel-userspace boundary (syscall wrappers and the `BrkAllocator`); some have inline rationale, most do not.

---

## Concerns the roadmap docs do not capture

These are surfaced from code only; the roadmap status fields would not lead a reader to suspect them.

- **W^X is absent.** Marked as a "Phase 6+" deferral but never delivered, and the README does not flag it as a known security gap.
- **IPC capability grants, bulk transfers, and timeouts are entirely absent.** The IPC engine is missing the features that would make it a true seL4-style capability IPC.
- **Phase 55c isolation tests are entirely scaffolding.** Phase 55c is claimed Complete; the four negative-path isolation tests have `todo!()` bodies. Privilege-isolation has zero executable coverage.
- **`fat_server` is a permanent ENOSYS stub** despite Phase 54 claiming the storage extraction.
- **Display server subscription push is structurally incomplete.** Out-of-process tools subscribing to surface lifecycle receive nothing.
- **Scheduler pi_lock wiring is missing** at four sites Phase 57a was supposed to deliver.
- **AMD IOMMU has no fault ISR** despite Phase 55a being treated as Complete.
- **VT-d scalable mode and queued invalidation are disabled.** Permanent limitations on modern VT-d hardware.
- **14 preemption + XSAVE tests are all `#[ignore]`d** waiting for "Track G activation" — Track G is not assigned to a completed or planned phase.
