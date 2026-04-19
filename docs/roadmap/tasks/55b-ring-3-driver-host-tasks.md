# Phase 55b — Ring-3 Driver Host: Task List

**Status:** Planned
**Source Ref:** phase-55b
**Depends on:** Phase 46 (System Services) ✅, Phase 50 (IPC Completion) ✅, Phase 54 (Deep Serverization) ✅, Phase 55 (Hardware Substrate) ✅, Phase 55a (IOMMU Substrate) ✅
**Goal:** Extract NVMe and Intel 82540EM e1000 out of ring 0 into supervised ring-3 driver processes on the Phase 54 service pattern. Four new capability-gated kernel primitives (`sys_device_claim`, `sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe`) issue device, MMIO, DMA, and IRQ capabilities through Phase 50's capability table; a shared `userspace/lib/driver_runtime` crate wraps them in HAL-shaped safe abstractions that mirror the Phase 55 kernel-side HAL. Kernel retains only the block- and network-layer facades (`RemoteBlockDevice`, `RemoteNic`) that forward requests to the driver processes over IPC. Driver crashes restart cleanly through the Phase 46 / Phase 51 service manager with documented blast radius (one in-flight request). Kernel LOC drops by at least the combined NVMe + e1000 source measured at Phase 55 close (approximately 2000 lines). Version bumps to v0.55.2.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Pure-logic foundation in `kernel-core` (`Capability::Device` variant, device-host ABI types, block / net driver IPC protocol schemas, `driver_runtime` abstract contracts) | None | Planned |
| B | Four kernel device-host syscalls (`sys_device_claim`, `sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe`) behind the Phase 50 capability table; BAR-window bounds-checked user mappings; IOMMU-gated DMA handle duality (user VA + IOVA); MSI / INTx → `Notification` bridging | A | Planned |
| C | `userspace/lib/driver_runtime` shared crate: HAL-shaped `DeviceHandle`, `Mmio<T>`, `DmaBuffer<T>`, `IrqNotification`; IRQ wait-loop helper; block / net IPC client helpers | A, B | Planned |
| D | NVMe extraction to `userspace/drivers/nvme/`; `RemoteBlockDevice` kernel facade in `kernel/src/blk/mod.rs`; delete `kernel/src/blk/nvme.rs` | A, B, C | Planned |
| E | e1000 extraction to `userspace/drivers/e1000/`; `RemoteNic` kernel facade in `kernel/src/net/mod.rs`; delete `kernel/src/net/e1000.rs` | A, B, C | Planned |
| F | Supervision and validation: Phase 46 / Phase 51 service registration for both driver processes; crash-and-restart regression; cross-device MMIO / DMA negative test; Phase 55 data-path smokes re-run through ring-3 drivers; kernel-LOC audit | D, E | Planned |
| G | Documentation and version: Phase 55b learning doc; Phase 15 / 55 / 55a subsystem doc updates; roadmap README rows; version bump to v0.55.2 | F | Planned |

---

## Engineering Discipline and Test Pyramid

These are preconditions for every code-producing task in this phase. A task cannot be marked complete if it violates any of them. Where a later task re-states a rule for emphasis, the rule here is authoritative.

### Test-first ordering (TDD)

- Tests for a code-producing task commit **before** the implementation that makes them pass. Git history for the touched files must show failing-test commits preceding green-test commits. "Tests follow" is not acceptable.
- Acceptance lists that say "at least N tests cover …" name *minimums*. If the implementation reveals a new case, add the test before closing the task.
- A task is not complete until every test it names can be executed via `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` (unit, contract, property) or `cargo xtask test` (integration).

### Test pyramid

| Layer | Location | Runs via | Covers |
|---|---|---|---|
| Unit | `kernel-core/src/device_host/`, `kernel-core/src/driver_ipc/` | `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` | Pure logic: `Capability::Device` variant invariants, MMIO-window bounds arithmetic, block / net IPC protocol serialize / deserialize round-trip, error enum decode |
| Contract | `kernel-core` shared harness (`kernel-core/tests/driver_runtime_contract.rs`, `kernel-core/tests/device_host_syscall_contract.rs`) | Same | `driver_runtime`'s `DeviceHandle`, `Mmio<T>`, `DmaBuffer<T>`, `IrqNotification` contracts pass an identical suite against a mock syscall backend and against the real syscall ABI shape — proves LSP compliance for every future driver |
| Property | `kernel-core` with `proptest` (available from Phase 43c) | Same | Block / net IPC message round-trip across arbitrary valid inputs; MMIO-window bounds-check on arbitrary `(base, len, requested_offset, requested_len)` tuples; IOVA handle-pair consistency across arbitrary allocation sequences |
| Integration | `kernel/tests/`, `userspace/drivers/*/tests/`, and the `xtask test` harness | `cargo xtask test` (QEMU with `-device nvme` / `-device e1000` / `-device intel-iommu`) | End-to-end: driver process claims device, maps BAR, allocates DMA, receives IRQs; NVMe 512 B read / write round-trip through ring-3 driver passes; e1000 ICMP echo through ring-3 driver passes; crash-and-restart regression; cross-device negative test |

Pure logic belongs in `kernel-core`. Hardware-dependent and syscall-gated wiring belongs in `kernel/src/` and `userspace/drivers/`. Tasks that straddle the boundary split their code along it so the pure part is host-testable; no task may defer this split to "later".

### SOLID and module boundaries

- **Single Responsibility.** Each new module owns one concern: `kernel/src/syscall/device_host.rs` → syscall dispatch + capability validation; `kernel-core/src/device_host/` → ABI data types; `userspace/lib/driver_runtime/` → HAL-shaped wrappers consumed by every driver; `userspace/drivers/nvme/` → NVMe register semantics only; `userspace/drivers/e1000/` → e1000 register semantics only; `kernel/src/blk/remote.rs` → block forwarding facade; `kernel/src/net/remote.rs` → net forwarding facade.
- **Open / Closed and Dependency Inversion.** The extension seam is the `driver_runtime` API surface (C.2) and the four device-host syscalls (B). A third driver (VirtIO-blk, VirtIO-net, or a future AHCI driver) lands by consuming `driver_runtime` and the block / net IPC protocol — not by editing `driver_runtime` internals or the syscalls. Kernel-side callers consume drivers through `RemoteBlockDevice` / `RemoteNic`, not through concrete driver processes.
- **Interface Segregation.** Each device-host syscall exposes a narrow, capability-typed surface. `sys_device_claim` yields only a `Capability::Device` handle; BAR access, DMA, and IRQ subscription each require a separate capability-gated call. A driver that needs only MMIO does not obtain DMA rights implicitly.
- **Liskov Substitution.** `RemoteBlockDevice` is substitutable for the Phase 55 in-kernel NVMe driver at every `kernel/src/blk/mod.rs` call site; `RemoteNic` is substitutable for the Phase 55 in-kernel e1000 driver at every `kernel/src/net/mod.rs` call site. The VFS layer, TCP stack, and `net_server` see no behavioral change beyond error surface extension for driver-absent states (documented in F.1). The `driver_runtime` contract suite (A.4, exercised in C.2) proves this.

### DRY

- The `Capability::Device` variant, `DeviceCapKey`, `MmioWindowDescriptor`, `DmaHandle`, and `DeviceHostError` live **once** in `kernel-core::device_host`. No syscall handler redefines them; no driver redefines them; grep for any of these names across the workspace returns exactly one definition.
- Block-driver IPC message schemas (`BLK_READ`, `BLK_WRITE`, `BLK_STATUS`) live once in `kernel-core::driver_ipc::block`. `RemoteBlockDevice` (kernel) and the NVMe driver (userspace) both consume the same schema; divergence is caught at compile time by the shared types.
- Net-driver IPC message schemas (`NET_SEND_FRAME`, `NET_RX_FRAME`, `NET_LINK_STATE`) live once in `kernel-core::driver_ipc::net`. `RemoteNic` (kernel) and the e1000 driver (userspace) both consume the same schema.
- The `driver_runtime` HAL wrappers wrap the four syscalls exactly once. `userspace/drivers/nvme/` and `userspace/drivers/e1000/` never call `syscall_lib` directly for device-host operations; they go through `driver_runtime`.
- The service-definition contract for driver processes reuses the Phase 46 / Phase 51 `.conf` format; Phase 55b adds no new service-manager conventions.

### Error discipline

- Non-test code contains no `.unwrap()`, `.expect()`, `panic!()`, `todo!()`, or `unreachable!()` outside of documented fail-fast initialization points (e.g., driver-process cannot claim the only device its manifest names). Every such site carries an inline comment naming the audited reason it is safe.
- Every module boundary returns typed `Result<T, NamedError>` with named enums per subsystem: `DeviceHostError`, `DriverRuntimeError`, `BlockDriverError`, `NetDriverError`, `RemoteDeviceError`. Error variants are data, not stringly-typed; callers can match and recover.
- Driver-process crash is a named, logged event. On `SIGSEGV` / `SIGILL` / unexpected exit, the service manager logs one structured `driver.restart` event naming the driver, PID, exit code, and uptime before restarting. Restart is also a named event (`driver.restarted`).

