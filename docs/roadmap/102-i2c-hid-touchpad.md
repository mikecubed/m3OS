# Phase 102 - I2C-HID Touchpad (Intel LPSS DesignWare I2C)

**Status:** In progress — **Tracks A/B/C (the host-tested pure-logic core) landed + green**: the DesignWare I2C register/bit map + `DW_IC_DATA_CMD` transfer planner + `TX_ABRT` decode (`kernel-core/src/i2c/designware.rs`), the HID-over-I2C v1.0 descriptor + RESET/SET_POWER/GET_REPORT command frames + length-prefixed input-report parse (`kernel-core/src/i2c/hid_over_i2c.rs`), and `decode_touchpad_report` reusing the Phase 92b `ReportField` machinery (`kernel-core/src/usb/hid_report.rs`) — all host-tested in `kernel-core` (`cargo xtask check` green). Remaining: Track D (the ring-3 `i2c-hid` daemon — ACPI controller claim + MMIO master + transport + `mouse_server` inject — and the four-place binary wiring) and Track E (Dell bare-metal validation). QEMU models none of this hardware, so the pure logic is the CI surface; the live datapath is bench-only per `docs/appendix/bare-metal-validation.md`.
**Source Ref:** phase-102
**Depends on:** Phase 101 (ACPI Platform Foundation — `_HID`/`_CRS` device + interrupt-resource enumeration; gating sibling-arc phase), Phase 92b (HID Report-Protocol decode — `parse_report_descriptor` + `ReportField`) ✅, Phase 56 (`mouse_server` `PointerEvent` pipeline + inject path) ✅
**Builds on:** Reuses the Phase 92b HID Report-Descriptor parser (`kernel_core::usb::hid_report::parse_report_descriptor` → `ReportField` layout, with the host-tested `extract_bits`/`sign_extend` decode primitives) and the Phase 56 `mouse_server` `MOUSE_EVENT_INJECT` (label 3) inject path that `usb-hid` already drives. It adds the two substrates that do **not** exist anywhere in the tree today: an Intel LPSS **DesignWare I2C controller** master driver and the **HID-over-I2C** transport. The HID report-descriptor *language* is bus-agnostic, so once an I2C-HID device's report descriptor reaches `parse_report_descriptor` the decode path is shared with USB HID — only the wire transport below it is new.
**Primary Components:** `kernel-core/src/i2c/designware.rs` (new — DesignWare register map + master-transfer command-FIFO state machine, host-tested), `kernel-core/src/i2c/hid_over_i2c.rs` (new — HID-over-I2C v1.0 descriptor + command-frame codec, host-tested), `kernel-core/src/usb/hid_report.rs` (extend with `decode_touchpad_report` — digitizer/multitouch usages reusing `ReportField`), `userspace/drivers/i2c-hid` (new ring-3 daemon — claims the LPSS controller, maps its MMIO, runs the DesignWare master, drives I2C-HID, parses multitouch, injects into `mouse_server`), `userspace/lib/driver_runtime` (`device.rs`/`mmio.rs`/`irq.rs` — reused; a new ACPI-resourced MMIO/IRQ claim path where the controller is not PCI-visible), `docs/appendix/bare-metal-validation.md` (the HW-only validation protocol)

## Milestone Goal

The Dell Tiger Lake laptop's **built-in multitouch touchpad** (Elan `04F3:311C`, ACPI `_HID` `DLL0945`, on Intel LPSS DesignWare `i2c_designware.1`) drives the real GUI cursor. The touchpad enumerates from ACPI, the `dwiic` controller binds and brings up its master engine, the HID-over-I2C descriptor + report-descriptor fetch succeeds, finger motion moves the compositor cursor, a clickpad press / tap registers as a button, and a two-finger drag scrolls. This is the single substantial item between a text login and a daily-usable GUI on the Dell — it **replaces the Phase 100 interim USB mouse** with the machine's actual pointer (the laptop has no PS/2 pointer at all). The same controller + transport path is reused, with an AMD MMIO backend, by Phase 108 (HP OmniBook / Strix Point).

## Why This Phase Exists

Phase 100 gets a GUI session on screen and drives the cursor with a plugged-in USB mouse over the existing `usb-hid → mouse_server` inject path — deliberately interim, because a laptop is not usable with a permanently-attached external mouse. The real built-in pointer on the Dell (and on essentially every modern Windows-class ultrabook) is an **I2C-HID touchpad**: a HID device that speaks the standard HID report-descriptor language but over an I2C bus instead of USB, hung off an Intel Low-Power-Subsystem (LPSS) **DesignWare** I2C controller, with its interrupt delivered as a GPIO pin (`GpioInt`) rather than a USB interrupt endpoint.

