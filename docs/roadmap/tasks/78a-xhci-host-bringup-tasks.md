# Phase 78a — USB Host Foundation: xHCI Host-Controller Bring-Up: Task List

**Status:** Complete
**Source Ref:** phase-78a
**Depends on:** Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 67 (IOMMU Substrate Completion) ✅
**Goal:** Stand up the `xhci` ring-3 host-controller driver: claim the `qemu-xhci` controller, complete the full xHCI bring-up (register discovery → BIOS handoff → reset → DCBAA/scratchpad/contexts → command/event rings → MSI-X → run), and reach a first `Enable Slot` Command Completion event delivered off the event ring **by interrupt**. Also lands the two kernel-side prerequisites the ring-3 claim path lacks — **PCI Bus Master Enable** and **MSI-X table programming** — and a `xhci-bringup-smoke` gate. Kernel bumped to `0.78.0`. This is the first of three Phase 78 sub-phases ([78a](../78a-xhci-host-bringup.md) → [78b](../78b-usb-enumeration-hub.md) → [78c](../78c-usb-hid-and-release.md)); enumeration, hub, HID, and the full keystroke gate are 78b/78c.

> **Source-verified (2026-05-30):** all integration points checked against `main` and the xHCI surface researched against Redox `xhcid`, the Intel xHCI 1.2b spec, iPXE/Linux, and reviewed by five adversarial critics. Key facts: the device-host syscalls are `sys_device_claim`/`sys_device_mmio_map`/`sys_device_dma_alloc`/`sys_device_irq_subscribe` (`kernel/src/syscall/device_host.rs`, `0x1120`–`0x1126`); ring-3 drivers stage under `/drivers/` via `DRIVERS_ENTRIES` (`kernel/src/fs/ramdisk.rs:1150`), gated by `is_authorized_driver_process` (`device_host.rs:126`); the ring-3 claim path does **not** set Bus Master Enable (only the in-kernel virtio drivers do); `userspace/lib/syscall-lib` has no thread primitive, so the driver is a single-threaded drain-on-wake loop (the NVMe model).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | xHCI host controller driver (ring 3): register discovery, BIOS handoff + reset + run, DCBAA/scratchpad/contexts, command + event rings (ERST, cycle bit, doorbell), MSI-X interrupter + event loop, PORTSC reset/speed | Phase 55b ✅, Phase 67 ✅ | Complete |
| B | Kernel/PCI enablement + driver hosting: Bus Master Enable + MSI-X table programming; stage the `xhci` driver under `/drivers/` and start it | Phase 55b ✅ | Complete |
| C | Bring-up smoke gate + `0.78.0` version bump | A, B | Complete |

---

## Track A — xHCI Host Controller Driver (ring 3)

### A.1 — `xhci` driver crate scaffold: claim controller + map BAR0

**Files:**
- `userspace/drivers/xhci/` (new crate — mirror `userspace/drivers/nvme/` and `userspace/drivers/e1000/`)
- `userspace/lib/driver_runtime/src/lib.rs` (reuse `DeviceHandle`, `Mmio`)

**Symbol:** `program_main`, `driver_runtime::DeviceHandle::claim`, `driver_runtime::Mmio`
**Why it matters:** Establishes the ring-3 driver shape every later task hangs off. The driver must be staged under `/drivers/` (Track B.2) so the `is_authorized_driver_process` gate (`kernel/src/syscall/device_host.rs:126`) permits `sys_device_claim`. NVMe/e1000 prove the pattern: claim a sentinel BDF, map BAR0, emit a boot sentinel.

