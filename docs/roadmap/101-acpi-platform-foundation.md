# Phase 101 - ACPI Platform Foundation (AML + device/resource enumeration + SCI)

**Status:** Planned
**Source Ref:** phase-101
**Depends on:** Phase 15 (ACPI table parse — RSDP/RSDT/XSDT, MADT, FADT) ✅, Phase 55a (DMAR/IVRS decode + the `kernel-core` host-tested table-decoder pattern) ✅, Phase 55b (capability-gated device-host syscalls + `Notification` IRQ objects) ✅
**Builds on:** Extends the existing **static** ACPI-table parsing in `kernel/src/acpi/mod.rs` (RSDP → RSDT/XSDT → MADT/FADT/MCFG/DMAR/IVRS) into a real ACPI **namespace** with a pragmatic **AML interpreter**, so devices that exist *only* in AML (the Elan/`DLL0945` I2C-HID touchpad, the battery, thermal zones, the lid switch) can be enumerated and their resources/interrupts resolved. The static table parse stays; this phase adds the DSDT/SSDT layer on top of it.
**Primary Components:** new `kernel-core/src/acpi/` (host-tested AML interpreter + namespace builder + `_CRS` resource decoder, mirroring `kernel-core/src/iommu/tables.rs`), new `userspace/acpid` (ring-3 daemon hosting the interpreter + an IPC query/event service), `kernel/src/acpi/mod.rs` (FADT extension: DSDT/X_DSDT pointer + `SCI_INT` + PM1/GPE block addresses + a table-blob accessor), `kernel/src/arch/x86_64/{interrupts.rs,apic.rs}` (SCI ISR vector + IOAPIC redirection + hardware ack/mask), `kernel/src/syscall/` + `kernel_core::device_host` (the thin kernel SCI→`Notification` + table-blob + PM/GPE register surface `acpid` binds), new `scripts/acpi-baremetal-validate.md` (bare-metal runbook)

## Milestone Goal

m3OS gains an **ACPI namespace** built by a pragmatic AML interpreter: walk the DSDT and any SSDTs, build the device tree, evaluate `_STA`/`_HID`/`_CID`/`_CRS` control methods, and answer the question every laptop driver asks — *"what bus / slave address / IRQ / GPIO is device X on?"* On the reference Dell Precision 5560 (Tiger Lake), an `_HID` lookup for `DLL0945` finds the touchpad device node and its `_CRS` yields the I2C slave address + the `GpioInt` pin/polarity that the Phase 102 I2C-HID driver needs to attach; a System Control Interrupt (lid-close or power-button) is demuxed by the kernel, routed to a ring-3 `acpid`, evaluated through the matching GPE method, and delivered to a subscriber. This is the platform substrate that **both** the I2C-HID touchpad (Phase 102) and laptop power management (Phase 103) sit on.

## Why This Phase Exists

There is **no AML interpreter and no ACPI namespace** anywhere in the tree — only static table parsing (`kernel/src/acpi/mod.rs` walks fixed-layout tables: `find_table`, `parse_madt`, `parse_fadt`, `parse_mcfg`, `parse_dmar`, `parse_ivrs`). Everything those functions read is a flat C struct at a known offset. The devices a laptop bring-up actually needs are *not* in any static table: they are AML objects in the DSDT. The Tiger Lake handoff (`docs/handoffs/2026-06-25-usb-log-persistence-and-keyboard.md`) pinned the built-in pointer as the **I2C-HID `DLL0945` / Elan `04F3:311C` on `i2c_designware.1`** — an ACPI `_HID` device whose I2C slave address and `GpioInt` come from its `_CRS`. It cannot be brought up without ACPI device + resource enumeration.