m3OS has **no I2C controller driver and no I2C-HID transport anywhere in the tree** — the entire substrate below the HID layer is missing. What m3OS *does* have, from Phase 92b, is a complete host-tested HID report-descriptor parser and pointer decode, and from Phase 56 a `mouse_server` that already accepts injected `PointerEvent`s from a driver TCB. So the work is genuinely bounded: build the I2C master + the HID-over-I2C transport, decode the multitouch reports through the existing `ReportField` machinery, and inject — the compositor is completely unchanged. It is also a from-scratch bring-up against hardware **QEMU does not model at all**, so it carries the bare-metal validation discipline established for the Phase 96 `ure` line.

## Learning Goals

- Understand the **DesignWare I2C master**: how a target address (`DW_IC_TAR`), a command/data FIFO (`DW_IC_DATA_CMD` with the RESTART/STOP/READ bits), the interrupt-status registers, and the clock-count (HCNT/LCNT) registers compose a polled or interrupt-driven I2C transaction — and why an I2C read is "write the register address, RESTART, read N bytes."
- Learn the **HID-over-I2C v1.0** protocol: the fixed HID Descriptor read from the descriptor register (giving the report-descriptor / input / output / command register addresses + max lengths), the RESET and SET_POWER command frames, and how an input report is a length-prefixed blob read from the input register when the device asserts its interrupt.
- See how a **GpioInt** differs from a PCI/MSI interrupt: the touchpad signals "report ready" by toggling a GPIO pin whose ACPI `_CRS` resource resolves (through a GPIO/pinctrl block) to a platform interrupt line — and why a polled fallback exists for bring-up.
- See how **multitouch / Windows-Precision-Touchpad reports** (Usage Page 0x0D Digitizers: Tip Switch, Contact Identifier, Contact Count, plus Generic-Desktop X/Y and a clickpad Button) decode through the *same* `ReportField` layout the USB HID path uses, and how absolute contact coordinates are turned into the relative `PointerEvent` deltas the `mouse_server` pipeline expects.
- Understand **ACPI as the device-discovery substrate** for a non-PCI-enumerable peripheral: the controller's MMIO base, the touchpad's 7-bit I2C slave address, the bus speed, and the GpioInt all come from ACPI `_CRS` (Phase 101), not from probing.

## Feature Scope

### Track A — Intel LPSS DesignWare I2C controller driver

A ring-3 master driver for the Synopsys DesignWare I2C IP block that Intel LPSS exposes (OpenBSD `dwiic(4)` — `sys/dev/acpi/dwiic.c` / `dwiicvar.h`, ISC/BSD-licensed — re-expressed in Rust, with Linux `i2c-designware-*.c` used only as a constant/sequence cross-check). The pure register-layout constants and the master-transfer command-FIFO state machine (build the per-byte `DW_IC_DATA_CMD` words with RESTART on the first read byte and STOP on the last, drain `DW_IC_DATA_CMD` reads against `DW_IC_RXFLR`, observe `DW_IC_RAW_INTR_STAT` TX_ABRT / STOP_DET) live in `kernel-core/src/i2c/designware.rs` and are **host-tested**. The ring-3 daemon claims the controller device (via Phase 101 ACPI enumeration), maps its MMIO window, disables the controller (`DW_IC_ENABLE=0`), programs `DW_IC_CON` (master, restart-enable, 7-bit), the speed-mode HCNT/LCNT timings (Fast-mode 400 kHz default, from the bus-speed `_CRS` field), and `DW_IC_TAR`, re-enables, and then executes combined write-then-read transactions on behalf of the I2C-HID layer.

### Track B — HID-over-I2C transport

The HID-over-I2C v1.0 protocol on top of the Track A master (OpenBSD `ihidev(4)` — `sys/dev/i2c/ihidev.c` — as the structural reference). The codec (the `I2cHidDescriptor` layout — `wHIDDescLength`, `bcdVersion`, `wReportDescLength`, `wReportDescRegister`, `wInputRegister`, `wMaxInputLength`, `wOutputRegister`, `wCommandRegister`, `wDataRegister`, vendor/product IDs — plus the RESET / SET_POWER / GET_REPORT command-frame builders and the 2-byte length-prefix input-report parse) lives in `kernel-core/src/i2c/hid_over_i2c.rs` and is host-tested. The daemon's transport: (1) reads the HID Descriptor from the descriptor register (its address comes from ACPI for the device); (2) fetches the Report Descriptor from `wReportDescRegister` and feeds it to `parse_report_descriptor`; (3) issues RESET then SET_POWER(ON); (4) reads input reports from `wInputRegister`, triggered by the **GpioInt** asserting (from `_CRS`) — with a timed-poll fallback at the report interval for bring-up when the GPIO interrupt cannot yet be routed.

