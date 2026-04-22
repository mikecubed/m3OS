# Phase 55c — Ring-3 Driver Correctness Closure: Task List

**Status:** Planned
**Source Ref:** phase-55c
**Depends on:** Phase 6 (IPC Core) ✅, Phase 50 (IPC Completion) ✅, Phase 52c (Kernel Architecture Evolution) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅
**Goal:** Close three ring-3-specific correctness gaps Phase 55b left behind: **R3** (event-multiplexing deadlock — post-handshake SSH hang over `--device e1000`), **R2** (IOMMU VT-d / AMD-Vi identity coverage missing for device MMIO BARs under `--iommu`), and **R1** (userspace `sendto()` never observes `EAGAIN` during ring-3 driver restart). All three ship together because they share the `kernel/src/net/remote.rs`, `kernel/src/ipc`, and `userspace/lib/driver_runtime` surfaces; splitting would force the same files to churn three times. Kernel version bumps to `v0.55.3`. Updates `docs/appendix/phase-55b-residuals.md` to strike R1 and R2.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | **R3 kernel-core:** bound-notification state model, `WakeKind` tag, property tests for signal/send race atomicity | None | Planned |
| B | **R3 kernel wiring:** `BOUND_TCB` / `TCB_BOUND_NOTIF` arrays, `sys_notif_bind` syscall, `ipc_recv_msg` wake-kind extension, TCB-teardown binding release, ISR-safety audit | A | Planned |
| C | **R2 kernel-core:** `BarCoverage` invariant + host tests for identity-coverage per claimed device | None | Planned |
| D | **R2 kernel wiring:** VT-d and AMD-Vi per-device domain extension to insert identity-mapped BAR pages at `sys_device_claim`; failure surfaces a typed `DeviceHostError::Internal` with a structured `iommu.missing_bar_coverage` log event | C | Planned |
| E | **R3 `driver_runtime`:** `IpcBackend::recv` returns `RecvResult`; mock backend + `NetServer` / `BlockServer` dispatch; `IrqNotification::bind_to_endpoint` helper | A, B | Planned |
| F | **R3 e1000 consumer:** `subscribe_and_bind`, collapsed `run_io_loop`, removal of the standalone `irq.wait()` path | E | Planned |
| G | **R1 kernel:** `sys_net_send` (or `sys_sendto` extension — final shape decided in G.1), `RemoteNic::send_frame` → `-EAGAIN` translation through `net_error_to_neg_errno`, socket-layer preference for `RemoteNic` when registered | None (parallel to A/B) | Planned |
| H | **R1 userspace:** `e1000-crash-smoke` EAGAIN assertion; unignore `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds` | G | Planned |
| I | **Regressions:** `scripts/ssh_e1000_banner_check.sh` (R3), `cargo xtask device-smoke --device {nvme,e1000} --iommu` (R2) wired into the same CI lane, EAGAIN smoke (R1) | D, F, H | Planned |
| J | **Documentation + version:** 55b residuals strike-throughs; Phase 56 bound-notification precondition note; `v0.55.3` bump | I | Planned |

---

## Engineering Discipline and Test Pyramid

These are preconditions for every code-producing task in this phase. A task cannot be marked complete if it violates any of them.

### Test-first ordering (TDD)

- Tests for every code-producing task commit **before** the implementation that makes them pass. Git history for the touched files must show failing-test commits preceding green-test commits.
- Acceptance lists that say "at least N tests cover …" name *minimums*. If the implementation reveals a new case, add the test before closing the task.
- A task is not complete until every test it names runs green via `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`, `cargo test -p driver_runtime --target x86_64-unknown-linux-gnu`, or `cargo xtask test`.

### Test pyramid

| Layer | Location | Runs via | Covers |
|---|---|---|---|
| Unit | `kernel-core/src/ipc/bound_notif.rs`, `kernel-core/src/iommu/bar_coverage.rs` | `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` | R3 bind/unbind invariants, `WakeKind` round-trip, `EBUSY` cases; R2 BAR identity-coverage invariant across arbitrary BAR layouts |
| Property | `kernel-core` with `proptest` (Phase 43c) | Same | R3 arbitrary interleavings of bind/signal/send/recv/drop land in a consistent state; R2 arbitrary BAR layouts yield deterministic identity coverage |
| Contract | `userspace/lib/driver_runtime/tests/recv_result_contract.rs` | `cargo test -p driver_runtime --target x86_64-unknown-linux-gnu` | `RecvResult::Message` and `RecvResult::Notification` both flow through every `handle_next` consumer without loss or duplication; mock and syscall backends pass identical suites |
| Integration | `userspace/drivers/e1000/tests/bound_notif_smoke.rs` + xtask QEMU harness | `cargo xtask test` and `cargo xtask run --device e1000` | End-to-end: TCP connect → SSH version exchange → server banner received; driver loop executes at least one `Notification` wake and at least one `Message` wake per connection |
| Regression | `scripts/ssh_e1000_banner_check.sh`, `cargo xtask device-smoke --iommu`, `cargo xtask regression --test e1000-restart-crash` | `cargo xtask ssh-e1000-banner-check`, existing device-smoke, existing regression subcommand | Scripted SSH-banner probe over `--device e1000` (R3); IOMMU-on variants of nvme / e1000 device-smoke (R2); EAGAIN on mid-restart send (R1) |