### Observability

- Every capability grant logs one structured event (`device_host.claim`, `device_host.mmio_map`, `device_host.dma_alloc`, `device_host.irq_subscribe`) keyed by driver PID, BDF, and capability ID. Events are emitted at info level and survive the driver-process lifetime for post-mortem analysis.
- `RemoteBlockDevice` and `RemoteNic` log a one-line `driver.absent` event (at warn level) when a request arrives for a driver process that has crashed and is mid-restart; subsequent successful requests log no event. This surfaces in-flight-request loss without spamming the log during normal operation.
- Driver-process crash and restart cycles are visible through the Phase 51 `service status` admin command; a driver stuck in a restart loop exceeds its `max_restart` and is marked `failed` with the same semantics as any other Phase 46 service.

### Capability safety

- Every new syscall takes a `Capability::Device` handle (or a handle derived from one, e.g. `Capability::Mmio`) as its first argument after the syscall number. The kernel validates the handle on every call using Phase 50's `CapabilityTable` machinery; unauthenticated or cross-process use returns `-EBADF` without side effect.
- A driver process that is killed and restarted re-acquires all capabilities by re-running its init (`sys_device_claim`, `sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe`). No capability state survives the kill. F.3 asserts this with a negative test that attempts to reuse a capability across a restart.
- Per the Phase 55a IOMMU contract, a DMA allocation is bound to a specific `PciDeviceHandle`'s domain; `sys_device_dma_alloc` may not return a handle a driver can use against another device. Cross-device DMA through `sys_device_dma_alloc` is a design-time impossibility, validated by F.3's negative test.

### Concurrency and IRQ safety

- `Notification`-based IRQ delivery to ring 3 preserves the Phase 6 / Phase 50 contract: the kernel ISR does the minimum (read status, ack hardware, signal notification word, send EOI) and never calls into userspace. The driver process blocks in `notification_wait` and wakes on signal; userspace IRQ handling runs in task context, not interrupt context.
- Service-manager restart acquires the driver's capability slots serially; restart is not concurrent with new claims for the same BDF. The lock ordering (registry → capability table → device-host registry) is documented in `kernel/src/syscall/device_host.rs` module docs and in the learning doc (G.1).

### Resource bounds

- Per-driver capability slot count is bounded (initial cap: 32 MMIO windows, 128 DMA handles, 8 IRQ subscriptions). Exceeding the cap returns `DeviceHostError::CapacityExceeded`; it never panics and never degrades the kernel.
- Block and net IPC client queues carry a documented depth cap (default: 64 in-flight requests per driver). Full queue returns `RemoteDeviceError::Busy` (mapped to `EAGAIN` at the syscall boundary); callers retry per existing Phase 54 `vfs_server` pattern.
- Driver-process memory is bounded by process rlimits (Phase 46). The service manager enforces `memory_max` for each driver `.conf`; a driver exceeding it is restarted with the standard restart bound.

---

## Track A — Pure-Logic Foundation in `kernel-core`

### A.1 — `Capability::Device` variant and device-host ABI types

**Files:**
- `kernel-core/src/ipc/capability.rs`
- `kernel-core/src/device_host/mod.rs` (new)
- `kernel-core/src/device_host/types.rs` (new)

**Symbol:** `Capability::Device`, `Capability::Mmio`, `Capability::Dma`, `Capability::DeviceIrq`, `DeviceCapKey`, `MmioWindowDescriptor`, `DmaHandle`, `DeviceHostError`
**Why it matters:** The kernel-side syscall handlers (Track B) and the userspace `driver_runtime` (Track C) must agree on the capability shape and the descriptor types threaded through IPC. Declaring them in `kernel-core` lets both sides compile against the same types and lets the pure-logic invariants (BDF format, IOVA alignment, error classification) be host-tested without a running kernel.

**Acceptance:**
- [ ] Tests commit first (failing) and pass after implementation lands — evidence in `git log --follow kernel-core/src/device_host/types.rs`
- [ ] `Capability::Device { key: DeviceCapKey }` is added as a new variant to the existing `Capability` enum without breaking any Phase 50 capability shape; existing `Capability::Endpoint`, `Capability::Notification`, `Capability::Grant` variants are unchanged
- [ ] `Capability::Mmio { device: DeviceCapKey, bar_index: u8, len: usize }`, `Capability::Dma { device: DeviceCapKey, iova: u64, len: usize }`, and `Capability::DeviceIrq { device: DeviceCapKey, notif: NotifId }` are added as derived capabilities that carry back-references to the owning device capability
- [ ] `DeviceCapKey` is a `#[repr(C)]` struct of `(segment: u16, bus: u8, dev: u8, func: u8)`; `PartialEq`, `Eq`, `Hash` are derived; serialization is stable across boot
- [ ] `MmioWindowDescriptor { phys_base: u64, len: usize, bar_index: u8, prefetchable: bool, cache_mode: MmioCacheMode }`; unit test proves round-trip round-trip over IPC-payload bytes
- [ ] `DmaHandle { user_va: usize, iova: u64, len: usize }`; unit test proves `user_va` and `iova` are independently validated (one may be zero-valued in identity-map fallback contexts — documented)
- [ ] `DeviceHostError` is a named enum with variants `NotClaimed`, `AlreadyClaimed`, `InvalidBarIndex`, `BarOutOfBounds`, `IovaExhausted`, `IommuFault`, `CapacityExceeded`, `IrqUnavailable`, `BadDeviceCap`, `Internal`; variants are data, not strings
- [ ] `DRIVER_RESTART_TIMEOUT_MS: u32 = 1000` is declared once in `kernel-core::device_host::types` and consumed by `RemoteBlockDevice` (D.4), `RemoteNic` (E.4), the crash-restart regression (F.2), and every restart-bound acceptance item in Tracks D / E / F — no acceptance item is permitted to hand-roll a literal timeout
- [ ] No new external crate dependencies; `kernel-core` remains `no_std` + `alloc`

### A.2 — Block-driver IPC protocol schema

**File:** `kernel-core/src/driver_ipc/block.rs` (new)

**Symbol:** `BLK_READ`, `BLK_WRITE`, `BLK_STATUS`, `BlkRequestHeader`, `BlkReplyHeader`, `BlockDriverError`, `encode_blk_request`, `decode_blk_request`, `encode_blk_reply`, `decode_blk_reply`
**Why it matters:** `RemoteBlockDevice` (kernel, D.4) and the NVMe driver process (userspace, D.2–D.3) must speak exactly one protocol. Declaring the schema in `kernel-core` makes it host-testable and guarantees both sides compile against the same message layout; divergence becomes a compile error instead of a runtime corruption bug.

**Acceptance:**
- [ ] Tests commit first
- [ ] Message labels: `BLK_READ = 0x5501`, `BLK_WRITE = 0x5502`, `BLK_STATUS = 0x5503` (numbering reserved from the existing Phase 54 VFS protocol block so the ranges do not collide)
- [ ] `BlkRequestHeader { kind: u16, cmd_id: u64, lba: u64, sector_count: u32, flags: u32 }` plus a bulk-payload grant for write data
- [ ] `BlkReplyHeader { cmd_id: u64, status: BlockDriverError, bytes: u32 }`; reply carries the read data as a bulk-payload grant
- [ ] `BlockDriverError` has variants `Ok`, `IoError`, `InvalidLba`, `DeviceAbsent`, `Busy`, `DriverRestarting`, `InvalidRequest`
- [ ] Property test: arbitrary `BlkRequestHeader` and `BlkReplyHeader` survive `encode`→`decode` losslessly
- [ ] Property test: a truncated or malformed payload decodes to `Err(DecodeError)` without panic
- [ ] Explicit upper bounds declared: `MAX_SECTORS_PER_REQUEST = 256`; requests exceeding the bound are rejected at the `RemoteBlockDevice` facade, not at the driver

### A.3 — Net-driver IPC protocol schema

**File:** `kernel-core/src/driver_ipc/net.rs` (new)

**Symbol:** `NET_SEND_FRAME`, `NET_RX_FRAME`, `NET_LINK_STATE`, `NetFrameHeader`, `NetLinkEvent`, `NetDriverError`, `encode_net_send`, `decode_net_send`, `encode_net_rx_notify`, `decode_net_rx_notify`
**Why it matters:** The same reasoning as A.2 but for the network driver. `RemoteNic` forwards `send_frame`; received frames flow back to the kernel network stack through a notification plus a bulk-payload grant. Link-state changes propagate as a typed event so TCP retransmit logic can see link-down without polling.