### Track C — Multitouch report parse → PointerEvent

A new `decode_touchpad_report` in `kernel-core/src/usb/hid_report.rs` (reusing `ReportField`, `extract_bits`, `sign_extend`) that decodes a Windows-Precision-Touchpad / digitizer input report (OpenBSD `imt(4)` — `sys/dev/i2c/imt.c` — as the reference): per-contact Tip Switch (Digitizer usage 0x42), Contact Identifier (0x51), Contact Count (0x54), Generic-Desktop X (0x30) / Y (0x31), and the clickpad Button (Button page 0x09). The daemon maps the decoded contacts to `PointerEvent`s: a single down-contact's absolute position is differenced against the previous frame into relative `dx`/`dy` (scaled from the touchpad logical range), a clickpad/physical button or tap produces `PointerButton::Down/Up`, and (at least) a two-contact gesture produces a wheel-scroll `PointerEvent`. Host-tested against captured Precision-Touchpad descriptors + reports.

### Track D — `mouse_server` inject integration + lifecycle + new-binary wiring

The daemon registers as a driver TCB and injects each `PointerEvent` into `mouse_server` via `MOUSE_EVENT_INJECT` (label 3) — the exact path `usb-hid` uses, gated by `ipc_peer_is_driver(reply_cap)` — so the compositor and `display_server` are unchanged. It handles attach (bind on ACPI enumeration) and clean detach/teardown (release the device cap + GpioInt subscription, stop injecting). This track also does the four-place new-binary wiring for `i2c-hid` (workspace `members`, the `bins` array in `xtask/src/main.rs` `build_userspace`, the `include_bytes!` + `BIN_ENTRIES` in `kernel/src/fs/ramdisk.rs`, and a `services.d/i2c-hid.conf` in both `populate_ext2_files` and `KNOWN_CONFIGS`).

### Track E — Validation (host tests + bare-metal protocol)

Because QEMU models none of this hardware, validation is split: the I2C-HID descriptor codec, the DesignWare transfer state machine, and the multitouch report decode are **host-tested in `kernel-core`** (the falsifiable CI surface); the live datapath is **validated on the Dell** following `docs/appendix/bare-metal-validation.md` (the Phase 98 Track A.5 protocol — USB-boot the image, capture boot/serial over AMT Serial-over-LAN pre-network and the network log sink post-network, and assert the on-device-render cursor-motion arm). HW milestones carry the **"Validated-on-HW (run N, date)"** status convention rather than a bare "Complete."

## Important Components and How They Work

### `kernel-core/src/i2c/designware.rs` — the master engine (pure logic)

Register offsets + bit constants for the DesignWare I2C block (`DW_IC_CON`, `DW_IC_TAR`, `DW_IC_DATA_CMD`, `DW_IC_SS/FS_SCL_HCNT/LCNT`, `DW_IC_ENABLE`, `DW_IC_STATUS`, `DW_IC_TXFLR`/`DW_IC_RXFLR`, `DW_IC_INTR_STAT`/`MASK`, `DW_IC_RAW_INTR_STAT`, `DW_IC_CLR_INTR`, `DW_IC_COMP_PARAM_1`), plus a transfer planner that turns a "write `addr` bytes, RESTART, read `n` bytes" request into the ordered sequence of `DW_IC_DATA_CMD` words (READ bit on read slots, RESTART on the first read, STOP on the last) and the expected RX byte count. No `unsafe`, no MMIO — the daemon supplies the actual reads/writes through `driver_runtime::Mmio`, so this state machine is exercised entirely on the host. TX_ABRT decoding (the abort-source register) surfaces a typed error instead of a hang.

### `kernel-core/src/i2c/hid_over_i2c.rs` — the transport codec (pure logic)

The `I2cHidDescriptor` parse (a fixed little-endian layout) and the command-frame builders. A GET_REPORT or SET_POWER frame is `[cmd-reg-lo, cmd-reg-hi, opcode/report-type byte, ...]` written to the command register; an input report read returns `[len-lo, len-hi, report-bytes...]` from the input register (a length of 0 means "no data / reset-complete"). All host-tested against byte vectors so the wire framing is pinned independently of hardware.

### `userspace/drivers/i2c-hid` — the daemon