Pure logic belongs in `kernel-core`. Syscall wiring and MMIO-adjacent work belongs in `kernel/src/`. Driver-facing surface belongs in `userspace/lib/driver_runtime/`. Device-specific changes belong in `userspace/drivers/{e1000,nvme}/`. No task may straddle the boundary without splitting its code.

### SOLID and module boundaries

- **Single Responsibility.** `kernel-core::ipc::bound_notif` owns the R3 pure-logic state model. `kernel/src/ipc/notification.rs` owns the ISR-reachable `BOUND_TCB` array. `kernel/src/ipc/endpoint.rs` owns the recv-path bound-aware fast path. `kernel/src/ipc/mod.rs` owns `sys_notif_bind` dispatch. `kernel-core::iommu::bar_coverage` owns the R2 invariant. `kernel/src/iommu/{vtd,amdvi}.rs` owns the R2 wiring. `kernel/src/net/remote.rs` owns the R1 EAGAIN translation. `kernel/src/syscall/net.rs` owns the R1 syscall dispatch. `driver_runtime::ipc` owns the `RecvResult` surface. No module takes on a second concern.
- **Open / Closed.** The e1000 driver consumes the bound-notification primitive through `driver_runtime::RecvResult`; it does not reach into `syscall_lib` directly. A future driver lands by consuming the same `RecvResult` surface — not by editing `driver_runtime` or the kernel. The R2 BAR-coverage helper is called once per claim; adding a third IOMMU backend (e.g., a future RISC-V IOMMU) lands by adding a backend file, not by editing the helper.
- **Interface Segregation.** `sys_notif_bind` exposes a minimal surface: two capability handles in, one status code out. `sys_net_send` — if chosen over extending `sys_sendto` — exposes only the send path; receive stays in the existing socket surface.
- **Liskov Substitution.** NVMe's `BlockServer::handle_next` is substitutable before and after E.2 — the `RecvResult::Notification` arm is a documented no-op for drivers that have no bound notification. Existing NVMe tests stay green. `RemoteNic::send_frame` is substitutable at every caller before and after G.3 — the EAGAIN path replaces an existing generic-error path, not a silent success.

### DRY

- The R3 bound-notification state model lives **once** in `kernel-core::ipc::bound_notif`. The kernel's `notification.rs` and `endpoint.rs` consume it; pure-logic tests and the kernel path share the same invariants.
- `WakeKind` and its `WAKE_KIND_NOTIFICATION` sentinel live **once** in `kernel-core::ipc`. No file redefines the bit layout.
- `RecvResult` lives **once** in `driver_runtime::ipc`. Drivers consume it; `NetServer`/`BlockServer` dispatch on it; mock and syscall backends produce it.
- The R2 `BarCoverage` invariant helper lives **once** in `kernel-core::iommu`. VT-d and AMD-Vi backends both call it; no backend redefines BAR-coverage semantics.
- The R1 errno translation (`net_error_to_neg_errno`) already exists — Phase 55c does not duplicate it. `sys_net_send` and any `sys_sendto` extension both route through the same function.

### YAGNI

- No syscall, struct field, or capability bit is added speculatively. `sys_notif_bind` carries exactly the two caps it needs; it does not pre-reserve space for future bind flags.
- The `BOUND_TCB` array is sized at `MAX_NOTIFS`. If the bound-slot count needs to grow, extend `MAX_NOTIFS` with a tracked rationale — do not pre-allocate.
- `RecvResult` has exactly two variants. A third variant (e.g., `TimerFired`) is deferred until a driver actually needs it.
- G.1's design memo picks one send-path shape and does not implement both.

### Boy Scout Rule

- Leave every file you touch cleaner than you found it: fix a stale comment, remove a dead import, or clarify an opaque variable name. Keep changes scoped to the task at hand; do not open unrelated refactors.
- When a task reveals a lint suppression (`#[allow(...)]`) with no documented justification, either add a justification comment or remove it.
- Dead test stubs that are no longer accurate must be updated or removed before the task is marked complete — do not leave misleading `#[ignore]` annotations without a comment naming the exact blocker.

### Error discipline

- Non-test code contains no `.unwrap()`, `.expect()`, `panic!()`, `todo!()`, or `unreachable!()` outside documented fail-fast sites. Every such site carries an inline comment naming the reason.
- `sys_notif_bind` returns `-EBUSY` on double-bind and `-EBADF` on invalid capability. No panic, no unwinding.
- `sys_device_claim` returns `DeviceHostError::Internal` when R2's identity-coverage invariant fails. The caller observes the failure through the existing error surface.
- `sys_net_send` / extended `sys_sendto` returns `-EAGAIN` when `RemoteNic::send_frame` yields `NetDriverError::DriverRestarting`. Other `NetDriverError` values map to existing errnos via `net_error_to_neg_errno`.

