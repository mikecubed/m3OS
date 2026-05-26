# Phase 78 - USB Host Foundation (xHCI + Hub + HID)

**Status:** Planned
**Source Ref:** phase-78
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 67 (IOMMU Substrate) ✅
**Builds on:** Extends the Phase 55b/55c ring-3 driver-host substrate with the first USB stack the project ships — a userspace xHCI host driver, a minimal USB core, and a HID class driver capable of presenting at least one keyboard and one mouse as the same `kbd_server` / `mouse_server` clients Phase 56 already feeds
**Primary Components:** `userspace/drivers/xhci/` (new), `userspace/drivers/usb-core/` (new), `userspace/drivers/usb-hid/` (new), `kernel/src/arch/x86_64/syscall/mod.rs` (USB-IRQ wiring through existing Phase 55b device-host primitives), `kernel-core/src/usb/` (host-testable framing logic), `kernel-core/src/iommu/` (BAR + DMA buffer wiring)

## Milestone Goal

m3OS boots on a modern x86_64 laptop or desktop without a PS/2 port and finds at least one USB keyboard and one USB mouse via xHCI enumeration. Both devices feed `kbd_server` / `mouse_server` through the same wire format Phase 56 introduced for PS/2 input. This is the single biggest 1.0 unblocker per Phase 74a §3.

## Why This Phase Exists

Phase 74a §3 documents the hard truth: m3OS today has zero bytes of USB code anywhere in the kernel or userspace. Every modern laptop has zero PS/2 ports. On the dev laptop (HP OmniBook with Strix Halo), m3OS would boot, paint a framebuffer, and then sit at a black screen with no keyboard or mouse. A 1.0 release without USB-HID is not a real-hardware 1.0 release.

xHCI is the only host-controller standard worth implementing — EHCI and UHCI are obsolete, OHCI never mattered on x86. xHCI handles USB 1.1/2.0/3.x devices on the same controller, so one driver covers the whole legacy + modern device space.

## Learning Goals

- Understand how a USB host controller schedules transfers via TRB rings (Transfer Request Blocks)
- Learn how USB enumeration walks the descriptor tree: device → configuration → interface → endpoint
- See how HID report descriptors are parsed (input items, usage pages, usage IDs) and how the keyboard/mouse boot protocols simplify the parsing problem
- Understand why a hub is its own USB device that the host driver must enumerate before downstream devices are reachable
- Learn how the IOMMU substrate from Phase 67 enables a ring-3 USB driver to issue DMA transfers safely

## Feature Scope

### Track A — xHCI host controller driver (ring 3)

- **A.1** — PCI probe via the Phase 55b device-host syscall surface. Filter on class code `0x0C0330` (USB xHCI).
- **A.2** — MMIO BAR map via `iommu_map_bar`. Reset controller. Allocate Command Ring, Event Ring, and per-slot Transfer Rings using `DmaBuffer<T>`.
- **A.3** — Implement the four TRB types needed for boot HID: Normal, Setup, Data, Status. Handle the Command Completion, Transfer, and Port Status Change events.
- **A.4** — Port-status polling for hot-plug detection (deferred — pre-1.0 uses initial enumeration only).

### Track B — USB core (ring 3)

- **B.1** — Generic device enumeration walker. Issue `GET_DESCRIPTOR(DEVICE)`, then `GET_DESCRIPTOR(CONFIG)` and walk interfaces.
- **B.2** — Per-device address assignment (xHCI Slot Context). Set Configuration. Per-interface driver lookup.
- **B.3** — Hub class support: enumerate downstream ports of every detected hub. The dev laptop has six xHCI controllers each with internal hub topology.

### Track C — HID class driver (ring 3)

- **C.1** — Boot Protocol keyboard. 8-byte report → standard scancodes → `kbd_server` typed `KeyEvent` (already defined in Phase 56). Phase 56's input dispatch path does not change.
- **C.2** — Boot Protocol mouse. 3-byte report → relative `dx`/`dy` + button bitfield → `mouse_server` typed `PointerEvent`.
- **C.3** — Report Protocol skeleton (parse HID report descriptor, derive field offsets). Boot Protocol is enough for 1.0 keyboards and mice; Report Protocol unlocks touchpads, gaming mice, multi-touch displays — deferred unless time permits.

### Track D — Kernel-side wiring

- **D.1** — Add the new IRQ vectors for xHCI controllers to `kernel/src/arch/x86_64/interrupts.rs` via the existing Phase 55b `sys_device_irq_bind` path. No new syscalls.
- **D.2** — Add `usb-core.conf` + `xhci.conf` + `usb-hid.conf` to `kernel/initrd/etc/services.d/` so `session_manager` starts the stack on every boot.