**Acceptance:**
- [ ] Tests commit first
- [ ] Message labels: `NET_SEND_FRAME = 0x5511`, `NET_RX_FRAME = 0x5512`, `NET_LINK_STATE = 0x5513`
- [ ] `NetFrameHeader { kind: u16, frame_len: u16, flags: u32 }`; payload is the Ethernet frame via bulk grant
- [ ] `NetLinkEvent { up: bool, mac: [u8; 6], speed_mbps: u32 }`
- [ ] `NetDriverError` has variants `Ok`, `LinkDown`, `RingFull`, `DeviceAbsent`, `DriverRestarting`, `InvalidFrame`
- [ ] Property test: arbitrary `NetFrameHeader` and `NetLinkEvent` round-trip losslessly
- [ ] MTU handling: frames longer than `MAX_FRAME_BYTES = 1522` (Ethernet + VLAN tag) decode to `NetDriverError::InvalidFrame`

### A.4 — `driver_runtime` abstract contracts

**File:** `kernel-core/src/driver_runtime/contract.rs` (new)

**Symbol:** `DeviceHandleContract`, `MmioContract`, `DmaBufferContract`, `IrqNotificationContract`, `DriverRuntimeError`
**Why it matters:** The `driver_runtime` wrappers (Track C) must pass a contract suite that proves they are LSP-compliant against both a mock syscall backend (for host tests) and the real kernel syscalls (in QEMU). Declaring the contract shape in `kernel-core` is the DRY seam that lets the same suite run against both.

**Acceptance:**
- [ ] Tests commit first; a `MockBackend` impl ships in `kernel-core/tests/fixtures/driver_runtime_mock.rs` before the contracts merge
- [ ] `DeviceHandleContract` declares: `claim(DeviceCapKey) -> Result<Handle, DriverRuntimeError>`, `release(Handle) -> Result<(), _>`
- [ ] `MmioContract` declares: `map(handle: &Handle, bar: u8) -> Result<MmioWindow, _>`, `read_u32(&MmioWindow, offset) -> u32`, `write_u32(&MmioWindow, offset, value)`, and 8 / 16 / 64-bit variants
- [ ] `DmaBufferContract` declares: `allocate(handle: &Handle, size, align) -> Result<DmaBuffer, _>`, `user_va`, `iova`, `len` accessors; `Drop` releases the handle
- [ ] `IrqNotificationContract` declares: `subscribe(handle: &Handle, vector_hint: Option<u8>) -> Result<IrqNotif, _>`, `wait`, `ack`
- [ ] `DriverRuntimeError` has variants mirroring `DeviceHostError` plus `UserFaultOnMmio`, `DmaHandleExpired`, `IrqTimeout`
- [ ] Module docs cite the contract suite filename (`kernel-core/tests/driver_runtime_contract.rs`) as the authoritative behavioral spec for every impl

---

## Track B — Kernel Device-Host Syscalls

### B.1 — `sys_device_claim` and the `Capability::Device` issuance path

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/syscall/device_host.rs` (new)
- `kernel/src/pci/mod.rs`

**Symbol:** `sys_device_claim`, `DeviceHostRegistry`, `PciDeviceHandle::into_capability`
**Why it matters:** The first device-host syscall. Transfers a `PciDeviceHandle` (Phase 55 Track C) to the calling process as a `Capability::Device` entry in the process's Phase 50 capability table. Replaces the in-kernel `claim_pci_device` call site for ring-3 drivers and establishes the capability-gated pattern the other three syscalls will follow. A single bug here — wrong ownership transfer, missed capability validation, double-claim leak — cascades into every ring-3 driver.

**Acceptance:**
- [ ] Tests commit first: a `cargo xtask test` integration test with a stub driver process proves claim succeeds on first call and returns `-EBUSY` on a second claim of the same BDF
- [ ] Syscall number is reserved in the `0x11xx` device-host block (e.g., `0x1120 = SYS_DEVICE_CLAIM`) alongside the other three; numbers are defined once in `kernel-core::device_host::syscalls`
- [ ] Signature: `sys_device_claim(segment: u16, bus: u8, dev: u8, func: u8) -> isize` (negative errno on failure, non-negative `CapHandle` on success)
- [ ] Success path: allocates a `Capability::Device { key: DeviceCapKey }` in the caller's `CapabilityTable`, records the (PID, DeviceCapKey) pair in `DeviceHostRegistry`, logs `device_host.claim`
- [ ] Failure modes with typed errors: `-EBUSY` (already claimed by another process), `-ENODEV` (BDF not present), `-EACCES` (caller lacks the policy right to claim devices; Phase 48 credential gate)
- [ ] When the owning process exits or is killed, its `Capability::Device` entries are released and the device is eligible for re-claim; the Phase 46 / Phase 51 supervisor's restart of the driver exercises this path
- [ ] Double-drop is safe: releasing a `Capability::Device` twice returns `-EBADF`, not panic
- [ ] Race: concurrent `sys_device_claim` for the same BDF from two processes — exactly one succeeds; regression test asserts this with two threads

### B.2 — `sys_device_mmio_map` with bounds-checked user mapping

**Files:**
- `kernel/src/syscall/device_host.rs`
- `kernel/src/mm/mod.rs`
- `kernel/src/pci/bar.rs`

**Symbol:** `sys_device_mmio_map`, `BarMapping::map_to_user`, `Capability::Mmio`
**Why it matters:** Maps a claimed device's BAR into the driver process's address space as read-write. Bounds-checking the map request against the actual BAR size (from the PCI BAR-sizing algorithm, Phase 55 B.2) prevents a driver from requesting (and obtaining) a window that straddles into an adjacent device's registers — the exact class of bug IOMMU cannot catch because it operates on DMA, not on MMIO.

**Acceptance:**
- [ ] Tests commit first: integration test proves a request beyond the BAR's actual size returns `-EINVAL` and does not install a mapping
- [ ] Signature: `sys_device_mmio_map(dev_cap: CapHandle, bar_index: u8) -> isize` returning the user VA on success, negative errno on failure
- [ ] Capability validation: `dev_cap` is resolved via `CapabilityTable::lookup`; a non-`Capability::Device` handle returns `-EBADF`; a capability not owned by the caller's PID returns `-EPERM`
- [ ] BAR resolution: `bar_index` is looked up in the `PciDeviceHandle`'s BAR table; an out-of-range index returns `-EINVAL`; a zero-size BAR returns `-ENODEV`
- [ ] Mapping: the BAR's physical region is mapped into the caller's address space with `UC` (uncacheable) for MMIO or `WC` for prefetchable BARs; mapping respects the BAR's actual size and is rejected if the caller's AS lacks free VA
- [ ] On success, a `Capability::Mmio { device, bar_index, len }` is installed in the caller's cap table pointing at the mapping; dropping the device cap implicitly drops the Mmio cap (cleanup cascade)
- [ ] Negative test: a driver holding `Capability::Device` for BDF `A` cannot request `sys_device_mmio_map` against a cap it fabricated for BDF `B`; the fabricated handle resolves to `-EBADF` (asserted by F.3)
- [ ] On process exit: all `Capability::Mmio` mappings are torn down and the BAR's kernel-side reservation is released

### B.3 — `sys_device_dma_alloc` through Phase 55a IOMMU domains

**Files:**
- `kernel/src/syscall/device_host.rs`
- `kernel/src/mm/dma.rs`
- `kernel/src/iommu/mod.rs`

**Symbol:** `sys_device_dma_alloc`, `sys_device_dma_handle_info`, `DmaBuffer::allocate_for_user`, `Capability::Dma`
**Why it matters:** The sharpest privilege line in the phase. IOMMU-gated DMA means a ring-3 driver programs its own device's DMA engines without being able to scribble over unrelated kernel memory or another device's DMA. The kernel must (a) allocate physical frames, (b) install an IOVA mapping through the device's Phase 55a `DmaDomain`, (c) map the same frames read-write into the driver's user AS, and (d) return both addresses atomically so the driver cannot race between MMIO programming and the mapping landing. Wrong ordering here produces IOMMU faults under race, and — worse — in the identity-map fallback path it produces silent cross-device corruption.

**Acceptance:**
- [ ] Tests commit first: integration test proves the returned user VA is readable/writable by the driver, the IOVA is the one installed in the device's IOMMU domain, and both views read the same byte for an arbitrary written byte
- [ ] Signature: `sys_device_dma_alloc(dev_cap: CapHandle, size: usize, align: usize) -> isize` returning a non-negative `Capability::Dma` `CapHandle` on success
- [ ] Capability validation: `dev_cap` resolved as in B.2; a non-`Capability::Device` handle returns `-EBADF`
- [ ] Allocation order: reserve IOVA first, then map physical frames (via Phase 53a buddy allocator), then install IOMMU mapping (via Phase 55a `IommuUnit::map`), then install user-side page-table mapping. Failures at each step roll back cleanly — no partial state leaks
- [ ] On success, `Capability::Dma { device, iova, len }` is installed and `sys_device_dma_alloc` returns the non-negative `CapHandle` index
- [ ] `sys_device_dma_handle_info(dma_cap: CapHandle, out: *mut DmaHandle) -> isize` is a read-only sibling syscall that copies `DmaHandle { user_va, iova, len }` into a caller-provided buffer; reserved as `0x1123 = SYS_DEVICE_DMA_HANDLE_INFO` in the `kernel-core::device_host::syscalls` block alongside the other four primitives, capability-validated identically to `sys_device_mmio_map`, and returns `-EBADF` for a non-`Capability::Dma` handle. The separate accessor avoids truncating the triple through an `isize` return channel
- [ ] In identity-map fallback (Phase 55a E.3): `iova == phys_addr`; the behavior matches the IOMMU-active path for the driver (same accessor returns the same tuple shape), and is logged on first allocation per domain as a `device_host.dma_alloc.identity` event
- [ ] Cross-device negative: a driver holding `Capability::Device` for BDF `A` cannot obtain a DMA handle whose IOVA is valid against BDF `B`'s domain; the IOMMU unit rejects the request (asserted by F.3)
- [ ] On process exit: all `Capability::Dma` entries trigger `DmaBuffer::drop` which unmaps the IOVA, flushes the IOMMU TLB, frees the user mapping, and returns frames to the buddy allocator

### B.4 — `sys_device_irq_subscribe` and notification bridging

**Files:**
- `kernel/src/syscall/device_host.rs`
- `kernel/src/ipc/notification.rs`
- `kernel/src/pci/mod.rs`
- `kernel/src/arch/x86_64/interrupts.rs`

**Symbol:** `sys_device_irq_subscribe`, `DeviceIrq::bind_notification`, `Capability::DeviceIrq`, `irq_notification_signal`
**Why it matters:** Interrupt delivery is the last kernel-retained mechanism a ring-3 driver needs. The kernel allocates the MSI / MSI-X vector (or installs a legacy INTx handler), attaches a `Notification` (Phase 50), and the driver process blocks on `notification_wait`. The ISR does the minimum (ack hardware, signal notification bit, send EOI); the driver drains the ring in task context. Getting this wrong produces lost interrupts (driver sleeps forever) or spurious wakes (driver spins hot).

**Acceptance:**
- [ ] Tests commit first: integration test proves a driver process blocks in `notification_wait`, the test harness triggers a synthetic device IRQ, the process wakes with the signaled bit set, and acks cleanly
- [ ] Signature: `sys_device_irq_subscribe(dev_cap: CapHandle, vector_hint: u32, notification_index: u32) -> isize` where `notification_index` is the bit index in the caller's notification word the IRQ should set
- [ ] Capability validation as in B.2 and B.3
- [ ] Vector allocation: MSI-preferred, MSI-X if the device advertises capability, INTx as last resort. Reuses the Phase 55 `allocate_msi_vectors` and `install_intx_irq` paths from `PciDeviceHandle`
- [ ] On success, `Capability::DeviceIrq { device, notif }` is installed; the `notif` points at the caller's existing `Notification` object (or a freshly allocated one if the caller passes `SENTINEL_NEW`)
- [ ] ISR path: the kernel ISR is a thin shim that calls `notification_signal` atomically, acks the device IRQ bit, sends EOI; no allocation, no locking, no IPC from within the ISR (Phase 6 invariants preserved)
- [ ] IRQ masking / unmasking: a userspace `sys_device_irq_ack` (or the act of clearing the notification bit) unmasks the vector so the next interrupt can fire; design avoids the lost-interrupt window by masking before signal and unmasking only on ack
- [ ] On process exit: the vector is released, the MSI capability on the device is disabled, and the notification is unbound

---

## Track C — `userspace/lib/driver_runtime`

### C.1 — Crate scaffolding, workspace integration, build pipeline

**Files:**
- `userspace/lib/driver_runtime/Cargo.toml` (new)
- `userspace/lib/driver_runtime/src/lib.rs` (new)
- `Cargo.toml`
- `xtask/src/main.rs`

**Symbol:** `driver_runtime` crate, workspace member, xtask `bins` entry as a library (no standalone binary yet)
**Why it matters:** The shared library has no binary of its own — it is the dependency `userspace/drivers/nvme` and `userspace/drivers/e1000` pull in. Missing the workspace member or `xtask` dependency wiring is the first class of failure that would prevent either driver from compiling, so landing the crate shell before any driver port removes that risk.

**Acceptance:**
- [ ] Tests commit first: a `cargo test -p driver_runtime` stub that imports the crate and proves the host-side mock backend compiles
- [ ] `Cargo.toml` workspace `members` includes `userspace/lib/driver_runtime`
- [ ] Crate is `#![no_std]` with `alloc` feature; depends on `syscall-lib`, `kernel-core` (for ABI types from Track A), and `spin`
- [ ] Crate builds under the same target triple the drivers use; no standalone xtask `bins` entry because it is a library
- [ ] Public API surface is re-exported from `lib.rs` exactly as the drivers will consume: `pub mod device;`, `pub mod mmio;`, `pub mod dma;`, `pub mod irq;`, `pub mod ipc;`
- [ ] Crate-level module docs name this surface as the template for future hardware-owning userspace services (Phase 56-or-later USB HID, GPU / display-engine drivers, VirtIO-blk / VirtIO-net extraction); stability is documented — post-55b additions extend the API, they do not reshape it
- [ ] CI `cargo xtask check` passes after the crate lands