### Observability

- R3: bind syscall logs `ipc.notif_bind` at info level (PID, notif cap, endpoint cap). Unbind on TCB teardown logs `ipc.notif_unbind`. The e1000 driver logs one line per wake-kind transition.
- R2: `sys_device_claim` emits `device_host.claim.bar_coverage` for each BAR's identity-map insertion (info). Coverage failures emit `iommu.missing_bar_coverage` at warn level with BDF and BAR index.
- R1: `RemoteNic::send_frame` logs `driver.absent` (warn, deduplicated) on the first send during a restart window; subsequent sends during the same window do not re-log until restart completes.

### Capability safety

- `sys_notif_bind` validates both capability handles on every call. An invalid cap returns `-EBADF` without side effect. A cap belonging to another process returns `-EBADF` (cross-process bind is never permitted).
- A driver restart re-runs bind as part of its init sequence. No binding state survives the kill.
- R1's `sys_net_send` does not introduce a new capability — it reuses the existing socket-fd capability from the POSIX socket layer.
- R2's BAR identity map is scoped to the device's own domain — the driver cannot use its identity-mapped MMIO to touch another device's BAR.

### Concurrency and IRQ safety

- The `BOUND_TCB[MAX_NOTIFS]` array is lock-free (`AtomicI32` per slot). `signal_irq` reads it without acquiring any lock, honoring rule 3 of the ISR contract.
- Mutation of `BOUND_TCB` from task context goes through the existing `WAITERS` mutex wrapped in `without_interrupts`.
- The endpoint recv path acquires endpoint queue lock → notification `WAITERS` slot in a fixed order to prevent deadlock. Lock ordering is documented in `kernel/src/ipc/endpoint.rs` module docs.
- R2's IOMMU-domain MMIO insert runs at claim time only — never on the hot path. No IRQ interaction.

---

## Track A — R3 kernel-core state model

### A.1 — Failing tests for the bound-notification state machine

**Files:**
- `kernel-core/src/ipc/bound_notif.rs` (new)
- `kernel-core/src/ipc/mod.rs` (add `pub mod bound_notif`)

**Symbol:** `BoundNotifTable`
**Why it matters:** The bind/unbind/signal/recv interleavings are where the ring-3 deadlock hides. Locking the invariants in a pure-logic model proves correctness before any unsafe kernel code is written.

**Acceptance:**
- [ ] `bind_then_rebind_same_pair_is_idempotent`.
- [ ] `bind_same_notif_to_different_tcb_returns_busy`.
- [ ] `bind_different_notif_to_same_tcb_returns_busy`.
- [ ] `unbind_clears_slot`.
- [ ] `tcb_drop_clears_all_bindings_owned_by_tcb`.
- [ ] `notif_free_clears_binding_and_returns_tcb`.
- [ ] At least 6 unit tests commit red before any `BoundNotifTable` implementation.

### A.2 — `WakeKind` tag and encoding

**File:** `kernel-core/src/ipc/wake_kind.rs` (new)
**Symbol:** `WakeKind`, `encode_wake_kind`, `decode_wake_kind`
**Why it matters:** The label field of `IpcMessage` carries either a peer-provided label or a notification-bit mask. Encoding both through one field lets us extend `ipc_recv_msg` without a new syscall or a wider ABI.

**Acceptance:**
- [ ] `WakeKind::Message(label)` and `WakeKind::Notification(bits)` round-trip through a single `u64`.
- [ ] `WAKE_KIND_NOTIFICATION = 1 << 63`; peer labels always clear bit 63 (assertion in encode).
- [ ] Tests: round-trip for arbitrary `u63` label, arbitrary `u64` bit mask, mixed interleavings.
- [ ] At least 3 unit tests land red before encode/decode implementation.

### A.3 — Property tests for wake-race atomicity

**File:** `kernel-core/src/ipc/bound_notif_proptest.rs` (new)
**Symbol:** `bound_notif_race_safety`
**Why it matters:** The seL4-style guarantee is that no signal-plus-send race can lose either wake. Property tests make this explicit on the model.

**Acceptance:**
- [ ] Given an arbitrary sequence of `bind`, `signal(bits)`, `send(label)`, `recv()`, the state after each `recv()` satisfies: exactly one wake is dispatched **or** at least one source becomes pending and is observable on the next recv.
- [ ] Signals arriving during a blocked recv are never merged with an earlier send's label.
- [ ] `proptest` configured with at least 1024 cases; runs under `cargo test -p kernel-core --release --target x86_64-unknown-linux-gnu`.

---

## Track B — R3 kernel wiring

### B.1 — `BOUND_TCB` / `TCB_BOUND_NOTIF` arrays + ISR-safety audit

**Files:**
- `kernel/src/ipc/notification.rs`
- `kernel/src/task/mod.rs`

**Symbols:** `BOUND_TCB`, `TCB_BOUND_NOTIF`, inline-doc audit update
**Why it matters:** These arrays are touched by `signal_irq` from ISR context. They must be lock-free on the signal path; any future rule-3 audit must find them already compliant.

