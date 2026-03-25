# Phase 15 — Hardware Discovery (ACPI + PCI)

**Branch:** `phase-15-hardware-discovery`
**Depends on:** Phase 3 (Interrupts) ✅, Phase 14 (Shell) ✅
**Status:** ✅ Complete — all 49 tasks done, QEMU-validated.
**Documentation:** [`docs/15-hardware-discovery.md`](docs/15-hardware-discovery.md)

## Track Status

| Track | Scope | Status |
|---|---|---|
| A | ACPI table discovery and parsing | ✅ done |
| B | Local APIC initialization | ✅ done |
| C | I/O APIC initialization | ✅ done |
| D | Timer migration (PIT → LAPIC timer) | ✅ done |
| E | PCI bus enumeration | ✅ done |
| F | Validation + documentation | ✅ done |

---

## Track A — ACPI Table Discovery

| Task | Description | Status |
|---|---|---|
| P15-T001 | Read `boot_info.rsdp_addr` and store in global `Once<PhysAddr>` | ✅ |
| P15-T002 | Define RSDP v1/v2 structures | ✅ |
| P15-T003 | Implement `validate_rsdp()`: verify signature and checksum | ✅ |
| P15-T004 | Define ACPI SDT header struct | ✅ |
| P15-T005 | Implement `parse_rsdt()` / `parse_xsdt()` | ✅ |
| P15-T006 | Implement SDT signature lookup | ✅ |
| P15-T007 | Define MADT structures (Local APIC, I/O APIC, ISO entries) | ✅ |
| P15-T008 | Implement `parse_madt()` | ✅ |
| P15-T009 | Define FADT structure (minimal) | ✅ |
| P15-T010 | Log ACPI discovery results | ✅ |

## Track B — Local APIC Initialization

| Task | Description | Status |
|---|---|---|
| P15-T011 | Read Local APIC base address from MADT / MSR fallback | ✅ |
| P15-T012 | Verify LAPIC MMIO page accessible via `physical_memory_offset` | ✅ |
| P15-T013 | Define LAPIC register offsets | ✅ |
| P15-T014 | Implement `lapic_init()`: enable LAPIC via Spurious register | ✅ |
| P15-T015 | Add spurious interrupt handler at vector 0xFF | ✅ |
| P15-T016 | Implement `lapic_eoi()` | ✅ |

## Track C — I/O APIC Initialization

| Task | Description | Status |
|---|---|---|
| P15-T017 | Read I/O APIC base address from MADT | ✅ |
| P15-T018 | Implement I/O APIC register access (IOREGSEL/IOWIN) | ✅ |
| P15-T019 | Read I/O APIC Version register | ✅ |
| P15-T020 | Define redirection table entry format | ✅ |
| P15-T021 | Program redirection for IRQ 1 (keyboard) | ✅ |
| P15-T022 | Program redirection for IRQ 4 (COM1 serial) | ✅ |
| P15-T023 | Mask all unused I/O APIC redirection entries | ✅ |
| P15-T024 | Disable legacy 8259 PIC | ✅ |
| P15-T025 | Update keyboard IRQ handler → `lapic_eoi()` | ✅ |
| P15-T026 | Update serial IRQ handler → `lapic_eoi()` | ✅ |

## Track D — Timer Migration (PIT → LAPIC Timer)

| Task | Description | Status |
|---|---|---|
| P15-T027 | Calibrate LAPIC timer using PIT one-shot | ✅ |
| P15-T028 | Store calibrated ticks-per-ms value | ✅ |
| P15-T029 | Configure LAPIC timer in periodic mode (vector 32, ~10ms) | ✅ |
| P15-T030 | Update timer IRQ handler → `lapic_eoi()` | ✅ |
| P15-T031 | Verify TICK_COUNT increments and scheduler fires | ✅ |
| P15-T032 | Stop the PIT after LAPIC timer is running | ✅ |

## Track E — PCI Bus Enumeration

| Task | Description | Status |
|---|---|---|
| P15-T033 | Implement `pci_config_read_u32(bus, device, function, offset)` | ✅ |
| P15-T034 | Implement `pci_config_read_u16` and `pci_config_read_u8` helpers | ✅ |
| P15-T035 | Define `PciDevice` struct | ✅ |
| P15-T036 | Implement `pci_scan()`: iterate bus/device/function space | ✅ |
| P15-T037 | Read class, subclass, BARs, interrupt line for each device | ✅ |
| P15-T038 | Store devices in static array | ✅ |
| P15-T039 | Expose `pci_device_list()` read-only accessor | ✅ |
| P15-T040 | Log full PCI device list at boot | ✅ |

## Track F — Validation and Documentation

| Task | Description | Status |
|---|---|---|
| P15-T041 | Acceptance: kernel boots using LAPIC timer | ✅ |
| P15-T042 | Acceptance: keyboard via I/O APIC works | ✅ |
| P15-T043 | Acceptance: legacy 8259 PIC fully masked/disabled | ✅ |
| P15-T044 | Acceptance: boot log prints PCI device list | ✅ |
| P15-T045 | Acceptance: ACPI logs CPU count and APIC IDs | ✅ |
| P15-T046 | Acceptance: shell, pipes, utilities work without regression | ✅ |
| P15-T047 | `cargo xtask check` passes | ✅ |
| P15-T048 | QEMU boot validation — no panics | ✅ |
| P15-T049 | Write `docs/15-hardware-discovery.md` | ✅ |
