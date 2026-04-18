# Phase 55 — Hardware Substrate: Task List

**Status:** Complete
**Source Ref:** phase-55
**Depends on:** Phase 15 (Hardware Discovery) ✅, Phase 16 (Network) ✅, Phase 24 (Persistent Storage) ✅, Phase 54 (Deep Serverization) ✅
**Goal:** Extend the QEMU/VirtIO-first system into a narrow, testable real-hardware story with a documented donor strategy, a reusable hardware-access layer, and at least one serious real-hardware storage and networking path on named reference targets.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Donor strategy and reference hardware matrix | None | ✅ Done |
| B | PCI modernization (PCIe MCFG, MSI/MSI-X, device binding) | A | ✅ Done |
| C | Hardware-access layer (BAR mapping, DMA, IRQ delivery) | B, Phase 53a (buddy allocator) | ✅ Done |
| D | NVMe storage driver | B, C | ✅ Done |
| E | Intel e1000 network driver | B, C | ✅ Done |
| F | Validation, documentation, and version | D, E | ✅ Done |

---

## Track A — Donor Strategy and Reference Hardware Matrix

### A.1 — Finalize and cross-reference the driver-sourcing donor strategy

**File:** `docs/evaluation/hardware-driver-strategy.md`
**Symbol:** `Recommended donor strategy by source` (section heading)
**Why it matters:** The evaluation docs already contain a comprehensive donor strategy (specs first, Redox second, BSD third, Linux as reference). This task ensures the strategy is explicitly adopted as Phase 55 policy, that any gaps identified during implementation planning are closed, and that the adoption is recorded as a concrete cross-reference rather than an implicit assumption.

**Acceptance:**
- [ ] `docs/evaluation/hardware-driver-strategy.md` is reviewed and any implementation-discovered corrections are applied
- [ ] `docs/evaluation/redox-driver-porting.md` is reviewed for alignment with the NVMe and e1000 targets chosen in A.2
- [ ] The Phase 55 learning doc (`docs/55-hardware-substrate.md`) contains a "Donor Strategy" section that cross-references the evaluation doc and records which specific Redox drivers were used as references
- [ ] Any deviation from the donor strategy during Phase 55 is documented with rationale in the learning doc

### A.2 — Define the reference hardware matrix

**File:** `docs/roadmap/55-hardware-substrate.md`
**Symbol:** `Reference hardware matrix` (new section or appendix)
**Why it matters:** "Works on real hardware" means nothing without named targets. The matrix makes the support promise bounded and testable.

**Acceptance:**
- [ ] The matrix names at least one storage target (NVMe-class) and one network target (Intel e1000-class)
- [ ] Each target entry records: device class, PCI vendor/device IDs, QEMU emulation flag, physical test hardware (if available), and validation status
- [ ] The matrix distinguishes QEMU-emulated targets (validated in CI) from physical-hardware targets (validated manually)
- [ ] Physical-hardware entries carry an explicit IOMMU caveat: "VT-d / AMD-Vi enabled systems may block driver DMA until IOMMU mappings exist; IOMMU support is deferred per Phase 55 design doc"
- [ ] The matrix is referenced from both the Phase 55 design doc and the Phase 55 learning doc

### A.3 — Document QEMU validation configurations for reference targets

**File:** `docs/55-hardware-substrate.md` (learning doc) and/or `docs/roadmap/55-hardware-substrate.md`
**Symbol:** `Reference QEMU configurations` (subsection)
**Why it matters:** The QEMU command lines used to validate NVMe and e1000 bring-up must be recorded before driver development starts so development and CI target the same configuration. The xtask integration that exposes these as flags is handled separately in F.1 — A.3 is the documentation precursor so implementation and documentation cannot drift.

**Acceptance:**
- [ ] QEMU NVMe configuration recorded: `-drive file=nvme.img,if=none,id=nvme0 -device nvme,serial=deadbeef,drive=nvme0`
- [ ] QEMU e1000 configuration recorded: `-device e1000,netdev=net0 -netdev user,id=net0`
- [ ] The recorded configurations are explicitly cross-referenced from F.1's xtask `--device` flags
- [ ] Existing VirtIO configurations remain the default and are not broken by the documented commands

### A.4 — Verify evaluation gate checks before closing Phase 55

**File:** `docs/roadmap/55-hardware-substrate.md`
**Symbol:** `Evaluation Gate` (table)
**Why it matters:** The design doc defines four evaluation gates (service-boundary readiness, donor-source readiness, validation environment, release posture) that must be verified before the phase can close. Without an explicit verification task, these gates are likely to be skipped.