**Acceptance:**
- [ ] `BOUND_TCB: [AtomicI32; MAX_NOTIFS]` parallel to `ISR_WAITERS`. Default `-1`.
- [ ] `TCB_BOUND_NOTIF: [AtomicU8; MAX_TASKS]` default `NotifId::NONE`.
- [ ] `signal_irq` reads `BOUND_TCB[idx]` without acquiring any lock.
- [ ] Module-level doc in `notification.rs` lists the binding tables under the existing ISR-safety invariants.
- [ ] No new unsafe block introduced; atomicity carried entirely by the atomic types.

### B.2 — Failing tests for `sys_notif_bind`

**File:** `kernel-core/tests/sys_notif_bind_contract.rs` (new)
**Symbol:** contract test against a fake kernel-like state
**Why it matters:** The syscall's contract is small and testable without a live kernel; the contract test locks behavior before kernel work starts.

**Acceptance:**
- [ ] `bind_matches_bound_notif_table` — `sys_notif_bind` updates both `BOUND_TCB` and `TCB_BOUND_NOTIF`.
- [ ] `bind_returns_ebusy_on_double_bind_different_target`.
- [ ] `bind_returns_ebadf_on_invalid_notif_cap`.
- [ ] `bind_returns_ebadf_on_invalid_endpoint_cap`.
- [ ] `idempotent_same_pair_returns_zero`.
- [ ] Tests commit red.

### B.3 — Implement `sys_notif_bind`

**Files:**
- `kernel/src/ipc/mod.rs`
- `kernel/src/syscall.rs`

**Symbol:** `sys_notif_bind`
**Why it matters:** The one new syscall.

**Acceptance:**
- [ ] Syscall number allocated as the next free slot after the current IPC block (`0x1111`).
- [ ] `ipc_recv_msg` doc comment updated to note the new wake-kind encoding.
- [ ] Every test in B.2 now passes.
- [ ] `cargo xtask check` passes.

### B.4 — Extend `ipc_recv_msg` with bound-notification fast path

**File:** `kernel/src/ipc/endpoint.rs`
**Symbol:** `recv_msg`
**Why it matters:** Kernel-side load-bearing change. The caller's bound notification is consulted before parking on the endpoint; a signaled notification wakes the caller with `WakeKind::Notification`.

**Acceptance:**
- [ ] Fast path: bound notification with pending bits → drain atomically, return `WakeKind::Notification(bits)` without touching the endpoint queue.
- [ ] Slow path: caller registers as both endpoint receiver and notification waiter, blocks. Subsequent `send` or `signal` wakes with the corresponding `WakeKind`.
- [ ] Lock order documented: endpoint → notification.
- [ ] `WakeKind` encoded through the `IpcMessage.label` field per A.2.
- [ ] Integration test in `kernel/tests/bound_recv.rs` (QEMU harness): signal during blocked recv → `RecvResult::Notification`; message during blocked recv → `RecvResult::Message`.
- [ ] No regression in existing `kernel/tests/ipc_call_reply.rs`.

