# Phase 78 — USB Host Foundation (xHCI + Hub + HID): Task List

**Status:** Planned
**Source Ref:** phase-78
**Depends on:** Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 67 (IOMMU Substrate Completion) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 74 (IPC Capability Grants / page-grant bulk transport) ✅
**Goal:** Stand up m3OS's first USB stack as supervised ring-3 processes — a userspace xHCI host-controller driver, a host-testable USB core (descriptor parsing + enumeration state machine + hub), and a HID class driver that presents a Boot-Protocol USB keyboard and mouse as additional producers on the existing Phase 56 `kbd_server` / `mouse_server` input path. After this phase a modern x86_64 laptop or desktop with no PS/2 port can drive the m3OS login prompt and GUI over USB. This is the single biggest 1.0 real-hardware unblocker per the Phase 74a §3 audit. Kernel bumped to `0.78.0`.

> **Review note (source-verified 2026-05-30):** This task list was authored after a full source-verification pass against `main` and a research pass over Redox OS (`redox-os/drivers`: `xhcid`, `usbhubd`, `usbhidd`, `usbscsid`, `pcid`), the Intel xHCI 1.2b spec, iPXE/Linux xHCI bring-up, and the USB HID 1.11 spec. Six claims in the Phase 78 **design doc** drifted from reality and are corrected here and in `docs/roadmap/78-usb-host-foundation.md`:
>
> 1. **Syscall names are wrong.** The design doc cites `sys_device_pci_probe`, `sys_device_irq_bind`, and `iommu_map_bar`. The real Phase 55b/67 surface (all in `kernel/src/syscall/device_host.rs`, numbers `0x1120`–`0x1126` in `kernel-core/src/device_host/syscalls.rs`) is: `sys_device_claim` (BDF claim), `sys_device_mmio_map` (BAR map — this is the real "iommu_map_bar"), `sys_device_dma_alloc` + `sys_device_dma_handle_info`, `sys_device_irq_subscribe` (the real IRQ bind — already prefers **MSI-X**), and `sys_device_pio_read` / `sys_device_pio_write`.
> 2. **There is no PCI class-code filter.** `sys_device_claim(segment, bus, dev, func)` takes a BDF only; NVMe/e1000 use a hardcoded `SENTINEL_BDF`. The design doc's "Filter on class code `0x0C0330`" is **not** an existing primitive — controller discovery becomes Track D.1.
> 3. **Ring-3 drivers stage under `/drivers/`, not `/bin/`.** They must be embedded in `DRIVERS_ENTRIES` (`kernel/src/fs/ramdisk.rs:1150`, mounted at `/drivers`, line 1196), **not** `BIN_ENTRIES`, or the `is_authorized_driver_process` gate (`device_host.rs:126`, prefix `/drivers/`) denies `sys_device_claim` with `-EACCES`.
> 4. **The design doc omits the bulk of real xHCI bring-up.** Missing-but-mandatory: register-region discovery (`CAPLENGTH`/`RTSOFF`/`DBOFF`), `USBLEGSUP` BIOS/OS handoff, `USBCMD.HCRST`+`USBSTS.CNR` reset, `CONFIG.MaxSlotsEn`, **DCBAA**+`DCBAAP`, **Scratchpad Buffer Array**→`DCBAA[0]`, **Event Ring + ERST** (`ERSTSZ`→`ERSTBA`→`ERDP`), **MSI-X interrupter** (`IMAN`/`IMOD`), **context size** (`HCCPARAMS1.CSZ` 32/64), **Enable Slot / Configure Endpoint / Evaluate Context** (not just Address Device), **Link + Event TRBs** (Command Completion / Transfer / Port Status Change), cycle-bit + doorbell, and **PORTSC reset + speed detection**. These move out of the implicit/deferred bucket into explicit Track A tasks.
> 5. **`KeyEvent`/`PointerEvent` are already defined and stable.** `kernel-core/src/input/events.rs:146` (`KeyEvent`, 20-byte wire = `KEY_EVENT_WIRE_SIZE`) and `:199` (`PointerEvent`, 37-byte wire = `POINTER_EVENT_WIRE_SIZE`). USB-HID reuses these codecs verbatim; no new wire format.
> 6. **HID-Boot needs `SET_IDLE`**, and the interrupt-IN endpoint must be brought into the controller via **Configure Endpoint** and polled with **Normal TRBs** at `bInterval`. The design doc lists only `SET_PROTOCOL(0)`.
>
> **Scope realism:** A full xHCI + USB-core + hub + HID stack is materially larger than a Phase-77-style bundle (every track here is multi-week, not sub-week). If implementation pressure demands it, split delivery into sub-phases mirroring the Phase 76 → 76b/76c/76d pattern — **78a** (Track A host bring-up to first Command Completion event), **78b** (Track B enumeration + hub), **78c** (Track C HID + Track D wiring + Track E release) — promoting the kernel to `0.78.0` only at the final sub-phase. This task list is authored as one design; the sub-phase cut is a delivery decision, not a redesign.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | xHCI host-controller driver (ring 3): register discovery, BIOS handoff + reset + run, DCBAA/scratchpad/contexts, command + event rings (ERST, cycle bit, doorbell), MSI-X interrupter + IRQ reactor, PORTSC reset/speed | Phase 55b ✅, Phase 67 ✅ | Planned |
| B | USB core: host-testable descriptor parsing + enumeration state machine (Enable Slot → Address Device BSR → GET_DESCRIPTOR → SET_CONFIGURATION → Configure Endpoint), hub class + `PortId` topology, host↔class IPC protocol crate | A | Planned |
| C | HID class driver (ring 3): Boot-Protocol keyboard + mouse → `KeyEvent`/`PointerEvent`, injected into `kbd_server`/`mouse_server`; Report-Protocol skeleton (deferred) | B | Planned |
| D | Kernel-side wiring + integration: ring-3 PCI class enumeration, MSI-X programming verification, the 4-place build/ramdisk/service wiring (driver variant), `usb-smoke` acceptance gate | A, B, C | Planned |
| E | Documentation + release: Phase 78 learning doc, `0.78.0` version bump | A–D | Planned |

---

## Track A — xHCI Host-Controller Driver (ring 3)

### A.1 — `xhci` driver crate scaffold: claim controller + map BAR0