**Acceptance:**
- [ ] Service-boundary readiness: confirm Phase 54 has narrowed the kernel enough that NVMe and e1000 drivers do not widen the TCB beyond the hardware-access layer contract
- [ ] Donor-source readiness: confirm specs, Redox references, and any BSD/Linux behavioral references are identified for each shipped driver
- [ ] Validation environment: confirm the reference machines, QEMU configs, or lab setup are documented and reproducible
- [ ] Release posture: confirm the project has an agreed narrow hardware promise recorded in the reference matrix
- [ ] Gate verification results are documented in the Phase 55 learning doc or the design doc itself

---

## Track B — PCI Modernization

### B.1 — Parse MCFG ACPI table and enable PCIe MMIO configuration space

**Files:**
- `kernel/src/acpi/mod.rs`
- `kernel/src/pci/mod.rs`

**Symbol:** `McfgEntry`, `pcie_mmio_config_read`, `pcie_mmio_config_write`
**Why it matters:** The current PCI implementation uses legacy I/O ports (`0xCF8`/`0xCFC`) which limit config space to 256 bytes per function. PCIe MMIO (via the ACPI MCFG table) exposes the full 4096-byte extended config space needed for capability parsing (MSI/MSI-X, power management, AER) used by NVMe and other modern devices.

**Acceptance:**
- [ ] MCFG ACPI table is parsed during ACPI init, extracting base address, segment group, and bus range
- [ ] `pcie_mmio_config_read(bus, device, function, offset)` reads from the MMIO-mapped extended config space
- [ ] `pcie_mmio_config_write(bus, device, function, offset, value)` writes to MMIO config space
- [ ] Legacy I/O-based `pci_config_read_*` / `pci_config_write_*` remain as fallback when MCFG is absent
- [ ] Extended config space reads (offsets >= 256) work correctly for devices on MCFG-covered bus segments
- [ ] All existing QEMU tests pass unchanged

### B.2 — Add MSI/MSI-X capability parsing and vector allocation

**File:** `kernel/src/pci/mod.rs`
**Symbol:** `MsiCapability`, `MsixCapability`, `allocate_msi_vectors`
**Why it matters:** NVMe controllers require MSI or MSI-X for completion queue interrupt delivery. Legacy INTx interrupts are shared and slow. MSI/MSI-X provides dedicated per-queue interrupt vectors that eliminate spurious interrupt overhead.

**Acceptance:**
- [ ] PCI capability list walking finds MSI (capability ID `0x05`) and MSI-X (capability ID `0x11`) structures
- [ ] `MsiCapability` parses message address, message data, and per-vector mask fields
- [ ] `MsixCapability` parses table offset/BIR and PBA offset/BIR, maps the MSI-X table via BAR
- [ ] `allocate_msi_vectors(device, count)` programs the MSI/MSI-X capability and returns allocated APIC vector numbers
- [ ] Allocated vectors are routed to the correct APIC IDT entries
- [ ] Devices without MSI/MSI-X fall back to legacy INTx interrupt routing

### B.3 — Create PCI device claim and driver binding protocol

**File:** `kernel/src/pci/mod.rs`
**Symbol:** `PciDevice`, `claim_pci_device`, `PCI_DEVICE_REGISTRY`
**Why it matters:** Currently PCI enumeration stores results in a flat `PCI_DEVICES` array with no ownership tracking. Multiple drivers probing for the same device would conflict. A claim protocol prevents double-binding and gives each driver a stable handle to its assigned device.

**Acceptance:**
- [ ] `PciDevice` struct wraps a PCI function's bus/device/function address with vendor/device/class metadata
- [ ] `claim_pci_device(vendor_id, device_id)` returns an exclusive `PciDevice` handle or an error if already claimed
- [ ] `PCI_DEVICE_REGISTRY` tracks which devices are claimed and by which driver
- [ ] Claimed devices expose config-space read/write through the `PciDevice` handle (not global functions)
- [ ] The existing VirtIO-blk and VirtIO-net init paths are migrated to use `claim_pci_device` without behavior change
- [ ] All existing QEMU tests pass unchanged

---

## Track C — Hardware-Access Layer

### C.1 — BAR mapping abstraction for MMIO and port I/O

**File:** `kernel/src/pci/bar.rs` (new)
**Symbol:** `BarMapping`, `MmioRegion`, `map_bar`
**Why it matters:** Every hardware driver needs access to device registers through BAR-mapped MMIO or port I/O. Currently, VirtIO drivers hardcode BAR0 port I/O addresses. A reusable BAR mapping abstraction prevents each new driver from inventing its own register-access scheme.