### B.5 — TCB teardown clears binding

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process::on_exit` (or nearest equivalent)
**Why it matters:** A driver crash must not leave a dangling `BOUND_TCB` entry pointing at a freed task slot.

**Acceptance:**
- [ ] Process exit walks `TCB_BOUND_NOTIF` for the dying task, clears the corresponding `BOUND_TCB` entry, and clears the self slot.
- [ ] Notification release (`sys_notif_release`) clears the binding if present, and wakes the bound TCB with a defined "source gone" state.
- [ ] Test: `process_exit_clears_binding_smoke` in `kernel/tests/`.

---

## Track C — R2 kernel-core BAR coverage

### C.1 — Failing tests for `BarCoverage`

**Files:**
- `kernel-core/src/iommu/bar_coverage.rs` (new)
- `kernel-core/src/iommu/mod.rs` (add `pub mod bar_coverage`)

**Symbol:** `BarCoverage`, `assert_bar_identity_mapped`
**Why it matters:** Lock the invariant — "every claimed-device BAR is identity-mapped in the device's IOMMU domain" — in pure-logic tests before touching VT-d / AMD-Vi code.

**Acceptance:**
- [ ] `single_bar_identity_maps`.
- [ ] `multi_bar_identity_maps`.
- [ ] `missing_bar_fails_assertion_with_typed_error`.
- [ ] `zero_length_bar_is_noop` (some BARs are vestigial).
- [ ] `bar_overlap_detected` (two BARs sharing a physical page — documented legal; identity map covers the union).
- [ ] At least 5 unit tests commit red.

### C.2 — Property tests for arbitrary BAR layouts

**File:** `kernel-core/src/iommu/bar_coverage_proptest.rs` (new)
**Symbol:** `bar_coverage_properties`
**Why it matters:** BARs can occupy arbitrary physical ranges; property tests prove the invariant holds across all of them.

**Acceptance:**
- [ ] For arbitrary `(base, length)` tuples, the identity map covers every page in `[base, base+length)`.
- [ ] For overlapping BARs, the identity map covers the union.
- [ ] `proptest` configured with at least 1024 cases.

---

## Track D — R2 kernel wiring

### D.1 — VT-d domain extension for BAR identity mapping

**File:** `kernel/src/iommu/vtd.rs`
**Symbol:** `VtdDomain::install_bar_identity_maps`
**Why it matters:** Closes the R2 residual on VT-d hosts (Intel). `CTRL.RST` writes reach hardware.

**Acceptance:**
- [ ] Per-device domain setup inserts identity-mapped 4 KiB pages covering each BAR's `(base, length)` pair.
- [ ] The insert is called from `sys_device_claim` after `BarCoverage::assert_bar_identity_mapped` succeeds on the pure-logic side.
- [ ] Integration test: `cargo xtask device-smoke --device nvme --iommu` passes end-to-end.

### D.2 — AMD-Vi domain extension for BAR identity mapping

**File:** `kernel/src/iommu/amdvi.rs`
**Symbol:** `AmdViDomain::install_bar_identity_maps`
**Why it matters:** Parity with VT-d on AMD hosts. Mirror of D.1 against the AMD-Vi backend.

**Acceptance:**
- [ ] Same invariants as D.1, same test shape.
- [ ] Integration test: `cargo xtask device-smoke --device e1000 --iommu` passes end-to-end on an AMD-Vi-capable QEMU machine type (where available; VT-d remains the default smoke).

### D.3 — Failure surface + logging

**File:** `kernel/src/syscall/device_host.rs`
**Symbol:** `sys_device_claim`
**Why it matters:** Failures must be observable — not silent.

**Acceptance:**
- [ ] If `BarCoverage::assert_bar_identity_mapped` fails, `sys_device_claim` returns `DeviceHostError::Internal`.
- [ ] A structured `iommu.missing_bar_coverage` log event is emitted at warn level with BDF and BAR index.
- [ ] Unit test: injected failure path produces the expected errno and log event.

---

## Track E — R3 `driver_runtime` surface

### E.1 — Failing tests for `RecvResult` dispatch

**File:** `userspace/lib/driver_runtime/tests/recv_result_contract.rs` (new)
**Symbol:** `RecvResult`, `IpcBackend::recv`
**Why it matters:** Locks behavior before changing the trait signature; catches any `handle_next` consumer we miss.

**Acceptance:**
- [ ] `mock_backend_emits_both_variants`.
- [ ] `net_server_handle_next_dispatches_notification_variant`.
- [ ] `net_server_handle_next_dispatches_message_variant`.
- [ ] `block_server_handle_next_ignores_notification_variant`.
- [ ] Tests commit red.

### E.2 — Extend `IpcBackend::recv` to `RecvResult`

**Files:**
- `userspace/lib/driver_runtime/src/ipc/mod.rs`
- `userspace/lib/driver_runtime/src/ipc/net.rs`
- `userspace/lib/driver_runtime/src/ipc/block.rs`

**Symbols:** `RecvResult`, `IpcBackend::recv`, `NetServer::handle_next`, `BlockServer::handle_next`
**Why it matters:** Shared surface every ring-3 driver sees. Changing it once keeps drivers aligned.

**Acceptance:**
- [ ] `IpcBackend::recv -> Result<RecvResult, DriverRuntimeError>`.
- [ ] `SyscallBackend::recv` decodes `WakeKind` from the kernel (bit 63 of `IpcMessage.label`) and returns the matching variant.
- [ ] `NetServer::handle_next` grows a two-closure API: one for messages, one for notifications.
- [ ] `BlockServer::handle_next` grows the same shape; notification closure default is a no-op.
- [ ] Every E.1 test passes.

### E.3 — `IrqNotification::bind_to_endpoint` helper

**File:** `userspace/lib/driver_runtime/src/irq.rs`
**Symbol:** `IrqNotification::bind_to_endpoint`
**Why it matters:** Single helper so drivers don't replicate the bind syscall plumbing.

**Acceptance:**
- [ ] `IrqNotification::bind_to_endpoint(&self, ep: EndpointCap) -> Result<(), DriverRuntimeError>`.
- [ ] Delegates to `syscall_lib::sys_notif_bind`.
- [ ] Host test exercises the mock backend; syscall test exercises the live ABI in a QEMU integration test.

---

## Track F — R3 e1000 consumer

### F.1 — Failing integration test for bound-notification wake

**File:** `userspace/drivers/e1000/tests/bound_notif_smoke.rs` (new)
**Symbol:** `drives_both_arms_of_run_io_loop`
**Why it matters:** Before touching the loop, prove the test harness can observe both wake arms.

**Acceptance:**
- [ ] Test uses the `FakeMmio` + mock `IpcBackend` already present in `io.rs` tests.
- [ ] Harness emits a `Notification` first and a `Message` second; `run_io_loop` dispatches both once.
- [ ] Test commits red.

### F.2 — Collapse `run_io_loop` onto the bound-notification model

**File:** `userspace/drivers/e1000/src/io.rs`
**Symbol:** `run_io_loop`, `subscribe_and_bind` (new)
**Why it matters:** User-visible fix. Post-handshake SSH deadlock goes away.

**Acceptance:**
- [ ] `subscribe_and_bind(&device, endpoint)` — subscribes IRQ, binds notification to endpoint, arms IMS in that order.
- [ ] `run_io_loop` contains no `irq.wait()` call. Blocks only on `endpoint.recv_multi(&irq_notif)`.
- [ ] On `RecvResult::Notification { bits }`: `handle_irq_and_drain`, `irq.ack(bits)`.
- [ ] On `RecvResult::Message(req)`: `send_frame` + reply.
- [ ] F.1's test passes.
- [ ] Existing e1000 host tests stay green.

### F.3 — Remove standalone `irq.wait()` path

**File:** `userspace/drivers/e1000/src/io.rs`
**Symbol:** `run_io_loop`
**Why it matters:** Leaving both paths present invites future drift.

**Acceptance:**
- [ ] `grep "irq.wait" userspace/drivers/e1000/src/` returns no hits in `run_io_loop`.
- [ ] `IrqNotification::wait` stays in `driver_runtime` for non-bound consumers with a doc note explaining when to use `bind_to_endpoint` instead.

---

## Track G — R1 kernel `sys_net_send` / EAGAIN wiring

### G.1 — Design decision: new `sys_net_send` vs extend `sys_sendto`

**File:** `docs/appendix/phase-55c-net-send-shape.md` (new — short design memo)
**Symbol:** N/A
**Why it matters:** The residual doc recommends "either a new syscall or teach the existing path". The two shapes have different ABI costs; G.1 picks one and records the rationale before implementation.

**Acceptance:**
- [ ] Memo names the chosen shape (`sys_net_send` vs `sys_sendto` extension), the rationale, and the rejected alternative's tradeoffs.
- [ ] Memo references the existing `sys_block_{read,write}` pattern as the precedent.
- [ ] Memo lists the exact list of files that change under the chosen shape.

### G.2 — Failing tests for mid-restart EAGAIN

**File:** `kernel-core/tests/driver_restart.rs`
**Symbol:** `net_error_to_neg_errno_driver_restarting_is_eagain`
**Why it matters:** The test exists; it needs a companion that proves the errno is observable end-to-end.

**Acceptance:**
- [ ] New test `sys_net_send_mid_restart_returns_eagain` proves that a `RemoteNic::send_frame` call returning `NetDriverError::DriverRestarting` surfaces as `-EAGAIN` through the chosen syscall.
- [ ] Test commits red.

### G.3 — Implement the chosen send-path shape

**Files:**
- `kernel/src/syscall/net.rs` (if new syscall)
- `kernel/src/net/socket/mod.rs` (if extended `sys_sendto`)
- `kernel/src/net/remote.rs`

**Symbols:** `sys_net_send` or `sys_sendto::send`, `RemoteNic::send_frame`
**Why it matters:** The load-bearing R1 change.

**Acceptance:**
- [ ] On `DriverRestarting`, returns `-EAGAIN`.
- [ ] Other `NetDriverError` values map through the existing `net_error_to_neg_errno`.
- [ ] Socket layer prefers `RemoteNic` when registered; falls back to virtio-net otherwise.
- [ ] G.2's test passes.
- [ ] No regression in existing UDP/TCP tests.

---

## Track H — R1 userspace smoke

### H.1 — `e1000-crash-smoke` EAGAIN assertion

**File:** `userspace/e1000-crash-smoke/src/main.rs`
**Symbol:** `assert_eagain_during_restart`
**Why it matters:** The load-bearing regression for R1. A regression here means the kernel's `DriverRestarting` error went invisible again.

**Acceptance:**
- [ ] The binary triggers a driver kill mid-send via the existing crash harness.
- [ ] Asserts the follow-up `sendto()` returns `-EAGAIN`.
- [ ] Exits with a distinct non-zero code if the assertion fails; exit 0 on success.
- [ ] `cargo xtask regression --test e1000-restart-crash` executes this binary.

### H.2 — Unignore `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds`

**File:** `kernel-core/tests/driver_restart.rs`
**Symbol:** `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds`
**Why it matters:** The stub was ignored because the EAGAIN surface didn't exist. G + H.1 create the surface.

**Acceptance:**
- [ ] `#[ignore]` removed.
- [ ] Test passes under the existing QEMU regression harness.

