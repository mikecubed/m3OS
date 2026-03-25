# Phase 15 — Hardware Discovery (ACPI + PCI)

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
- PCIe extended config space (MCFG / MMIO)
- MSI / MSI-X interrupt routing
- HPET as timer source
- Application Processor startup (Phase 17: SMP)
- PCI BAR MMIO mapping for device drivers (Phase 16: Network)