## Important Components and How They Work

### TRB rings

xHCI uses three kinds of ring: a single Command Ring (host → controller), a single Event Ring (controller → host) shared across slots, and one Transfer Ring per endpoint per slot. Each ring is a contiguous physical region carrying 16-byte TRBs with a producer/consumer cycle bit. DMA is the only way data crosses the ring 3 / hardware boundary — the IOMMU substrate from Phase 67 maps the rings via `iommu_map_bar` so a malicious driver cannot point DMA at arbitrary physical memory.

### USB enumeration

When a port reports Connect Status Change, the host driver issues a Reset, then a Set Address (via xHCI Address Device command), then walks the descriptor tree to discover what kind of device is attached. The HID class driver registers for `bInterfaceClass == 0x03` (HID); the hub class driver registers for `bDeviceClass == 0x09` (HUB).

### Boot Protocol HID parsing

`SET_PROTOCOL(0)` puts a HID device into Boot Protocol. Keyboard reports become 8 bytes (1 modifier byte + 1 reserved byte + 6 scancode bytes, all USB HID Usage IDs); mouse reports become 3 bytes (1 button-bitfield byte + signed `dx` byte + signed `dy` byte). This is enough for keyboard + mouse boot input on essentially every USB HID device in existence.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 driver-host primitives (`sys_device_pci_probe`, `sys_device_pio_*`, `sys_device_mmio_map`, `sys_device_irq_bind`) without modification.
- Reuses Phase 67's `iommu_map_bar` and `DmaBuffer<T>` for safe DMA.
- Reuses Phase 56's `KeyEvent` / `PointerEvent` wire formats — `kbd_server` and `mouse_server` source from USB-HID exactly the same way they source from PS/2 today.
- Slots into Phase 64's `session_manager` start sequence between `mouse_server` and `audio_server` so the input stack is ready before the user-facing services come up.

## Implementation Outline

1. Bring up the xHCI driver as a standalone binary that prints `[xhci] N ports detected` on the dev laptop's QEMU emulation (`-device qemu-xhci`).
2. Add USB core + descriptor walker; print the full enumerated tree on boot.
3. Add hub class support so downstream devices on the laptop's six controllers come up.
4. Add HID class with Boot Protocol; verify keystrokes from a real USB keyboard reach `kbd_server`.
5. Add Boot Protocol mouse; verify pointer movement reaches `mouse_server`.
6. Wire the three new services into `session_manager` and `init` `KNOWN_CONFIGS`.
7. Bump kernel to `0.78.0` (driver-only phase, but the version cut aligns with the audit-blocker closure).

## Acceptance Criteria

- `cargo xtask run` under `qemu-xhci` emulation enumerates a virtual USB keyboard and feeds keystrokes to the m3OS login prompt.
- A new `cargo xtask usb-smoke` gate verifies the full enumeration → HID-report → `kbd_server` chain.
- On the dev laptop (HP OmniBook, Strix Halo): a USB keyboard plugged into any of the six xHCI controllers types into the m3OS shell.
- `display_server`-aware: keystrokes route through the focus-aware dispatcher introduced in Phase 56 (no change to the dispatcher itself).
- No regression in PS/2 input — both stacks coexist; PS/2 still works for QEMU's i8042 emulation.
- Kernel bumped to `0.78.0`.

## Companion Task List

- [Phase 78 Task List](./tasks/78-usb-host-foundation-tasks.md) — to be authored when implementation planning begins.

## How Real OS Implementations Differ

- Linux ships drivers for EHCI, UHCI, OHCI, xHCI plus dozens of optional class drivers (mass-storage, CDC-ACM, audio, video, MTP). m3OS at 1.0 supports xHCI + Hub + HID only.
- Linux's USB stack handles hot-plug events through a userspace-visible netlink interface; m3OS at 1.0 enumerates once at boot and does not surface hot-plug.
- Linux's HID stack has thousands of quirk entries for misbehaving devices; m3OS at 1.0 ships zero quirks and accepts whatever the Boot Protocol returns.
- Real OSes implement USB-3 SuperSpeed and SuperSpeedPlus link training paths; m3OS at 1.0 uses xHCI's controller-managed link training but does not expose link-state policy.

## Deferred Until Later

- USB mass storage (would let USB sticks work — meaningful but post-1.0)
- USB audio (HDA in Phase 80 is the 1.0 audio bet)
- USB video (UVC webcams — post-1.0)
- Hot-plug event surface to userspace
- USB Report Protocol parsing (multi-touch, touchpads, gaming mice)
- USB-C / Power Delivery / DisplayPort alternate mode
- xHCI debug capability (DbC) for kernel debug-over-USB
