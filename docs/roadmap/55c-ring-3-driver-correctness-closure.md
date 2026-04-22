# Phase 55c - Ring-3 Driver Correctness Closure

**Status:** Planned
**Source Ref:** phase-55c
**Depends on:** Phase 55b (Ring-3 Driver Host) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 50 (IPC Completion) ✅, Phase 52c (Kernel Architecture Evolution) ✅, Phase 6 (IPC Core) ✅
**Builds on:** Phase 55b shipped ring-3 NVMe and e1000 drivers but left three ring-3-specific correctness gaps unresolved. Two were recorded explicitly in `docs/appendix/phase-55b-residuals.md` — **R1** (`sys_net_send` surface for userspace-visible `EAGAIN` during driver restart) and **R2** (IOMMU VT-d identity-map coverage for device MMIO BARs under `--iommu`). The third — **R3** (event-multiplexing deadlock in the ring-3 driver main loop) — was discovered post-closure when SSH over `--device e1000` hung after the TCP handshake. Phase 55c closes all three under one ring-3-correctness umbrella so Phase 56 (display, input) starts from a driver substrate that is actually correct, not just partially working.
**Primary Components:** kernel/src/ipc/notification, kernel/src/ipc/endpoint, kernel/src/ipc/mod (syscall gate), kernel-core/src/ipc (bound-notification state model + `WakeKind`), kernel/src/iommu (identity-mapped MMIO windows for claimed-device BARs), kernel-core/src/iommu (BAR identity-coverage invariant), kernel/src/net (`sys_net_send` dispatch), kernel/src/net/remote (driver-restart error surface, EAGAIN mapping), userspace/lib/driver_runtime (RecvResult surface, net-send helper), userspace/drivers/e1000 (first bound-notification consumer), userspace/drivers/nvme (IOMMU MMIO regression consumer, opt-in event-mux migration), userspace/e1000-crash-smoke (EAGAIN assertion)

## Milestone Goal

Three ring-3-driver correctness gaps Phase 55b did not close are closed together in Phase 55c:

1. **R3 — Event multiplexing.** A ring-3 driver binds its IRQ notification to its command endpoint and blocks in a single `recv()` that wakes for either source. SSH over `--device e1000` reaches a server banner within 5 s.
2. **R2 — IOMMU MMIO coverage.** `cargo xtask device-smoke --device nvme --iommu` and `--device e1000 --iommu` both pass end-to-end. Every claimed-device BAR is identity-mapped in the device's IOMMU domain so `CTRL.RST` writes reach the hardware.
3. **R1 — Userspace EAGAIN visibility.** A userspace `sendto()` through the e1000 path observes `EAGAIN` while the ring-3 driver is mid-restart, instead of silently succeeding via virtio-net or surfacing a generic failure. The `e1000-crash-smoke` binary asserts this.

The three workstreams ship together because they have overlapping surfaces (R3 changes the recv path; R1 builds a new `sys_net_send` that parallels `sys_block_{read,write}`; R2 exercises the same driver bring-up path the bound-notification loop then consumes). Splitting them across phases would force the same files to churn three times.

## Why This Phase Exists

### R3 — the SSH-over-e1000 deadlock

The e1000 driver's main loop `userspace/drivers/e1000/src/io.rs::run_io_loop` alternately blocks on two distinct kernel primitives — `IrqNotification::wait()` (on a `Notification`) and `NetServer::handle_next()` (on an `Endpoint`) — with no way to service the other source while parked. The observed failure:

1. TCP handshake completes: client's SYN IRQs the NIC, the driver wakes from `irq.wait()`, publishes the frame, falls through to `handle_next()`. Kernel TCP posts SYN-ACK, `handle_next()` returns, frame goes out. Same for the client's ACK.
2. Driver re-enters `handle_next()` — **blocks on `ipc_recv_msg`** waiting for the next TX request.
3. Client sends the SSH version string. The e1000 RXs the frame and raises IRQ 11. Kernel sets the RX bit on the driver's notification.
4. **Driver is not in `irq.wait()`** — it is parked inside `ipc_recv_msg`. The notification bit stays set; the RX descriptor stays undrained in the NIC.
5. Kernel TCP never sees the client data; sshd never reads it; no TX request is ever queued to the endpoint; `handle_next()` blocks forever. Deadlock.

