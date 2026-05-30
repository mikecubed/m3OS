# Phase 78b - USB Host Foundation: Enumeration + Hub

**Status:** Planned
**Source Ref:** phase-78b
**Depends on:** Phase 78a (xHCI Host-Controller Bring-Up), Phase 74 (IPC Capability Grants / page-grant bulk transport) ✅, Phase 67 (IOMMU Substrate Completion) ✅
**Builds on:** Second sub-phase of the [Phase 78](./78-usb-host-foundation.md) USB theme. Takes the live xHCI controller from [78a](./78a-xhci-host-bringup.md) and makes it discover devices: a host-testable USB core (descriptor parsing + the enumeration state machine), a hub class driver, the host↔class IPC protocol crate, and the multi-controller PCI class enumeration the dev laptop needs. Sub-phase 78c adds the HID class driver and input integration on top.
**Primary Components:** `kernel-core/src/usb/` (descriptor + enumeration state machine, host-tested), `userspace/lib/usb-core/` (new — shared protocol + client lib), `userspace/drivers/usbhub/` (new), `userspace/drivers/xhci/` (enumeration driver glue), `kernel/src/syscall/device_host.rs` (new `sys_device_pci_enumerate`)

## Milestone Goal

With the controller running, m3OS enumerates the USB device tree: it issues `Enable Slot` → `Address Device` → the descriptor walk → `SET_CONFIGURATION` → `Configure Endpoint` for each device, enumerates hubs and their downstream ports, and prints the full enumerated tree on boot. On the dev laptop, the multi-controller PCI class enumeration means devices on any of the six xHCI controllers are discovered, not just one sentinel-BDF controller.

## Why This Phase Exists

78a proves the controller is alive but does nothing with it. Enumeration is the class-agnostic discovery layer every USB device — keyboard, mouse, hub, mass storage — depends on, and it is pure-logic-heavy (descriptor parsing, a state machine, the `PortId` topology) so it is highly host-testable and belongs in its own sub-phase before any class driver. It also lands the two structural pieces a real multi-device system needs: the host↔class IPC protocol crate (so 78c's HID driver has a contract to consume) and the PCI class-enumeration syscall (so the headline "six controllers on the laptop" goal is reachable, not just a single sentinel BDF).

## Learning Goals

- Learn how USB enumeration walks the descriptor tree: device → configuration → interface → endpoint, with the short-then-full config read by `wTotalLength`.
- Understand why xHCI replaces the raw USB `SET_ADDRESS` with the `Address Device` command, and why a full-speed device needs the BSR two-step to learn EP0 Max Packet Size.
- See why a hub is its own USB device that must be enumerated, powered, and reset before downstream devices are reachable, and how a `PortId` models the nested tree.
- Learn how a microkernel keeps bulk transfer data out of IPC payloads by crossing it as page-capability grants.

## Feature Scope

### Track A — USB core (host-testable in `kernel-core`, shared via `usb-core`)

The descriptor model + parser (`kernel-core/src/usb/descriptor.rs`), the enumeration state machine (`Enable Slot` → `Address Device` BSR two-step → descriptor walk → `SET_CONFIGURATION` → `Configure Endpoint`, host-tested against a mock command/event interface), and the host↔class IPC protocol crate (`usb-core`) where descriptors/setup cross as IPC payloads and transfer buffers cross as Phase 74 page grants.

### Track B — Hub class

A `usbhub` ring-3 driver (Redox `usbhubd` model): enumerate the hub interface, `SetPortFeature(PORT_POWER)` per downstream port, reset and walk them; the `PortId` topology (root-port + hub-depth + parent) host-tested so the nested-hub logic is verified even when QEMU cannot host a nested hub.

### Track C — Multi-controller discovery + version bump

A committed capability-gated `sys_device_pci_enumerate(class, subclass, prog_if)` (built on the existing in-kernel `PciMatch::ClassSubclass`) so `xhci` discovers every class-`0x0C0330` controller rather than a hardcoded BDF. Kernel version bumped to `0.78.1`.

## Important Components and How They Work

### USB enumeration

When a port reports Connect Status Change, the host resets the port (78a), reads the speed, issues `Enable Slot` to get a slot ID, allocates an Output Device Context into `DCBAA[slot]`, and runs `Address Device` (a BSR=1 pre-read first discovers full-speed EP0 Max Packet Size, corrected via `Evaluate Context`, then BSR=0 assigns the address). It walks the descriptor tree over EP0 control transfers, chooses a configuration with `SET_CONFIGURATION`, and brings the interrupt-IN endpoint into the controller with `Configure Endpoint`.

### Host↔class protocol

The `usb-core` crate defines the typed contract the host publishes and class drivers consume (`GetDescriptors`, `ConfigureEndpoints`, `ControlRequest`, `SubmitTransfer`) plus a thin `UsbClient` API (the m3OS analogue of Redox `XhciClientHandle`). Descriptors and setup packets cross as IPC call/reply payloads; transfer buffers cross as Phase 74 page grants, never as IPC payloads.

### Hub topology

A hub (`bDeviceClass 0x09`) is enumerated like any device, then its downstream ports are powered and reset. `PortId { root_hub_port, hub_depth, parent }` models the tree so nested hubs resolve to a route string.

## How This Builds on Earlier Phases

- Consumes the live controller, command/event rings, and contexts from 78a.
- Reuses Phase 74's page-grant bulk transport for transfer buffers in the `usb-core` protocol.
- Reuses Phase 67's IOMMU-routed `DmaBuffer<T>` for the per-device transfer rings and contexts allocated during enumeration.

## Implementation Outline

1. Descriptor model + parser in `kernel-core/src/usb/`, host-tested against captured blobs.
2. Enumeration state machine (Enable Slot → Address Device BSR → descriptors → SET_CONFIGURATION → Configure Endpoint), host-tested with a mock interface, then wired to the 78a controller; print the full tree on boot.
3. `usb-core` protocol + client lib.
4. `usbhub` driver + `PortId` topology (host-tested).
5. `sys_device_pci_enumerate` so all controllers are discovered.
6. Bump kernel to `0.78.1`.

## Acceptance Criteria

- Under `qemu-xhci`, an attached device enumerates to Configured and the full descriptor tree prints on boot.
- The descriptor parser and enumeration state machine are host-tested in `kernel-core/src/usb/` (incl. the BSR two-step and the `Address Device` input-context fields).
- A hub-behind-hub topology is host-tested via `PortId` (and exercised live via QEMU `nec-usb-xhci` + `usb-hub` if reachable, else documented).
- `sys_device_pci_enumerate` returns every class-`0x0C0330` BDF (host-tested against a synthetic device list) and `xhci` claims each controller it returns.
- Kernel bumped to `0.78.1`.

## Companion Task List

- [Phase 78b Task List](./tasks/78b-usb-enumeration-hub-tasks.md)

## How Real OS Implementations Differ

- Linux keeps much enumeration logic inside the host driver; m3OS factors the class-agnostic core into a host-testable `kernel-core/src/usb/` library shared by host, hub, and class drivers.
- Real stacks surface hot-plug to userspace (netlink/udev); m3OS at 1.0 enumerates once at boot.
- Production hubs handle Transaction Translators and split transactions for low/full-speed devices behind high-speed hubs; m3OS at 1.0 targets the common boot-device topology.

## Deferred Until Later

- HID class driver + input integration + the full `usb-smoke` keystroke gate (Phase 78c)
- USB Report Protocol parsing, mass storage, audio, video (post-1.0)
- Hot-plug event surface to userspace; Transaction-Translator split transactions