Claims the LPSS controller device cap from Phase 101's ACPI enumeration (`driver_runtime::device::DeviceHandle::claim`), maps the controller MMIO with `driver_runtime::mmio::Mmio::map` (BAR index when the controller is PCI-visible; a new ACPI-`_CRS`-Memory-region claim path when it is hidden/ACPI-only), subscribes to the GpioInt via `driver_runtime::irq::IrqNotification::subscribe` (or polls), runs the Track A master to execute the Track B transport, decodes with Track C, and injects with Track D. It owns no writable shared memory beyond its own buffers and never blocks in an interrupt context.

### Reused: the Phase 92b decode + Phase 56 inject

`parse_report_descriptor` consumes the I2C-HID report descriptor unchanged (the descriptor language is identical to USB HID). `mouse_server`'s `handle_mouse_inject` (the `MOUSE_EVENT_INJECT` arm) already decodes the 37-byte `PointerEvent` wire payload, gates the caller with `ipc_peer_is_driver`, and queues onto the bounded injected-event ring that drains ahead of the PS/2 stream — so a touchpad and a USB mouse are simply two parallel injecting drivers.

## How This Builds on Earlier Phases

- **Depends on Phase 101 (ACPI)** for `_HID` device match (`DLL0945`), the controller MMIO base + bus speed, the touchpad's 7-bit I2C slave address, and the GpioInt — none of which is PCI-probeable. ACPI-before-I2C-HID is the explicit sequencing trap the Phase 98 charter exists to avoid.
- **Reuses Phase 92b** (`kernel_core::usb::hid_report`): `parse_report_descriptor` + `ReportField` + the `extract_bits`/`sign_extend` decode primitives are shared verbatim; Track C adds only the digitizer-usage decode (Usage Page 0x0D) that touchpads need beyond the mouse/tablet usages 92b already handles.
- **Reuses Phase 56** (`mouse_server` `MOUSE_EVENT_INJECT`, label 3) and the Phase 78c driver-TCB inject gate (`ipc_peer_is_driver`) — the same path `usb-hid` drives, so the compositor stack is untouched.
- **Replaces the Phase 100 interim USB mouse** as the laptop's primary GUI pointer (the USB-mouse path stays as a fallback / external-mouse option).
- **Reuses the Phase 96 / Phase 98 bare-metal bring-up workflow** — the AMT Serial-over-LAN capture, the network log sink, and the `docs/appendix/bare-metal-validation.md` protocol (USB passthrough does not apply to an on-board I2C device, but the boot/serial/network-sink capture path does).
- **Hands off to Phase 108** — the Track A controller engine and the Track B/C decode are reused for the HP OmniBook with an **AMD** controller backend (AMD `AMDI0010` MMIO DesignWare I2C + `AMDI0030` GPIO for the HID IRQ); the `kernel-core` logic is bus/vendor-agnostic and the daemon swaps only the MMIO-source + GPIO-IRQ wiring.

## Implementation Outline

1. **Track A** — add `kernel-core/src/i2c/designware.rs` (register map + transfer-FIFO planner + TX_ABRT decode; host tests for combined write-read sequencing and abort handling). Scaffold the `i2c-hid` daemon's controller module: claim the device, map MMIO, disable → program `DW_IC_CON`/HCNT-LCNT/`DW_IC_TAR` → enable; implement a polled `i2c_transfer(addr, write, read_len)` driving the planner against the live registers.
2. **Track B** — add `kernel-core/src/i2c/hid_over_i2c.rs` (descriptor parse + command-frame builders + input-report length-prefix parse; host tests). Wire the daemon transport: HID-descriptor read → report-descriptor fetch → `parse_report_descriptor` → RESET → SET_POWER(ON) → input-report reads.
3. **Track C** — add `decode_touchpad_report` (+ `Contact`/`TouchpadFrame` types) to `kernel-core/src/usb/hid_report.rs` reusing the existing primitives; host tests against captured Precision-Touchpad descriptors/reports. In the daemon, map decoded contacts to `PointerEvent`s (absolute→relative differencing, button, two-finger scroll).
4. **Track D** — register the daemon as a driver TCB; inject via `MOUSE_EVENT_INJECT`; implement attach/detach lifecycle; complete the four-place new-binary wiring + `services.d/i2c-hid.conf`. Subscribe to the GpioInt (or fall back to timed polling).
5. **Track E** — land the `kernel-core` host tests as the CI surface; run the bare-metal validation pass on the Dell per `docs/appendix/bare-metal-validation.md` and record it as "Validated-on-HW (run N, date)".

## Acceptance Criteria