**Acceptance:**
- [x] New `no_std` crate `userspace/drivers/xhci` with `program_main`, `BrkAllocator` global allocator, `needs_alloc = true`
- [x] Claims the QEMU `qemu-xhci` controller via `DeviceHandle::claim(SENTINEL_BDF)` using the known BDF QEMU assigns (parallel to e1000's `SENTINEL_BDF` in `userspace/drivers/e1000/src/main.rs:97`)
- [x] Maps BAR0 read-write via `Mmio` (`sys_device_mmio_map`); writes a `[xhci] claimed bus:dev.func` boot marker
- [x] Reads `HCSPARAMS1.MaxPorts` and prints `[xhci] N ports detected`

### A.2 — Register-region discovery + capability parse (host-testable)

**Files:**
- `userspace/drivers/xhci/src/capability.rs` (new)
- `kernel-core/src/usb/xhci/regs.rs` (new — pure-logic field decoders, host-tested)

**Symbol:** `CapabilityRegs`, `caplength`, `rtsoff`, `dboff`, `Hcsparams1`, `Hcsparams2`, `Hccparams1`, `context_size`
**Why it matters:** Every operational/runtime/doorbell register access is at a runtime-computed offset (`Operational = BAR + CAPLENGTH`, `Runtime = BAR + RTSOFF`, `Doorbell = BAR + DBOFF`). `HCCPARAMS1.CSZ` selects 32- vs 64-byte contexts, which changes every later structure layout. Hardcoding offsets fails across controllers.

**Acceptance:**
- [x] Operational/Runtime/Doorbell base addresses computed at runtime from `CAPLENGTH` (cap+0x00), `RTSOFF` (cap+0x18), `DBOFF` (cap+0x14)
- [x] `HCSPARAMS1` (MaxSlots/MaxIntrs/MaxPorts), `HCSPARAMS2` (Max Scratchpad Buffers + ERST Max), `HCCPARAMS1` (`CSZ`, `AC64`, `xECP`) decoded into typed structs
- [x] Pure-logic decoders live in `kernel-core/src/usb/xhci/regs.rs` with host tests asserting field extraction from known register words (incl. the split Max-Scratchpad-Buffers `[31:27]<<5 | [25:21]` encoding)
- [x] Context size (32 vs 64) is selected from `CSZ` and threaded into all later context allocation (A.4)

### A.3 — BIOS/OS handoff, controller reset, and run sequence

**Files:**
- `userspace/drivers/xhci/src/capability.rs` (xECP walk)
- `userspace/drivers/xhci/src/operational.rs` (new)

**Symbol:** `release_bios_ownership` (`USBLEGSUP`), `reset_controller`, `Usbcmd`, `Usbsts`, `set_max_slots_enabled`, `run`
**Why it matters:** Real hardware boots with the firmware owning the controller; operational registers (`CONFIG`, `DCBAAP`, `CRCR`) are silently ignored until `USBSTS.CNR` clears after reset. Skipping the CNR wait is the classic "controller ignores my pointers and never posts events" bug.

**Acceptance:**
- [x] Walk the xECP capability list; if `USBLEGSUP` (cap id 1) is present, request OS ownership and poll until the BIOS-owned bit clears (no-op on QEMU, which reports no `USBLEGSUP` — documented)
- [x] Stop the controller (clear `USBCMD.R/S`, wait `USBSTS.HCH=1`), set `USBCMD.HCRST`, poll until `HCRST` self-clears **and** `USBSTS.CNR=0` **before** any `CONFIG`/`DCBAAP`/`CRCR` write
- [x] `CONFIG.MaxSlotsEn` written from `HCSPARAMS1.MaxSlots` (≥1)
- [x] **Consolidated ordered-init checklist** (enforced as acceptance, not just prose): after `CNR=0` → `CONFIG.MaxSlotsEn` → `DCBAAP` (A.4) → scratchpad into `DCBAA[0]` (A.4) → `CRCR` (A.5) → `ERSTSZ`→`ERSTBA`→`ERDP` (A.5) → `IMAN.IE`/`IMOD` (A.6) → **PCI Bus Master Enable confirmed (B.1)** → `USBCMD.R/S=1`. `USBCMD.RUN` is set only after every one of those is done; verified by `USBSTS.HCH` clearing. (xHCI 1.2b §4.2.)
- [x] **Bus mastering is a hard precondition:** xHCI DMAs nothing and posts no events until PCI Bus Master Enable is set (B.1). The run sequence asserts BME is enabled before `R/S=1`

### A.4 — DCBAA + scratchpad buffers + device/slot/endpoint contexts (IOMMU-mapped)

**Files:**
- `userspace/drivers/xhci/src/context.rs` (new)
- `kernel-core/src/usb/xhci/context.rs` (new — context layouts, host-tested for both CSZ sizes)

**Symbol:** `Dcbaa`, `ScratchpadArray`, `InputContext`, `SlotContext`, `EndpointContext`, `driver_runtime::DmaBuffer`
**Why it matters:** The DCBAA is the single table the xHC walks to find every device's Output Device Context; `Address Device` has nowhere to write without it. When `HCSPARAMS2` reports nonzero Max Scratchpad Buffers (common on real HW), leaving `DCBAA[0]` null faults/hangs the controller at Run. Every controller-visible structure must be DMA-mapped through the Phase 67 IOMMU substrate so the controller can only reach granted pages.

**Acceptance:**
- [x] DCBAA allocated via `DmaBuffer` (`sys_device_dma_alloc`), `(MaxSlotsEn+1)` 64-bit entries, 64-byte aligned; its **IOVA** (`DmaBuffer::iova()`, not a CPU pointer) programmed into `DCBAAP`
- [x] If Max Scratchpad Buffers > 0: allocate that many `PAGESIZE`-register-sized, page-aligned `DmaBuffer`s, build a 64-bit IOVA pointer array, and write the array's IOVA into `DCBAA[0]`
- [x] `InputContext`/`SlotContext`/`EndpointContext` structs sized per `CSZ`; host tests assert correct field offsets for **both** 32- and 64-byte layouts
- [x] A grep/diff confirms every controller-visible structure (DCBAA, scratchpad pages + array, rings, contexts) programs an IOVA from `DmaBuffer`, never a raw `physical_address()`/CPU pointer

### A.5 — Command Ring + Event Ring + ERST + TRB machinery (cycle bit, Link TRB, doorbell)

**Files:**
- `userspace/drivers/xhci/src/ring.rs`, `src/trb.rs`, `src/event.rs` (new)
- `kernel-core/src/usb/xhci/trb.rs` (new — TRB encode/decode + cycle-bit logic, host-tested)

**Symbol:** `TrbRing`, `Trb` (Normal/SetupStage/DataStage/StatusStage/Link), `EventRing`, `Erst`, `enqueue`, `ring_doorbell`, `CommandCompletionEvent`, `TransferEvent`, `PortStatusChangeEvent`
**Why it matters:** TRB rings are the core data-movement mechanism; the event ring is the **only** channel by which the controller reports completions and port changes. The `ERSTSZ`→`ERSTBA`→`ERDP` ordering arms the interrupter; getting ERDP wrong makes the controller think the ring is full and stop posting. The cycle bit is how host and controller agree which TRBs are valid.

**Acceptance:**
- [x] Command ring: 16-byte TRBs with a trailing **Link TRB** (Toggle Cycle set); `CRCR` = ring IOVA `| RCS`
- [x] Event ring: one or more segments + an **ERST** (`{seg base, seg size}` entries); program `ERSTSZ` first, then `ERSTBA`, then `ERDP`; `ERDP` advanced and `EHB` cleared as events are consumed; event ring has **no** Link TRB (wraps per ERST sizes)
- [x] TRB encode/decode for Normal, Setup Stage, Data Stage, Status Stage, Link; event TRB parse for **Command Completion**, **Transfer** (completion code + residual), **Port Status Change**
- [x] Producer cycle-bit logic for the command/transfer rings (wrap + toggle at Link TRB); **separately**, the event-ring **consumer** maintains its own Consumer Cycle State (starts at 1, toggles on each ERST segment-boundary wrap) and consumes a TRB only when its Cycle bit == CCS; on each drain, `ERDP` is written with the current dequeue IOVA **and** the `EHB` (Event Handler Busy, bit 3) set to clear it — host-tested across an ERST segment boundary
- [x] **Device Context Index (DCI) + doorbell targeting** specified and host-tested: EP0 (bidirectional default control) = DCI 1; for endpoint number `N`, `DCI = 2*N + (IN ? 1 : 0)` (so interrupt-IN endpoint 1 = DCI 3); Doorbell 0 = Command Ring; a slot's doorbell Target field = the endpoint DCI; the Input Control Context Add Flags are DCI-indexed (`A0` = Slot Context, `A1` = EP0 Context). A write barrier precedes every doorbell write. (This formula is load-bearing in 78b/78c too.)
- [x] **Milestone proof:** an `Enable Slot` command is enqueued, Doorbell 0 rung, and its Command Completion event consumed off the event ring with the matching slot ID (proves ring + ERST + doorbell + cycle bit are all wired) — initially via poll, then via A.6 interrupt

### A.6 — MSI-X interrupter + single-threaded event loop (event-ring drain → completion wake)

**Files:**
- `userspace/drivers/xhci/src/runtime.rs`, `src/event_loop.rs` (new)
- `userspace/lib/driver_runtime/src/irq.rs` (reuse `IrqNotification`)

**Symbol:** `driver_runtime::IrqNotification::subscribe` / `wait`, `Iman`, `Imod`, in-flight table keyed by `(slot_id, ep_dci)`
**Why it matters:** Without an enabled interrupter and a wired MSI-X vector, the event ring fills but no IRQ fires and the driver hangs waiting for completions. Per m3OS interrupt rules the kernel ISR only acks + signals a `Notification`; the driver does the work in ring 3. **Concurrency note (source-verified):** `userspace/lib/syscall-lib` exposes **no** userspace thread/`clone`/`spawn` primitive, and no existing driver spawns a thread — so this is **not** a separate reactor thread. The model is the single-threaded **drain-on-wake** loop the NVMe driver already uses (`userspace/drivers/nvme/src/io.rs`, `wait_completion`: drain completions, then block inline in `IrqNotification::wait`). The HID interrupt-IN `bInterval` polling (78c) is serviced from the same loop, not a concurrent timer. If genuine concurrency is ever required, a userspace thread primitive is a prerequisite and a separate scope addition — flagged, not assumed.

**Acceptance:**
- [x] `sys_device_irq_subscribe` (via `IrqNotification`) binds the controller IRQ to a `Notification` (MSI-X preferred — the substrate prefers MSI-X → MSI → INTx, `allocate_device_vector` doc comment at `kernel/src/syscall/device_host.rs:1653`); `IMAN.IE` set, `IMOD` interval set, `IMAN.IP` handled write-1-clear
- [x] A **single-threaded event loop** blocks in `IrqNotification::wait`, drains the event ring on wake, and matches Transfer/Command-Completion events to outstanding requests via a `(slot, ep)`-keyed in-flight table — mirroring the NVMe `wait_completion` drain-on-wake pattern; **no busy-poll**, **no separate thread**
- [x] Port Status Change events are routed to a handler stub (the enumeration consumer is 78b)
- [x] Verified under `qemu-xhci`: the A.5 `Enable Slot` completion arrives via interrupt + event ring (not poll)

### A.7 — PORTSC port reset + speed detection (RW1C-safe)

**Files:**
- `userspace/drivers/xhci/src/port.rs` (new)
- `kernel-core/src/usb/xhci/port.rs` (new — PORTSC bit logic, host-tested)

**Symbol:** `Portsc`, `reset_port`, `port_speed`, `ep0_max_packet_for_speed`, `PORTSC_PRESERVE_MASK`
**Why it matters:** A device stays Powered/Disabled until its port is reset; `Enable Slot`/`Address Device` target nothing otherwise. The detected speed selects EP0 Max Packet Size — and the values are speed-specific: **Low = 8, Full = 8 (default until the 78b BSR pre-read learns the real value), High = 64, SuperSpeed = 512** (`bMaxPacketSize0 = 9`, i.e. 2^9). Programming 64 for a SuperSpeed device is a spec violation that breaks SS control transfers. PORTSC change bits are RW1C — a careless write clobbers them, a classic bug.

**Acceptance:**
- [x] Per-port `PORTSC` accessed at `op + 0x400 + 0x10*(port-1)`; enumeration is **triggered by a Port Status Change event with `CSC=1`** (then read `CCS` for connect/disconnect) — distinguish the edge (`CSC`) from the level (`CCS`) so enumeration is neither missed nor duplicated
- [x] On connect: USB2 ports get an explicit `PR` write, then wait `PRC=1`, RW1C-clear `PRC`, and confirm `PED=1` before `Enable Slot`; USB3 ports omit the `PR` write (controller-driven reset/training) and reach Enabled directly
- [x] Port-speed field decoded → Low/Full/High/SuperSpeed → EP0 Max Packet Size = **8 / 8 / 64 / 512** (full-speed corrected via the 78b BSR pre-read + Evaluate Context); host tests assert each speed, **including the SuperSpeed = 512 (`bMaxPacketSize0 = 9`) case**
- [x] PORTSC writes apply a preserve-mask so RW1C change bits (`CSC`/`PEC`/`PRC`) are not accidentally cleared while writing `PR`; host-tested
- [x] A connected `qemu-xhci` HID device's port reaches the Enabled state

---

## Track B — Kernel/PCI Enablement + Driver Hosting

### B.1 — PCI enablement for xHCI: Bus Master Enable + MSI-X programming

**Files:**
- `kernel/src/syscall/device_host.rs` (`sys_device_claim`, line 596; `sys_device_irq_subscribe`, line 1478; `allocate_device_vector`, line 1653)
- `kernel/src/pci/mod.rs` (`claim_pci_device_by_bdf`, line 654; the `write_config_u16(0x04, …)` BME pattern used by the in-kernel virtio drivers)
- `kernel/src/arch/x86_64/interrupts.rs` (`register_device_irq`, line 2297; `dispatch_device_irq`, line 2359)

**Symbol:** PCI Command register (offset `0x04`) Bus Master Enable + Memory Space; `sys_device_irq_subscribe` MSI-X capability + table programming
**Why it matters:** **Source-verified CRITICAL gap.** xHCI is a pure bus-master DMA device — it DMAs nothing and posts **zero** events until **PCI Bus Master Enable** (Command reg `0x04`, bit 2) and Memory Space (bit 0) are set. The only code in the tree that sets BME today is the **in-kernel** virtio drivers (`kernel/src/net/virtio_net.rs`, `kernel/src/blk/virtio_blk.rs`, via `write_config_u16(0x04, cmd | 0x05)`); the ring-3 claim path (`sys_device_claim` → `claim_pci_device_by_bdf`) does **not** enable BME, and there is no ring-3 PCI-config-write syscall. If BME stays off, the controller reaches RUN and the driver waits on the event ring forever with no diagnostic. Separately, INTx is unreliable on xHCI, so MSI-X must be the live IRQ path.

**Acceptance:**
- [x] `sys_device_claim` enables PCI **Bus Master Enable + Memory Space** (Command reg `0x04` bits `0|2`) for the claimed device — mirroring the in-kernel virtio `write_config_u16(0x04, cmd | 0x05)` — verified by reading back the Command register after claim; A.3's run sequence asserts BME is on before `USBCMD.R/S=1`
- [x] `sys_device_irq_subscribe` programs the xHCI controller's PCI MSI-X capability + table (vector, message address + data) with the MSI-X **Enable** bit set (not just kernel-vector allocation); the MSI-X-if-advertised ordering is the `allocate_device_vector` doc comment at `kernel/src/syscall/device_host.rs:1653`
- [x] Any gap exposed because the existing path was only exercised by NVMe/e1000 is closed concretely — defined as: the xHCI MSI-X table is programmed with vector address+data and `IMAN.IE` is set, confirmed by the controller posting an MSI-X interrupt in the end-to-end check below
- [x] Verified end-to-end: with BME on and MSI-X enabled, an xHCI event posts an MSI-X interrupt that signals the driver's `Notification` (the A.6 proof)

### B.2 — Build + ramdisk + service wiring for the `xhci` driver

**Files:**
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`build_userspace_bins`, line 795; `bins` array, line 800; `populate_ext2_files` service configs, ~line 12597)
- `kernel/src/fs/ramdisk.rs` (`DRIVERS_ENTRIES`, line 1150; mounted at `/drivers`, line 1196)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`, lines 185–230)
- `kernel/initrd/etc/services.d/xhci.conf` (new)

**Symbol:** `bins` tuple `(pkg, bin, needs_alloc=true)`, `DRIVERS_ENTRIES`, `xhci.conf`
**Why it matters:** **Source-verified gotcha.** Ring-3 drivers must be staged in `DRIVERS_ENTRIES` (under `/drivers/`), **not** `BIN_ENTRIES` (under `/bin/`), or the `is_authorized_driver_process` gate (`device_host.rs:126`) denies `sys_device_claim`. The driver must be built, embedded, and started for the bring-up smoke gate to exercise it.

**Acceptance:**
- [x] `xhci` added as a Cargo `member` and to the `bins` array with `needs_alloc = true` (it depends on `kernel-core`)
- [x] `xhci` binary embedded in `DRIVERS_ENTRIES` (`ramdisk.rs`) at `/drivers/xhci`
- [x] `xhci.conf` added to `kernel/initrd/etc/services.d/` **and** `init` `KNOWN_CONFIGS`; uses `command=/drivers/xhci`, `type=daemon`, `restart=on-failure`
- [x] `cargo xtask clean` run after adding the config (forces ext2 disk recreation)

---

## Track C — Bring-up Smoke Gate + Release

### C.1 — `xhci-bringup-smoke` gate

**Files:**
- `xtask/src/main.rs` (new `cmd_xhci_bringup_smoke`; QEMU `-device qemu-xhci` arg addition; smoke step)
- `userspace/drivers/xhci/` (PASS sentinel emitted on first Command Completion)

**Symbol:** `cmd_xhci_bringup_smoke`, QEMU `-device qemu-xhci`, `XHCI_BRINGUP:enable-slot:OK`
**Why it matters:** A `[xhci] N ports detected` serial line proves only that the daemon ran — not that the event ring + interrupter delivered a real completion. There is no `qemu-xhci` device in the xtask QEMU args today, so this gate adds it and asserts the load-bearing milestone.

**Acceptance:**
- [x] QEMU launched with `-device qemu-xhci` (new in the xtask QEMU arg builder)
- [x] The gate asserts a real `Enable Slot` Command Completion event was consumed off the event ring **via MSI-X** (the driver emits `XHCI_BRINGUP:enable-slot:OK` only on a real interrupt-delivered completion, not on a poll fallback)
- [x] A serial `[xhci] N ports detected` sentinel alone is explicitly **not** sufficient for PASS
- [x] Wired as `cargo xtask xhci-bringup-smoke` with an opt-in pre-push gate `M3OS_USB_REGRESSION=1` (mirrors the heavyweight `htop-render-probe` / `compositor-stress` gates and the AGENTS.md hooks table)

### C.2 — Bump kernel version to `0.78.0`

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]` (currently `0.77.0`, line 3)
**Why it matters:** Project convention is a version bump per shipped (sub-)phase; 78a opens the `0.78.x` line, mirroring how Phase 76 opened `0.76.x`. The "USB host stack" capability bullet in `AGENTS.md` is deferred to 78c, when the capability becomes user-visible (HID input).

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.78.0"`
- [x] `Cargo.lock` regenerated (via `cargo xtask check`)
- [x] `AGENTS.md` kernel version updated to `v0.78.0` (version string only — the capability-inventory bullet lands at 78c per the keep-it-small policy)
- [x] `docs/roadmap/README.md` Phase 78a row Status updated to "Complete"; design-doc + task-doc Status headers set to Complete
- [x] `cargo xtask check` passes
- [x] Git tag `v0.78.0` — recommended at sub-phase merge (left to the merge step)

---

## Documentation Notes

- **This is the host-controller bring-up only.** Reaching a first `Enable Slot` Command Completion event by interrupt is the whole milestone; enumeration, hub, and HID are explicitly out of scope (78b/78c).
- **Bus Master Enable is the highest-risk silent failure (B.1).** The ring-3 claim path does not set BME today; without it the controller reaches RUN and the event-ring wait hangs forever with no diagnostic. A.3 asserts BME before `R/S=1`.
- **No userspace thread primitive exists.** The driver is a single-threaded drain-on-wake event loop (the NVMe `wait_completion` model), not a reactor thread (A.6).
- **Redox `xhcid` is the closest blueprint** for the internal module split (capability/operational/runtime/doorbell/context/ring/trb/event/port). The deliberate divergence: back every DMA structure with the IOMMU-routed `DmaBuffer<T>` so the controller is confined to granted pages (Redox hands raw physical addresses).
- **Host-test everything pure-logic** in `kernel-core/src/usb/xhci/`: register decoders, TRB encode/decode + cycle bit, context layouts (both `CSZ` sizes), PORTSC bits. Only MMIO/DMA/IRQ glue is ring-3-only.
- After adding the service config (B.2), run `cargo xtask clean` to force ext2 disk recreation.
