# Phase 15 — Hardware Discovery (ACPI + PCI)

**Aligned Roadmap Phase:** Phase 15
**Status:** Complete
**Source Ref:** phase-15

Phase 15 replaces the legacy 8259 PIC with the APIC interrupt system and adds
PCI device enumeration.  The kernel now discovers hardware topology from ACPI
tables at boot and uses the Local APIC timer for preemptive scheduling.

## ACPI Table Chain

```
BootInfo.rsdp_addr
        |
        v
   RSDP (v2)          "RSD PTR " signature, checksum-validated
        |
        v
   XSDT                64-bit pointers to child SDTs
   /    |    \    \    \
FACP  APIC  HPET WAET BGRT
(FADT) (MADT)
```

The kernel parses the RSDP, validates checksums for both v1 (20 bytes) and v2
(36 bytes), then walks the XSDT (preferred) or RSDT (fallback) to locate child
tables by their 4-byte signatures.

### MADT (Multiple APIC Description Table)

The MADT ("APIC" signature) contains variable-length entries describing the
interrupt controller topology:

| Type | Structure | Purpose |
|------|-----------|---------|
| 0 | Local APIC | One per CPU — APIC ID + processor ID + flags |
| 1 | I/O APIC | Base address for the I/O APIC MMIO registers |
| 2 | Interrupt Source Override | Remaps ISA IRQs to different GSIs |

On QEMU, the MADT reports 1 CPU (APIC ID 0), I/O APIC at 0xFEC00000, and
five IRQ source overrides (notably IRQ 0 → GSI 2 for the PIT timer).

### FADT (Fixed ACPI Description Table)

The kernel reads the `IAPC_BOOT_ARCH` flags (offset 109) to detect whether
the firmware indicates a legacy 8259 PIC.  The kernel will migrate to the
APIC when MADT/I/O APIC information is available, but falls back to the
legacy PIC if that data is missing or incomplete.

## Local APIC

The Local APIC (at physical 0xFEE00000, accessed via the bootloader's
identity-mapped physical memory offset) provides per-CPU interrupt handling:

- **Spurious vector**: 0xFF (registered in IDT, handler is a no-op with no EOI)
- **EOI register**: written with 0 after servicing each interrupt
- **TPR**: set to 0 to accept all interrupt priorities

### LAPIC Timer

The LAPIC timer replaces the PIT as the scheduler's preemption source:

1. **Calibration**: PIT channel 2 in one-shot mode provides a ~10ms time
   reference.  The LAPIC timer counts down from 0xFFFFFFFF with divide-by-16.
   Elapsed ticks / 10 = ticks per millisecond.
2. **Periodic mode**: LVT Timer register configured with vector 32 and bit 17
   (periodic).  Initial count = ticks_per_ms * 10 for a 100 Hz tick rate.
3. **Handoff**: The `USING_APIC` flag switches interrupt handlers from PIC EOI
   to LAPIC EOI atomically after the LAPIC timer starts.

## I/O APIC

The I/O APIC (at physical 0xFEC00000) routes external interrupts to Local
APICs via redirection table entries:

| ISA IRQ | GSI | Vector | Purpose |
|---------|-----|--------|---------|
| 1 | 1 | 33 | PS/2 Keyboard |
| 4 | 4 | 36 | COM1 Serial (programmed but kept masked until a serial IRQ handler is installed) |
| 0 | 2* | 32 | PIT Timer (used during calibration only, then masked) |

\* IRQ 0 has a MADT source override mapping it to GSI 2 on QEMU.

All other redirection entries, including COM1's, are masked.  The MADT Interrupt Source Override
entries are applied when programming each IRQ — the override's `flags` field
determines polarity (active-high vs active-low) and trigger mode (edge vs
level).

### Legacy PIC Disable

After the I/O APIC is fully programmed and the LAPIC timer is running, the
legacy 8259 PIC is disabled by writing 0xFF to both data ports (0x21 and
0xA1), masking all lines.

## PCI Bus Enumeration

PCI configuration space is accessed via legacy port I/O:

- **Port 0xCF8** (CONFIG_ADDRESS): `(1 << 31) | (bus << 16) | (dev << 11) | (fn << 8) | (offset & 0xFC)`
- **Port 0xCFC** (CONFIG_DATA): 32-bit read/write

The kernel scans bus 0–255, device 0–31, function 0–7.  Multi-function devices
are detected via bit 7 of the header type register.  For each discovered
function, the kernel reads vendor/device IDs, class/subclass, BARs (header
type 0 only), and interrupt line/pin.

### QEMU Device List