**Acceptance:**
- [ ] `BarMapping` enum distinguishes MMIO (memory-mapped) and PIO (port I/O) BARs
- [ ] `map_bar(pci_device, bar_index)` reads the BAR register, determines type and size, and returns a `BarMapping`
- [ ] MMIO BARs are mapped into kernel virtual address space with uncacheable memory type
- [ ] `MmioRegion` provides `read_reg::<T>(offset)` and `write_reg::<T>(offset, value)` using volatile operations
- [ ] PIO BARs provide equivalent typed port I/O wrappers
- [ ] BAR size detection uses the standard write-ones/read-back PCI BAR sizing algorithm
- [ ] At least 4 tests in `kernel-core` cover BAR decoding: (1) 32-bit MMIO BAR type + size, (2) PIO BAR type + size, (3) 64-bit MMIO BAR spanning two BAR slots with correct upper-half decoding, (4) prefetchable flag and zero-size BAR handling

### C.2 — DMA buffer allocation and management

**File:** `kernel/src/mm/dma.rs` (new)
**Symbol:** `DmaBuffer`, `DmaPool`, `alloc_dma_buffer`
**Why it matters:** Queue-based drivers (NVMe, e1000) require physically-contiguous, DMA-safe memory for descriptor rings, command queues, and data buffers. A shared DMA abstraction prevents each driver from directly calling the frame allocator and manually tracking physical-to-virtual address pairs.

**Acceptance:**
- [ ] `DmaBuffer<T>` holds both the kernel virtual address and the corresponding physical (bus) address
- [ ] `alloc_dma_buffer(size, alignment)` allocates physically-contiguous frames via the buddy allocator and maps them with appropriate cache attributes
- [ ] `DmaBuffer<T>` implements `Deref<Target = T>` and `DerefMut` for ergonomic access
- [ ] `Drop` for `DmaBuffer` returns frames to the buddy allocator
- [ ] `DmaBuffer::physical_address()` returns the bus-visible physical address for programming into device descriptor rings
- [ ] Alignment guarantees are explicit: callers can request page-aligned or device-specific alignment
- [ ] At least 3 tests cover allocation, deref access, and drop/reclaim behavior

### C.3 — IRQ delivery contract for device drivers

**Files:**
- `kernel/src/pci/mod.rs`
- `kernel/src/arch/x86_64/interrupts.rs` (or equivalent IDT module)

**Symbol:** `DeviceIrq`, `register_device_irq`, `device_irq_handler`
**Why it matters:** Drivers need a uniform way to register interrupt handlers and receive IRQ notifications. Currently, VirtIO drivers wire interrupt handling through ad-hoc `AtomicBool` flags checked in ISR context. A reusable contract lets MSI/MSI-X and legacy INTx interrupts share one driver-facing interface.

**Acceptance:**
- [ ] `DeviceIrq` struct represents an allocated interrupt vector (MSI, MSI-X, or legacy INTx)
- [ ] `register_device_irq(vector, handler_fn)` installs a driver-provided handler for the vector
- [ ] The handler runs in ISR context with the standard constraints: no allocation, no blocking, ack interrupt and signal a wait queue or notification
- [ ] Legacy INTx handlers check the ISR status register to avoid spurious interrupt work
- [ ] MSI/MSI-X handlers are vector-specific with no sharing and no ISR status check needed
- [ ] The existing VirtIO-blk and VirtIO-net ISR paths can be expressed through `DeviceIrq` without behavior change

### C.4 — Device driver registration and discovery integration

**File:** `kernel/src/pci/mod.rs`
**Symbol:** `DriverEntry`, `register_driver`, `probe_all_drivers`
**Why it matters:** A lightweight driver registration mechanism lets the kernel discover and bind drivers to PCI devices during init, replacing the current pattern where each subsystem independently scans `PCI_DEVICES`. This keeps discovery centralized and makes adding new drivers a one-line registration.

**Acceptance:**
- [ ] `DriverEntry` struct pairs a PCI match rule (vendor/device ID or class code) with an init function pointer
- [ ] `register_driver(entry)` adds a driver to the global driver table
- [ ] `probe_all_drivers()` iterates unclaimed PCI devices and calls matching driver init functions
- [ ] Probe order is deterministic (e.g., by bus/device/function number)
- [ ] The existing VirtIO-blk and VirtIO-net initialization is migrated to use `register_driver` and `probe_all_drivers`
- [ ] New NVMe and e1000 drivers register through the same mechanism

### C.5 — Migrate existing VirtIO drivers to the hardware-access layer

**Files:**
- `kernel/src/blk/virtio_blk.rs`
- `kernel/src/net/virtio_net.rs`
- `kernel/src/pci/mod.rs`