**Files:**
- `userspace/drivers/xhci/` (new crate — mirror `userspace/drivers/nvme/` and `userspace/drivers/e1000/`)
- `userspace/lib/driver_runtime/src/lib.rs` (reuse `DeviceHandle`, `Mmio`)

**Symbol:** `program_main`, `driver_runtime::DeviceHandle::claim`, `driver_runtime::Mmio`
**Why it matters:** Establishes the ring-3 driver shape every later task hangs off. The driver must be staged under `/drivers/` (Track D.3) so the `is_authorized_driver_process` gate (`kernel/src/syscall/device_host.rs:126`) permits `sys_device_claim`. NVMe/e1000 prove the pattern: claim a sentinel BDF, map BAR0, emit a boot sentinel.

**Acceptance:**
- [ ] New `no_std` crate `userspace/drivers/xhci` with `program_main`, `BrkAllocator` global allocator, `needs_alloc = true`
- [ ] Claims the QEMU `qemu-xhci` controller via `DeviceHandle::claim(SENTINEL_BDF)` using the known BDF QEMU assigns (parallel to e1000's `SENTINEL_BDF` in `userspace/drivers/e1000/src/main.rs:97`)
- [ ] Maps BAR0 read-write via `Mmio` (`sys_device_mmio_map`); writes a `[xhci] claimed bus:dev.func` boot marker
- [ ] Reads `HCSPARAMS1.MaxPorts` and prints `[xhci] N ports detected` (the Implementation-Outline step-1 milestone)

### A.2 — Register-region discovery + capability parse (host-testable)

**Files:**
- `userspace/drivers/xhci/src/capability.rs` (new)
- `kernel-core/src/usb/xhci/regs.rs` (new — pure-logic field decoders, host-tested)

**Symbol:** `CapabilityRegs`, `caplength`, `rtsoff`, `dboff`, `Hcsparams1`, `Hcsparams2`, `Hccparams1`, `context_size`
**Why it matters:** Every operational/runtime/doorbell register access is at a runtime-computed offset (`Operational = BAR + CAPLENGTH`, `Runtime = BAR + RTSOFF`, `Doorbell = BAR + DBOFF`). `HCCPARAMS1.CSZ` selects 32- vs 64-byte contexts, which changes every later structure layout. Hardcoding offsets fails across controllers.

**Acceptance:**
- [ ] Operational/Runtime/Doorbell base addresses computed at runtime from `CAPLENGTH` (cap+0x00), `RTSOFF` (cap+0x18), `DBOFF` (cap+0x14)
- [ ] `HCSPARAMS1` (MaxSlots/MaxIntrs/MaxPorts), `HCSPARAMS2` (Max Scratchpad Buffers + ERST Max), `HCCPARAMS1` (`CSZ`, `AC64`, `xECP`) decoded into typed structs
- [ ] Pure-logic decoders live in `kernel-core/src/usb/xhci/regs.rs` with host tests asserting field extraction from known register words (incl. the split Max-Scratchpad-Buffers `[31:27]<<5 | [25:21]` encoding)
- [ ] Context size (32 vs 64) is selected from `CSZ` and threaded into all later context allocation (A.4, B.2)

### A.3 — BIOS/OS handoff, controller reset, and run sequence

**Files:**
- `userspace/drivers/xhci/src/capability.rs` (xECP walk)
- `userspace/drivers/xhci/src/operational.rs` (new)

**Symbol:** `release_bios_ownership` (`USBLEGSUP`), `reset_controller`, `Usbcmd`, `Usbsts`, `set_max_slots_enabled`, `run`
**Why it matters:** Real hardware boots with the firmware owning the controller; operational registers (`CONFIG`, `DCBAAP`, `CRCR`) are silently ignored until `USBSTS.CNR` clears after reset. Skipping the CNR wait is the classic "controller ignores my pointers and never posts events" bug.

**Acceptance:**
- [ ] Walk the xECP capability list; if `USBLEGSUP` (cap id 1) is present, request OS ownership and poll until the BIOS-owned bit clears (no-op on QEMU, which reports no `USBLEGSUP` — documented)
- [ ] Stop the controller (clear `USBCMD.R/S`, wait `USBSTS.HCH=1`), set `USBCMD.HCRST`, poll until `HCRST` self-clears **and** `USBSTS.CNR=0` **before** any `CONFIG`/`DCBAAP`/`CRCR` write
- [ ] `CONFIG.MaxSlotsEn` written from `HCSPARAMS1.MaxSlots` (≥1)
- [ ] **Consolidated ordered-init checklist** (enforced as acceptance, not just prose): after `CNR=0` → `CONFIG.MaxSlotsEn` → `DCBAAP` (A.4) → scratchpad into `DCBAA[0]` (A.4) → `CRCR` (A.5) → `ERSTSZ`→`ERSTBA`→`ERDP` (A.5) → `IMAN.IE`/`IMOD` (A.6, or its poll equivalent) → **PCI Bus Master Enable confirmed (D.2)** → `USBCMD.R/S=1`. `USBCMD.RUN` is set only after every one of those is done; verified by `USBSTS.HCH` clearing. (xHCI 1.2b §4.2.)
- [ ] **Bus mastering is a hard precondition:** xHCI DMAs nothing and posts no events until PCI Bus Master Enable is set (D.2). The run sequence asserts BME is enabled before `R/S=1`

### A.4 — DCBAA + scratchpad buffers + device/slot/endpoint contexts (IOMMU-mapped)

**Files:**
- `userspace/drivers/xhci/src/context.rs` (new)
- `kernel-core/src/usb/xhci/context.rs` (new — context layouts, host-tested for both CSZ sizes)

**Symbol:** `Dcbaa`, `ScratchpadArray`, `InputContext`, `SlotContext`, `EndpointContext`, `driver_runtime::DmaBuffer`
**Why it matters:** The DCBAA is the single table the xHC walks to find every device's Output Device Context; `Address Device` has nowhere to write without it. When `HCSPARAMS2` reports nonzero Max Scratchpad Buffers (common on real HW), leaving `DCBAA[0]` null faults/hangs the controller at Run. Every controller-visible structure must be DMA-mapped through the Phase 67 IOMMU substrate so the controller can only reach granted pages.

**Acceptance:**
- [ ] DCBAA allocated via `DmaBuffer` (`sys_device_dma_alloc`), `(MaxSlotsEn+1)` 64-bit entries, 64-byte aligned; its **IOVA** (`DmaBuffer::iova()`, not a CPU pointer) programmed into `DCBAAP`
- [ ] If Max Scratchpad Buffers > 0: allocate that many `PAGESIZE`-register-sized, page-aligned `DmaBuffer`s, build a 64-bit IOVA pointer array, and write the array's IOVA into `DCBAA[0]`
- [ ] `InputContext`/`SlotContext`/`EndpointContext` structs sized per `CSZ`; host tests assert correct field offsets for **both** 32- and 64-byte layouts
- [ ] A grep/diff confirms every controller-visible structure (DCBAA, scratchpad pages + array, rings, contexts) programs an IOVA from `DmaBuffer`, never a raw `physical_address()`/CPU pointer

### A.5 — Command Ring + Event Ring + ERST + TRB machinery (cycle bit, Link TRB, doorbell)

**Files:**
- `userspace/drivers/xhci/src/ring.rs`, `src/trb.rs`, `src/event.rs` (new)
- `kernel-core/src/usb/xhci/trb.rs` (new — TRB encode/decode + cycle-bit logic, host-tested)

**Symbol:** `TrbRing`, `Trb` (Normal/SetupStage/DataStage/StatusStage/Link), `EventRing`, `Erst`, `enqueue`, `ring_doorbell`, `CommandCompletionEvent`, `TransferEvent`, `PortStatusChangeEvent`
**Why it matters:** TRB rings are the core data-movement mechanism; the event ring is the **only** channel by which the controller reports completions and port changes. The `ERSTSZ`→`ERSTBA`→`ERDP` ordering arms the interrupter; getting ERDP wrong makes the controller think the ring is full and stop posting. The cycle bit is how host and controller agree which TRBs are valid.

**Acceptance:**
- [ ] Command ring: 16-byte TRBs with a trailing **Link TRB** (Toggle Cycle set); `CRCR` = ring IOVA `| RCS`
- [ ] Event ring: one or more segments + an **ERST** (`{seg base, seg size}` entries); program `ERSTSZ` first, then `ERSTBA`, then `ERDP`; `ERDP` advanced and `EHB` cleared as events are consumed; event ring has **no** Link TRB (wraps per ERST sizes)
- [ ] TRB encode/decode for Normal, Setup Stage, Data Stage, Status Stage, Link; event TRB parse for **Command Completion**, **Transfer** (completion code + residual), **Port Status Change**
- [ ] Producer cycle-bit logic for the command/transfer rings (wrap + toggle at Link TRB); **separately**, the event-ring **consumer** maintains its own Consumer Cycle State (starts at 1, toggles on each ERST segment-boundary wrap) and consumes a TRB only when its Cycle bit == CCS; on each drain, `ERDP` is written with the current dequeue IOVA **and** the `EHB` (Event Handler Busy, bit 3) set to clear it — host-tested across an ERST segment boundary
- [ ] **Device Context Index (DCI) + doorbell targeting** specified and host-tested: EP0 (bidirectional default control) = DCI 1; for endpoint number `N`, `DCI = 2*N + (IN ? 1 : 0)` (so interrupt-IN endpoint 1 = DCI 3); Doorbell 0 = Command Ring; a slot's doorbell Target field = the endpoint DCI; the Input Control Context Add Flags are DCI-indexed (`A0` = Slot Context, `A1` = EP0 Context). A write barrier precedes every doorbell write. (This formula is load-bearing in B.2/C.1 too.)
- [ ] **Milestone proof:** an `Enable Slot` command is enqueued, Doorbell 0 rung, and its Command Completion event consumed off the event ring with the matching slot ID (proves ring + ERST + doorbell + cycle bit are all wired) — initially via poll, then via A.6 interrupt

### A.6 — MSI-X interrupter + single-threaded event loop (event-ring drain → completion wake)

**Files:**
- `userspace/drivers/xhci/src/runtime.rs`, `src/irq_reactor.rs` (new)
- `userspace/lib/driver_runtime/src/irq.rs` (reuse `IrqNotification`)

**Symbol:** `driver_runtime::IrqNotification::subscribe` / `wait`, `Iman`, `Imod`, in-flight table keyed by `(slot_id, ep_dci)`
**Why it matters:** Without an enabled interrupter and a wired MSI-X vector, the event ring fills but no IRQ fires and the driver hangs waiting for completions. Per m3OS interrupt rules the kernel ISR only acks + signals a `Notification`; the driver does the work in ring 3. **Concurrency note (source-verified):** `userspace/lib/syscall-lib` exposes **no** userspace thread/`clone`/`spawn` primitive, and no existing driver spawns a thread — so this is **not** a separate reactor thread. The model is the single-threaded **drain-on-wake** loop the NVMe driver already uses (`userspace/drivers/nvme/src/io.rs`, `wait_completion`: drain completions, then block inline in `IrqNotification::wait`). The HID interrupt-IN `bInterval` polling (C.1) is serviced from the same loop, not a concurrent timer. If genuine concurrency is ever required, a userspace thread primitive is a prerequisite and a separate scope addition — flagged, not assumed.

**Acceptance:**
- [ ] `sys_device_irq_subscribe` (via `IrqNotification`) binds the controller IRQ to a `Notification` (MSI-X preferred — the substrate prefers MSI-X → MSI → INTx, `allocate_device_vector` doc comment at `kernel/src/syscall/device_host.rs:1653`); `IMAN.IE` set, `IMOD` interval set, `IMAN.IP` handled write-1-clear
- [ ] A **single-threaded event loop** blocks in `IrqNotification::wait`, drains the event ring on wake, and matches Transfer/Command-Completion events to outstanding requests via a `(slot, ep)`-keyed in-flight table — mirroring the NVMe `wait_completion` drain-on-wake pattern; **no busy-poll**, **no separate thread**
- [ ] Port Status Change events are routed to the enumeration path (Track B)
- [ ] Verified under `qemu-xhci`: the A.5 `Enable Slot` completion arrives via interrupt + event ring (not poll)

### A.7 — PORTSC port reset + speed detection (RW1C-safe)

**Files:**
- `userspace/drivers/xhci/src/port.rs` (new)
- `kernel-core/src/usb/xhci/port.rs` (new — PORTSC bit logic, host-tested)

**Symbol:** `Portsc`, `reset_port`, `port_speed`, `ep0_max_packet_for_speed`, `PORTSC_PRESERVE_MASK`
**Why it matters:** A device stays Powered/Disabled until its port is reset; `Enable Slot`/`Address Device` target nothing otherwise. The detected speed selects EP0 Max Packet Size — and the values are speed-specific: **Low = 8, Full = 8 (default until the B.2 BSR pre-read learns the real value), High = 64, SuperSpeed = 512** (`bMaxPacketSize0 = 9`, i.e. 2^9). Programming 64 for a SuperSpeed device is a spec violation that breaks SS control transfers. PORTSC change bits are RW1C — a careless write clobbers them, a classic bug.

**Acceptance:**
- [ ] Per-port `PORTSC` accessed at `op + 0x400 + 0x10*(port-1)`; enumeration is **triggered by a Port Status Change event with `CSC=1`** (then read `CCS` for connect/disconnect) — distinguish the edge (`CSC`) from the level (`CCS`) so enumeration is neither missed nor duplicated
- [ ] On connect: USB2 ports get an explicit `PR` write, then wait `PRC=1`, RW1C-clear `PRC`, and confirm `PED=1` before `Enable Slot`; USB3 ports omit the `PR` write (controller-driven reset/training) and reach Enabled directly
- [ ] Port-speed field decoded → Low/Full/High/SuperSpeed → EP0 Max Packet Size = **8 / 8 / 64 / 512** (full-speed corrected via the B.2 BSR pre-read + Evaluate Context); host tests assert each speed, **including the SuperSpeed = 512 (`bMaxPacketSize0 = 9`) case**
- [ ] PORTSC writes apply a preserve-mask so RW1C change bits (`CSC`/`PEC`/`PRC`) are not accidentally cleared while writing `PR`; host-tested
- [ ] A connected `qemu-xhci` HID device's port reaches the Enabled state

---

## Track B — USB Core

### B.1 — Descriptor model + parser (host-testable in `kernel-core`)

**Files:**
- `kernel-core/src/usb/descriptor.rs` (new — pure-logic, host-tested)
- `userspace/lib/usb-core/` (new crate re-exporting the kernel-core types for ring-3 consumers)

**Symbol:** `DeviceDescriptor`, `ConfigDescriptor`, `InterfaceDescriptor`, `EndpointDescriptor`, `HidDescriptor`, `parse_config_tree`
**Why it matters:** Descriptor parsing is class-agnostic pure logic — it belongs in `kernel-core` where it is host-testable, unlike Redox which keeps it inside the `xhcid` binary. This is the shared foundation both the host enumerator and class drivers consume. `kernel-core/src/usb/` does not exist before this phase.

**Acceptance:**
- [ ] Typed structs for device/config/interface/endpoint + HID descriptors; `parse_config_tree` walks a configuration blob (short read then full read by `wTotalLength`) into typed interfaces + endpoints
- [ ] Host tests parse real captured descriptor blobs for a boot keyboard, a boot mouse, and a hub, asserting `bInterfaceClass`/`SubClass`/`Protocol` and endpoint addresses/`bInterval`
- [ ] `userspace/lib/usb-core` exposes these types to ring-3 drivers (added as a Cargo member in D.3)

### B.2 — Enumeration state machine (Enable Slot → Address Device BSR → descriptors → Configure Endpoint)

**Files:**
- `userspace/drivers/xhci/src/enumerate.rs` (new — drives the controller)
- `kernel-core/src/usb/enumerate.rs` (new — the state machine, host-tested against a mock command/event interface)

**Symbol:** `EnumState`, `enumerate_device`, `address_device` (BSR two-step), `control_transfer` (Setup/Data/Status), `configure_endpoint`, `evaluate_context`
**Why it matters:** xHCI replaces the raw USB `SET_ADDRESS` with the `Address Device` command, but `Enable Slot`, `Configure Endpoint`, and `Evaluate Context` are **all** required besides it to attach a device and run an interrupt endpoint. The BSR=1 pre-read is needed to learn full-speed EP0 Max Packet Size before the real address assignment.

**Acceptance:**
- [ ] `Enable Slot` → allocate Output Device Context → install in `DCBAA[slot]`; for full-speed: `Address Device` BSR=1 (Default state, no SET_ADDRESS) → read EP0 Max Packet Size → `Evaluate Context` to correct it → `Address Device` BSR=0 to assign the address
- [ ] **`Address Device` Input Context fields** populated and host-tested: Input Control Context **Add Flags = `0x3`** (`A0` Slot + `A1` EP0 Default Control Endpoint); Slot Context = Route String, Root Hub Port Number, Speed, Context Entries (1); EP0 Endpoint Context = EP Type **Control**, Max Packet Size (per A.7 speed: 8/8/64/512), TR Dequeue Pointer = EP0 transfer-ring IOVA with `DCS`, Error Count (`CErr`) = 3. (Missing these → Address Device returns a Context-State/Parameter error.)
- [ ] Control transfers issued as Setup Stage + (optional Data Stage) + Status Stage TRB sequences on the EP0 transfer ring; `GET_DESCRIPTOR(Device)` then `GET_DESCRIPTOR(Config)` short-then-full; `SET_CONFIGURATION(bConfigurationValue)`
- [ ] `Configure Endpoint` adds the interrupt-IN endpoint context after `SET_CONFIGURATION` (Add Flag at the endpoint's DCI per the A.5 formula)
- [ ] The enumeration state machine (states + transitions + error/timeout handling) is host-tested with a mock interface in `kernel-core`
- [ ] Under `qemu-xhci`, an attached `usb-kbd` enumerates to Configured with its interrupt-IN endpoint running; the full descriptor tree is printed on boot (Implementation-Outline step 2)

### B.3 — Hub class support + `PortId` topology

**Files:**
- `userspace/drivers/usbhub/` (new ring-3 driver, mirroring Redox `usbhubd`)
- `kernel-core/src/usb/hub.rs` (new — hub descriptor + `PortId` topology, host-tested)

**Symbol:** `enumerate_hub`, `set_port_feature` (`PORT_POWER`), `PortId { root_hub_port, hub_depth, parent }`
**Why it matters:** Downstream devices are unreachable until each hub (`bDeviceClass 0x09`) is enumerated, powered, and its ports reset. The dev laptop's six xHCI controllers each carry internal hub topology, and the USB tree must be representable for nested hubs.

**Acceptance:**
- [ ] Hub interface enumerated; `SetPortFeature(PORT_POWER)` issued per downstream port; downstream ports reset and walked
- [ ] `PortId` carries root-hub-port index + hub depth + parent so the topology is a tree; the topology model (insert nested child, resolve route string, walk parents) is **host-tested in `kernel-core`** so the nested-hub logic is verified even when QEMU cannot host a nested hub
- [ ] Best-effort live check: a hub-behind-hub device (QEMU `nec-usb-xhci` + `usb-hub`) enumerates end-to-end, or the QEMU limitation is documented (the host test above is the load-bearing verification, not the live run)
- [ ] Decision recorded: hub logic runs as the `usbhub` ring-3 driver (Redox model) with core logic in `usb-core`, rather than folded into the host binary

### B.4 — Host↔class IPC protocol crate + bulk via page grants

**Files:**
- `userspace/lib/usb-core/src/protocol.rs` (new — typed request/reply messages + thin client API)
- reuses the Phase 74 page-grant bulk transport

**Symbol:** `UsbRequest` (`GetDescriptors`, `ConfigureEndpoints`, `ControlRequest`, `SubmitTransfer`), `UsbClient` (the m3OS analogue of Redox `XhciClientHandle`)
**Why it matters:** This is the contract the host publishes and the class drivers consume. It must honor the m3OS IPC rule: descriptors and setup packets cross as small call/reply payloads; transfer buffers cross as **page-capability grants**, never as IPC payloads.

**Acceptance:**
- [ ] Typed protocol: open device, get-descriptors, configure-endpoints, control-request, submit interrupt/bulk transfer — defined once in `usb-core` and shared by `xhci`, `usbhub`, and `usb-hid`
- [ ] Descriptors + setup packets cross as IPC call/reply payloads; transfer buffers cross as Phase 74 page grants (verified: no transfer payload exceeds the IPC inline payload path)
- [ ] A thin `UsbClient` library API (not raw IPC opcodes) is what class drivers link. **Lifecycle model (reconciled with D.3):** the class drivers are **static long-lived daemons** started by `session_manager` (not per-device children forked by the host — the userspace-first rule forbids host-forks-children). On attach, the host (`xhci`) sends a **device-attach IPC notification** carrying `(port, interface class/subclass/protocol)` to the matching running daemon, which then drives that interface via `UsbClient`. The `(host endpoint, port, interface)` handoff is therefore an **IPC message to a running daemon**, never `exec` arguments to a freshly spawned process

---

## Track C — HID Class Driver (ring 3)

### C.1 — Boot-Protocol keyboard → `KeyEvent`

**Files:**
- `userspace/drivers/usb-hid/` (new crate)
- `kernel-core/src/usb/hid.rs` (new — `hid_usage_to_keycode` table + report decode, host-tested)

**Symbol:** `set_protocol`, `set_idle`, `parse_boot_keyboard_report`, `hid_usage_to_keycode`
**Why it matters:** This is the actual input path. `SET_PROTOCOL(0)` puts the device in Boot Protocol (no report-descriptor parsing needed); `SET_IDLE(0)` suppresses duplicate/streamed reports; the interrupt-IN endpoint must be brought into the controller via `Configure Endpoint` (B.2) and polled with Normal TRBs at `bInterval`.

**Acceptance:**
- [ ] Registers for `bInterfaceClass 0x03` / `SubClass 0x01` / `Protocol 0x01`; issues `SET_PROTOCOL(0)` (`bmRequestType 0x21`, `bRequest 0x0B`, `wValue 0`) and `SET_IDLE` with `wValue = (0 << 8) | 0` (duration 0 = report only on change, report ID 0 = all reports) so the keyboard does not stream duplicate reports
- [ ] Polls the interrupt-IN endpoint with Normal TRBs at `bInterval`; decodes the **first 8 bytes** of the boot report `[modifier][reserved][keycode0..keycode5]` (HID Usage IDs), handling the rollover/`0x01` error code
- [ ] HID Usage ID → `KeyEvent` (keycode/symbol/modifiers/`kind`) via the host-tested `hid_usage_to_keycode` table
- [ ] The `KeyEvent` is encoded with the existing `kernel-core` codec (`KEY_EVENT_WIRE_SIZE` = 20 bytes) — no new wire format introduced

### C.2 — Boot-Protocol mouse → `PointerEvent`

**File:** `userspace/drivers/usb-hid/src/mouse.rs`; `kernel-core/src/usb/hid.rs`
**Symbol:** `parse_boot_mouse_report`
**Why it matters:** The 3-byte boot mouse report maps directly to the Phase 56 relative-pointer model.

**Acceptance:**
- [ ] Registers for `bInterfaceClass 0x03` / `Protocol 0x02`; reads the endpoint's `wMaxPacketSize` bytes and decodes the **first 3 bytes** `[button bitfield][signed dx][signed dy]`, **ignoring any trailing bytes** (real boot mice often send 4+ bytes with a wheel in byte 4; the Boot Protocol only guarantees the first-3-byte layout, so the driver must accept a report `>= 3` bytes, not assume exactly 3)
- [ ] Produces a `PointerEvent` (relative `dx`/`dy` + button bitfield) via the existing `kernel-core` codec (`POINTER_EVENT_WIRE_SIZE` = 37 bytes)
- [ ] Host tests cover report decode including sign extension, button-bit mapping, **and a 4-byte report decoding to the same `PointerEvent` as its 3-byte prefix**

### C.3 — Inject USB input into `kbd_server` / `mouse_server` (Phase 56 dispatch unchanged)

**Files:**
- `userspace/kbd_server/src/main.rs`
- `userspace/mouse_server/src/main.rs`

**Symbol:** new inbound IPC labels `KBD_EVENT_INJECT` / `MOUSE_EVENT_INJECT`; a bounded pending-event queue in `KeyboardPipeline` / the mouse pipeline; merge into the existing `KBD_EVENT_PULL` (label 2) / `MOUSE_EVENT_PULL` (label 1) replies
**Why it matters:** Making USB an additional **producer** keeps `display_server`'s `InputWiring` and the `InputDispatcher` (`kernel-core/src/input/dispatch.rs:304`/`:379`) completely unchanged — USB and PS/2 merge into the same pull stream the compositor already drains. **Source-verified scope note:** `kbd_server`/`mouse_server` today are strictly **synchronous single-endpoint loops** (`ipc_recv` → match label → `ipc_reply`) with **no pending-event buffer** — there is nowhere for an asynchronously injected event to land. So this task is a real change to those servers, not just "add a label": it must add a bounded pending-event queue and define how an inject `ipc_call` interleaves with `PULL` waiters on the single endpoint.

**Acceptance:**
- [ ] `kbd_server` gains a **bounded pending-`KeyEvent` queue** in `KeyboardPipeline`; a new `KBD_EVENT_INJECT` handler enqueues pushed `KeyEvent`s from `usb-hid`, and `handle_kbd_event_pull` drains that queue **and** the PS/2 (`SYS_READ_SCANCODE`, `0x1007`) stream into each `KBD_EVENT_PULL` reply, with a defined drain priority (injected vs PS/2) and a defined inject reply contract
- [ ] `mouse_server` gains the analogous bounded pending-`PointerEvent` queue + `MOUSE_EVENT_INJECT` handler, merged into `MOUSE_EVENT_PULL` replies alongside the PS/2 packet (`SYS_READ_MOUSE_PACKET`, `0x1015`) stream
- [ ] The single-endpoint interleaving of an inject `ipc_call` with `PULL` waiters is defined (no reply-cap collision, no dropped events under a full queue) and documented
- [ ] `InputDispatcher::route_key_event` / `route_pointer_event` and `display_server/src/input.rs::InputWiring` are unchanged (verified by diff)
- [ ] PS/2 input still works under QEMU's i8042 emulation — both producers coexist (no regression)
- [ ] The rejected alternative (`usb-hid` as a third direct `display_server` `InputSource`) is documented with the reason (would fork focus/grab routing outside the single dispatcher)

### C.4 — Report-Protocol skeleton (deferred unless time permits)

**File:** `kernel-core/src/usb/hid_report.rs` (new)
**Symbol:** `parse_report_descriptor`
**Why it matters:** Report-Protocol parsing unlocks touchpads, gaming mice, and multi-touch — but Boot Protocol is sufficient for every 1.0 keyboard and mouse, so this is genuinely deferrable.

**Acceptance:**
- [ ] A minimal report-descriptor item parser (Input items, Usage Page, Usage, Report Size, Report Count) deriving field bit-offsets — **host-tested only**
- [ ] Explicitly **not** wired to any live device for 1.0; the design-doc "Deferred Until Later" entry for Report Protocol is honored

---

## Track D — Kernel-Side Wiring + Integration

### D.1 — Ring-3 PCI class enumeration for controller discovery

**Files:**
- `kernel/src/syscall/device_host.rs` (new minimal enumeration syscall, if added)
- `kernel-core/src/device_host/syscalls.rs` (syscall-number registration, currently `0x1120`–`0x1126`)

**Symbol:** `sys_device_pci_enumerate(class, subclass, prog_if)` (new, committed); `crate::pci::PciMatch::ClassSubclass` (`kernel/src/pci/mod.rs:1555`, currently `#[allow(dead_code)]`) as the in-kernel matcher to expose
**Why it matters:** **Source-verified gap.** `sys_device_claim` takes a BDF only — there is no class-code filter, and no ring-3 PCI-config-space read path exists (NVMe/e1000 hardcode `SENTINEL_BDF`). The design doc's "filter on class code `0x0C0330`" assumes a primitive that does not exist. Because the **headline milestone is the no-PS/2 laptop with six xHCI controllers**, the multi-controller path cannot be left "if added" — it is committed here (a single QEMU controller still bootstraps on a sentinel BDF as an interim, but is not the deliverable).

**Acceptance:**
- [ ] A new capability-gated syscall `sys_device_pci_enumerate(class, subclass, prog_if)` is added to `kernel/src/syscall/device_host.rs` (next free number after `0x1126` in `kernel-core/src/device_host/syscalls.rs`), returning the BDFs of every device matching class `0x0C0330`, built on the existing in-kernel `PciMatch::ClassSubclass` matcher (`pci/mod.rs:1555`, removing its `dead_code` allow)
- [ ] The new syscall is gated by the same `/drivers/` exec-path authorization as `sys_device_claim` (`is_authorized_driver_process`, `device_host.rs:126`)
- [ ] `xhci` discovers **all** xHCI controllers via this enumeration and claims each (one driver instance per controller, or one instance iterating the set) — no hardcoded BDF on the real-hardware path; the `qemu-xhci` sentinel BDF remains only as an interim bootstrap
- [ ] Host test: the enumeration filter returns exactly the class-`0x0C0330` BDFs from a synthetic PCI device list
- [ ] The design-doc A.1/D.1 wording is corrected (done in this phase's design-doc edit) to mark class enumeration as a committed new requirement, not an existing primitive

### D.2 — PCI enablement for xHCI: Bus Master Enable + MSI-X programming

**Files:**
- `kernel/src/syscall/device_host.rs` (`sys_device_claim`, line 596; `sys_device_irq_subscribe`, line 1478; `allocate_device_vector`, line 1653)
- `kernel/src/pci/mod.rs` (`claim_pci_device_by_bdf`, line 654; the `write_config_u16(0x04, …)` BME pattern used by the in-kernel virtio drivers)
- `kernel/src/arch/x86_64/interrupts.rs` (`register_device_irq`, line 2297; `dispatch_device_irq`, line 2359)

**Symbol:** PCI Command register (offset `0x04`) Bus Master Enable + Memory Space; `sys_device_irq_subscribe` MSI-X capability + table programming
**Why it matters:** **Source-verified CRITICAL gap.** xHCI is a pure bus-master DMA device — it DMAs nothing and posts **zero** events until **PCI Bus Master Enable** (Command reg `0x04`, bit 2) and Memory Space (bit 0) are set. The only code in the tree that sets BME today is the **in-kernel** virtio drivers (`kernel/src/net/virtio_net.rs`, `kernel/src/blk/virtio_blk.rs`, via `write_config_u16(0x04, cmd | 0x05)`); the ring-3 claim path (`sys_device_claim` → `claim_pci_device_by_bdf`) does **not** enable BME, and there is no ring-3 PCI-config-write syscall. If BME stays off, the controller reaches RUN and the driver waits on the event ring forever with no diagnostic — this is the most likely "implementer hits a wall" failure mode. Separately, INTx is unreliable on xHCI, so MSI-X must be the live IRQ path.

**Acceptance:**
- [ ] `sys_device_claim` enables PCI **Bus Master Enable + Memory Space** (Command reg `0x04` bits `0|2`) for the claimed device — mirroring the in-kernel virtio `write_config_u16(0x04, cmd | 0x05)` — verified by reading back the Command register after claim; A.3's run sequence asserts BME is on before `USBCMD.R/S=1`
- [ ] `sys_device_irq_subscribe` programs the xHCI controller's PCI MSI-X capability + table (vector, message address `0x04` data) with the vector address+data written and the MSI-X **Enable** bit set (not just kernel-vector allocation); the MSI-X-if-advertised ordering is the `allocate_device_vector` doc comment at `kernel/src/syscall/device_host.rs:1653`
- [ ] Any gap exposed because the existing path was only exercised by NVMe/e1000 is closed concretely — defined as: the xHCI MSI-X table is programmed with vector address+data and `IMAN.IE` is set, confirmed by the controller posting an MSI-X interrupt in the end-to-end check below
- [ ] Verified end-to-end: with BME on and MSI-X enabled, an xHCI event posts an MSI-X interrupt that signals the driver's `Notification` (the A.6 proof)

### D.3 — Build + ramdisk + service wiring (ring-3 driver variant of the 4-place flow)

**Files:**
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`build_userspace_bins`, line 795; `bins` array, line 800; `populate_ext2_files` service configs, ~line 12597)
- `kernel/src/fs/ramdisk.rs` (`DRIVERS_ENTRIES`, line 1150; mounted at `/drivers`, line 1196)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`, lines 185–230)
- `kernel/initrd/etc/services.d/` (new `*.conf` files)

**Symbol:** `bins` tuple `(pkg, bin, needs_alloc=true)`, `DRIVERS_ENTRIES`, `xhci.conf`/`usb-core.conf`/`usb-hid.conf`
**Why it matters:** **Source-verified gotcha.** Ring-3 drivers must be staged in `DRIVERS_ENTRIES` (under `/drivers/`), **not** `BIN_ENTRIES` (under `/bin/`), or the `is_authorized_driver_process` gate (`device_host.rs:126`) denies `sys_device_claim`. Service configs for daemons use `restart=on-failure` and `command=/drivers/<name>`.

**Acceptance:**
- [ ] `xhci`, `usbhub`, `usb-hid`, and the `usb-core` lib added as Cargo `members`; driver binaries added to the `bins` array with `needs_alloc = true` (they depend on `kernel-core`)
- [ ] Driver binaries embedded in `DRIVERS_ENTRIES` (`ramdisk.rs`) at `/drivers/xhci`, `/drivers/usbhub`, `/drivers/usb-hid`; any smoke helper bins go in `BIN_ENTRIES`
- [ ] `xhci.conf` / `usb-core.conf` / `usb-hid.conf` added to `kernel/initrd/etc/services.d/` **and** `init` `KNOWN_CONFIGS`; each uses `command=/drivers/<name>`, `type=daemon`, `restart=on-failure`, with `depends=` ordering `xhci → usb-core → usb-hid`
- [ ] **Class drivers are static `type=daemon` services** started by `session_manager` (`DECLARED_SESSION_STEP_NAMES`, `kernel-core/src/session_supervisor.rs:89`) before `greeter`. On attach, `xhci` sends a device-attach IPC notification to the running `usb-hid` daemon (the B.4 model) — the daemon is **not** forked per device, honoring the userspace-first "no host-forks-children" rule. (One long-lived `usb-hid` claims HID interfaces dynamically over IPC; it is not parameterized by `exec` args.)
- [ ] `cargo xtask clean` run after adding the configs (forces ext2 disk recreation)

### D.4 — `usb-smoke` acceptance gate (QMP + serial; asserts a real transfer)

**Files:**
- `xtask/src/main.rs` (new `cmd_usb_smoke`; QEMU arg additions; `smoke_test_script` step or dedicated subcommand)
- `userspace/drivers/usb-hid/` (PASS sentinel) and/or `userspace/smoke-runner/src/main.rs`

**Symbol:** `cmd_usb_smoke`, QEMU `-device qemu-xhci -device usb-kbd -device usb-mouse`, `SMOKE:usb:PASS`
**Why it matters:** **Source-verified gap:** there is **no** `qemu-xhci`/`usb-kbd` device in the xtask QEMU args today. A serial `[xhci] N ports detected` sentinel proves only that the daemon ran — not that the event ring and interrupter delivered a real HID report. Per the AGENTS.md headless-framebuffer guidance, real rendering/input must be asserted via QMP, not a serial wait.

**Acceptance:**
- [ ] QEMU launched with `-device qemu-xhci` plus `-device usb-kbd` and `-device usb-mouse` (new in the xtask QEMU arg builder)
- [ ] The gate asserts, **in causal order** (the emulated `usb-kbd` only emits an interrupt-IN report in response to an injected key — so injection precedes the Transfer-event observation, never after): (1) an `Enable Slot` Command Completion event is observed (event ring + interrupter live); (2) a QMP `send-key` is injected into the emulated `usb-kbd`; (3) the resulting interrupt-IN **Transfer event** carrying the 8-byte boot report is observed and decoded to a `KeyEvent`; (4) the keystroke reaches the login/shell prompt (USB → `usb-hid` → `kbd_server` → prompt), verified via QMP `screendump` (PPM occupancy) or a serial echo
- [ ] Mouse path: a QMP `input-send-event` relative mouse motion is injected into `usb-mouse` and the resulting `PointerEvent` is asserted to reach `mouse_server` (or, if `input-send-event` mouse injection proves unreachable in the harness, the C.2 mouse path is explicitly marked host-test-only and the gate does not imply live mouse verification)
- [ ] A serial sentinel alone (e.g. `[xhci] N ports detected`) is explicitly **not** sufficient for PASS
- [ ] Wired as `cargo xtask usb-smoke` with an opt-in pre-push gate `M3OS_USB_REGRESSION=1` (mirrors the heavyweight `htop-render-probe` / `compositor-stress` gates and the AGENTS.md hooks table)
- [ ] PS/2 i8042 input still passes its existing `smoke-test` coverage (no regression)

---

## Track E — Documentation + Release

### E.1 — Create the Phase 78 learning doc

**File:** `docs/78-usb-host-foundation.md`
**Symbol:** N/A
**Why it matters:** A learner-friendly doc scoped to Phase 78 consolidates the USB bring-up story — TRB rings, the event-ring/interrupter completion model, descriptor-tree enumeration, and HID Boot Protocol — so readers do not reconstruct it from four tracks. Follows the "aligned legacy learning doc" template in `docs/appendix/doc-templates.md`.

**Acceptance:**
- [ ] File exists at `docs/78-usb-host-foundation.md`
- [ ] Required template fields populated: `**Aligned Roadmap Phase:** Phase 78`, `**Status:**`, `**Source Ref:** phase-78`, `**Supersedes Legacy Doc:** new`
- [ ] Overview explains, learner-first, why USB-HID is the 1.0 real-hardware unblocker (modern laptops have no PS/2 port) and how a ring-3 + IOMMU-DMA driver issues hardware transfers safely
- [ ] "What This Doc Covers" walks TRB rings, the event ring + interrupter (why an IRQ, not a poll, signals completion), the enumeration descriptor walk, and the HID boot-report layouts
- [ ] Key Files table cites the **real** files (`userspace/drivers/xhci`, `userspace/drivers/usbhub`, `userspace/drivers/usb-hid`, `userspace/lib/usb-core`, `kernel-core/src/usb/`, `kernel/src/syscall/device_host.rs`, the `kernel-core/src/input` codecs)
- [ ] "How This Phase Differs From Later USB Work" notes the deferrals (mass storage, UVC, USB audio, Report Protocol, hot-plug surface)
- [ ] Related Roadmap Docs links the design doc + this task list
- [ ] Authored **after** Tracks A–D so it cites the actual mechanism chosen (sentinel-BDF vs class enumeration, MSI-X, the `kbd_server`/`mouse_server` inject path)

### E.2 — Bump kernel version to `0.78.0`

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]` (currently `0.77.0`, line 3)
**Why it matters:** Project convention is one minor-version bump per shipped phase; disciplined version tracking signals a complete, shippable phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.78.0"`
- [ ] `Cargo.lock` regenerated (via `cargo xtask check`)
- [ ] `AGENTS.md` kernel version updated to `v0.78.0` and a new **"USB host stack"** capability-class bullet added (this introduces a new capability class, so a bullet is warranted under the file's keep-it-small maintenance policy — detailed record stays in `docs/roadmap/`)
- [ ] `docs/roadmap/README.md` Phase 78 row Status updated to "Complete" and the Tasks cell pointed at this list; design-doc + task-doc Status headers set to Complete
- [ ] `cargo xtask check` passes
- [ ] Git tag `v0.78.0` — recommended at phase merge (left to the merge step)

---

## Documentation Notes

- **Six design-doc claims were source-verified as stale and corrected** (see the Review note under the header): wrong syscall names (`sys_device_claim`/`sys_device_mmio_map`/`sys_device_dma_alloc`/`sys_device_irq_subscribe`, not `pci_probe`/`mmio`-`iommu_map_bar`/`irq_bind`), no class-code filter primitive, `/drivers/` staging via `DRIVERS_ENTRIES`, the large set of omitted-but-mandatory xHCI essentials, the already-stable `KeyEvent`/`PointerEvent` codecs, and the missing `SET_IDLE`. The Phase 78 **design doc** is amended in the same PR.
- **The biggest risk is scope.** This is a from-scratch host-controller stack, not a bundle of small fixes. Track A alone (register discovery → reset → DCBAA → scratchpad → command/event rings → MSI-X → PORTSC) is the bulk of the work and must reach a first Command Completion event before B/C are meaningful. If needed, split delivery into **78a/78b/78c** sub-phases (Phase 76 precedent) and bump to `0.78.0` only at the last.
- **Redox `xhcid` is the closest blueprint** and should guide the internal module split (capability/operational/runtime/doorbell/context/ring/trb/event/port/extended/irq_reactor/device_enumerator). Two deliberate divergences from Redox: (1) back every DMA structure with the IOMMU-routed `DmaBuffer<T>` so the controller is confined to granted pages (Redox hands raw physical addresses); (2) route class-driver spawning through `session_manager` rather than letting the host fork/supervise children, keeping capability minting in the trusted service manager.
- **Host-test everything that is pure logic.** Per the project's `kernel-core` discipline, the register-field decoders, TRB encode/decode + cycle bit, context layouts (both CSZ sizes), PORTSC bit logic, descriptor parser, enumeration state machine, and HID report/usage decoders all live in `kernel-core/src/usb/` and are testable on the host — only the MMIO/DMA/IRQ glue is ring-3-only.
- **Acceptance is QMP-driven, not serial-only.** A `[xhci] N ports detected` line proves the daemon ran; it does not prove the event ring, interrupter, enumeration, or HID path work. The `usb-smoke` gate (D.4) must assert a real Command Completion event, a real boot-keyboard Transfer event, and a keystroke reaching the prompt via QMP `send-key` + `screendump`.
- **The HID-input integration keeps Phase 56 untouched** by making `usb-hid` an injector into `kbd_server`/`mouse_server` (C.3) rather than a new dispatcher client — the `InputDispatcher` and `display_server` `InputWiring` are unchanged, and PS/2 and USB coexist as parallel producers.
- **Bus Master Enable is the highest-risk silent failure (D.2).** xHCI DMAs nothing and posts no events until PCI Bus Master Enable (Command reg `0x04` bit 2) is set, and the ring-3 claim path does **not** set it today (only the in-kernel virtio drivers do, via `write_config_u16(0x04, cmd|0x05)`). If an implementer skips this, the controller reaches RUN and the event-ring wait hangs forever with no diagnostic. A.3's run sequence asserts BME is on before `R/S=1`.
- **MSI-X is load-bearing and partially pre-built.** The substrate prefers MSI-X in `sys_device_irq_subscribe` (`allocate_device_vector` doc comment at `device_host.rs:1653`); D.2 verifies it actually programs the xHCI controller's MSI-X table (not just a vector), because INTx-only delivery on `qemu-xhci` is unreliable.
- **No userspace thread primitive exists.** `syscall-lib` has no `thread_create`/`clone`, so the xHCI driver is a single-threaded drain-on-wake event loop (the NVMe `wait_completion` model), not a reactor thread (A.6). HID `bInterval` polling is serviced from the same loop. Genuine concurrency would be a separate prerequisite.
- **The `kbd_server`/`mouse_server` inject (C.3) is a real server change**, not just a new label: those servers are synchronous single-endpoint pull loops with no pending-event buffer, so C.3 adds a bounded pending-event queue and defines the inject/PULL interleaving.
- After adding any new service config or staged driver binary (D.3), run `cargo xtask clean` to force ext2 disk recreation.