Virtio-net does not hit this because virtio-net is still an in-kernel driver with no main-loop wake problem. The NVMe ring-3 driver does not hit this because block devices are purely request-response — completion polling happens inside the request handler, so `handle_next()` alone suffices. **e1000 is the first ring-3 driver that genuinely mixes async hardware events with sync IPC requests.** Every driver Phase 56 and 57 add (display vsync, mouse events, audio buffer completion) has the same shape.

### R2 — IOMMU MMIO identity coverage

Per `docs/appendix/phase-55b-residuals.md`, `cargo xtask device-smoke --device {nvme,e1000} --iommu` both time out in the driver's bring-up / self-test step after ~2 s. Root cause: VT-d remapping covers the device MMIO window; the driver's `CTRL.RST` write (NVMe `CC.EN` / e1000 `CTRL.RST`) is silently dropped because the IOMMU domain has no identity-mapped entry for the BAR region. This is invisible under the original F.4 smoke (which only asserted an `init: driver.registered` boot log line) and surfaces only under the F.4b tighter assertions (`NVME_SMOKE:rw:PASS`, `E1000_SMOKE:link:PASS`).

This is a correctness blocker for any 1.0 claim of "IOMMU-isolated ring-3 drivers" and is a prerequisite for R3's acceptance test under `--iommu`.

### R1 — userspace EAGAIN visibility

Per `docs/appendix/phase-55b-residuals.md`, `RemoteNic::send_frame` in `kernel/src/net/remote.rs` correctly returns `NetDriverError::DriverRestarting` to kernel-side callers during a driver restart window, but the existing userspace net datapath (`sys_sendto`, UDP/TCP socket writes) does not route through that surface — it prefers the virtio-net fallback or surfaces a generic error. Consequence: a userspace application observes a silent success (virtio-net handled it) or a generic failure, never the ring-3-specific `EAGAIN`. The `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds` stub in `kernel-core/tests/driver_restart.rs` is `#[ignore]`-d for this reason.

The R1 residual recommended Phase 60 (Networking and GitHub) as the owner. Phase 55c pulls it forward because it shares surfaces with R3 (both touch `kernel/src/net/remote.rs`) and because shipping Phase 58 with this gap makes the 1.0 "supervised ring-3 drivers" claim weaker than it has to be.

### Scheduling rationale

1. All three are ring-3-driver correctness gaps. Grouping them yields one review, one regression harness update, one version bump, one learning-doc delta.
2. R2 is an R1 prerequisite under `--iommu`: the e1000 driver cannot even start without MMIO coverage.
3. R3 is the only novel primitive (bound notifications). R1 and R2 are wiring closures on existing infrastructure.
4. Phase 56's display and input drivers need R3's primitive on day one (vsync notifications interleaved with composition requests; HID interrupts interleaved with mode-set requests). Landing R3 before 56 is free; landing it after means a follow-up sweep across three drivers plus the display server.
5. The project explicitly models after seL4 (CLAUDE.md: *"Synchronous rendezvous + async notification objects (seL4-style)"*). Bound notifications are the canonical seL4 answer to the TCB-waits-on-two-sources problem.

## Learning Goals

- Understand the fundamental wake-model asymmetry between synchronous rendezvous (Endpoint) and asynchronous signaling (Notification), and why a microkernel needs a composition primitive to reconcile them.
- Learn the seL4 bound-notification pattern: one notification bound to a TCB, checked atomically on every endpoint recv, wakes the TCB for either source.
- See how ISR-safety constraints from the 2026-04-21 scheduler-lock post-mortem shape the implementation — the bind table is reachable from both task and interrupt contexts and must follow rule 3 of the ISR contract.
- Understand the capability-handle surface for binding: a driver receives a notification capability (Phase 55b `sys_device_irq_subscribe`) and an endpoint capability (Phase 50 `sys_ipc_create_endpoint`) and composes them with a new `sys_notif_bind`.
- Understand how IOMMU domains must cover device MMIO (not just DMA) so the driver's own programming writes reach the hardware, and why this is an identity-mapping rather than a translated one.
- Understand how a mid-restart driver propagates a typed error (`NetDriverError::DriverRestarting`) through the kernel's `RemoteNic` facade to a userspace `sendto()` as `EAGAIN`, and why this preserves the caller's retry semantic without sprinkling driver-specific logic into the socket layer.

## Feature Scope

### R3.1 — Bound-notification state model