**Symbol:** `virtio_blk_init`, `virtio_net_init`
**Why it matters:** The VirtIO drivers are the only existing hardware drivers and the best validators for the new hardware-access layer. Migrating them before building NVMe and e1000 ensures that BAR mapping, DMA, IRQ delivery, and device binding work end-to-end on a known-good driver before being used by new code.

**Acceptance:**
- [ ] VirtIO-blk initialization uses `claim_pci_device` instead of manual PCI scan
- [ ] VirtIO-blk uses `map_bar` instead of hardcoded BAR0 I/O port extraction
- [ ] VirtIO-blk uses `DmaBuffer` for virtqueue descriptor and buffer allocations instead of raw `alloc_contiguous_frames`
- [ ] VirtIO-blk uses `register_device_irq` instead of ad-hoc ISR wiring
- [ ] VirtIO-blk completion path switches from spin-polling the used ring in `read_sectors` / `write_sectors` to IRQ-driven wakeup: the IRQ handler walks the used ring and wakes one task per completion via `wake_task`, and requesters `block_current_unless_woken` instead of busy-looping while holding `DRIVER` (routed here from `docs/debug/54-followups.md` item 5)
- [ ] VirtIO-net receives the same migration as VirtIO-blk, including IRQ-driven receive completion
- [ ] Both VirtIO drivers register through `register_driver` / `probe_all_drivers`
- [ ] All existing QEMU tests pass unchanged after the migration

---

## Track D — NVMe Storage Driver

### D.0 — NVMe register and command definitions in kernel-core

**File:** `kernel-core/src/nvme.rs` (new)
**Symbol:** `NvmeRegs`, `NvmeCommand`, `NvmeCompletion`, `NvmeCap`
**Why it matters:** Register offsets, command opcodes, capability bit layouts, and descriptor formats are pure data definitions with no hardware dependency. Placing them in `kernel-core` makes them host-testable and keeps the kernel-side `nvme.rs` focused on hardware interaction rather than data-format parsing.

**Acceptance:**
- [ ] `NvmeRegs` defines named register offsets: `CAP` (0x00), `VS` (0x08), `CC` (0x14), `CSTS` (0x1C), `AQA` (0x24), `ASQ` (0x28), `ACQ` (0x30), doorbell stride
- [ ] `NvmeCommand` is a 64-byte `#[repr(C)]` struct with opcode, flags, command ID, namespace ID, PRP1, PRP2, and CDW10-15 fields
- [ ] `NvmeCompletion` is a 16-byte `#[repr(C)]` struct with command-specific result, SQ head, SQ ID, command ID, status/phase fields
- [ ] `NvmeCap` provides accessor methods for parsing the 64-bit CAP register (MQES, CQR, doorbell stride, timeout, CSS)
- [ ] Admin and I/O opcode constants are defined (Identify `0x06`, Create I/O CQ `0x05`, Create I/O SQ `0x01`, Read `0x02`, Write `0x01`)
- [ ] At least 3 host tests validate command construction, capability parsing, and completion status extraction
- [ ] All tests pass via `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`

### D.1 — NVMe controller discovery and register mapping

**File:** `kernel/src/blk/nvme.rs` (new)
**Symbol:** `NvmeController`, `nvme_probe`
**Why it matters:** NVMe is the dominant modern storage interface. Its queue-based, register-mapped design is architecturally close to VirtIO, making it the highest-leverage first real-hardware storage target. This task establishes PCI discovery and BAR0 MMIO register access for the NVMe controller.

**Acceptance:**
- [ ] `nvme_probe` claims the NVMe PCI device (class `01:08:02`, or vendor/device match) via `claim_pci_device`
- [ ] BAR0 is mapped via the hardware-access layer (`map_bar`) as an MMIO region
- [ ] NVMe controller registers are accessible: `CAP`, `VS`, `CC`, `CSTS`, `AQA`, `ASQ`, `ACQ`
- [ ] Controller reset sequence executes: disable (`CC.EN=0`), wait for `CSTS.RDY=0`, configure, enable (`CC.EN=1`), wait for `CSTS.RDY=1`
- [ ] Controller reset has a bounded timeout derived from `CAP.TO` (NVMe spec: `CAP.TO` is in 500 ms units); if `CSTS.RDY` does not reach the expected state within the timeout, `nvme_probe` returns a bring-up error instead of looping forever
- [ ] Controller version and capabilities (queue entry sizes, doorbell stride, max queue entries) are parsed from `CAP` and `VS`

### D.2 — NVMe admin queue and identify command

**Files:**
- `kernel/src/blk/nvme.rs`
- `kernel/src/mm/dma.rs`