This is the hidden prerequisite the original GUI-workstation charter missed (Phase 98 calls it out explicitly): charting I2C-HID as self-contained would stall the moment it needs the ACPI-provided address + interrupt. The battery (`_BST`/`_BIF`), thermal zones (`_TMP`), and the lid / power-button (SCI notifications) that Phase 103 needs are equally AML-gated. Phase 15 gave us the *tables*; Phase 101 gives us the *namespace* the tables only point at (via the FADT's DSDT pointer, which `parse_fadt` does not read today — it stops at `IAPC_BOOT_ARCH`, offset 109).

## Learning Goals

- Understand the difference between **static ACPI tables** (fixed-layout C structs — MADT, FADT, MCFG) and the **AML namespace** (a bytecode-defined object tree in the DSDT/SSDTs that must be *interpreted* to discover devices and their resources).
- Learn a pragmatic **AML interpreter** subset — the opcode/`PkgLength`/`NameString` encoding, control-method evaluation (`Store`/`If`/`While`/`Return`/arithmetic), the named-object model (`Device`/`Method`/`Name`/`OperationRegion`/`Field`), and the `RegionSpace` boundary where AML reaches real hardware — without building a full ACPICA-class VM.
- See how `_HID`/`_CID` device matching (string vs `EisaId`-encoded integer) + `_STA` presence + `_CRS` resource decode turn a bytecode blob into "device DLL0945 is an I2C slave at address 0x2c with a level/active-low `GpioInt` on the SoC GPIO controller."
- Understand the **System Control Interrupt** (SCI): a single shared, level-triggered interrupt that demuxes ACPI fixed events (power button, RTC) and General-Purpose Events (lid, EC, battery) — why the *kernel* must own the hardware ack/mask to avoid an interrupt storm while *userspace* runs the AML policy (`_Lxx`/`_Exx`/`_Qxx` methods + `Notify()` routing).
- Confront the **ring-0-vs-ring-3 split** honestly: an AML interpreter is large and runs arbitrary firmware bytecode; the microkernel-idiomatic answer is a ring-3 `acpid` over a thin kernel surface, with the kernel keeping only the parts that *must* be privileged (FADT parse, SCI hardware demux).

## Feature Scope

### Track A — AML interpreter (pragmatic subset)

A new host-tested interpreter under `kernel-core/src/acpi/aml/` that implements the AML subset sufficient for *device enumeration* — not a full AML VM. It covers the opcode stream + `PkgLength` + `NameString` decode, control-method evaluation (`Store`, `If`/`Else`, `While`, `Return`, `Local0..7`/`Arg0..6`, integer + logical + buffer/package ops, method invocation), and the named-object model (`Scope`, `Device`, `Method`, `Name`, `OperationRegion`, `Field`). The point where AML touches hardware (`OperationRegion` reads/writes in `SystemMemory`/`SystemIO`/`PCI_Config`/`EmbeddedController`) is abstracted behind a `RegionSpace` backend trait so the interpreter is pure logic — host tests use a mock backend; the production backend (Track E) delegates to the ring-3 device-host syscalls. Behavior is referenced against ACPICA/uACPI, not copied. Bounded recursion + bounded loop iteration + no-panic-on-malformed-AML are safety requirements (the interpreter runs untrusted firmware bytecode).

### Track B — ACPI namespace build + device tree + `_HID`/`_CID` matching

A new `kernel-core/src/acpi/namespace.rs` walks one DSDT + N SSDTs (multiple definition blocks merged into one tree — SSDTs commonly extend a scope defined in the DSDT), builds the namespace as a node arena (`Scope`/`Device`/`Method`/`Name`/`Processor`/`ThermalZone`), resolves `NameString` paths (root `\`, parent-prefix `^`, multi-name segments), and exposes device matching: `find_by_hid("DLL0945")`, `_CID` fallback, and `EisaId` decode for integer-encoded `_HID`s (e.g. `PNP0C0A` = battery, `PNP0C0D` = lid, `PNP0C0C` = power button). `_STA` is evaluated to filter present/enabled devices (absent `_STA` defaults to present, per the spec).

### Track C — `_CRS` resource decode

A new `kernel-core/src/acpi/resource.rs` decodes the `_CRS` resource-descriptor stream (small + large resource items, end tag + checksum) into a typed `DeviceResources` struct other drivers consume. The descriptors that matter for the laptop: the **I2C SerialBus** connection (slave address, bus speed, the `ResourceSource` path of the controller it sits on), **GpioInt/GpioIo** (pin number(s), edge/level, polarity, the GPIO controller `ResourceSource`), and the classic **IRQ / Memory32Fixed / FixedMemory** descriptors (for the embedded controller and legacy devices). This is the track that lets a driver ask "what bus/address/IRQ is device X on" and get a populated answer.

### Track D — SCI handler + GPE dispatch + `_Lxx`/`_Exx` + `Notify()` routing

The kernel learns to receive the SCI: `parse_fadt` is extended to read the fields it skips today (`SCI_INT`, `PM1a_EVT_BLK`/`PM1a_CNT_BLK`, `GPE0_BLK`/`GPE0_BLK_LEN`, and the DSDT/`X_DSDT` pointer). The `SCI_INT` GSI is routed through the existing IOAPIC redirection machinery (`apic::ioapic_write_redir` / `gsi_to_pin`, level-triggered active-low) to a dedicated ISR vector. The ISR **demuxes** the level-triggered SCI: read `PM1_STS` + `GPE_STS`, mask the asserted bits in the enable registers (so a level SCI does not storm before userspace services it), EOI, and signal `acpid`'s `Notification` with the pending event bitmap. `acpid` then evaluates the matching GPE method (`_Lxx` level / `_Exx` edge) or EC query (`_Qxx`), re-enables the GPE through the kernel, and routes any AML `Notify(device, code)` to ring-3 subscribers over IPC (battery `0x80` status-change → the Phase 103 power daemon; lid/button to the session). Fixed events (power-button `PWRB`, lid `LID0`) map to their handlers.

### Track E — Ring-3 `acpid` hosting + thin kernel surface (the split decision)

The honest split: the AML interpreter (Track A) is large and executes arbitrary firmware bytecode, so it runs in a **ring-3 `acpid`** daemon, not in ring 0. The kernel keeps only what must be privileged and exposes a thin surface `acpid` binds:

- a **table-blob accessor** so `acpid` can fetch the DSDT/SSDT bytes the FADT points at (read-only),
- the **SCI → `Notification`** subscription (reusing the Phase 55b `Notification` + `SYS_DEVICE_IRQ_SUBSCRIBE` ISR-shim pattern, extended for the platform SCI GSI rather than a PCI device), and
- **PM1/GPE register access** plus the AML `OperationRegion` backends (`SystemIO`/`SystemMemory`/`PCI_Config`/`EmbeddedController`) delegated through the existing capability-gated `device_host` PIO/MMIO/config syscalls.

`acpid` exposes an IPC **query/event service** (`FindByHid`, `GetResources`, `Subscribe` + event push) that the Phase 102 touchpad driver and Phase 103 power daemon call. The alternative — a full in-kernel ACPICA-style interpreter — is rejected for size, fault isolation, and host-testability, but the kernel necessarily retains the level-SCI hardware-ack (a userspace-only handler would let the level interrupt storm).

### Track F — Validation

QEMU *does* model a generic ACPI namespace (a DSDT, the PM1 power-button SCI, GPE0), so the **substrate** is partly CI-able even though the **target laptop devices** are not. The split:

- **Host tests** (always-on CI) on a DSDT captured from the Dell (`acpidump` via the `usb-logsink` boot.log path): AML opcode-subset decode, namespace build + `_HID` match (`DLL0945`), and `_CRS` I2C/GpioInt decode — the pure-logic surface the bare-metal validation strategy explicitly keeps in CI.
- A **QEMU `acpi-smoke` gate** that builds the namespace from QEMU's own DSDT, enumerates the emulated devices, and fires the **power-button SCI** (`qmp system_powerdown`) to exercise the kernel demux → `acpid` GPE-dispatch → `Notify()` path on the emulated namespace.
- The **HW-only arms** (touchpad `_HID`/`_CRS` on real silicon, lid/battery SCI) follow the **bare-metal validation protocol** in `docs/appendix/bare-metal-validation.md`, recorded as `Validated-on-HW (run N, date)` with captured log-sink sentinels — never a bare "Complete."

## Important Components and How They Work

### `kernel-core/src/acpi/` — the host-tested interpreter + namespace + resources

This is the largest deliverable and lives in `kernel-core` exactly like `kernel-core/src/iommu/tables.rs` (`decode_dmar`/`decode_ivrs`): pure logic, `no_std`-and-`std`, host-tested on captured firmware bytes. `aml/` holds the decoder + evaluator + object model; `namespace.rs` builds and queries the tree; `resource.rs` decodes `_CRS`. No `unsafe`. The `RegionSpace` trait is the seam: in tests it is a `Vec`-backed mock; in production `acpid` implements it over `device_host` syscalls. Keeping all of this in `kernel-core` is what makes the AML subset falsifiably testable without hardware.

### `kernel/src/acpi/mod.rs` — FADT extension + table-blob accessor

`parse_fadt` today reads only `IAPC_BOOT_ARCH` (offset 109) to log whether a legacy 8259 is present. Track D extends it to read the fields it skips: the DSDT physical pointer (offset 40 / `X_DSDT` offset 140), `SCI_INT` (offset 46), the PM1a event/control blocks, and `GPE0_BLK`/`GPE0_BLK_LEN`. These are cached alongside the existing `MADT_INFO`/`MCFG_INFO` `Once<…>` statics. A new accessor hands `acpid` the DSDT/SSDT byte ranges (located via `find_table`/`SDT_ENTRIES`, translated through `phys_to_virt`) read-only — `acpid` parses them, the kernel does not.

### `kernel/src/arch/x86_64/{apic.rs,interrupts.rs}` — SCI routing + demux

The SCI is just another GSI. `ioapic_init` already programs redirection entries for the ISA timer/keyboard IRQs using `ioapic_write_redir` + `gsi_to_pin` + the `acpi::irq_override` overrides; the SCI adds one more entry (level-triggered, active-low) for `SCI_INT`'s GSI pointed at a new ISR vector registered in `interrupts.rs`. The ISR is the kernel's *only* AML-adjacent code: it reads/masks the PM1/GPE status+enable registers (hardware ack so the level line de-asserts) and signals `acpid`'s `Notification`. All *policy* (which method to run, what the event means) lives in `acpid`.

### `userspace/acpid` — the ring-3 interpreter host

A new daemon wired into the four required places (workspace member, xtask `bins`, ramdisk `BIN_ENTRIES`, `services.d/acpid.conf` + `KNOWN_CONFIGS`). At start it fetches the DSDT/SSDT blobs from the kernel, builds the namespace (Track B), and serves the query/event protocol. It implements the `RegionSpace` backend over `device_host` PIO/MMIO/config syscalls, subscribes the SCI `Notification`, and dispatches GPE/fixed events. It owns no writable shared memory beyond its device-host grants and never blocks in an interrupt context — the SCI arrives as a `Notification` wake, not an in-handler callback.

## How This Builds on Earlier Phases

- **Extends Phase 15** by adding the AML/namespace layer on top of the static `find_table`/`parse_fadt`/`AcpiSdtHeader` parse — the FADT it already locates now yields its DSDT pointer + SCI/PM/GPE fields instead of just `IAPC_BOOT_ARCH`.
- **Reuses the Phase 55a pattern** — the `kernel-core` host-tested table decoder (`decode_dmar`/`decode_ivrs` in `iommu/tables.rs`) is the exact template for `kernel-core/src/acpi/`'s AML + `_CRS` decoders.
- **Reuses the Phase 55b device-host substrate** — `acpid`'s `OperationRegion` backends and PM/GPE register access ride the capability-gated `SYS_DEVICE_PIO_READ/WRITE`, `SYS_DEVICE_MMIO_MAP`, `SYS_DEVICE_CONFIG_READ/WRITE` syscalls, and the SCI is delivered via the `Notification` + `SYS_DEVICE_IRQ_SUBSCRIBE` ISR-shim machinery (`kernel/src/syscall/device_host.rs`).
- **Reuses the Phase 96 / Phase 98 bare-metal validation workflow** — `--usb-passthrough` is not applicable (the touchpad is on the internal I2C bus, not USB), but the AMT-SOL pre-network capture, `usb-logsink` boot.log, and network log sink from `docs/appendix/bare-metal-validation.md` are how the HW-only arms are recorded.
- **Is the gating substrate for Phase 102 (I2C-HID touchpad) and Phase 103 (laptop power)** — both consume `acpid`'s `FindByHid`/`GetResources` query and SCI event routing. ACPI-before-I2C-HID is one of the two sequencing traps Phase 98's charter exists to avoid.

## Implementation Outline

1. **Track A** — scaffold `kernel-core/src/acpi/aml/{decode,interp,object}.rs`: opcode/`PkgLength`/`NameString` decode, control-method evaluator, named-object model, the `RegionSpace` backend trait + a mock, and the bounded-recursion/loop + no-panic safety guards. Host tests over a captured DSDT.
2. **Track B** — `kernel-core/src/acpi/namespace.rs`: node arena + path resolution, DSDT+SSDT merge, `_STA` evaluation, `find_by_hid` / `_CID` / `EisaId` decode. Host tests assert `DLL0945` resolves on the captured Dell DSDT.
3. **Track C** — `kernel-core/src/acpi/resource.rs`: resource-stream decoder → `DeviceResources`; I2C SerialBus, GpioInt/GpioIo, IRQ/Memory descriptors. Host test decodes the touchpad's `_CRS` (slave address + GpioInt).
4. **Track E** — scaffold `userspace/acpid` (four-place wiring); the thin kernel surface (table-blob accessor, SCI `Notification` subscribe, the `RegionSpace`-over-`device_host` backend); the IPC query/event service. Record the split rationale.
5. **Track D** — extend `parse_fadt` (DSDT/SCI_INT/PM1/GPE); route the SCI GSI through `ioapic_write_redir` to a new ISR; the kernel PM1/GPE demux + mask + `Notification` signal; `acpid` GPE/`_Lxx`/`_Exx`/`_Qxx` dispatch + `Notify()` routing.
6. **Track F** — host tests on the captured DSDT (always-on CI); the QEMU `acpi-smoke` gate (namespace build + power-button SCI); the `scripts/acpi-baremetal-validate.md` runbook + the recorded Dell run; the `M3OS_ACPI_REGRESSION` AGENTS row.

## Acceptance Criteria

- The AML interpreter evaluates the device-enumeration opcode subset (`Store`/`If`/`Else`/`While`/`Return`/`Local`/`Arg`, integer + logical ops, `Package`/`Buffer`/`Field`/`Method`/`OperationRegion`) — host tests in `kernel-core` over a captured Dell Tiger Lake DSDT evaluate `_STA` to its expected value and never panic on truncated/malformed AML (return `AmlError`).
- The namespace builds from one DSDT + ≥1 SSDT (merged), and `find_by_hid("DLL0945")` returns the touchpad device node; `EisaId` decode round-trips `PNP0C0A`/`PNP0C0D`/`PNP0C0C` (battery/lid/power-button) — host-tested.
- `_CRS` decode yields the touchpad's **I2C slave address + the controller `ResourceSource` path** and its **`GpioInt` pin + polarity/trigger** as a populated `DeviceResources` struct — host-tested on the captured `_CRS` bytes, and **Validated-on-HW** that the values match the real device (the address the Phase 102 driver attaches at).
- `parse_fadt` reads and logs `SCI_INT` + `GPE0_BLK` + a non-zero DSDT pointer on the Dell; the SCI GSI is routed to its ISR via `ioapic_write_redir` (level/active-low).
- An SCI fires, the kernel demuxes + masks the asserted PM1/GPE bits (no interrupt storm) and signals `acpid`; `acpid` evaluates the matching `_Lxx`/`_Exx`/`_Qxx` method, re-enables the GPE, and routes a `Notify()` to a subscriber — exercised for the **power-button** in QEMU (`acpi-smoke`, `qmp system_powerdown`) and **Validated-on-HW** for the **lid switch** on the Dell.
- The AML interpreter runs in ring-3 `acpid`; ring 0 contains no AML VM — only the FADT parse, the SCI demux, and the thin table-blob / `Notification` / register surface.
- The host-testable + QEMU-testable surface is green in CI (`acpi-smoke` + the `kernel-core` host tests); the HW-only arms carry `Validated-on-HW (run N, date)` per `docs/appendix/bare-metal-validation.md`, with captured log-sink sentinels referenced from `scripts/acpi-baremetal-validate.md`.

## Companion Task List

- [Phase 101 Task List](./tasks/101-acpi-platform-foundation-tasks.md)

## How Real OS Implementations Differ

- **Linux / *BSD embed ACPICA** (or, in newer Linux, are migrating subsystems toward `uACPI`) — a ~50 K-line reference AML interpreter with the full operator set, the global lock, `_REG`/`_INI`/`_OSI` method evaluation, full EC transaction handling, dynamic table load (`Load`/`LoadTable`), and runtime device hotplug. Phase 101's interpreter is a *device-enumeration subset* — enough to build the namespace and evaluate `_STA`/`_HID`/`_CID`/`_CRS` + the laptop GPE methods, deliberately not a complete VM.
- Production stacks run the interpreter **in the kernel** (ACPICA is kernel-resident); m3OS runs it in **ring-3 `acpid`** for fault isolation and host-testability, keeping only the SCI hardware-ack privileged — a microkernel-idiomatic split closer to how some research microkernels host ACPI than to a monolith.
- Real ACPI subsystems handle the **embedded controller** (`PNP0C09`) with a dedicated EC driver, burst mode, and the `_GPE`/`_Qxx` query protocol; this phase implements the minimum EC `OperationRegion` access its GPE methods touch, not a full EC stack (that grows in Phase 103).
- Mature OSes treat power/thermal/lid as a long-lived subsystem with `_PSV`/`_CRT` thermal trip points, `_PSS`/`_PCT` cpufreq, and full S3/S0ix; Phase 101 only delivers the *enumeration + event-delivery substrate* — the policy is Phase 103.

## Deferred Until Later

- **Full ACPI power management** — battery (`_BST`/`_BIF`), thermal zones (`_TMP`/`_PSV`/`_CRT`), backlight, cpufreq (`_PSS`/`_PCT`), and S3/S0ix suspend-resume — is **Phase 103** (it consumes this phase's namespace + SCI substrate).
- **The I2C-HID touchpad datapath** (DesignWare LPSS I2C controller + I2C-HID transport + multitouch report parse → `mouse_server`) is **Phase 102** (it consumes this phase's `_HID`/`_CRS` query).
- **A complete embedded-controller driver** (burst mode, `_GPE` indirection, full `_Qxx` set) — only the EC `OperationRegion`/`_Qxx` access the laptop's GPE methods need is in scope here; the full EC stack rides Phase 103.
- **Dynamic table load** (`Load`/`LoadTable`/`Unload`), runtime device hotplug `Notify()` for non-laptop events, and AML operators outside the enumeration subset — deferred until a workload needs them.
- **AMD platform ACPI specifics** (the OmniBook/Strix Point `AMDI0010` I2C + `AMDI0030` GPIO `_CRS` variants, `pinctrl-amd`) — the decode is the same `_CRS` machinery, but the bare-metal validation is **Phase 108**.
- **ACPI on real AMD silicon validation** generally — covered by the Phase 108 OmniBook bring-up; Phase 101's HW validation is on the Intel Dell only.