### C.2 — `DeviceHandle`, `Mmio<T>`, `DmaBuffer<T>` safe wrappers

**Files:**
- `userspace/lib/driver_runtime/src/device.rs` (new)
- `userspace/lib/driver_runtime/src/mmio.rs` (new)
- `userspace/lib/driver_runtime/src/dma.rs` (new)

**Symbol:** `DeviceHandle`, `Mmio<T>`, `DmaBuffer<T>`
**Why it matters:** These are the three HAL-shaped wrappers the driver ports will consume. Their shape must mirror the Phase 55 kernel-side HAL (`BarMapping`, `MmioRegion`, `DmaBuffer`) so the register-handling code in `nvme.rs` and `e1000.rs` ports with minimal change — a ~1:1 substitution, not a rewrite. Getting the shape wrong here forces either an abstract-over-everything generic that the drivers cannot use, or two divergent copies.

**Acceptance:**
- [ ] Tests commit first against the `MockBackend` from A.4
- [ ] `DeviceHandle` wraps `Capability::Device` with RAII drop (calls `sys_device_release` on drop); `DeviceHandle::claim(bdf) -> Result<Self, DriverRuntimeError>`
- [ ] `Mmio<T>` wraps `Capability::Mmio` and exposes `read_reg::<U: Copy>(offset) -> U`, `write_reg::<U: Copy>(offset, value)`, with volatile semantics; signature is identical to Phase 55's `MmioRegion<T>` so the driver port is a rename
- [ ] `DmaBuffer<T>` wraps `Capability::Dma` plus the `DmaHandle`; `user_ptr() -> *mut T`, `iova() -> u64`, `len() -> usize`; `Deref<Target = T>` and `DerefMut` so drivers access ring memory as they do today; `Drop` unmaps
- [ ] Every wrapper's public API appears in the contract suite (`kernel-core/tests/driver_runtime_contract.rs`) and passes against `MockBackend`
- [ ] One integration test proves the same contract suite passes against the real syscall backend inside QEMU
- [ ] Null-pointer safety: `Mmio::new` and `DmaBuffer::new` refuse to construct from zero or unaligned addresses

### C.3 — IRQ notification wait loop helper

**File:** `userspace/lib/driver_runtime/src/irq.rs` (new)

**Symbol:** `IrqNotification`, `IrqNotification::wait`, `IrqNotification::ack`, `irq_loop`
**Why it matters:** Every driver process has the same skeleton: `loop { irq.wait(); drain_ring(); irq.ack(); }`. Factoring the wait-ack rhythm into `driver_runtime` eliminates copy-pasted wakeup logic between NVMe and e1000 and isolates any future doorbell-batching optimization to one place.

**Acceptance:**
- [ ] Tests commit first: contract test with `MockBackend` proves `wait` blocks until `signal` is called and returns the signaled bit mask
- [ ] `IrqNotification::subscribe(device: &DeviceHandle, vector_hint: Option<u8>) -> Result<Self, DriverRuntimeError>`
- [ ] `wait(&self) -> u64` blocks via `notification_wait` and returns the pending bits; documented semantics match Phase 50
- [ ] `ack(&self, bits: u64) -> Result<(), _>` clears the bits and unmasks the vector via `sys_device_irq_ack`
- [ ] `irq_loop(notif: &IrqNotification, mut f: impl FnMut())` is a convenience wrapper for drivers that do the same thing on every interrupt
- [ ] One negative test: `ack` with bits the caller did not observe returns `DriverRuntimeError::InvalidAck`, not a panic

### C.4 — Block and net IPC client helpers

**Files:**
- `userspace/lib/driver_runtime/src/ipc/block.rs` (new)
- `userspace/lib/driver_runtime/src/ipc/net.rs` (new)

