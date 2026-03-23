# Phase 15 - Hardware Discovery

## Milestone Goal

Replace the hardcoded, single-core interrupt model with proper hardware discovery
via ACPI and PCI, and switch from the legacy 8259 PIC to the APIC. This unlocks
multi-core support and arbitrary device discovery in later phases.

```mermaid
flowchart TD
    UEFI["UEFI firmware\n(RSDP pointer in BootInfo)"] --> ACPI["ACPI tables\n(RSDP → RSDT/XSDT → MADT, FADT)"]
    ACPI --> MADT["MADT\n(CPU/APIC topology)"]
    ACPI --> FADT["FADT\n(legacy PIC present?)"]
    MADT --> LAPIC["Local APIC\n(per-core timer + IPI)"]
    MADT --> IOAPIC["I/O APIC\n(IRQ routing)"]
    LAPIC --> Sched["scheduler\n(preemption timer)"]
    IOAPIC --> IRQ["interrupt dispatch"]

    PCI["PCI bus scan\n(config space)"] --> Devices["device list\n(VID/DID, BARs, IRQ lines)"]
    Devices --> Net["future: NIC driver"]
    Devices --> AHCI["future: AHCI/SATA"]
```

## Learning Goals

- Understand how ACPI tables describe the hardware topology to the OS.
- See how the APIC model replaces the legacy 8259 PIC and why it must.
- Learn what PCI configuration space is and how devices are enumerated.

## Feature Scope

- **ACPI parsing**:
  - walk RSDP → RSDT/XSDT → iterate SDTs
  - parse MADT: extract Local APIC base, I/O APIC base, IRQ source overrides
  - parse FADT: detect whether legacy PIC is present
- **Local APIC initialization**:
  - map the LAPIC MMIO registers
  - configure the LAPIC timer for periodic preemption (replaces PIT-driven timer)
  - send end-of-interrupt (EOI) through LAPIC instead of PIC
- **I/O APIC initialization**:
  - program IRQ redirection table entries for keyboard and serial
  - disable legacy 8259 PIC
- **PCI bus enumeration**:
  - scan bus 0–255, device 0–31, function 0–7 via config space reads
  - record vendor ID, device ID, class, BAR addresses, and IRQ line for each device
  - expose the device list through a simple read-only kernel API

## Implementation Outline

1. Parse the RSDP address from `BootInfo` (UEFI already locates it).
2. Walk the RSDT/XSDT and collect pointers to each SDT.
3. Parse the MADT and record the Local APIC base address and all I/O APIC descriptors.
4. Identity-map the Local APIC MMIO page and write the initialization sequence.
5. Program the I/O APIC redirection table for the IRQs the kernel currently handles.
6. Disable the 8259 PIC.
7. Switch the scheduler's timer source from the PIT to the LAPIC timer.
8. Implement PCI config space reads (port I/O: address port 0xCF8, data port 0xCFC).
9. Enumerate all PCI functions and store the device list in a static kernel array.

## Acceptance Criteria

- The kernel boots using the LAPIC timer for preemption instead of the PIT.
- Keyboard interrupts are delivered via the I/O APIC without regression.
- The legacy 8259 PIC is fully masked and disabled.
- A boot log entry prints the full PCI device list with vendor ID and class codes.
- ACPI table parsing logs the CPU count and APIC IDs found in the MADT.

## Companion Task List

- [Phase 15 Task List](./tasks/15-hardware-discovery-tasks.md)

## Documentation Deliverables

- explain the ACPI table chain: RSDP → RSDT/XSDT → individual SDTs
- document the MADT structure and what each entry type means
- explain the Local APIC vs. I/O APIC split and which handles what
- document PCI config space layout and how the scan loop works
- explain why the legacy PIC cannot be used on multi-core systems

## How Real OS Implementations Differ

Production kernels use ACPI's AML bytecode interpreter to handle dynamic hardware
configuration (hotplug, power transitions, embedded controller queries). PCI
enumeration extends to PCIe extended config space (4 KB per function) and uses
MCFG tables for MMIO-mapped access. IRQ routing on modern systems runs through
MSI/MSI-X rather than the I/O APIC. This phase uses only the static descriptor tables
and legacy port-I/O PCI access, which is sufficient for a single-machine QEMU target.

## Deferred Until Later

- ACPI AML interpreter and dynamic hardware events
- PCIe extended config space (MCFG)
- MSI and MSI-X interrupt routing
- PCI device power management (D-states)
- ACPI S-states (sleep, hibernate)
- PCIe hotplug