**Symbol:** `AdminQueue`, `NvmeCommand`, `nvme_identify_controller`, `nvme_identify_namespace`
**Why it matters:** The admin queue is the control path for all NVMe management commands. The Identify command returns controller and namespace metadata needed to configure I/O queues and determine device capacity.

**Acceptance:**
- [ ] Admin submission and completion queues are allocated as `DmaBuffer` with physically-contiguous pages
- [ ] Queue doorbell registers are accessed at the correct stride offset from BAR0
- [ ] Submission queue entries are constructed using `kernel_core::nvme::NvmeCommand` (defined in D.0) — the kernel-side admin code does not redefine the 64-byte layout
- [ ] Completion queue entries are typed as `kernel_core::nvme::NvmeCompletion` (defined in D.0)
- [ ] Identify Controller command (`opcode 0x06`, CNS=1) returns model, serial, firmware revision, and queue limits
- [ ] Identify Namespace command (`opcode 0x06`, CNS=0) returns namespace size, capacity, and LBA format
- [ ] Completion queue entries are polled and the phase bit is used to detect new completions
- [ ] Admin-queue helpers return a bounded error (rather than blocking forever) if a completion does not arrive within a configurable per-command timeout

### D.3 — NVMe I/O queue pairs and block read/write

**Files:**
- `kernel/src/blk/nvme.rs`
- `kernel/src/blk/mod.rs`

**Symbol:** `IoQueuePair`, `nvme_read_sectors`, `nvme_write_sectors`, `NVME_READY`
**Why it matters:** I/O queues are the data path. Read and Write commands using Physical Region Page (PRP) lists transfer data between host memory and the NVMe namespace. This is the path that makes NVMe a usable block device for the VFS layer.

**Acceptance:**
- [ ] I/O submission and completion queues are allocated via `DmaBuffer` and created using the Create I/O Completion Queue (`opcode 0x05`) and Create I/O Submission Queue (`opcode 0x01`) admin commands
- [ ] MSI/MSI-X interrupt vector is assigned to the I/O completion queue
- [ ] `nvme_read_sectors(namespace, lba, count, buffer)` issues a Read command (`opcode 0x02`) with PRP entries pointing to the destination buffer
- [ ] `nvme_write_sectors(namespace, lba, count, buffer)` issues a Write command (`opcode 0x01`) with PRP entries pointing to the source buffer
- [ ] PRP list construction handles buffers spanning multiple pages
- [ ] `NVME_READY` flag signals to the block subsystem that NVMe is available
- [ ] `read_sectors` and `write_sectors` in `kernel/src/blk/mod.rs` dispatch to NVMe when `NVME_READY` is true and the target device is an NVMe namespace
- [ ] QEMU NVMe device reads and writes return correct data (verified by reading a known filesystem image)

### D.4 — NVMe interrupt handling and completion path

**File:** `kernel/src/blk/nvme.rs`
**Symbol:** `nvme_completion_handler`, `NvmeCompletionEntry`
**Why it matters:** NVMe uses MSI/MSI-X interrupts to signal I/O completion. The completion path must update the completion queue head doorbell and wake any blocked tasks. Without interrupt-driven completion, NVMe would require busy-polling that wastes CPU cycles.

**Acceptance:**
- [ ] MSI/MSI-X vector is registered via `register_device_irq` for the I/O completion queue
- [ ] `nvme_completion_handler` processes all pending completion entries (phase-bit walk) in a single ISR invocation
- [ ] Completion queue head doorbell is updated after processing
- [ ] Blocked tasks waiting on I/O completion are woken via the scheduler wait queue
- [ ] Fallback polling path exists for environments where MSI/MSI-X allocation fails

---

## Track E — Intel e1000 Network Driver

### E.0 — e1000 register and descriptor definitions in kernel-core

**File:** `kernel-core/src/e1000.rs` (new)
**Symbol:** `E1000Regs`, `E1000TxDesc`, `E1000RxDesc`, `E1000CtrlFlags`
**Why it matters:** Register offsets, descriptor layouts, and control flag definitions are pure data with no hardware dependency. Placing them in `kernel-core` makes them host-testable and keeps the kernel-side driver focused on MMIO interaction rather than layout parsing.