Each `Notification` gains an optional bound-TCB pointer. When set, the binding is one-to-one: the notification is bound to exactly one TCB, and that TCB has at most one bound notification. Attempting to re-bind either side returns `EBUSY`. The binding is cleared on TCB death (process exit, driver crash) and on notification free (`sys_notif_release`). A rebind is possible after either side releases.

### R3.2 — Bound-aware receive

`ipc_recv_msg(ep_cap)` is extended so the kernel checks the caller's bound notification before parking on the endpoint:

- If the bound notification has pending bits, `ipc_recv_msg` returns immediately with a new *kind* of result — "notification wake" — and atomically drains the bits.
- Otherwise, the caller blocks on the endpoint as before, but **also** registers as the notification's waiter. A subsequent signal (from ISR `signal_irq` or task-context `signal`) wakes the caller with the same "notification wake" result.
- If a peer IPC send arrives first, the caller wakes with the existing "message wake" result.
- The two wake paths are serialized by the kernel: a signal that arrives after the recv has already committed to a message still lands as pending bits, visible on the next recv.

### R3.3 — Syscall surface

One new syscall:

- `sys_notif_bind(notif_cap, ep_cap) -> 0 | -EBUSY | -EBADF` — one-shot bind. Idempotent when re-bound with the same `(notif_cap, ep_cap)` pair.

One extended syscall:

- `ipc_recv_msg(ep_cap, msg_ptr, buf_ptr, buf_len)` — return value gains a small success-kind channel: `0` for message wake, `1` for notification wake, negative errnos unchanged for real syscall failures. On message wake, `IpcMessage.label` stays the peer's label as before. On notification wake, the drained notification bits are written to `IpcMessage.data[0]`. This keeps wake discrimination disjoint from existing protocols that already use negative errnos in `IpcMessage.label`.

### R3.4 — `driver_runtime` RecvResult

A new `RecvResult` enum in `userspace/lib/driver_runtime/src/ipc/mod.rs`:

```rust
pub enum RecvResult {
    Message(RecvFrame),
    Notification { bits: u64 },
}
```

`SyscallBackend::recv` returns `RecvResult`. `NetServer::handle_next` and `BlockServer::handle_next` gain a two-closure form (one for messages, one for notifications). Drivers that do not need IRQ multiplexing (NVMe today) keep a no-op `Notification` arm; they opt in later.

### R3.5 — e1000 consumer

`userspace/drivers/e1000/src/io.rs::run_io_loop` collapses to one blocking call per iteration:

```rust
loop {
    match endpoint.recv_multi(&irq_notif) {
        RecvResult::Notification { bits } => {
            handle_irq_and_drain(&mut device, &net_server);
            irq.ack(bits);
        }
        RecvResult::Message(req) => {
            let status = send_frame(&mut device, &req.frame).into();
            endpoint.reply(NetReply { status });
        }
    }
}
```

`subscribe_irq` additionally issues `sys_notif_bind(irq_notif_cap, endpoint_cap)` before the loop starts. `arm_irqs` is unchanged.

### R2.1 — IOMMU BAR identity-coverage invariant

`kernel-core/src/iommu/` gains a pure-logic invariant and accompanying test: for every claimed `PciDeviceHandle`, the device's IOMMU domain carries an identity mapping of every BAR's `(base, length)` pair. The invariant is enforced at `sys_device_claim` time (the kernel has the BAR metadata from PCI enumeration) and revalidated at `sys_device_mmio_map`.

### R2.2 — VT-d / AMD-Vi domain setup extends to MMIO

`kernel/src/iommu/vtd.rs` and `kernel/src/iommu/amdvi.rs` extend per-device domain setup to insert identity-mapped 4 KiB pages covering each BAR. The existing DMA domain plumbing is unchanged. The domain's page-table walker handles both DMA and MMIO without branching because both are identity-mapped.

### R2.3 — Device-smoke regression asserts `--iommu` parity

The existing `device_smoke_script_nvme` and `device_smoke_script_e1000` in `xtask/src/main.rs` already accept `--iommu`. The regression harness is updated so a `--iommu` failure fails the same CI lane that catches the non-IOMMU case, not a separate optional lane. The Phase 55b R2 residual entry is struck.

### R1.1 — `sys_net_send` syscall