**Symbol:** `BlockServer::new`, `BlockServer::handle_next`, `NetServer::new`, `NetServer::handle_next`
**Why it matters:** Drivers must speak the A.2 / A.3 schemas, but the serialization / reply-length / bulk-grant bookkeeping is identical across drivers. Factoring it into `driver_runtime` keeps `userspace/drivers/nvme/` and `userspace/drivers/e1000/` focused on device semantics. Both drivers see the same `handle_next` signature: "pull one request, dispatch to a user-supplied closure, reply with the result."

**Acceptance:**
- [ ] Tests commit first: mock-backed `handle_next` proves the closure receives exactly the decoded request shape and the reply serializes back across `MockBackend`
- [ ] `BlockServer::new(endpoint: EndpointCap) -> Self`
- [ ] `BlockServer::handle_next<F>(&self, f: F) -> Result<(), DriverRuntimeError>` where `F: FnMut(BlkRequest) -> BlkReply`
- [ ] `NetServer::new(endpoint: EndpointCap) -> Self`; `NetServer::handle_next<F>` equivalent; `NetServer::publish_rx_frame(&self, frame: &[u8]) -> Result<(), _>` for the driver-pushes-RX-to-kernel direction
- [ ] `NetServer::publish_link_state(&self, state: NetLinkEvent)` for link-up / link-down events
- [ ] No panic on malformed request; malformed decodes into `Err(DecodeError)` and a reply with `BlockDriverError::InvalidRequest` or `NetDriverError::InvalidFrame` is emitted

---

## Track D — NVMe Driver Extraction

### D.1 — `userspace/drivers/nvme/` crate scaffolding

**Files:**
- `userspace/drivers/nvme/Cargo.toml` (new)
- `userspace/drivers/nvme/src/main.rs` (new; initial shell)
- `Cargo.toml`
- `xtask/src/main.rs`
- `kernel/src/fs/ramdisk.rs`

**Symbol:** `nvme_driver` crate, `main`, `BIN_ENTRIES` ramdisk entry, xtask `bins` entry
**Why it matters:** Adding a userspace binary requires four coordinated changes (workspace member, xtask build pipeline, ramdisk embedding, optional service config) — AGENTS.md names them explicitly. Landing the crate shell first and proving it boots (even as a stub that exits `0`) removes every "why does the process not spawn" failure mode before any real driver logic lands.

**Acceptance:**
- [ ] Tests commit first: `cargo xtask run` boots the stub driver and the boot log records `nvme_driver: spawned` before exit
- [ ] Workspace `members` includes `userspace/drivers/nvme`
- [ ] xtask `bins` array gains `{ name: "nvme_driver", needs_alloc: true }`
- [ ] Ramdisk `BIN_ENTRIES` includes `("/drivers/nvme", include_bytes!("..."))`
- [ ] Crate is `#![no_std]` `#![no_main]`, uses `syscall_lib::entry_point!(program_main)`, `BrkAllocator` global
- [ ] Depends on `driver_runtime`, `syscall-lib` (with `alloc` feature), `kernel-core`
- [ ] Stub `program_main` claims a sentinel BDF if present, logs, and exits; real driver logic lands in D.2

### D.2 — Port NVMe controller bring-up to `driver_runtime`

**File:** `userspace/drivers/nvme/src/init.rs` (new)

**Symbol:** `nvme_probe_userspace`, `NvmeController` (userspace port)
**Why it matters:** This is the extraction's core. Every register access that uses `crate::pci::bar::MmioRegion` today must become `driver_runtime::Mmio`; every `crate::mm::dma::DmaBuffer` must become `driver_runtime::DmaBuffer`. Because the wrapper shapes match (C.2), this is a rename + import shuffle, not a re-architecture. The `kernel_core::nvme` register / command / completion types stay — they were always pure data.

**Acceptance:**
- [ ] Tests commit first: a userspace-side smoke proves controller bring-up from `CC.EN=0` → Identify succeeds against `-device nvme` in QEMU, with the driver running in ring 3
- [ ] Reset sequence is byte-for-byte equivalent to Phase 55 D.1: `CC.EN=0`, wait `CSTS.RDY=0` with bounded `CAP.TO` timeout, program `AQA`/`ASQ`/`ACQ`, set `CC.EN=1`, wait `CSTS.RDY=1`
- [ ] Admin queue: allocates SQ and CQ via `DmaBuffer<[NvmeCommand]>` / `DmaBuffer<[NvmeCompletion]>`; queue layouts come from `kernel_core::nvme` (no duplication)
- [ ] Identify Controller + Identify Namespace commands succeed; returned model / serial / namespace size is logged
- [ ] Failure modes (reset timeout, admin timeout) return `BlockDriverError::IoError` to clients via the IPC protocol rather than panicking
- [ ] The driver process stays alive after bring-up, blocked in `BlockServer::handle_next`

### D.3 — Port I/O queue pair, IRQ handling, and block I/O path

**File:** `userspace/drivers/nvme/src/io.rs` (new)

**Symbol:** `IoQueuePair`, `handle_read`, `handle_write`, `drain_completions`
**Why it matters:** The block I/O hot path. Reuses the Phase 55 D.3 PRP construction, Create I/O CQ / SQ sequences, and MSI-X interrupt wiring — all of which become `driver_runtime`-mediated. Completion draining runs in task context woken by `IrqNotification::wait` (replacing the Phase 55 D.4 ISR-level `wake_task`).

**Acceptance:**
- [ ] Tests commit first: userspace-side smoke writes a sentinel to LBA 0, reads it back via `sys_block_read` (now served by `RemoteBlockDevice` → IPC → this driver), and compares
- [ ] Create I/O CQ / Create I/O SQ admin commands run before accepting I/O; MSI-X vector allocated via `driver_runtime::IrqNotification::subscribe`
- [ ] `handle_read(lba, sector_count)` builds a Read command, programs PRP1 / PRP2, rings the SQ doorbell, waits for completion
- [ ] `handle_write(lba, sector_count, data)` symmetric
- [ ] PRP-list overflow page allocation uses `driver_runtime::DmaBuffer`; frees on reply
- [ ] Completion drain walks the phase bit, writes the CQ-head doorbell, and replies to the IPC caller with `BLK_STATUS`
- [ ] In-flight-at-crash semantics: if the driver process is killed mid-I/O, the client receives `BlockDriverError::DriverRestarting` within `DRIVER_RESTART_TIMEOUT_MS` (A.1); subsequent requests succeed after restart completes

### D.4 — `RemoteBlockDevice` kernel facade

**Files:**
- `kernel/src/blk/remote.rs` (new)
- `kernel/src/blk/mod.rs`

**Symbol:** `RemoteBlockDevice`, `RemoteBlockDevice::register`, `blk::read_sectors`, `blk::write_sectors`
**Why it matters:** `kernel/src/blk/mod.rs` is the VFS / FAT / ext2 layer's point of contact. `RemoteBlockDevice` is substitutable (LSP) for the Phase 55 in-kernel NVMe driver: same `read_sectors` / `write_sectors` shape, same error values; difference is that the call now forwards to an IPC endpoint instead of walking NVMe queues inline. This is the change that makes the 2000-line kernel LOC drop visible.

**Acceptance:**
- [ ] Tests commit first: kernel-side integration test proves `blk::read_sectors` returns the same bytes whether served by VirtIO-blk (in-kernel) or by `RemoteBlockDevice` (IPC to the ring-3 NVMe driver)
- [ ] `RemoteBlockDevice::register(endpoint: EndpointId, device_name: &str)` installs a forwarding entry in the block-layer dispatch
- [ ] `read_sectors` / `write_sectors` dispatch order: NVMe-via-RemoteBlockDevice (if registered), VirtIO-blk (kernel) otherwise; matches the Phase 55 priority with the in-kernel NVMe driver removed
- [ ] A request while the driver is mid-restart blocks for up to `DRIVER_RESTART_TIMEOUT_MS` (A.1), which the facade reads at construction time and may be overridden per-driver via the service `.conf`; on timeout returns `EIO`
- [ ] Bulk-payload grants for write data are single-use (Phase 50 contract); test proves a grant cannot be replayed across requests
- [ ] Facade is ~100 lines including module docs; no register logic, no PRP handling — all of that lives in `userspace/drivers/nvme/`

### D.5 — Delete `kernel/src/blk/nvme.rs` and audit residue

**Files:**
- `kernel/src/blk/nvme.rs` (deleted)
- `kernel/src/blk/mod.rs`
- `kernel-core/src/nvme.rs` (audit — keep if `userspace/drivers/nvme` consumes it, delete if not)

**Symbol:** n/a (deletion + audit)
**Why it matters:** The phase's headline outcome — "the kernel no longer contains NVMe driver logic" — is a delete, not an add. Leaving `nvme.rs` on disk with an `#[allow(dead_code)]` attribute would miss the Phase 55b Acceptance Criterion and make the LOC drop invisible.