**Acceptance:**
- [ ] `E1000Regs` defines named register offsets: `CTRL` (0x0000), `STATUS` (0x0008), `ICR` (0x00C0), `IMS` (0x00D0), `IMC` (0x00D8), `RCTL` (0x0100), `TCTL` (0x0400), `RDBAL` (0x2800), `RDBAH` (0x2804), `RDLEN` (0x2808), `RDH` (0x2810), `RDT` (0x2818), `TDBAL` (0x3800), `TDBAH` (0x3804), `TDLEN` (0x3808), `TDH` (0x3810), `TDT` (0x3818), `RAL0` (0x5400), `RAH0` (0x5404)
- [ ] `E1000RxDesc` is a 16-byte `#[repr(C)]` struct with buffer_addr (u64), length (u16), checksum (u16), status (u8), errors (u8), special (u16)
- [ ] `E1000TxDesc` is a 16-byte `#[repr(C)]` struct with buffer_addr (u64), length (u16), cso (u8), cmd (u8), status (u8), css (u8), special (u16)
- [ ] `E1000CtrlFlags` and `E1000RctlFlags` define named bitflags for CTRL and RCTL register fields
- [ ] At least 3 host tests validate descriptor layout (size, alignment) and flag composition
- [ ] All tests pass via `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`

### E.1 — e1000 register definitions and device initialization

**File:** `kernel/src/net/e1000.rs` (new)
**Symbol:** `E1000Device`, `e1000_probe`, `E1000_REGS`
**Why it matters:** The Intel 82540EM (e1000) is the most commonly emulated NIC — it is QEMU's default and has extensive public Intel documentation. Its simple ring-buffer model makes it the highest-leverage first real-hardware networking target. This task establishes PCI discovery, MMIO register access, and hardware initialization.

**Acceptance:**
- [ ] `e1000_probe` claims the e1000 PCI device (vendor `0x8086`, device `0x100E` for 82540EM) via `claim_pci_device`
- [ ] BAR0 is mapped via the hardware-access layer as an MMIO region
- [ ] Register constants are defined for at least: `CTRL`, `STATUS`, `ICR`, `IMS`, `IMC`, `RCTL`, `TCTL`, `RDBAL`, `RDBAH`, `RDLEN`, `RDH`, `RDT`, `TDBAL`, `TDBAH`, `TDLEN`, `TDH`, `TDT`, `RAL0`, `RAH0`
- [ ] Device initialization: global reset (`CTRL.RST`), wait for reset complete, configure `CTRL` (auto-speed, link up, no PHY reset)
- [ ] MAC address is read from `RAL0`/`RAH0` (or EEPROM if needed) and stored in `E1000Device`
- [ ] Multicast table array is cleared

### E.2 — e1000 TX/RX descriptor rings and DMA setup

**File:** `kernel/src/net/e1000.rs`
**Symbol:** `TxDescRing`, `RxDescRing`, `E1000TxDesc`, `E1000RxDesc`
**Why it matters:** The e1000 uses hardware DMA descriptor rings for packet transmission and reception. Each descriptor points to a DMA buffer where the hardware reads (TX) or writes (RX) packet data. Correct ring setup is the foundation for all packet I/O.

**Acceptance:**
- [ ] Ring slots are typed as `kernel_core::e1000::E1000RxDesc` and `kernel_core::e1000::E1000TxDesc` (defined in E.0) — the kernel-side driver does not redefine the descriptor layout
- [ ] `RxDescRing` allocates a `DmaBuffer` array of receive descriptors and pre-allocates per-descriptor packet buffers
- [ ] `TxDescRing` allocates a `DmaBuffer` array of transmit descriptors
- [ ] Ring base addresses and lengths are programmed into `RDBAL`/`RDBAH`/`RDLEN` and `TDBAL`/`TDBAH`/`TDLEN`
- [ ] Receive is enabled: `RCTL` configured with buffer size, broadcast accept, strip CRC
- [ ] Transmit is enabled: `TCTL` configured with collision threshold and backoff
- [ ] Ring sizes are configurable but default to 256 descriptors

### E.3 — e1000 interrupt handling and packet receive

**Files:**
- `kernel/src/net/e1000.rs`
- `kernel/src/net/dispatch.rs`

**Symbol:** `e1000_interrupt_handler`, `e1000_receive_packets`
**Why it matters:** The e1000 signals packet arrival via interrupts. The receive path must walk the RX ring, extract completed packets, and feed them into the existing Ethernet/IPv4/TCP/UDP stack through the network dispatch layer.

**Acceptance:**
- [ ] IRQ is registered via `register_device_irq` (MSI if available, legacy INTx as fallback)
- [ ] `e1000_interrupt_handler` reads `ICR` to determine interrupt cause, clears handled interrupts
- [ ] `e1000_receive_packets` walks the RX ring from the software tail to the hardware head, collecting completed descriptors
- [ ] Received packets are passed to `kernel/src/net/dispatch.rs` for Ethernet frame processing (same entry point as VirtIO-net)
- [ ] RX descriptors are recycled: buffer re-allocated, descriptor reset, tail pointer advanced
- [ ] Link-status-change interrupts update the device link state