```
00:00.0 8086:1237 06/00 (Host Bridge)
00:01.0 8086:7000 06/01 (ISA Bridge)
00:01.1 8086:7010 01/01 (IDE Controller)
00:01.3 8086:7113 06/80 (Other Bridge)
00:02.0 1234:1111 03/00 (VGA Controller)
00:03.0 8086:100e 02/00 (Ethernet Controller)
```

## Why the PIC Can't Do SMP

The 8259 PIC is a single-core interrupt controller — it can only deliver
interrupts to one CPU.  The APIC system (Local APIC per core + I/O APIC as
central router) is required for SMP because:

1. Each core has its own Local APIC with a unique ID
2. The I/O APIC can route interrupts to any Local APIC by destination ID
3. Inter-Processor Interrupts (IPIs) are sent core-to-core via the ICR registers
4. The LAPIC timer provides per-core preemption (each core runs its own timer)

Phase 17 (SMP) will use the APIC infrastructure built here to start Application
Processors and distribute interrupts across cores.

## Module Layout

```
kernel/src/acpi/mod.rs          RSDP, RSDT/XSDT, MADT, FADT parsing
kernel/src/arch/x86_64/apic.rs  LAPIC + I/O APIC + timer calibration
kernel/src/pci/mod.rs           PCI config space enumeration
```

## Deferred

- ACPI AML interpreter and dynamic hardware events
- HPET as timer source
- Application Processor startup (Phase 17: SMP)
- PCI BAR MMIO mapping for device drivers (Phase 16: Network)

## Phase 55 Additions (PCIe MCFG, MSI/MSI-X)

Phase 15 originally deferred PCIe extended config space and MSI/MSI-X. Both
were added in Phase 55 as prerequisites for NVMe and to prepare the kernel
for PCIe-era devices:

- **PCIe MCFG / MMIO configuration space.** The ACPI MCFG table is parsed
  during init, extracting base address, segment group, and bus range.
  `pcie_mmio_config_read` / `pcie_mmio_config_write` (in
  `kernel/src/pci/mod.rs`) reach the full 4096-byte extended config space,
  which the legacy port-I/O path could not address. Legacy
  `pci_config_read_*` / `pci_config_write_*` remain as fallback when MCFG
  is absent.
- **MSI and MSI-X capability parsing.** PCI capability list walking finds
  MSI (cap ID `0x05`) and MSI-X (cap ID `0x11`) structures and decodes them
  into `MsiCapability` / `MsixCapability` types. `allocate_msi_vectors`
  programs the capability and returns allocated APIC vector numbers
  routed to the correct IDT entries. Devices without MSI/MSI-X fall back
  to legacy INTx, which is how classic e1000 (QEMU 82540EM) is wired.
- **Device claim protocol.** `claim_pci_device` returns an exclusive
  `PciDeviceHandle` so two drivers cannot bind the same device. This is
  the ownership gate used by the NVMe, e1000, and migrated VirtIO drivers.

See [Phase 55 — Hardware Substrate](./55-hardware-substrate.md) for how
these pieces compose into the hardware-access layer that the NVMe and
e1000 drivers use.

## Phase 55a Additions (DMAR and IVRS)

Phase 55a extended the ACPI parser with two more tables that describe
DMA-remapping (IOMMU) hardware:

- **DMAR** (DMA Remapping) — Intel VT-d. Contains DRHD entries (one per
  IOMMU unit, with register base and device scope), RMRR entries
  (firmware-reserved memory that must remain identity-mapped), and
  ATSR/RHSA auxiliary tables.
- **IVRS** (I/O Virtualization Reporting Structure) — AMD-Vi. Contains
  IVHD blocks (types 10h, 11h, 40h) describing each IOMMU unit and its
  device entries, plus IVMD unity-map records (the AMD equivalent of
  RMRR).

The pure-logic decoders live in
[`kernel-core/src/iommu/tables.rs`](../kernel-core/src/iommu/tables.rs) and
produce typed records with named error enums (`DmarParseError`,
`IvrsParseError`). The kernel-side glue in `kernel/src/acpi/mod.rs` only
locates the signatures, validates the checksum, and hands the body off;
when both tables are present on a malformed multi-vendor platform, DMAR
wins and IVRS is logged-and-ignored.

The decoded unit descriptors feed the new IOMMU subsystem rather than
being consumed inside `acpi/`. See
[Phase 55a — IOMMU Substrate](./55a-iommu-substrate.md) for the
subsystem-level architecture and
[docs/roadmap/55a-iommu-substrate.md](./roadmap/55a-iommu-substrate.md)
for the authoritative design.