**Acceptance:**
- [ ] `kernel/src/blk/nvme.rs` is removed from the working tree and from `mod.rs`
- [ ] `kernel_core::nvme` is retained because `userspace/drivers/nvme/` consumes it; a comment in its module docs records the new consumer so future audits do not mistakenly delete it
- [ ] Grep of `kernel/src/` for `nvme`, `NvmeController`, `NvmeCommand`, etc. returns only the `RemoteBlockDevice` integration point in `kernel/src/blk/mod.rs` (documented cross-reference)
- [ ] Kernel compiles and all existing tests pass after the delete; `cargo xtask check` is green

---

## Track E — e1000 Driver Extraction

### E.1 — `userspace/drivers/e1000/` crate scaffolding

**Files:**
- `userspace/drivers/e1000/Cargo.toml` (new)
- `userspace/drivers/e1000/src/main.rs` (new)
- `Cargo.toml`
- `xtask/src/main.rs`
- `kernel/src/fs/ramdisk.rs`

**Symbol:** `e1000_driver` crate, `main`, `BIN_ENTRIES` ramdisk entry, xtask `bins` entry
**Why it matters:** Same four-place coordination as D.1. Landing the shell first removes spawn-failure modes before the port begins.

**Acceptance:**
- [ ] Tests commit first: `cargo xtask run` boots the stub driver; boot log records `e1000_driver: spawned`
- [ ] Workspace `members` includes `userspace/drivers/e1000`
- [ ] xtask `bins` array gains `{ name: "e1000_driver", needs_alloc: true }`
- [ ] Ramdisk `BIN_ENTRIES` includes `("/drivers/e1000", include_bytes!("..."))`
- [ ] Crate shape mirrors D.1 (`#![no_std]`, `#![no_main]`, `BrkAllocator`, `entry_point!`)

### E.2 — Port e1000 device init + descriptor rings to `driver_runtime`

**Files:**
- `userspace/drivers/e1000/src/init.rs` (new)
- `userspace/drivers/e1000/src/rings.rs` (new)

**Symbol:** `E1000Device` (userspace port), `TxDescRing`, `RxDescRing`
**Why it matters:** Same mechanical port as D.2 with e1000 register semantics. `kernel_core::e1000` register / descriptor types are consumed by the userspace driver unchanged; `driver_runtime::Mmio` replaces `kernel/src/pci/bar::MmioRegion`; `driver_runtime::DmaBuffer` replaces `kernel/src/mm/dma::DmaBuffer`.

**Acceptance:**
- [ ] Tests commit first: userspace-side smoke boots the driver, issues global reset, reads MAC from `RAL0`/`RAH0`, logs the address
- [ ] `CTRL.RST`-based reset with bounded spin-limit matches Phase 55 E.1
- [ ] TX and RX rings allocated as `DmaBuffer<[E1000TxDesc; TX_RING_SIZE]>` / `DmaBuffer<[E1000RxDesc; RX_RING_SIZE]>`
- [ ] Per-slot packet buffers allocated as `DmaBuffer<[u8; RX_BUF_SIZE]>` / `DmaBuffer<[u8; TX_BUF_SIZE]>`
- [ ] `RDBAL`/`RDBAH`/`RDLEN` and `TDBAL`/`TDBAH`/`TDLEN` programmed with IOVA (not user VA), per the Phase 55a contract; `IOVA == PhysAddr` in identity-map fallback
- [ ] RX pre-post leaves `RDT` one short of head, matching Intel §13 guidance
- [ ] `RCTL` / `TCTL` configured for 2048-byte buffers, broadcast accept, collision threshold

### E.3 — Port e1000 RX / TX path and link-state handling

**File:** `userspace/drivers/e1000/src/io.rs` (new)

**Symbol:** `handle_irq`, `drain_rx`, `handle_tx`, `link_state_atomic`
**Why it matters:** Completes the port. The ISR-level logic from Phase 55 E.3 becomes task-context logic in the driver process, triggered by `IrqNotification::wait`. Link-down handling — the non-happy path — is explicitly tested here because ring-3 extraction makes the blast radius narrower: a link-down bug no longer stalls the kernel.

**Acceptance:**
- [ ] Tests commit first: userspace-side smoke sends an ICMP echo from the OS through the ring-3 driver and receives the reply
- [ ] IRQ subscription via `IrqNotification::subscribe`; MSI preferred, INTx fallback
- [ ] On IRQ wake: `ICR` read, link-state (`LSC` bit) updated in an `AtomicBool`, RX ring drained from `RDH` / `RDT`
- [ ] RX frames published to the kernel via `NetServer::publish_rx_frame`
- [ ] `handle_tx(frame)`: copy into the next TX descriptor's `DmaBuffer`, set `EOP|IFCS|RS` in `cmd`, advance `TDT`
- [ ] Link-down path: `handle_tx` returns `NetDriverError::LinkDown` while the link atomic is clear; link-up wrap-around drains in-flight TX before re-enabling (same semantic as Phase 55 E.4)
- [ ] On driver-process kill: next send attempt returns `NetDriverError::DriverRestarting`; subsequent sends succeed after restart within `DRIVER_RESTART_TIMEOUT_MS` (A.1)

### E.4 — `RemoteNic` kernel facade

**Files:**
- `kernel/src/net/remote.rs` (new)
- `kernel/src/net/mod.rs`

**Symbol:** `RemoteNic`, `RemoteNic::register`, `net::send_frame`, inbound RX dispatch
**Why it matters:** Same substitution as D.4, for networking. `net::send_frame` dispatches to `RemoteNic` when a ring-3 driver is registered; RX frames arrive via IPC and route into `kernel/src/net/dispatch.rs` — the same entry point VirtIO-net uses. The TCP and UDP layers (Phase 54 `net_server`) are unaffected.

**Acceptance:**
- [ ] Tests commit first: kernel-side integration test proves ICMP echo works whether served by VirtIO-net (kernel) or by `RemoteNic` (IPC to the ring-3 e1000 driver)
- [ ] `RemoteNic::register(endpoint: EndpointId, mac: MacAddr)` installs a forwarding entry in the network-layer dispatch
- [ ] `net::send_frame` dispatches: `RemoteNic` first if registered, VirtIO-net otherwise (matches Phase 55 priority with in-kernel e1000 removed)
- [ ] RX frames arrive as `NET_RX_FRAME` IPC messages; a notification + bulk-payload grant delivers the frame to `kernel/src/net/dispatch.rs::process_rx_frames`
- [ ] Link-state transitions propagate into the net subsystem: link-down causes pending TCP retransmit timers to reset per existing Phase 16 behavior
- [ ] Facade is ~150 lines including RX dispatch; no register logic

### E.5 — Delete `kernel/src/net/e1000.rs` and audit residue

**Files:**
- `kernel/src/net/e1000.rs` (deleted)
- `kernel/src/net/mod.rs`
- `kernel-core/src/e1000.rs` (kept — consumed by `userspace/drivers/e1000`)

**Symbol:** n/a (deletion + audit)
**Why it matters:** Symmetric to D.5. Makes the extraction visible in the LOC count.

**Acceptance:**
- [ ] `kernel/src/net/e1000.rs` is removed
- [ ] `kernel_core::e1000` is retained; module-docs comment records the new consumer
- [ ] Grep of `kernel/src/` for `e1000`, `E1000Device`, etc. returns only the `RemoteNic` integration point in `kernel/src/net/mod.rs`
- [ ] Kernel compiles and all existing tests pass; `cargo xtask check` is green

---

## Track F — Supervision and Validation

### F.1 — Service-manager registration for both driver processes

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files`)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`)
- `etc/services.d/nvme_driver.conf` (new, embedded)
- `etc/services.d/e1000_driver.conf` (new, embedded)

**Symbol:** service `.conf` files, `init` degraded-mode rules, `note_extracted_service_degradation`
**Why it matters:** Driver processes participate in the Phase 46 / Phase 51 service model like any other service. Explicit `.conf` files — with chosen `restart` policy, `max_restart`, `memory_max`, and `depends` ordering — make the supervision contract auditable. Skipping this step would leave the drivers running via ad-hoc spawn and would miss the "driver-process crash is restartable by the supervisor" acceptance.

**Acceptance:**
- [ ] Tests commit first: `cargo xtask run` boots with both drivers registered; `service status nvme_driver` and `service status e1000_driver` both report `running`
- [ ] `nvme_driver.conf`: `name=nvme_driver`, `command=/drivers/nvme`, `type=daemon`, `restart=on-failure`, `max_restart=5`. No `depends=` line — the IOMMU substrate (Phase 55a) is kernel-internal init, not a supervised service, and the Phase 46 / Phase 51 service graph has no ordering dependency the driver must express
- [ ] `e1000_driver.conf`: same shape with `name=e1000_driver`, `command=/drivers/e1000`
- [ ] Both configs are embedded in the ext2 data disk via `populate_ext2_files`; both names appear in `init`'s `KNOWN_CONFIGS` fallback
- [ ] `init` logs a structured `driver.registered` event per driver at boot
- [ ] `cargo xtask clean` is documented as required after the first addition (existing AGENTS.md rule applies)

### F.2 — Crash-and-restart regression test

**Files:**
- `xtask/src/main.rs`
- `kernel/tests/driver_restart.rs` (new)
- `userspace/drivers/nvme/tests/restart_smoke.rs` (new)

