# Phase 78b — USB Host Foundation: Enumeration + Hub: Task List

**Status:** Planned
**Source Ref:** phase-78b
**Depends on:** Phase 78a (xHCI Host-Controller Bring-Up), Phase 74 (IPC Capability Grants / page-grant bulk transport) ✅, Phase 67 (IOMMU Substrate Completion) ✅
**Goal:** Make the live 78a controller discover devices: a host-testable USB core (descriptor parser + enumeration state machine), the host↔class IPC protocol crate, a `usbhub` class driver with a host-tested `PortId` topology, and the committed `sys_device_pci_enumerate` so all xHCI controllers are found. The full device tree enumerates to Configured and prints on boot. Kernel bumped to `0.78.1`. Second of three Phase 78 sub-phases ([78a](../78a-xhci-host-bringup.md) → [78b](../78b-usb-enumeration-hub.md) → [78c](../78c-usb-hid-and-release.md)).

> **Note:** Enumeration runs **inside** the `xhci` host driver (the Redox `xhcid` model), with the class-agnostic logic in the host-testable `kernel-core/src/usb/` + the shared `userspace/lib/usb-core` library. There is **no** separate "usb-core daemon" / `usb-core.conf` — `usb-core` is a library linked by `xhci`, `usbhub`, and (in 78c) `usb-hid`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | USB core: descriptor parser, enumeration state machine, host↔class IPC protocol crate | Phase 78a, Phase 74 ✅ | Planned |
| B | Hub class (`usbhub`) + `PortId` topology + build wiring for `usb-core`/`usbhub` | A | Planned |
| C | Multi-controller PCI class enumeration + `0.78.1` version bump | A, B | Planned |

---

## Track A — USB Core

### A.1 — Descriptor model + parser (host-testable in `kernel-core`)

**Files:**
- `kernel-core/src/usb/descriptor.rs` (new — pure-logic, host-tested)
- `userspace/lib/usb-core/` (new crate re-exporting the kernel-core types for ring-3 consumers)

**Symbol:** `DeviceDescriptor`, `ConfigDescriptor`, `InterfaceDescriptor`, `EndpointDescriptor`, `HidDescriptor`, `parse_config_tree`
**Why it matters:** Descriptor parsing is class-agnostic pure logic — it belongs in `kernel-core` where it is host-testable, unlike Redox which keeps it inside the `xhcid` binary. This is the shared foundation both the host enumerator and class drivers consume. `kernel-core/src/usb/` gained its `xhci/` submodule in 78a; this adds the device-model side.

**Acceptance:**
- [ ] Typed structs for device/config/interface/endpoint + HID descriptors; `parse_config_tree` walks a configuration blob (short read then full read by `wTotalLength`) into typed interfaces + endpoints
- [ ] Host tests parse real captured descriptor blobs for a boot keyboard, a boot mouse, and a hub, asserting `bInterfaceClass`/`SubClass`/`Protocol` and endpoint addresses/`bInterval`
- [ ] `userspace/lib/usb-core` exposes these types to ring-3 drivers (added as a Cargo member in B.2)

### A.2 — Enumeration state machine (Enable Slot → Address Device BSR → descriptors → Configure Endpoint)

**Files:**
- `userspace/drivers/xhci/src/enumerate.rs` (new — drives the controller)
- `kernel-core/src/usb/enumerate.rs` (new — the state machine, host-tested against a mock command/event interface)

**Symbol:** `EnumState`, `enumerate_device`, `address_device` (BSR two-step), `control_transfer` (Setup/Data/Status), `configure_endpoint`, `evaluate_context`
**Why it matters:** xHCI replaces the raw USB `SET_ADDRESS` with the `Address Device` command, but `Enable Slot`, `Configure Endpoint`, and `Evaluate Context` are **all** required besides it to attach a device and run an interrupt endpoint. The BSR=1 pre-read is needed to learn full-speed EP0 Max Packet Size before the real address assignment.