A new syscall `sys_net_send(socket_fd, buf_ptr, len, flags) -> isize` in `kernel/src/syscall/net.rs`. Routes through `RemoteNic::send_frame` when registered; falls back to the existing virtio-net path only when no ring-3 net driver is bound. On `NetDriverError::DriverRestarting`, returns `-EAGAIN` through `net_error_to_neg_errno`.

Alternative surface (chosen for minimal ABI churn): extend the existing `sys_sendto` to prefer `RemoteNic` and propagate the error byte. Final shape decided in the Track G design task (G.1 below); both variants land the same EAGAIN visibility.

### R1.2 — `e1000-crash-smoke` asserts EAGAIN

`userspace/e1000-crash-smoke/src/main.rs` grows a new assertion: during the mid-restart window, a `sendto()` call returns `-EAGAIN` (or equivalent). The assertion is the load-bearing acceptance check for R1.

### R1.3 — Unignore `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds`

`kernel-core/tests/driver_restart.rs` — the `#[ignore]` attribute on the named stub is removed; the test is promoted to the authoritative regression list.

## Important Components and How They Work

### Kernel-side binding table (R3)

`kernel/src/ipc/notification.rs` gains a `BOUND_TCB: [AtomicI32; MAX_NOTIFS]` array — one slot per notification, parallel to the existing `ISR_WAITERS`. A value of `-1` means "not bound"; otherwise the slot holds a `scheduler_task_idx` that must stay valid for the notification's lifetime. The array is lock-free so `signal_irq` remains ISR-safe; mutations go through `ALLOCATED` under the existing `WAITERS` mutex wrapped in `without_interrupts`.

`Process::on_exit` clears every entry in `BOUND_TCB` whose value is the exiting task's index. Notification release (`sys_notif_release`, existing syscall) already walks the waiters — the new binding is cleared in the same call site.

### Endpoint recv path (R3)

`kernel/src/ipc/endpoint::recv_msg` is extended to consult the caller's bound notification. The atomic sequence for a blocking recv is:

1. Take the endpoint queue lock.
2. If a sender is queued, rendezvous and return `WakeKind::Message(label)` — existing fast path.
3. Otherwise, look up the caller's bound notification (inverse lookup: `TCB_BOUND_NOTIF[task_idx]` gives `NotifId` in O(1)).
4. If the bound notification has pending bits (`PENDING[notif_idx]`), swap them atomically and return `WakeKind::Notification(bits)` — no endpoint parking.
5. Otherwise, register as both the endpoint's receiver **and** the notification's waiter, then block.
6. Wake occurs when either `endpoint::send` or `notification::signal*` runs. The wake path sets a `last_wake_kind` scalar on the TCB indicating which source fired; `recv_msg` inspects it and returns accordingly.

The atomic guarantee: between steps 3 and 5 the kernel holds locks that exclude both the notification's waiter-registration and the endpoint's sender-registration. A signal arriving during this window lands as pending bits and is caught by the step-4 swap; a send arriving lands in the endpoint queue and is caught by the step-2 fast path.

### IOMMU BAR identity coverage (R2)

`kernel-core/src/iommu/` adds a `BarCoverage` type that names each BAR's `(base, length)` pair and an invariant-check helper `assert_bar_identity_mapped(domain, coverage)`. The kernel's per-device domain setup calls this helper at claim time; a failure logs a structured `iommu.missing_bar_coverage` event and fails `sys_device_claim` with `DeviceHostError::Internal`.

The Phase 55a IOMMU substrate handles the address-space translation machinery; Phase 55c only extends the set of covered ranges to include MMIO.

### `sys_net_send` / RemoteNic EAGAIN propagation (R1)

`kernel/src/net/remote.rs::RemoteNic::send_frame` already returns `NetDriverError::DriverRestarting` when the driver is mid-restart. Phase 55c adds the missing glue: a userspace caller's send path now consults `RemoteNic` first, and on `DriverRestarting` translates to the errno `EAGAIN` via the existing `net_error_to_neg_errno`. The socket layer learns a one-line preference for `RemoteNic` when it is registered; the virtio-net path remains the fallback when no ring-3 driver owns the device.

### e1000 driver collapse (R3)

The removal of `irq.wait()` from the hot path is the load-bearing change. The driver subscribes the IRQ notification, binds it to the command endpoint, and then enters a single-blocking loop. Because the kernel serializes the wake decision, there is no way for a notification signal to be lost while the driver is parked on the endpoint, and no way for a peer send to be lost while the driver is parked on the notification.