---

## Track I — Regression harness

### I.1 — `scripts/ssh_e1000_banner_check.sh` (R3)

**File:** `scripts/ssh_e1000_banner_check.sh` (new)
**Symbol:** shell script
**Why it matters:** Any future change that re-breaks the driver wake path fails here before it fails in the field.

**Acceptance:**
- [ ] Boots `cargo xtask run --device e1000` headless.
- [ ] Waits for `sshd: listening on :22` (or equivalent).
- [ ] Connects `ssh -oBatchMode=yes root@127.0.0.1 -p2222`; captures first 64 bytes.
- [ ] Asserts the captured prefix contains `SSH-2.0-`.
- [ ] Fails with a distinct exit code if the banner is not received within 5 s.
- [ ] Mirrors the `scripts/ssh_wedge_check.sh` reference shape.

### I.2 — `cargo xtask ssh-e1000-banner-check` subcommand

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_ssh_e1000_banner_check` (new)
**Why it matters:** Single-command reproduction from CI or a developer shell.

**Acceptance:**
- [ ] Accepts `--timeout <secs>` (default 30) and `--display` (default off).
- [ ] Invokes `scripts/ssh_e1000_banner_check.sh` and surfaces its exit code.
- [ ] Xtask unit test proves the constructed command line matches the documented contract.

### I.3 — `cargo xtask device-smoke --iommu` parity (R2)

**File:** `xtask/src/main.rs`
**Symbol:** `device_smoke_script_nvme`, `device_smoke_script_e1000`
**Why it matters:** R2 acceptance is end-to-end. The CI lane that runs the non-IOMMU variant must also run the IOMMU variant.

**Acceptance:**
- [ ] Both scripts pass under `--iommu` within the same timeout envelope as the non-IOMMU variant (~6 s target).
- [ ] A failure in either IOMMU variant fails the same CI lane — not a separate optional lane.

### I.4 — EAGAIN regression (R1)

**File:** Existing `cargo xtask regression` harness entry for `e1000-restart-crash`
**Symbol:** N/A
**Why it matters:** The EAGAIN surface is regression-tested end-to-end.

**Acceptance:**
- [ ] The existing harness invocation observes H.1's exit code.
- [ ] A failing H.1 binary fails the harness.

### I.5 — CI hook

**File:** `.githooks/pre-push` (or equivalent CI workflow)
**Symbol:** N/A
**Why it matters:** Keeps the regression honest.

**Acceptance:**
- [ ] Pre-push hook (or equivalent) runs `cargo xtask ssh-e1000-banner-check` behind an env gate (`M3OS_E1000_REGRESSION=1`) so default `git push` stays fast.
- [ ] `cargo xtask check` docs list the env gate.

---

## Track J — Documentation + version

### J.1 — Strike R1 and R2 from the 55b residuals doc

**File:** `docs/appendix/phase-55b-residuals.md`
**Symbol:** N/A
**Why it matters:** The residuals doc is the canonical "what we know is still wrong" index. Once closed, R1 and R2 must vanish from it (with a pointer to Phase 55c's delivery).

**Acceptance:**
- [ ] R1 section replaced with a one-line "Closed in Phase 55c (Track G/H)" pointer.
- [ ] R2 section replaced with a one-line "Closed in Phase 55c (Track C/D)" pointer.
- [ ] Section 4 scheduling summary updated to reflect both as closed.

### J.2 — Post-mortem: `docs/post-mortems/2026-MM-DD-e1000-bound-notif.md`

**File:** `docs/post-mortems/2026-MM-DD-e1000-bound-notif.md` (new)
**Symbol:** N/A
**Why it matters:** The post-mortem trail captures what was learned at each design turn. R3 needs one; R1 / R2 can appear as brief entries under the same post-mortem's "adjacent closures" section.

**Acceptance:**
- [ ] Root cause statement: "Ring-3 driver main loop alternately blocks on `IrqNotification::wait` and `Endpoint::recv`; the two wake sources are serialized, not multiplexed."
- [ ] Why Phase 55b did not catch it: device-smoke exercises one-shot ICMP echo; SSH is the first sustained bidirectional workload.
- [ ] Fix applied: bound notifications per seL4's model.
- [ ] Confirmation: SSH banner arrives within 5 s after `cargo xtask run --device e1000`.
- [ ] Adjacent closures: R1 (EAGAIN surface) and R2 (IOMMU BAR identity-map) closed in the same phase.

### J.3 — Phase 56 driver template updates

**Files:**
- `docs/roadmap/56-display-and-input-architecture.md`
- `docs/roadmap/tasks/56-display-and-input-architecture-tasks.md`

**Symbol:** N/A
**Why it matters:** Every Phase 56 driver has the same mix of async events and sync requests. Phase 56 should specify the bound-notification usage up front.

**Acceptance:**
- [ ] Both Phase 56 docs name `RecvResult` + `IrqNotification::bind_to_endpoint` as the required pattern for display-driver and input-driver event loops.
- [ ] The Phase 56 tasks touching a driver loop list "consumes Phase 55c bound notifications" as a precondition.

### J.4 — Phase 55c learning doc

**File:** `docs/roadmap/55c-ring-3-driver-correctness-closure-learning.md` (new)
**Symbol:** N/A
**Why it matters:** Every completed phase requires a learning doc. Phase 56 authors and future contributors need a structured explanation of the three primitives Phase 55c added. Without it the roadmap has a gap at the exact point where Phase 56's driver loop design decisions depend on understanding what Phase 55c established.

**Acceptance:**
- [ ] Doc follows the **aligned legacy learning doc** template from `docs/appendix/doc-templates.md`: `Aligned Roadmap Phase`, `Status`, `Source Ref`, `Supersedes Legacy Doc` (N/A), `Overview`, `What This Doc Covers`, `Core Implementation`, `Key Files`, `How This Phase Differs From Later Work`, `Related Roadmap Docs`, `Deferred or Later-Phase Topics`.
- [ ] **What This Doc Covers** lists exactly: (1) bound notifications and the seL4 wake-model composition pattern (R3); (2) IOMMU domain MMIO identity mapping for claimed devices (R2); (3) driver-restart error propagation through the kernel's `RemoteNic` facade to userspace `EAGAIN` (R1).
- [ ] **Key Files** table names at minimum: `kernel-core/src/ipc/bound_notif.rs`, `kernel/src/ipc/notification.rs`, `kernel/src/ipc/endpoint.rs`, `kernel-core/src/iommu/bar_coverage.rs`, `kernel/src/net/remote.rs`, `userspace/lib/driver_runtime/src/ipc/mod.rs`, `userspace/drivers/e1000/src/io.rs`.
- [ ] **How This Phase Differs From Later Work** notes that Phase 56 consumes `RecvResult` and `IrqNotification::bind_to_endpoint` — those primitives are taught in this doc, not in Phase 56's doc.
- [ ] Doc added to `docs/roadmap/README.md` alongside the design doc and task doc links for Phase 55c.
- [ ] `docs/roadmap/55c-ring-3-driver-correctness-closure.md` **Companion Task List** section updated to include a link to this learning doc.

### J.5 — Version bump to `v0.55.3`

**Files:**
- `kernel/Cargo.toml`
- `AGENTS.md`
- `README.md`
- `docs/roadmap/README.md`
- `docs/roadmap/55c-ring-3-driver-correctness-closure.md` (Status flips to Complete)
- `docs/roadmap/tasks/55c-ring-3-driver-correctness-closure-tasks.md` (Status flips to Complete)

**Symbol:** N/A
**Why it matters:** The repo's declared version must stay in sync with the completed phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.55.3"`.
- [ ] `AGENTS.md` project-overview line updated.
- [ ] `README.md` version line updated.
- [ ] Both roadmap READMEs reflect Phase 55c as Complete.
- [ ] `cargo xtask check` passes.