- **Host-tested (CI, falsifiable):** `kernel-core` tests cover (a) the DesignWare transfer planner — a "write 1 register byte, RESTART, read N bytes" request produces the correct ordered `DW_IC_DATA_CMD` word sequence (READ/RESTART/STOP bits) and expected RX count, and a TX_ABRT raw-status decodes to a typed error; (b) the `I2cHidDescriptor` parse round-trips a known byte vector to the right register addresses/lengths, and the RESET/SET_POWER/GET_REPORT command frames + the input-report length-prefix parse match the HID-over-I2C v1.0 wire layout; (c) `decode_touchpad_report` decodes a captured Precision-Touchpad report to the expected contact count, per-contact tip-switch + X/Y, and button state, and a too-short report decodes without panicking.
- **Builds + wired:** `cargo xtask check` builds `i2c-hid`; it is embedded in the ramdisk and launched from `services.d/i2c-hid.conf`; it defines a `#[global_allocator]` and (using `kernel-core`/`Vec`) enables `needs_alloc`.
- **Validated-on-HW (run N, date) on the Dell Tiger Lake laptop** (per `docs/appendix/bare-metal-validation.md`): the touchpad enumerates via ACPI `_HID` `DLL0945`; the `dwiic` controller binds and its master engine completes I2C transactions (serial shows the MAC-equivalent here — the parsed HID Descriptor's vendor/product `04F3:311C` + report-descriptor length); the report descriptor fetches and parses; finger motion moves the compositor cursor (asserted by the on-device-render cursor-displacement arm, not merely a log line); a clickpad/tap press registers a button; and a two-finger drag scrolls.
- **Replacement:** with the touchpad bound, the GUI session is usable with no external mouse attached (the Phase 100 interim USB mouse is no longer required).
- **Reuse contract:** the controller engine + I2C-HID transport + multitouch decode in `kernel-core` are written bus/vendor-agnostic so Phase 108 can add the AMD `AMDI0010`/`AMDI0030` backend without touching them (noted in the Phase 108 charter row).

## Companion Task List

- [Phase 102 Task List](./tasks/102-i2c-hid-touchpad-tasks.md)

## How Real OS Implementations Differ

- **Linux** splits this across `i2c-designware-platform`/`i2c-designware-core` (the controller), `i2c-hid`/`i2c-hid-acpi` (the transport), `pinctrl-intel`/`gpiolib-acpi` (the GpioInt), and `hid-multitouch` (the report parse), each a substantial subsystem with runtime PM, DMA-mode transfers, and ACPI `_DSM` quirk handling. Phase 102 collapses the bring-up subset into one daemon + host-tested logic — closer to OpenBSD's `dwiic` + `ihidev` + `imt` trio.
- Production I2C masters use **DMA** and deep-FIFO interrupt coalescing; this driver uses PIO against the command/data FIFO sized for correctness over throughput (touchpad traffic is tiny).
- Real stacks route the **GpioInt** through a full pinctrl driver and an interrupt-controller hierarchy; m3OS resolves the `_CRS` GpioInt to a platform interrupt where it can and otherwise polls the input register — acceptable because a touchpad's report rate is bounded.
- Mature multitouch handles **gestures, palm rejection, pressure, and pointer acceleration curves** in a userspace input library (libinput). Phase 102 implements only the bring-up subset: cursor motion, click, and two-finger scroll.
- Real bring-up uses a **logic analyzer / I2C protocol analyzer** on the bus; this phase substitutes host-tested transfer planning + the AMT-SOL / network-log-sink capture from the Phase 96/98 workflow because that is what the reference hardware exposes.

## Deferred Until Later

- **Gestures beyond two-finger scroll** (pinch-zoom, three/four-finger swipe), pointer-acceleration curves, palm/thumb rejection, and tap-and-drag refinement — a later input-polish item; the bring-up target is cursor motion + click + two-finger scroll.
- **A general I2C bus service** (exposing arbitrary I2C slaves to other drivers — sensors, battery-fuel-gauge, embedded controllers) — Phase 102 wires the master only for the touchpad's needs; a shared `i2c` IPC service can ride a later phase (battery/EC work in Phase 103 may want it).
- **The AMD controller backend** (`AMDI0010` + `AMDI0030` GPIO) and bare-metal AMD validation — explicitly **Phase 108** (HP OmniBook / Strix Point), reusing this phase's `kernel-core` logic.
- **A real Intel pinctrl/GPIO driver** — Phase 102 resolves the GpioInt pragmatically (route-where-possible, else poll); a full pinctrl block is a later platform item shared with Phase 103 power (lid/power-button GPEs).
- **Stylus / pen digitizer input** and absolute-mode touch (touchscreen) — the parse machinery generalizes, but the `PointerEvent` mapping here targets a relative touchpad pointer.
- **I2C controller runtime power management / S0ix integration** — deferred to the Phase 103 power arc.