**Acceptance:**
- [ ] `Enable Slot` → allocate Output Device Context → install in `DCBAA[slot]`; for full-speed: `Address Device` BSR=1 (Default state, no SET_ADDRESS) → read EP0 Max Packet Size → `Evaluate Context` to correct it → `Address Device` BSR=0 to assign the address
- [ ] **`Address Device` Input Context fields** populated and host-tested: Input Control Context **Add Flags = `0x3`** (`A0` Slot + `A1` EP0 Default Control Endpoint); Slot Context = Route String, Root Hub Port Number, Speed, Context Entries (1); EP0 Endpoint Context = EP Type **Control**, Max Packet Size (per 78a speed: 8/8/64/512), TR Dequeue Pointer = EP0 transfer-ring IOVA with `DCS`, Error Count (`CErr`) = 3. (Missing these → Address Device returns a Context-State/Parameter error.)
- [ ] Control transfers issued as Setup Stage + (optional Data Stage) + Status Stage TRB sequences on the EP0 transfer ring; `GET_DESCRIPTOR(Device)` then `GET_DESCRIPTOR(Config)` short-then-full; `SET_CONFIGURATION(bConfigurationValue)`
- [ ] `Configure Endpoint` adds the interrupt-IN endpoint context after `SET_CONFIGURATION` (Add Flag at the endpoint's DCI per the 78a A.5 formula)
- [ ] The enumeration state machine (states + transitions + error/timeout handling) is host-tested with a mock interface in `kernel-core`
- [ ] Under `qemu-xhci`, an attached `usb-kbd` enumerates to Configured with its interrupt-IN endpoint running; the full descriptor tree is printed on boot

### A.3 — Host↔class IPC protocol crate + bulk via page grants

**Files:**
- `userspace/lib/usb-core/src/protocol.rs` (new — typed request/reply messages + thin client API)
- reuses the Phase 74 page-grant bulk transport

**Symbol:** `UsbRequest` (`GetDescriptors`, `ConfigureEndpoints`, `ControlRequest`, `SubmitTransfer`), `UsbClient` (the m3OS analogue of Redox `XhciClientHandle`)
**Why it matters:** This is the contract the host publishes and the class drivers consume. It must honor the m3OS IPC rule: descriptors and setup packets cross as small call/reply payloads; transfer buffers cross as **page-capability grants**, never as IPC payloads.

**Acceptance:**
- [ ] Typed protocol: open device, get-descriptors, configure-endpoints, control-request, submit interrupt/bulk transfer — defined once in `usb-core` and shared by `xhci`, `usbhub`, and (in 78c) `usb-hid`
- [ ] Descriptors + setup packets cross as IPC call/reply payloads; transfer buffers cross as Phase 74 page grants (verified: no transfer payload exceeds the IPC inline payload path)
- [ ] A thin `UsbClient` library API (not raw IPC opcodes) is what class drivers link. **Lifecycle model:** the class drivers are **static long-lived daemons** started by `session_manager` (not per-device children forked by the host — the userspace-first rule forbids host-forks-children). On attach, the host (`xhci`) sends a **device-attach IPC notification** carrying `(port, interface class/subclass/protocol)` to the matching running daemon, which then drives that interface via `UsbClient`. The handoff is an **IPC message to a running daemon**, never `exec` arguments to a freshly spawned process

---

## Track B — Hub Class + Build Wiring

### B.1 — Hub class support + `PortId` topology

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

### B.2 — Build + ramdisk + service wiring for `usb-core` + `usbhub`

**Files:**
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`build_userspace_bins`, line 795; `bins` array, line 800; `populate_ext2_files` service configs, ~line 12597)
- `kernel/src/fs/ramdisk.rs` (`DRIVERS_ENTRIES`, line 1150)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`, lines 185–230)
- `kernel/initrd/etc/services.d/usbhub.conf` (new)

**Symbol:** `bins` tuple `(pkg, bin, needs_alloc=true)`, `DRIVERS_ENTRIES`, `usbhub.conf`
**Why it matters:** `usb-core` is a library linked by the drivers (no service); `usbhub` is a ring-3 driver that must be staged under `/drivers/` (not `/bin/`) or the `is_authorized_driver_process` gate denies `sys_device_claim`. (`xhci` was staged in 78a; `usb-hid` is staged in 78c.)

**Acceptance:**
- [ ] `usb-core` added as a Cargo `member` (library, no service); `usbhub` added as a Cargo `member` + `bins` entry with `needs_alloc = true`
- [ ] `usbhub` binary embedded in `DRIVERS_ENTRIES` (`ramdisk.rs`) at `/drivers/usbhub`
- [ ] `usbhub.conf` added to `kernel/initrd/etc/services.d/` **and** `init` `KNOWN_CONFIGS`; uses `command=/drivers/usbhub`, `type=daemon`, `restart=on-failure`, `depends=xhci`
- [ ] `cargo xtask clean` run after adding the config (forces ext2 disk recreation)

---

## Track C — Multi-Controller Discovery + Release

### C.1 — Ring-3 PCI class enumeration for controller discovery

**Files:**
- `kernel/src/syscall/device_host.rs` (new enumeration syscall)
- `kernel-core/src/device_host/syscalls.rs` (syscall-number registration, currently `0x1120`–`0x1126`)
- `kernel/src/pci/mod.rs` (`PciMatch::ClassSubclass`, line 1555)

**Symbol:** `sys_device_pci_enumerate(class, subclass, prog_if)` (new, committed); `crate::pci::PciMatch::ClassSubclass` (`kernel/src/pci/mod.rs:1555`, currently `#[allow(dead_code)]`) as the in-kernel matcher to expose
**Why it matters:** **Source-verified gap.** `sys_device_claim` takes a BDF only — there is no class-code filter, and no ring-3 PCI-config-space read path exists (NVMe/e1000 hardcode `SENTINEL_BDF`). Because the **headline milestone is the no-PS/2 laptop with six xHCI controllers**, the multi-controller path is committed here (a single QEMU controller still bootstraps on the 78a sentinel BDF as an interim, but is not the deliverable).