**Symbol:** `driver_restart_regression`, `cargo xtask regression --test driver-restart`
**Why it matters:** The Acceptance Criterion "a driver-process crash during active I/O is restarted by the supervisor; subsequent I/O succeeds within a documented bound" is only a claim until a regression proves it. Fault injection is the proof.

**Acceptance:**
- [ ] Test spawns the NVMe driver, issues a write, kills the driver mid-write with `SIGKILL`, and asserts:
  - The outstanding write returns `BlockDriverError::DriverRestarting` within `DRIVER_RESTART_TIMEOUT_MS` (A.1)
  - The service manager logs `driver.restart` and `driver.restarted` events
  - A subsequent write to the same LBA succeeds
  - The written value matches the bytes the client provided (no partial-write corruption observable to the client)
- [ ] An analogous test for e1000: kill mid-send, assert `NetDriverError::DriverRestarting`, wait for restart, assert ICMP echo succeeds
- [ ] Restart bound is configurable via the `.conf`; default `5` in 30 s, test asserts `max_restart` enforcement by crashing the driver 6 times in a loop and observing `service status` returning `failed`
- [ ] Added to `cargo xtask regression --test driver-restart`

### F.3 — Cross-device negative test

**Files:**
- `kernel/tests/device_host_isolation.rs` (new)
- `userspace/drivers/nvme/tests/isolation.rs` (new)

**Symbol:** `cross_device_mmio_denied`, `cross_device_dma_denied`, `capability_forge_denied`
**Why it matters:** The Acceptance Criterion "a driver process cannot access a BAR or DMA region belonging to a device it did not claim" is the hardest-to-assert property of ring-3 extraction. The test must prove it by trying to do exactly the forbidden thing and asserting the kernel refuses.

**Acceptance:**
- [ ] Test 1: NVMe driver tries to `sys_device_mmio_map` a BAR on the e1000 BDF using a cap it holds for NVMe — returns `-EBADF`; the mapping is not installed (asserted by reading process VA layout before and after)
- [ ] Test 2: NVMe driver tries to `sys_device_dma_alloc` an IOVA that it attempts to validate against the e1000's IOMMU domain — the IOMMU unit rejects; the driver's DMA is allocated in its own domain, not e1000's
- [ ] Test 3: Driver fabricates a `CapHandle` value it never received and passes it to any device-host syscall — returns `-EBADF`, no side effect
- [ ] Test 4: Post-crash, a driver's pre-crash `CapHandle` values are invalid in the restarted process — tests by recording a handle value pre-crash, killing the driver, and asserting the restarted process gets fresh handles
- [ ] Runs in the IOMMU-active configuration (Phase 55a F.1 `--iommu` flag); skipped with a logged reason in identity-fallback (the cross-device DMA assertion is strictly weaker there and is documented as such)

### F.4 — Phase 55 data-path smokes through ring-3 drivers

**File:** `xtask/src/main.rs`

**Symbol:** `cargo xtask run --device nvme`, `cargo xtask run --device e1000`, NVMe / e1000 smoke test integration
**Why it matters:** Acceptance Criterion "`cargo xtask run --device nvme` and `cargo xtask run --device e1000` still pass their Phase 55 data-path and link-state smoke checks, now routed through ring-3 drivers". The same commands, the same assertions — the architecture change should be invisible at the command line.

**Acceptance:**
- [ ] `cargo xtask run --device nvme` boots; the 512 B round-trip at LBA 0 succeeds; the boot log contains `driver.registered: nvme_driver`
- [ ] `cargo xtask run --device e1000` boots; ICMP echo to the guest succeeds; TCP to the guest succeeds (telnet / ssh); the boot log contains `driver.registered: e1000_driver`
- [ ] Both tests run in the IOMMU-active configuration as well (Phase 55a F.1 `--iommu` flag) and pass there too
- [ ] Default `cargo xtask run` (VirtIO-only) continues to pass with no new driver processes spawned

### F.5 — Kernel LOC audit

**File:** `docs/roadmap/55b-ring-3-driver-host.md` (Acceptance Criteria)

**Symbol:** n/a (measurement + evidence in commit message)
**Why it matters:** The "Kernel line count drops by at least the combined NVMe + e1000 driver size" Acceptance Criterion is the headline metric of the phase. Recording the before-and-after numbers in the closing commit message makes the claim falsifiable.

**Acceptance:**
- [ ] Before-count of `kernel/src/blk/nvme.rs` + `kernel/src/net/e1000.rs` lines (measured at Phase 55 close) is recorded in the phase-close commit message (expected: approximately 2115 lines based on current measurement)
- [ ] After-count of the new facade files (`kernel/src/blk/remote.rs` + `kernel/src/net/remote.rs`) is recorded (target: < 300 combined)
- [ ] Net kernel LOC change is ≤ −1800 (at least 1800 lines removed from ring 0)
- [ ] Git history for the deletion commit shows both files deleted in a single change set for auditability
- [ ] Script `scripts/count_kernel_lines.sh` is added to the existing `scripts/` directory (alongside `gen-secure-boot-keys.sh`) and produces the same LOC numbers so the metric is reproducible

---

## Track G — Documentation and Version

### G.1 — Phase 55b learning doc

**File:** `docs/55b-ring-3-driver-host.md` (new)

**Symbol:** N/A (documentation)
**Why it matters:** Pairs with the roadmap doc to give a learner the conceptual frame: why a microkernel places drivers in ring 3, how capability-gated device-host syscalls deliver a safe subset of the hardware interface to unprivileged processes, how the Phase 55a IOMMU substrate makes ring-3 DMA safe, and how the Phase 46 / Phase 51 service manager supervises driver processes. The aligned-learning-doc template in `docs/appendix/doc-templates.md` (lines 167–214) is the authoritative shape.