## How This Builds on Earlier Phases

- Extends the Phase 6 `Notification` / `Endpoint` primitives with a binding relationship; the individual primitives are unchanged, only their composition grows.
- Reuses Phase 50's capability-table machinery: `sys_notif_bind` validates both capabilities on every call; no new capability type is introduced.
- Preserves the Phase 52c / 55b ISR contract: `signal_irq` remains lock-free and ISR-safe; the new `BOUND_TCB` array is lock-free on the signal path.
- Does not change the Phase 55b device-host syscalls (`sys_device_claim`, `sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe`) beyond extending `sys_device_claim`'s internal IOMMU-domain setup with BAR identity-coverage — the public syscall surface is unchanged.
- Extends the Phase 55a IOMMU substrate with a BAR identity-coverage invariant; the underlying DMAR/IVRS parsing and the per-device domain object model are reused as-is.
- Extends the Phase 54 / 55b `RemoteNic` facade with an error-byte propagation to userspace that closes the R1 residual.

## Implementation Outline

1. **R3 foundation**: add the kernel-core pure-logic state model for bound notifications and the `WakeKind` tag. TDD-first; at least 6 unit tests plus 1 property test commit red.
2. **R3 kernel wiring**: implement `sys_notif_bind` and extend `ipc_recv_msg`'s wake return. Write the contract tests that match the pure-logic model.
3. **R2 foundation**: add the `kernel-core::iommu::BarCoverage` invariant + host tests. TDD-first.
4. **R2 kernel wiring**: extend VT-d and AMD-Vi domain setup to insert identity-mapped BAR pages at `sys_device_claim` time. Wire the new invariant check into the existing device-smoke assertions so a missing identity map fails the CI lane.
5. **R3 driver_runtime**: extend `IpcBackend::recv` to return `RecvResult`. Update the mock backend and every existing `handle_next` call site. NVMe and block-server host tests stay green (the `Notification` arm is a no-op for them).
6. **R3 driver migration**: migrate the e1000 driver's main loop. Add a QEMU-level integration test — `cargo xtask run --device e1000` + a TCP loopback probe that proves sshd's banner arrives within 5 s.
7. **R1 syscall + facade**: land `sys_net_send` (or extend `sys_sendto` — G.1 picks the exact shape). Route through `RemoteNic::send_frame` when registered. Map `NetDriverError::DriverRestarting` to `-EAGAIN` through the existing `net_error_to_neg_errno`.
8. **R1 smoke**: extend `userspace/e1000-crash-smoke` with the EAGAIN assertion. Remove the `#[ignore]` from the driver-restart stub.
9. **Regression harness**: add `scripts/ssh_e1000_banner_check.sh` (R3) and ensure `cargo xtask device-smoke --device {nvme,e1000} --iommu` (R2) both run in the same CI lane as the non-IOMMU variants.
10. **Documentation + version**: update the Phase 55b residuals doc (strike R1 and R2, link-back R3 new post-mortem). Update the Phase 56 display-and-input plan to specify the bound-notification usage upfront. Bump kernel version to `v0.55.3`.

## Acceptance Criteria

### R3 — Event multiplexing

- `ssh root@localhost -p2222` against `cargo xtask run --device e1000` receives the server version banner and reaches the authentication prompt within 5 s of connection. Measured by `scripts/ssh_e1000_banner_check.sh`.
- `userspace/drivers/e1000/src/io.rs::run_io_loop` contains no `irq.wait()` call; all waking happens through the bound endpoint's `recv()`.
- A kernel-core host test proves that a `signal` arriving during a blocked `recv` wakes the recv with `RecvResult::Notification { bits }` carrying the signaled bits, and that a `send` arriving during the same blocked `recv` wakes it with `RecvResult::Message`. Order-dependent cases are tested in both arrival orders.
- A kernel-core host test proves that `sys_notif_bind` is idempotent when re-invoked with the same pair, and returns `-EBUSY` on either a double-bind of the same notification to a different TCB or a double-bind of a different notification to the same TCB.
- On driver-process crash, the binding is released atomically with the TCB teardown; a fresh `sys_notif_bind` after restart succeeds.

### R2 — IOMMU MMIO coverage