**Acceptance:**
- [ ] A new capability-gated syscall `sys_device_pci_enumerate(class, subclass, prog_if)` is added to `kernel/src/syscall/device_host.rs` (next free number after `0x1126` in `kernel-core/src/device_host/syscalls.rs`), returning the BDFs of every device matching class `0x0C0330`, built on the existing in-kernel `PciMatch::ClassSubclass` matcher (`pci/mod.rs:1555`, removing its `dead_code` allow)
- [ ] The new syscall is gated by the same `/drivers/` exec-path authorization as `sys_device_claim` (`is_authorized_driver_process`, `device_host.rs:126`)
- [ ] `xhci` discovers **all** xHCI controllers via this enumeration and claims each (one driver instance per controller, or one instance iterating the set) — no hardcoded BDF on the real-hardware path; the `qemu-xhci` sentinel BDF remains only as an interim bootstrap
- [ ] Host test: the enumeration filter returns exactly the class-`0x0C0330` BDFs from a synthetic PCI device list

### C.2 — Bump kernel version to `0.78.1`

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]` (will be `0.78.0` after 78a)
**Why it matters:** Each Phase 78 sub-phase bumps a patch version within the `0.78.x` line (the Phase 76 → 76b/76c/76d precedent). The "USB host stack" capability bullet in `AGENTS.md` still waits for 78c, when HID input is user-visible.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.78.1"`
- [ ] `Cargo.lock` regenerated (via `cargo xtask check`)
- [ ] `AGENTS.md` kernel version updated to `v0.78.1` (version string only)
- [ ] `docs/roadmap/README.md` Phase 78b row Status updated to "Complete"; design-doc + task-doc Status headers set to Complete
- [ ] `cargo xtask check` passes
- [ ] Git tag `v0.78.1` — recommended at sub-phase merge (left to the merge step)

---

## Documentation Notes

- **Enumeration is the class-agnostic discovery layer.** Keeping it host-testable in `kernel-core/src/usb/` (descriptor parser + state machine + `PortId`) means the logic is verified on the host; only the controller glue is ring-3-only.
- **Enumeration runs in the host driver**, not a separate daemon — `usb-core` is a shared library, not a service. This matches Redox `xhcid` and avoids an extra IPC hop.
- **The `Address Device` input context is a common silent-failure point** (A.2): the Add Flags must be `0x3` and the Slot/EP0 contexts must be fully populated or the command returns a Context-State error.
- **The multi-controller enumeration (C.1) is what makes the headline laptop goal reachable** — without it, only a single sentinel-BDF controller is usable.
- After adding the `usbhub.conf` service config (B.2), run `cargo xtask clean` to force ext2 disk recreation.
