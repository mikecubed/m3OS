# Phase 101 — ACPI Platform Foundation (AML + device/resource enumeration + SCI): Task List

**Status:** Planned
**Source Ref:** phase-101
**Depends on:** Phase 15 (ACPI table parse — RSDP/RSDT/XSDT, MADT, FADT) ✅, Phase 55a (DMAR/IVRS decode + the `kernel-core` host-tested table-decoder pattern) ✅, Phase 55b (capability-gated device-host syscalls + `Notification` IRQ objects) ✅
**Goal:** Build an ACPI **namespace** on top of the existing static-table parse: a pragmatic AML interpreter (Track A), the namespace + `_HID`/`_CID` device tree (Track B), `_CRS` resource decode (Track C), SCI/GPE event handling + `Notify()` routing (Track D), a ring-3 `acpid` hosting the interpreter over a thin kernel surface (Track E), and host + bare-metal validation (Track F). The end state: on the Dell Tiger Lake, an `_HID` lookup for `DLL0945` finds the touchpad node and its `_CRS` yields the I2C address + `GpioInt` the Phase 102 driver needs, and a lid/power SCI is demuxed and routed to userspace.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | AML interpreter (device-enumeration subset) in `kernel-core` — host-tested | — | Planned |
| B | Namespace build + device tree + `_HID`/`_CID` matching + `_STA` | A | Planned |
| C | `_CRS` resource decode (I2C SerialBus / GpioInt / IRQ / Memory) | A, B | Planned |
| D | SCI handler + GPE dispatch + `_Lxx`/`_Exx`/`_Qxx` + `Notify()` routing | A, B, E | Planned |
| E | Ring-3 `acpid` hosting + thin kernel surface + IPC query/event service (the split decision) | A, B, C | Planned |
| F | Validation — host tests on captured DSDT, QEMU `acpi-smoke`, bare-metal run | A, B, C, D, E | Planned |

---

## Track A — AML Interpreter (Device-Enumeration Subset)

> Reference ACPICA/uACPI *behavior*, not source. Lives in `kernel-core` so it is pure logic and host-tested, mirroring `kernel-core/src/iommu/tables.rs`.

### A.1 — AML stream decoder (opcodes, `PkgLength`, `NameString`)