### E.4 — e1000 packet transmit and network stack integration

**Files:**
- `kernel/src/net/e1000.rs`
- `kernel/src/net/mod.rs`

**Symbol:** `e1000_transmit`, `init_e1000`, `NetworkDriver`
**Why it matters:** The transmit path completes the NIC's data plane. Network stack integration ensures the existing TCP/UDP/ICMP code can use e1000 as a drop-in replacement for VirtIO-net without upper-layer changes.

**Acceptance:**
- [ ] `e1000_transmit(packet)` copies the packet into a TX descriptor buffer, sets EOP and IFCS command bits, and advances the TX tail pointer
- [ ] TX completion is checked via the DD status bit in transmitted descriptors; buffers are reclaimed
- [ ] `init_e1000()` is called from `probe_all_drivers` and sets the network driver dispatch to use e1000 when the device is present
- [ ] The existing `send_packet` path in `kernel/src/net/mod.rs` dispatches to e1000 or VirtIO-net based on which driver initialized
- [ ] Link-status-change interrupts flip a driver-level link flag; while the flag is down, `e1000_transmit` returns an `ENETDOWN`-equivalent error instead of silently enqueuing onto a ring the hardware will not drain, and the TX ring is drained of in-flight descriptors before re-enabling transmit on link-up
- [ ] ICMP ping through the e1000 driver works in QEMU (`-device e1000`)
- [ ] TCP connections through the e1000 driver work (telnet/SSH to the OS via e1000)

---

## Track F — Validation, Documentation, and Version

### F.1 — Add reproducible validation steps for real-hardware bring-up

**File:** `xtask/src/main.rs`
**Symbol:** `run_qemu_nvme`, `run_qemu_e1000` (or equivalent xtask subcommands)
**Why it matters:** A hardware phase is only finished if the project can reproduce the bring-up. Validation steps that require manual QEMU flag editing are not reproducible enough for a release narrative.

**Acceptance:**
- [ ] `cargo xtask run --device nvme` launches QEMU with an NVMe drive attached
- [ ] `cargo xtask run --device e1000` launches QEMU with an e1000 NIC instead of VirtIO-net
- [ ] Both flags can be combined: `cargo xtask run --device nvme --device e1000`
- [ ] Default behavior (no `--device` flags) remains unchanged: VirtIO-blk and VirtIO-net
- [ ] A smoke test validates that NVMe and e1000 are functional in their respective QEMU configurations

### F.2 — Create Phase 55 learning doc

**File:** `docs/55-hardware-substrate.md` (new)
**Symbol:** N/A (documentation deliverable)
**Why it matters:** The learning doc is a required Phase 55 deliverable per the design doc. It must follow the aligned learning-doc template from `docs/appendix/doc-templates.md` and explain the hardware-access layer, donor strategy, reference matrix, and how the chosen drivers fit into the system architecture.

**Acceptance:**
- [ ] `docs/55-hardware-substrate.md` exists and follows the aligned learning-doc template
- [ ] Sections cover: hardware-access layer design, donor strategy rationale, reference matrix, driver architecture, and how Phase 55 differs from later hardware work
- [ ] Key files table lists all new modules introduced in Phase 55
- [ ] Doc is linked from `docs/README.md`

### F.3 — Update roadmap, subsystem docs, and support matrix

**Files:**
- `docs/roadmap/README.md`
- `docs/README.md`
- `docs/15-hardware-discovery.md`
- `docs/16-network.md`
- `docs/24-persistent-storage.md`
- `docs/evaluation/hardware-driver-strategy.md`
- `docs/evaluation/redox-driver-porting.md`
- `docs/evaluation/roadmap/R08-hardware-substrate.md`
- `README.md`

**Symbol:** N/A (documentation updates)
**Why it matters:** Phase 55 changes the project's hardware posture from "virtual only" to "bounded real-hardware support." All upstream docs that describe supported hardware, PCI capabilities, storage paths, or network paths must reflect the new reality.

**Acceptance:**
- [ ] `docs/roadmap/README.md` Phase 55 row updated from "Deferred until implementation planning" to the task doc link
- [ ] `docs/roadmap/tasks/README.md` gains a Phase 55 row under `Convergence Phases` pointing at `./55-hardware-substrate-tasks.md`
- [ ] `docs/README.md` references the Phase 55 learning doc
- [ ] `docs/15-hardware-discovery.md` updated to mention PCIe MCFG and MSI/MSI-X
- [ ] `docs/16-network.md` updated to document the e1000 driver alongside VirtIO-net
- [ ] `docs/24-persistent-storage.md` updated to document NVMe alongside VirtIO-blk
- [ ] `docs/evaluation/roadmap/R08-hardware-substrate.md` updated to reflect Phase 55 completion status
- [ ] `README.md` project description updated to reflect real-hardware support
- [ ] `docs/evaluation/hardware-driver-strategy.md` and `docs/evaluation/redox-driver-porting.md` receive any corrections discovered during implementation