- `cargo xtask device-smoke --device nvme --iommu` passes end-to-end (~6 s target, matching the non-IOMMU variant).
- `cargo xtask device-smoke --device e1000 --iommu` passes end-to-end.
- A kernel-core host test in `kernel-core/src/iommu/` verifies BAR identity-coverage for any claimed device; a missing mapping fails the test.
- The Phase 55b R2 residual entry is struck in `docs/appendix/phase-55b-residuals.md`.

### R1 — Userspace EAGAIN visibility

- `userspace/e1000-crash-smoke/src/main.rs` observes `-EAGAIN` from its `sendto()` call during the mid-restart window.
- `cargo xtask regression --test e1000-restart-crash` passes end-to-end with the EAGAIN assertion enabled.
- `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds` in `kernel-core/tests/driver_restart.rs` is no longer `#[ignore]`-d.
- The Phase 55b R1 residual entry is struck.

### Phase-wide

- e1000 `device-smoke` (Phase 55b F.4b) continues to pass. Phase 55b's crash-and-restart regression continues to pass.
- `cargo xtask check` passes: clippy clean, rustfmt, kernel-core host tests green.
- Kernel version bumps to `v0.55.3` across `kernel/Cargo.toml`, `AGENTS.md`, `README.md`, and both roadmap READMEs.

## Companion Task List

- [Phase 55c Task List](./tasks/55c-ring-3-driver-correctness-closure-tasks.md)

## How Real OS Implementations Differ

- seL4 ships the R3 primitive — the `bind_notification` IPC capability and its integration with `seL4_Wait`/`seL4_Recv` — as a first-class part of the kernel from day one. Phase 55c brings m3OS to parity with the reference architecture for this specific design slice.
- Linux solves the analogous R3 problem with `epoll` / `io_uring` — a general-purpose multiplexer sitting above both file descriptors and eventfd. The generality buys batching and timer integration but costs a descriptor table and a signal demultiplexer; m3OS deliberately stays at the single-bound-notification level for now.
- Fuchsia uses "ports" — kernel objects that aggregate signals from multiple handles. Ports are more general than bound notifications (any handle, not just one notification) but require a port-wait syscall distinct from IPC recv. Phase 55c keeps the two syscalls unified because the driver use case is 1:1, not N:M.
- Linux's IOMMU code (R2 analog) identity-maps all RMRR (Reserved Memory Region Records) at boot; m3OS scoped that to claimed-device BARs only because the project has no legacy-device compatibility surface to worry about. The narrower scope is safer and shorter.
- Linux's EAGAIN-on-restart (R1 analog) goes through its driver-model probe-retry machinery with no visibility into userspace; m3OS surfaces the typed error explicitly so applications can back off rather than blocking.

## Deferred Until Later

- **Many-to-one binding** (multiple notifications bound to one TCB). The e1000 case needs exactly one; display/input/audio cases in Phase 56/57 can still be modeled with one binding per driver. A future phase can lift this if a driver is introduced that genuinely needs two independent async sources.
- **Timed recv** (`ipc_recv_timeout`). The e1000 deadlock goes away without it. Phase 55c ships only the bound-notification path; any timeout support is a separate design decision.
- **NVMe migration to bound-notification model.** NVMe today uses `handle_next` alone and polls for completion inside the handler, which is correct for its request-response shape. A future driver that needs interrupt-driven completion (NVMe with large queue depths where polling wastes CPU) can opt in then.
- **IOMMU coverage for MSI-X table regions.** The R2 fix covers BARs; MSI-X table regions live inside a specific BAR and are already covered transitively. A future hardening phase can audit this is still true after any PCIe-config-space changes.
- **Generalized EAGAIN over block IO.** R1 covers net send; the NVMe side already surfaces `DriverRestarting` but `sys_block_{read,write}` does not yet translate it. A later phase that hardens block-layer error surfaces can apply the same pattern.
- **Secondary e1000 IRQ-coalescing concerns flagged in the 55b residuals appendix** (multiple RX descriptor-ring wraparounds). Phase 55c does not attempt that — it only ensures the driver wakes at all.
- **Driver-side seccomp / syscall sandbox**, **hot-plug / surprise-removal**, **VirtIO-blk / VirtIO-net extraction**, **driver live-update / zero-downtime restart**, **multi-queue NVMe** — all remain deferred from the Phase 55b "Deferred Until Later" list. Phase 55c does not alter their scheduling.