**File:** `kernel-core/src/acpi/aml/decode.rs` (new)
**Symbol:** `AmlDecoder`, `decode_pkg_length`, `decode_name_string`, `Opcode`
**Why it matters:** Every later step (method eval, namespace build) consumes a decoded AML term stream; the variable-length `PkgLength` and the root/parent-prefix (`\`/`^`)/multi-segment `NameString` encodings are the foundation of the whole format.

**Acceptance:**
- [ ] `decode_pkg_length` decodes the 1–4-byte variable-length encoding (lead-byte `<6:4>` count) and is host-tested across all four widths.
- [ ] `decode_name_string` handles `RootChar`, `ParentPrefixChar` runs, `DualNamePrefix`/`MultiNamePrefix`, and a bare 4-char `NameSeg`.
- [ ] The opcode table covers the device-enumeration subset (Zero/One/Ones, Byte/Word/DWord/QWord prefixes, `Package`/`Buffer`, `Scope`/`Device`/`Method`/`Name`, `OperationRegion`/`Field`, `Store`/`If`/`Else`/`While`/`Return`, the integer + logical ops, `Local0..7`/`Arg0..6`) and decodes a captured DSDT term-by-term with **0 unknown-opcode panics**.

### A.2 — Control-method evaluator

**File:** `kernel-core/src/acpi/aml/interp.rs` (new)
**Symbol:** `Interpreter::eval_method`, `AmlValue`, `MethodScope`
**Why it matters:** `_STA`/`_HID`/`_CID`/`_CRS` and the GPE methods are AML *methods*; without an evaluator the namespace is inert bytecode.

**Acceptance:**
- [ ] Evaluates `Store`/`If`/`Else`/`While`/`Return`, integer arithmetic (`Add`/`Subtract`/`And`/`Or`/`ShiftLeft`/`ShiftRight`) and logical ops (`LEqual`/`LGreater`/`LAnd`/`LNot`), with `Local0..7`/`Arg0..6` scoping.
- [ ] A synthetic `_STA` method returning `0x0F` and a captured-DSDT `_STA` both evaluate to the expected `AmlValue::Integer` — host-tested.
- [ ] Method invocation passes args + returns a value; recursion is bounded (see A.4).

### A.3 — Named-object model + `OperationRegion`/`Field` + `RegionSpace` backend trait

**File:** `kernel-core/src/acpi/aml/object.rs` (new)
**Symbol:** `NamedObject`, `OperationRegion`, `FieldUnit`, `trait RegionSpace`
**Why it matters:** AML reaches hardware through `OperationRegion` (`SystemMemory`/`SystemIO`/`PCI_Config`/`EmbeddedController`) + `Field`; abstracting that behind a trait keeps the interpreter pure (mock backend in tests, `device_host`-backed in `acpid` per E.3).

**Acceptance:**
- [ ] `OperationRegion` + `Field` declarations register `FieldUnit`s with correct bit offset/width.
- [ ] A `Field` read/write is delegated to `RegionSpace::read`/`write` (region kind + offset + width), and a `Vec`-backed mock returns the backing byte — host-tested.
- [ ] The four region spaces are distinguishable by `RegionSpace` kind; unsupported spaces return `AmlError::UnsupportedRegion` rather than panicking.

### A.4 — Interpreter safety limits (untrusted bytecode)

**File:** `kernel-core/src/acpi/aml/interp.rs`
**Symbol:** `Interpreter::{MAX_METHOD_DEPTH, MAX_LOOP_ITERS}`, `AmlError`
**Why it matters:** AML is arbitrary firmware bytecode; a malformed/hostile DSDT must not panic the interpreter or loop forever (it runs in `acpid`, but a panic there loses ACPI for the whole system).

**Acceptance:**
- [ ] Method recursion beyond `MAX_METHOD_DEPTH` returns `AmlError::RecursionLimit`, not a stack overflow.
- [ ] A `While` exceeding `MAX_LOOP_ITERS` returns `AmlError::LoopLimit`.
- [ ] A truncated / byte-corrupted DSDT slice returns `Err(AmlError)` for **every** offset in a host-side truncation sweep (no panic, no UB).

---

## Track B — Namespace Build + Device Tree + Matching

### B.1 — Namespace node arena + path resolution

**File:** `kernel-core/src/acpi/namespace.rs` (new)
**Symbol:** `Namespace`, `NodeId`, `NodeKind` (`Scope`/`Device`/`Method`/`Name`/`Processor`/`ThermalZone`), `Namespace::resolve_path`
**Why it matters:** Devices, methods, and resources are nodes in a tree; without path resolution (`\_SB.PCI0.I2C1`) nothing can be looked up by name.

**Acceptance:**
- [ ] Building from a captured DSDT yields a node count > 0 with a `\` root and a `\_SB` system-bus scope.
- [ ] `resolve_path` resolves absolute (`\_SB.PCI0`), parent-prefixed (`^^DEV`), and relative names against a current scope — host-tested.

### B.2 — DSDT + SSDT load + merge

**File:** `kernel-core/src/acpi/namespace.rs`
**Symbol:** `Namespace::load_definition_block`, `Namespace::from_tables`
**Why it matters:** Real firmware splits objects across one DSDT + several SSDTs, and an SSDT routinely *extends* a scope (e.g. `Scope(\_SB.PCI0)`) defined in the DSDT; loading only the DSDT misses devices.

**Acceptance:**
- [ ] A DSDT + ≥1 SSDT merge into one namespace; a `Scope(...)` in the SSDT that extends a DSDT-defined path adds children under the existing node (no duplicate root).
- [ ] Host test loads a 2-block fixture and asserts a cross-block device resolves.

### B.3 — `_HID`/`_CID` matching + `EisaId` decode

**File:** `kernel-core/src/acpi/namespace.rs`
**Symbol:** `Namespace::find_by_hid`, `Namespace::find_by_cid`, `decode_eisa_id`
**Why it matters:** Drivers attach by `_HID`/`_CID`; the touchpad is `DLL0945` (a string `_HID`) and the battery/lid/power-button are integer `EisaId`-encoded `_HID`s — both forms must resolve.

**Acceptance:**
- [ ] `find_by_hid("DLL0945")` returns the touchpad device node on the captured Dell DSDT.
- [ ] `decode_eisa_id` round-trips `PNP0C0A` (battery), `PNP0C0D` (lid), `PNP0C0C` (power button) between the packed 32-bit integer form and the 7-char string — host-tested.
- [ ] A device matched by `_CID` (when `_HID` differs) is found via `find_by_cid`.

### B.4 — `_STA` presence/enable filtering

**File:** `kernel-core/src/acpi/namespace.rs`
**Symbol:** `Namespace::device_present`, `Namespace::iter_present_devices`
**Why it matters:** A device with `_STA` bit 0 (present) clear must not be enumerated; the spec also says an absent `_STA` means present + enabled.

**Acceptance:**
- [ ] `device_present` evaluates `_STA` and treats absent `_STA` as present (`0x0F`).
- [ ] A fixture device with `_STA` returning `0` is excluded from `iter_present_devices`; one returning `0x0F` is included — host-tested.

---

## Track C — `_CRS` Resource Decode

### C.1 — Resource-descriptor stream decoder

**File:** `kernel-core/src/acpi/resource.rs` (new)
**Symbol:** `decode_resources`, `ResourceItem`, `parse_small_item`/`parse_large_item`
**Why it matters:** `_CRS` returns a `Buffer` of chained small/large resource descriptors terminated by an end tag; every resource type rides this framing.

**Acceptance:**
- [ ] Decodes small (`<7>`=0) and large (`<7>`=1) resource items by tag, stops at the End Tag (0x79), and validates the end-tag checksum.
- [ ] A truncated/over-long `_CRS` buffer returns `Err`, not a panic — host-tested.

### C.2 — I2C SerialBus connection descriptor

**File:** `kernel-core/src/acpi/resource.rs`
**Symbol:** `I2cSerialBus`, `parse_serial_bus` (large item 0x8E, bus type 1)
**Why it matters:** The touchpad's I2C **slave address** and the **controller it sits on** come only from its `_CRS` I2C SerialBus descriptor — the single most important value Phase 102 needs.

**Acceptance:**
- [ ] Decodes slave address, bus speed, addressing mode, and the `ResourceSource` controller path (e.g. `\_SB.PC00.I2C1`) from the touchpad's `_CRS`.
- [ ] Host test asserts the captured `DLL0945` `_CRS` yields its expected 7-bit slave address + a non-empty controller path.

### C.3 — GpioInt / GpioIo descriptor

**File:** `kernel-core/src/acpi/resource.rs`
**Symbol:** `GpioInt`, `GpioIo`, `parse_gpio` (large item 0x8C)
**Why it matters:** The I2C-HID transport is interrupt-driven over a SoC GPIO line; the `GpioInt` pin + polarity/trigger from `_CRS` is what the Phase 102 driver arms.

**Acceptance:**
- [ ] Decodes GPIO connection type (Int vs Io), pin number(s), edge/level + active-high/low flags, and the GPIO controller `ResourceSource`.
- [ ] Host test asserts the touchpad's `GpioInt` pin + (level, active-low) flags from the captured `_CRS`.

### C.4 — IRQ / Memory32Fixed / FixedMemory descriptors

**File:** `kernel-core/src/acpi/resource.rs`
**Symbol:** `IrqResource`, `MemoryRange`, `parse_irq`, `parse_fixed_memory`
**Why it matters:** The embedded controller and legacy/platform devices report classic IRQ + MMIO-window resources; Phase 103's EC and any MMIO device need these.

**Acceptance:**
- [ ] Decodes the small IRQ descriptor (0x22/0x23) and the large Memory32Fixed (0x86) / FixedMemory descriptors into `IrqResource` / `MemoryRange`.
- [ ] Host test decodes an EC (`PNP0C09`) `_CRS` IRQ + MMIO window from a fixture.

### C.5 — Resolved `DeviceResources` query struct

**File:** `kernel-core/src/acpi/resource.rs`
**Symbol:** `DeviceResources { i2c: Option<I2cSerialBus>, gpio_int: Option<GpioInt>, irqs, mmio }`, `Namespace::device_resources`
**Why it matters:** Other drivers want one struct answering "what bus/address/IRQ/GPIO is device X on" — not a raw descriptor stream.

**Acceptance:**
- [ ] `Namespace::device_resources(node)` evaluates `_CRS` and returns a populated `DeviceResources` (interpreter → resource decode end-to-end).
- [ ] Host test: `device_resources(find_by_hid("DLL0945"))` returns `i2c.slave_address` and `gpio_int.pin` both populated.

---

## Track D — SCI Handler + GPE Dispatch + `Notify()` Routing

### D.1 — FADT extension (DSDT pointer + SCI/PM/GPE fields)

**File:** `kernel/src/acpi/mod.rs`
**Symbol:** `parse_fadt` (extend), new `FadtInfo` + `FADT_INFO: Once<FadtInfo>`
**Why it matters:** `parse_fadt` reads only `IAPC_BOOT_ARCH` (offset 109) today; the DSDT pointer (offset 40 / `X_DSDT` 140), `SCI_INT` (offset 46), the PM1a event/control blocks, and `GPE0_BLK`/`GPE0_BLK_LEN` are needed to find the namespace and receive the SCI.

**Acceptance:**
- [ ] `parse_fadt` reads + caches the DSDT/`X_DSDT` pointer, `SCI_INT`, `PM1a_EVT_BLK`/`PM1a_CNT_BLK`, and `GPE0_BLK`/`GPE0_BLK_LEN` into `FADT_INFO`.
- [ ] On the Dell the boot log shows a non-zero DSDT pointer + the `SCI_INT` GSI + the `GPE0_BLK` I/O port (`Validated-on-HW`); host-side parse test over a captured FADT asserts the field offsets.

### D.2 — SCI ISR + IOAPIC redirection for the SCI GSI

**Files:**
- `kernel/src/arch/x86_64/apic.rs`
- `kernel/src/arch/x86_64/interrupts.rs`

**Symbol:** `ioapic_route_sci` (new, over `ioapic_write_redir`/`gsi_to_pin`), `sci_interrupt_handler` (new ISR)
**Why it matters:** The SCI is a level-triggered, active-low GSI; it must be routed to a dedicated vector exactly like the ISA IRQs `ioapic_init` already programs, or no ACPI event is ever received.

**Acceptance:**
- [ ] The `SCI_INT` GSI (from D.1, honoring any `acpi::irq_override`) is programmed level-triggered/active-low to a new ISR vector.
- [ ] In QEMU, a `qmp system_powerdown` raises the SCI and increments an SCI-received counter (asserted by `acpi-smoke`, F.2).

### D.3 — Kernel SCI demux + hardware ack/mask + `Notification` signal

**File:** `kernel/src/acpi/sci.rs` (new)
**Symbol:** `sci_demux`, signals the `acpid` `Notification`
**Why it matters:** A level-triggered SCI will storm if userspace is the only handler; the kernel must read PM1_STS/GPE_STS, mask the asserted enable bits, EOI, and hand the *pending bitmap* to `acpid` — the privileged half of the split.

**Acceptance:**
- [ ] The ISR reads `PM1_STS` + `GPE_STS`, masks the asserted bits in `PM1_EN`/`GPE_EN`, EOIs, and signals `acpid`'s `Notification` with the pending event word — no interrupt storm (the line de-asserts).
- [ ] The pending PM1/GPE bitmap reaches `acpid` (verified by the QEMU power-button arm and the HW lid arm).
- [ ] No allocation / blocking / IPC-call inside the ISR (per the interrupt-handler convention) — only a register read/mask + `Notification` signal.

### D.4 — `acpid` GPE / fixed-event dispatch (`_Lxx`/`_Exx`/`_Qxx`)

**File:** `userspace/acpid/src/gpe.rs` (new)
**Symbol:** `dispatch_gpe`, `dispatch_fixed_event`, EC `_Qxx` query
**Why it matters:** Each asserted GPE bit maps to a `_Lxx` (level) or `_Exx` (edge) method; EC events run `_Qxx`; fixed events (power button `PWRB`, lid `LID0`) map to their handlers. Running them is the *policy* that makes an SCI mean something.

**Acceptance:**
- [ ] For each pending GPE bit, `acpid` evaluates the matching `_Lxx`/`_Exx` method (or the EC `_Qxx`) via the Track A interpreter, then re-enables that GPE through the kernel.
- [ ] The power-button fixed event runs its path in QEMU (`acpi-smoke`); the lid `_LID` returns a state on the Dell (`Validated-on-HW`).

### D.5 — `Notify()` routing to ring-3 subscribers

**File:** `userspace/acpid/src/notify.rs` (new)
**Symbol:** `route_notify(device_path, code)`, the subscriber table
**Why it matters:** AML control methods emit `Notify(device, code)` (battery `0x80` status-change, lid/dock changes); these must reach the Phase 103 power daemon / session over IPC, or events are evaluated and dropped.

**Acceptance:**
- [ ] A `Notify(dev, code)` executed by a GPE method is delivered to every subscriber of `dev` (or a wildcard) as `(device_path, notify_code)` over the E.4 IPC service.
- [ ] Host/integration test: a synthetic GPE method issuing `Notify` reaches a subscribed test client with the correct path + code.

---

## Track E — Ring-3 `acpid` Hosting + Thin Kernel Surface (The Split Decision)

### E.1 — `acpid` crate scaffold + four-place wiring

**Files:**
- `userspace/acpid/Cargo.toml`, `userspace/acpid/src/main.rs`
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`bins` array in `build_userspace`)
- `kernel/src/fs/ramdisk.rs` (`include_bytes!` + `BIN_ENTRIES`)
- `xtask/src/main.rs` `populate_ext2_files` + `userspace/init/src/main.rs` `KNOWN_CONFIGS` (a `services.d/acpid.conf`)

**Symbol:** `main` (daemon entry), `acpid.conf`
**Why it matters:** Missing any of the four wiring points means `acpid` is not built, not embedded, or not started (per the "Adding a New Userspace Binary" rule). `needs_alloc = true` (uses `kernel-core`/`Vec`).

**Acceptance:**
- [ ] `cargo xtask check` builds `acpid`; it is embedded in the ramdisk and launched from `services.d/acpid.conf` (and `init`'s builtin defaults for the no-data-disk bare-metal path).
- [ ] Defines a `#[global_allocator]` (`syscall_lib::heap::BrkAllocator`) and enables the `alloc` feature on `syscall-lib`.

### E.2 — Thin kernel ACPI surface (table blob + SCI subscribe + PM/GPE access)

**Files:**
- `kernel/src/syscall/` (new `sys_acpi_*` arm or a `device_host` extension)
- `kernel_core::device_host::syscalls` (any new constant)

**Symbol:** `sys_acpi_table_blob` (DSDT/SSDT bytes), SCI `Notification` subscribe (reusing the Phase 55b `SYS_DEVICE_IRQ_SUBSCRIBE` ISR-shim pattern), PM1/GPE register access
**Why it matters:** `acpid` must fetch the firmware bytes the FADT points at, bind the SCI as a `Notification`, and read/write PM1/GPE — without these the ring-3 interpreter has no input and no events.

**Acceptance:**
- [ ] `acpid` fetches the DSDT (+ each SSDT) bytes read-only from the kernel (located via `find_table`/`SDT_ENTRIES`, translated through `phys_to_virt`) and the namespace builds.
- [ ] `acpid` subscribes the SCI `Notification` and is woken when D.3 signals it.
- [ ] `acpid` reads `PM1_STS` / re-enables a `GPE_EN` bit through the surface (PIO/MMIO, capability-gated) — and a non-`acpid` process is denied.

### E.3 — `RegionSpace` backend over `device_host` syscalls

**File:** `userspace/acpid/src/region.rs` (new)
**Symbol:** `impl RegionSpace for DeviceHostRegions` (`SystemIO`/`SystemMemory`/`PCI_Config`/`EmbeddedController`)
**Why it matters:** AML `OperationRegion` reads/writes (e.g. a `_STA` that consults an EC register) must hit real hardware; this is the production implementation of the Track A.3 trait, delegating to the existing capability-gated `device_host` PIO/MMIO/config syscalls.

**Acceptance:**
- [ ] `SystemIO`/`SystemMemory`/`PCI_Config` reads/writes route to `SYS_DEVICE_PIO_*` / `SYS_DEVICE_MMIO_MAP` / `SYS_DEVICE_CONFIG_*`.
- [ ] A `Field`-backed `_STA` reading a real EC/PM register evaluates against hardware on the Dell (`Validated-on-HW`); an `EmbeddedController` region read returns the EC byte.

### E.4 — `acpid` IPC query/event service

**File:** `userspace/acpid/src/service.rs` (new)
**Symbol:** `AcpiQuery::{FindByHid, GetResources, Subscribe}`, the event-push channel
**Why it matters:** Phase 102 (touchpad) and Phase 103 (power) attach by asking `acpid` to resolve a device + its resources and to subscribe to its events; this is the public face of the namespace.

**Acceptance:**
- [ ] A client resolves `FindByHid("DLL0945")` → a device handle, `GetResources` → a `DeviceResources` (I2C addr + GpioInt), over IPC.
- [ ] A client `Subscribe`s to a device and receives a `Notify()` event pushed from D.5.
- [ ] The protocol is documented in the crate header (the contract Phase 102/103 consume).

### E.5 — Split-decision record (ring-3 interpreter, ring-0 SCI demux)

**File:** `userspace/acpid/src/main.rs` (module doc) + the phase design doc
**Symbol:** the architecture rationale comment
**Why it matters:** The spec requires deciding the ring-0-vs-ring-3 split honestly; recording *why* (interpreter size, fault isolation, host-testability) prevents a future drift back toward an in-kernel VM.

**Acceptance:**
- [ ] The rationale is recorded: AML interpreter in ring-3 `acpid`; ring 0 keeps only the FADT parse + SCI hardware-ack/demux + the thin blob/`Notification`/register surface.
- [ ] A grep of the kernel tree finds **no** AML opcode evaluator symbols in ring 0 (the interpreter lives only in `kernel-core` + `acpid`).

---

## Track F — Validation

> Per `docs/appendix/bare-metal-validation.md`: the host-testable + QEMU-testable surface stays in CI; the HW-only laptop-device arms carry `Validated-on-HW (run N, date)` — never a bare "Complete."

### F.1 — Host tests on a captured DSDT (always-on CI)

**Files:**
- `kernel-core/src/acpi/` tests
- `kernel-core/src/acpi/tests/fixtures/dell-tgl-dsdt.aml` (new — captured via `acpidump`)

**Symbol:** the `#[cfg(test)]` suites across `aml`/`namespace`/`resource`
**Why it matters:** The AML opcode subset, namespace build, `_HID` match, and `_CRS` decode are pure logic — exactly the surface the bare-metal validation strategy says must stay falsifiable in CI even though the devices are HW-only.

**Acceptance:**
- [ ] A committed Dell Tiger Lake DSDT fixture drives host tests: opcode-subset decode (0 unknown-opcode panics), namespace build, `find_by_hid("DLL0945")` resolves, and the touchpad `_CRS` yields I2C addr + GpioInt.
- [ ] `cargo xtask check` runs these (`kernel-core` host tests) green; truncation-sweep tests assert no panic on malformed AML.

### F.2 — QEMU `acpi-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_acpi_smoke` (new) + `M3OS_ACPI_REGRESSION`
**Why it matters:** QEMU models a generic ACPI namespace + the power-button SCI, so the *substrate* (namespace build, SCI demux → `acpid` dispatch → `Notify`) is CI-testable without the laptop — maximizing the non-HW-only surface.

**Acceptance:**
- [ ] Boots m3OS, asserts `acpid` built the namespace from QEMU's DSDT and enumerated the emulated devices (a sentinel line).
- [ ] A `qmp system_powerdown` raises the power-button SCI; the gate asserts the kernel demux signalled `acpid`, the power-button method ran, and a `Notify()` reached a test subscriber.
- [ ] `M3OS_ACPI_REGRESSION=1` row added to the `AGENTS.md` gate table; the laptop-device arms skip-with-reason in QEMU.

### F.3 — Bare-metal validation run (Dell Tiger Lake)

**File:** `scripts/acpi-baremetal-validate.md` (new runbook + results appendix)
**Symbol:** the recorded run
**Why it matters:** The headline claim — the touchpad `_HID`/`_CRS` and lid/battery SCI on real silicon — can only be proven on the machine; this records it per the established protocol.

**Acceptance:**
- [ ] Following `docs/appendix/bare-metal-validation.md`: boot the Dell from USB, capture (AMT SOL pre-network + `usb-logsink` boot.log / network sink) `acpid` enumeration, the `DLL0945` `_HID` resolve, its `_CRS` I2C address + `GpioInt`, and a lid/power-button SCI delivered + routed.
- [ ] The captured sentinels are quoted in the runbook results appendix; the design-doc / README Status carries `Validated-on-HW (run N, date)` for the HW-only arms.

---

## Documentation Notes

- This phase **extends** Phase 15's static-table parse (`kernel/src/acpi/mod.rs`) — it adds the AML/namespace layer; it does **not** replace `find_table`/`parse_madt`/`parse_mcfg`/`parse_dmar`/`parse_ivrs`, which stay as-is.
- The AML interpreter + `_CRS` decoder live in `kernel-core` (host-tested), deliberately mirroring `kernel-core/src/iommu/tables.rs` (`decode_dmar`/`decode_ivrs`) — record the precedent when it lands.
- `acpid` is the first ring-3 host for a *firmware bytecode interpreter*; note in the crate header that the split (ring-3 interpreter, ring-0 SCI demux) is deliberate and reuses the Phase 55b `device_host` + `Notification` substrate.
- This phase is the **gating substrate for Phase 102 (I2C-HID touchpad)** (consumes `FindByHid`/`GetResources`) and **Phase 103 (laptop power)** (consumes the SCI/GPE event routing). Keep the `acpid` query/event protocol (E.4) driver-agnostic — record the cross-reference when 102/103 land.
- `EisaId`/AML behavior is referenced against ACPICA/uACPI; keep the reference-not-copied note in the `kernel-core/src/acpi/` header (matching the BSD-`ure(4)` / `mt76` citation convention used elsewhere).
- Prefer exact files/symbols over directories as these land; update this list's checkboxes as tracks complete, and use `Validated-on-HW (run N, date)` rather than `Complete` for the HW-only Track F arms.