**Acceptance:**
- [ ] File follows the aligned-learning-doc template (`docs/appendix/doc-templates.md` lines 167–214) with frontmatter: `Aligned Roadmap Phase: Phase 55b`, `Status: Complete` (once phase lands), `Source Ref: phase-55b`, `Supersedes Legacy Doc: (none — new content)`
- [ ] "Overview" section: one paragraph explaining the Phase 55b scope as a learner-visible milestone (ring-0 drivers become ring-3 services, restartable, IOMMU-isolated)
- [ ] "What This Doc Covers" bullet list names the concepts owned by this phase: device-host capability primitives, MMIO bounds-checking, IOMMU-gated DMA, notification-forwarded IRQs, supervised restart — and calls out what is covered elsewhere (IOMMU mechanics in 55a, service-manager mechanics in 46 / 51)
- [ ] "Core Implementation" section covers, in learner-friendly prose: why drivers in ring 3 (fault isolation, restart, reduced TCB); the four device-host primitives and why each is a separate capability; how MMIO bounds-checking + IOMMU-gated DMA + notification-forwarded IRQs together form a safe ring-3 driver environment; the restart / in-flight-request contract; the difference between this phase's approach and Linux-style in-kernel drivers vs seL4's early-userspace drivers
- [ ] Documents the lock ordering between the device-host registry, capability table, and IOMMU unit (cross-referenced from B.1 / A.1 module docs)
- [ ] Documents the capability-cascade cleanup on process exit (`Device` → `Mmio` / `Dma` / `DeviceIrq` children all released)
- [ ] "Key Files" table lists `kernel/src/syscall/device_host.rs`, `kernel/src/blk/remote.rs`, `kernel/src/net/remote.rs`, `kernel-core/src/device_host/`, `kernel-core/src/driver_ipc/`, `userspace/lib/driver_runtime/`, `userspace/drivers/nvme/`, `userspace/drivers/e1000/`
- [ ] "How This Phase Differs From Later Driver Work" section notes: (a) Phase 56 (display and input) uses AF_UNIX + Phase 50 page grants rather than the four device-host syscalls, but adopts the `driver_runtime` API as the template for any future Phase 56-or-later hardware-owning userspace service (USB HID, GPU, display engine); (b) Phase 56's supervision of `display_server` / `kbd_server` / `mouse_server` reuses the `.conf` + `restart=on-failure` + `max_restart` pattern F.1 validates here; (c) Phase 58 (1.0 gate) depends on this phase being closed before shipping the ring-3-drivers claim; (d) VirtIO-blk / VirtIO-net extraction (Phase 57-range) reuses Track B and C unchanged
- [ ] "Related Roadmap Docs" links to `docs/roadmap/55b-ring-3-driver-host.md` and `docs/roadmap/tasks/55b-ring-3-driver-host-tasks.md`
- [ ] "Deferred or Later-Phase Topics" names VirtIO-blk / VirtIO-net extraction, driver-side seccomp, hot-plug, zero-downtime live update, multi-queue NVMe (matching the roadmap doc's Deferred list)
- [ ] Doc is linked from `docs/README.md`

### G.2 — Phase 15 / 55 / 55a subsystem doc updates

**Files:**
- `docs/15-hardware-discovery.md`
- `docs/55-hardware-substrate.md`
- `docs/55a-iommu-substrate.md`

**Symbol:** N/A (documentation)
**Why it matters:** The Phase 55 learning doc deliberately said "ring-0 placement is bounded; extraction deferred." Phase 55b closes that deferral; the earlier docs must cross-reference the new phase rather than continuing to read as if the deferral were indefinite.

**Acceptance:**
- [ ] Phase 55 doc's "Ring-0 placement is deliberate and bounded" note is updated to cross-reference Phase 55b for the extraction outcome; the "Deferred Until Later" entry for ring-3 driver extraction is moved to "Completed in Phase 55b"
- [ ] Phase 55a doc's "Support for Phase 55b" section (if present) is updated to cross-reference the delivered `sys_device_dma_alloc` syscall
- [ ] Phase 15 doc (Hardware Discovery) gains a one-paragraph note on the device-host capability model, with cross-reference to the Phase 55b learning doc
- [ ] No duplication of Phase 55b learning-doc content — cross-references only

### G.3 — Roadmap README rows

**Files:**
- `docs/roadmap/README.md`
- `docs/roadmap/tasks/README.md`

**Symbol:** Phase 55b row
**Why it matters:** Keeps the roadmap index in sync. The current `tasks/README.md` lists Phase 55b as "Deferred until implementation planning"; this task converts that row to the real task-doc link.

**Acceptance:**
- [ ] `docs/roadmap/README.md` Phase 55b row gains all template columns: `Phase | Theme | Primary Outcome | Status | Source Ref | Milestone | Tasks`; `Tasks` column links to `./tasks/55b-ring-3-driver-host-tasks.md`
- [ ] `docs/roadmap/tasks/README.md` Phase 55b row is updated from "Deferred until implementation planning" to the task-doc link; row placement remains between Phase 55a and Phase 56
- [ ] Both docs' `Status` column is flipped to `Complete` only when every Track A–G task in this file is closed
- [ ] Mermaid dependency diagram in `tasks/README.md` already includes `P55a --> P55b`; no edit needed

### G.4 — Version bump to v0.55.2

**Files:**
- `kernel/Cargo.toml`
- `AGENTS.md`
- `README.md`
- `docs/roadmap/README.md`
- `docs/roadmap/tasks/README.md`

**Symbol:** version string, project-overview summary
**Why it matters:** The Phase 55b design doc's Acceptance Criteria mandates the bump. Missing any of the five sites leaves version metadata inconsistent (Cargo.lock mismatch, AGENTS.md drift, incorrect roadmap status).

**Acceptance:**
- [ ] `kernel/Cargo.toml` `[package].version` is `"0.55.2"`
- [ ] `AGENTS.md` project-overview paragraph reflects kernel `v0.55.2` and names ring-3 drivers as the new capability
- [ ] `README.md` current-version string and capability summary updated consistently
- [ ] `docs/roadmap/README.md` and `docs/roadmap/tasks/README.md` Phase 55b status is `Complete`
- [ ] `Cargo.lock` regenerated in the same commit
- [ ] `cargo xtask check` passes after the bump

---

## Documentation Notes

- **Relative to Phase 55.** `kernel/src/blk/nvme.rs` and `kernel/src/net/e1000.rs` are deleted; their logic lives in `userspace/drivers/nvme/` and `userspace/drivers/e1000/`. `kernel/src/blk/mod.rs` and `kernel/src/net/mod.rs` gain `RemoteBlockDevice` / `RemoteNic` forwarding facades of roughly 100–150 lines each. `kernel_core::nvme` and `kernel_core::e1000` are retained — they are pure-data register / descriptor types consumed by the userspace drivers unchanged.

- **Relative to Phase 55a.** Phase 55a delivered the per-device `DmaDomain` lifetime bound to `PciDeviceHandle`. Phase 55b wraps the same primitive behind `sys_device_dma_alloc` so a ring-3 driver obtains a device-specific DMA handle without being able to create one against another device. Identity-map fallback (Phase 55a E.3) passes through: when `iommu::active()` is `false`, the syscall returns a `DmaHandle` whose `iova == phys_addr` and logs one `device_host.dma_alloc.identity` event per domain.

- **Relative to Phase 50.** `Capability::Device`, `Capability::Mmio`, `Capability::Dma`, `Capability::DeviceIrq` are four new variants added to the existing `Capability` enum. Validation follows the Phase 50 pattern exactly (per-process `CapabilityTable::lookup`, owner PID check). No new capability-transfer syscall is introduced — existing `sys_cap_grant` suffices for the narrow case of a driver handing an endpoint cap to a collaborating process.

- **Relative to Phase 46 / Phase 51.** Driver processes register through the existing service-manager `.conf` format. `restart=on-failure` with `max_restart=5` is the starting policy; tuning is a later operational matter. The `driver.restart` and `driver.restarted` event names are added to the syslog taxonomy. No new supervision primitive is introduced.

- **Relative to Phase 54.** The `vfs_server` / `net_server` / `fat_server` extraction pattern generalizes cleanly to device drivers. The `BlockServer` / `NetServer` helpers in `driver_runtime::ipc` are direct analogues of the Phase 54 protocol harnesses.

- **Support for Phase 56 (Display and Input).** Phase 56's primary substrate is AF_UNIX + Phase 50 page grants for client protocol and an existing in-kernel PS/2 IRQ12 mouse path — not the four PCIe-oriented device-host syscalls landed here. Phase 55b nevertheless has two concrete handoffs Phase 56 depends on and that this task doc preserves:
  1. **Supervised-ring-3-service pattern.** Phase 56 registers `display_server`, `kbd_server`, and `mouse_server` as supervised services using the same Phase 46 / Phase 51 `.conf` contract F.1 validates here for `nvme_driver` and `e1000_driver`. The `driver.registered` / `driver.restart` / `driver.restarted` event taxonomy, the `restart=on-failure` + `max_restart` policy shape, and the crash-and-restart regression harness (F.2) all become the precedent Phase 56's Track F supervision tasks cite.
  2. **`driver_runtime` API as template.** Track C delivers an API shape — `DeviceHandle`, `Mmio<T>`, `DmaBuffer<T>`, `IrqNotification`, the IPC-server helper pattern — that a future USB HID mouse driver, a dedicated GPU or display-engine driver, or any other PCIe hardware-owning userspace service can adopt without re-litigating the capability shape or protocol-client boilerplate. The API is not used by Phase 56's first three graphical services (they do not own PCIe devices), but it is the template Phase 56-or-later hardware extraction would consume. The Phase 55b learning doc (G.1) names this forward compatibility explicitly and the documented public-API stability promise below applies.
- **Support for Phase 57 and beyond.** VirtIO-blk and VirtIO-net extraction (obvious follow-up), tiling-compositor hardware-owning clients, and any future driver phase (AHCI, USB controllers, wireless) all reuse the Track B syscall set and Track C `driver_runtime`. Stability matters: the first post-55b phase that needs a new primitive (e.g., MSI-X PBA direct access for doorbell batching) extends the set; it does not reshape it.

- **Support for Phase 58 (1.0 Gate).** Phase 58 is a support-promise phase. Shipping 1.0 with "microkernel with supervised ring-3 drivers, IOMMU-isolated, restartable" is the stronger claim Phase 55b enables. The phase is written so the delivered contract is stable once closed — a post-55b change to any of the four syscalls or to the `driver_runtime` public API forces re-audit of every driver before 1.0.

- **Driver extraction is mechanical, not architectural.** Because `driver_runtime` mirrors the Phase 55 kernel-side HAL shapes (`Mmio<T>`, `DmaBuffer<T>`, IRQ registration), porting NVMe and e1000 is intended to be ~1:1 import substitution plus a thin main-loop wrapper, not a rewrite. Reviewers should expect the new `userspace/drivers/nvme/src/*.rs` files to be recognizable to anyone who read Phase 55 `kernel/src/blk/nvme.rs`.

- **In-flight requests at crash time.** The design admits a documented one-request blast radius: the request in flight when the driver crashes fails to the client with `DriverRestarting`; subsequent requests succeed after restart. This matches Phase 54's degraded-mode contract for extracted services.

- **What this phase does NOT do.** No driver-side seccomp beyond the default "only device-host syscalls allowed" posture (deferred). No hot-plug / surprise-removal handling (deferred). No VirtIO-blk / VirtIO-net extraction (deferred — obvious follow-up). No live update / zero-downtime restart (deferred — cold restart only). No multi-queue NVMe beyond the single I/O queue pair (deferred).

- **LSP / OCP / DIP summary.** `RemoteBlockDevice` is substitutable for the Phase 55 in-kernel NVMe driver; `RemoteNic` for in-kernel e1000. A third driver (VirtIO-blk, AHCI, VirtIO-net, another NIC) lands by implementing the `driver_runtime` contract and the block / net IPC protocol — no `driver_runtime` change, no kernel change. The contract suite (A.4, C.2) encodes this LSP promise and is run against any future impl before it ships.