### F.4 — Version bump to 0.55.0

**Files:**
- `kernel/Cargo.toml`
- `AGENTS.md` (project-overview version string)
- `README.md` (project overview / release notes)
- `docs/roadmap/README.md` (Phase 55 status column)
- `docs/roadmap/tasks/README.md` (Phase 55 status column, added in F.3)

**Symbol:** `version` field (Cargo.toml) and prose version mentions (docs)
**Why it matters:** The Phase 55 design doc requires the kernel version to be bumped to `0.55.0` when the phase lands. Phase 54 closure exposed that leaving "any other version references" open-ended permits drift between the crate version, the docs, and the roadmap status columns.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `[package].version` is `0.55.0`
- [ ] `AGENTS.md` project-overview paragraph reflects kernel `v0.55.0`
- [ ] `README.md` project description reflects the new kernel version
- [ ] `docs/roadmap/README.md` Phase 55 row status is `Complete`
- [ ] `docs/roadmap/tasks/README.md` Phase 55 row status is `Complete`
- [ ] A repo-wide search for the previous `0.54.x` version string returns no user-facing references that should have been bumped (generated lockfiles excepted)

---

## Documentation Notes

- Phase 15 introduced ACPI parsing, LAPIC, I/O APIC, and legacy PCI config-space access. Phase 55 extends PCI from legacy I/O to PCIe MMIO and adds MSI/MSI-X interrupt delivery.
- Phase 24 introduced VirtIO-blk storage with legacy VirtIO 0.9.5 port I/O. Phase 55 adds NVMe as the first non-VirtIO storage path using the new hardware-access layer.
- Phase 16 introduced VirtIO-net networking with legacy VirtIO 0.9.5 port I/O. Phase 55 adds Intel e1000 as the first non-VirtIO networking path.
- Phase 53a modernized the memory allocator. Phase 55's DMA buffer abstraction builds on the buddy allocator's `alloc_contiguous_frames` for physically-contiguous allocation.
- **Ring-0 placement is deliberate and bounded.** Phase 55 places the NVMe and e1000 drivers in ring 0 (`kernel/src/blk/nvme.rs`, `kernel/src/net/e1000.rs`) for bring-up simplicity, which is a conscious widening of the TCB relative to Phase 54's userspace-service direction. Extraction of these drivers into supervised ring-3 services (following the Phase 54 `vfs_server` / `net_server` pattern) is **deferred to a later phase**. To keep that door open, the hardware-access layer (BAR mapping, DMA, IRQ registration, device claim) is designed so its contracts are callable from a future userspace driver host rather than baked into kernel-only call sites.
- **Failure modes covered by acceptance criteria.** The phase deliberately specifies non-happy paths so drivers do not hang ring 0 under hardware faults: NVMe controller-reset timeout (D.1), admin-command completion timeout (D.2), e1000 link-down with in-flight TX (E.4), MSI/MSI-X allocation failure falls back to polling on NVMe (D.4) and to legacy INTx on e1000 (E.3). DMA allocation failure surfaces as a driver-init error rather than a panic.
- The donor strategy in `docs/evaluation/hardware-driver-strategy.md` was researched before Phase 55. Implementation should follow it: public specs first, Redox as device-logic reference, BSD as behavioral reference, Linux for quirks only.
- The Redox porting analysis in `docs/evaluation/redox-driver-porting.md` identifies e1000 and NVMe as "high feasibility" extraction targets. The reusable part is device-register logic; the Redox scheme/daemon/event glue is not portable.
- **Intel NIC scope.** Phase 55 targets the Intel 82540EM classic e1000 (`0x8086:0x100E`) only. The e1000e family (82574, 82576, etc.) is different silicon with separate register layouts and is **not** in scope; it is deferred to a later phase.
- New pure-logic code (register definitions, command structures, descriptor formats) belongs in `kernel-core` for host testability where practical. Hardware-dependent wiring (MMIO access, DMA allocation, ISR registration) belongs in `kernel/src/`.
- Host-side tests should use `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`.
- The existing VirtIO drivers should be migrated to use the new hardware-access layer (BAR mapping, device claim, IRQ registration) to validate the abstractions before NVMe and e1000 are built on them.