---

## Rollback Plan

### R3 rollback

If `sys_notif_bind` or the `ipc_recv_msg` wake-kind extension introduces a regression that a small patch cannot contain:

1. Revert the e1000 driver commit(s) under Track F. The driver reverts to the Phase 55b two-stage loop. SSH over e1000 regresses to the known-wedge state, but all other paths keep working.
2. Keep Tracks A / B / E landed — they are opt-in surfaces. No driver besides e1000 consumes them yet; the kernel surface is dormant.
3. Write a follow-up task identifying the specific regression and reopen this phase for a second pass.

The kernel-side arrays (`BOUND_TCB`, `TCB_BOUND_NOTIF`) are lock-free and carry zero cost when unused, so rollback does not require removing them.

### R2 rollback

If the IOMMU BAR identity mapping breaks non-IOMMU boots (it shouldn't — the path only runs when an IOMMU is present and enabled):

1. Revert Track D commits. `cargo xtask device-smoke --iommu` returns to the pre-55c timeout failure. Non-IOMMU boots are unaffected.
2. Track C's pure-logic helper is dormant; no need to revert it.

### R1 rollback

If `sys_net_send` (or the `sys_sendto` extension) breaks existing net paths:

1. Revert Track G commits. `sendto()` returns to the pre-55c generic-error behavior; EAGAIN is again invisible during restart but no data path is lost.
2. Track H's assertion fails; the regression harness correctly flags the rollback.
3. Leave the `#[ignore]` annotation in `kernel-core/tests/driver_restart.rs` reinstated.

## Documentation Notes

- **Learning doc is mandatory.** J.4 must be complete before the phase is marked Complete. The **aligned legacy learning doc** template from `docs/appendix/doc-templates.md` is the required shape — do not merge the phase-complete commit (J.5) without the learning doc in tree.
- **What changed vs Phase 55b.** Phase 55b shipped the ring-3 driver host but left three correctness gaps. Phase 55c closes all three. The learning doc and post-mortem (J.2) are the canonical record of what was wrong, why it was not caught earlier, and what Phase 55c fixed.
- **Prefer exact files over directories.** Every task's **File** / **Files** entry names a concrete path, not a directory. If a file is renamed or split during implementation, update this doc before closing the task.
- **Prefer exact symbols over generic descriptions.** Every task's **Symbol** entry names the specific function, type, or constant — not the module or crate. Generic descriptions like "net module" are not acceptable.
- The design doc names seL4 bound notifications as the reference for R3. Keep the comparison table in `docs/roadmap/55c-ring-3-driver-correctness-closure.md` ("How Real OS Implementations Differ") in sync with any future pivot away from the seL4 model.
- The Phase 56 precondition entries in J.3 must be reviewed during Phase 56 kickoff; if 55c slips, 56's planning baseline slips with it.
- The 55b residuals strike-through in J.1 is load-bearing documentation: subsequent readers should see "closed in 55c" rather than a stale open item. Do not close J.1 with a vague edit — the pointer must name the exact Track letters (G/H for R1, C/D for R2, A–F for R3).
