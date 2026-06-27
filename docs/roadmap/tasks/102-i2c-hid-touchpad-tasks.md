# Phase 102 — I2C-HID Touchpad (Intel LPSS DesignWare I2C): Task List

**Status:** Planned
**Source Ref:** phase-102
**Depends on:** Phase 101 (ACPI Platform Foundation — `_HID`/`_CRS` enumeration; gating sibling-arc phase), Phase 92b (HID Report-Protocol decode — `parse_report_descriptor` + `ReportField`) ✅, Phase 56 (`mouse_server` `MOUSE_EVENT_INJECT` inject path) ✅
**Goal:** Drive the Dell Tiger Lake laptop's built-in Elan I2C-HID multitouch touchpad (`04F3:311C`, ACPI `_HID` `DLL0945`, on Intel LPSS DesignWare `i2c_designware.1`) as the real GUI pointer — building the missing DesignWare I2C master + HID-over-I2C transport substrate, decoding multitouch reports through the existing Phase 92b `ReportField` machinery, and injecting `PointerEvent`s into `mouse_server` via the existing Phase 56 path so the compositor is unchanged. This replaces the Phase 100 interim USB mouse. The transport/controller/decode logic is host-tested in `kernel-core`; the live datapath is validated on the Dell per `docs/appendix/bare-metal-validation.md` (HW-only — QEMU models none of this).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Intel LPSS DesignWare I2C controller (register map + transfer FIFO state machine in `kernel-core`; ring-3 master engine in the daemon) | 101 | Planned |
| B | HID-over-I2C v1.0 transport (descriptor + report-descriptor fetch, RESET/SET_POWER, input-report reads on GpioInt) | A | Planned |
| C | Multitouch report parse → `PointerEvent` (digitizer usages reusing `ReportField`; absolute→relative + two-finger scroll) | B (92b) | Planned |
| D | `mouse_server` `MOUSE_EVENT_INJECT` integration + attach/detach lifecycle + four-place new-binary wiring | A, B, C | Planned |
| E | Validation (kernel-core host tests as the CI surface + the bare-metal HW protocol) | A, B, C, D | Planned |

---

## Track A — DesignWare I2C Controller

> Primary reference: OpenBSD **`dwiic(4)`** (`sys/dev/acpi/dwiic.c`, `dwiicvar.h`, ISC/BSD — re-expressed in Rust). Linux `i2c-designware-core.c` / `-master.c` used only as a register-constant / sequence cross-check.

### A.1 — DesignWare register map + transfer-FIFO planner (host-tested)

**File:** `kernel-core/src/i2c/designware.rs` (new module; add `pub mod i2c;` to `kernel-core/src/lib.rs`)
**Symbol:** `DwIcRegs` (register offsets), `plan_transfer`, `TransferPlan`, `decode_tx_abort`
**Why it matters:** The DesignWare master expresses an I2C read as "push the register-address bytes, then push READ command words with RESTART on the first and STOP on the last, then drain `DW_IC_DATA_CMD` against `DW_IC_RXFLR`." Encoding this command-word sequence is pure logic and must be exercised on the host because QEMU has no DesignWare I2C model.

**Acceptance:**
- [ ] Register offset + bit constants defined: `DW_IC_CON`, `DW_IC_TAR`, `DW_IC_DATA_CMD` (with `CMD_READ`/`CMD_RESTART`/`CMD_STOP` bits), `DW_IC_SS_SCL_HCNT/LCNT`, `DW_IC_FS_SCL_HCNT/LCNT`, `DW_IC_ENABLE`, `DW_IC_STATUS`, `DW_IC_TXFLR`, `DW_IC_RXFLR`, `DW_IC_INTR_STAT`/`MASK`, `DW_IC_RAW_INTR_STAT`, `DW_IC_CLR_INTR`, `DW_IC_TX_ABRT_SOURCE`, `DW_IC_COMP_PARAM_1`.
- [ ] `plan_transfer(write: &[u8], read_len)` returns the ordered `DW_IC_DATA_CMD` words (RESTART set on the first read slot, STOP on the last word) and the expected RX byte count; a write-only and a read-only transfer are both correct edge cases.
- [ ] `decode_tx_abort(raw)` maps a `DW_IC_TX_ABRT_SOURCE` value to a typed error (e.g. `TxAbort::AddrNoAck`) rather than a generic failure.
- [ ] Host tests in `kernel-core` assert the planned word sequence for a representative "write 1 addr byte → RESTART → read 4 bytes" I2C-HID register read, and that a NAK abort decodes to the expected variant.

### A.2 — Controller claim + MMIO map + master init (daemon)

**File:** `userspace/drivers/i2c-hid/src/controller.rs` (new)
**Symbol:** `DwI2c::bind`, `DwI2c::i2c_transfer`
**Why it matters:** Without disabling the controller, programming `DW_IC_CON` (master / restart-enable / 7-bit), the speed-mode HCNT/LCNT timings, and `DW_IC_TAR`, then re-enabling, the master will not issue valid bus cycles; this is the live half of Track A.
**Files (reused):** `userspace/lib/driver_runtime/src/device.rs` (`DeviceHandle::claim`), `userspace/lib/driver_runtime/src/mmio.rs` (`Mmio::map`)

**Acceptance:**
- [ ] Claims the LPSS controller device cap surfaced by Phase 101 ACPI enumeration and maps its MMIO window via `driver_runtime::mmio::Mmio::map` (PCI BAR index when the controller is PCI-visible; the A.3 ACPI-region path when hidden).
- [ ] Init sequence: `DW_IC_ENABLE=0` → program `DW_IC_CON` + Fast-mode (400 kHz default) HCNT/LCNT from the `_CRS` bus-speed field → `DW_IC_TAR` = the touchpad slave address → `DW_IC_ENABLE=1`; logs the resolved base address + slave address.
- [ ] `i2c_transfer(write, read_len)` drives the A.1 planner against the live FIFO (push words, poll `DW_IC_RXFLR`/`DW_IC_RAW_INTR_STAT` STOP_DET, drain reads) and returns the read bytes or a typed `TxAbort` error — never hangs (bounded poll budget).

### A.3 — ACPI-resourced MMIO/IRQ claim path

**Files:**
- `userspace/lib/driver_runtime/src/device.rs`, `userspace/lib/driver_runtime/src/mmio.rs` (new ACPI-region claim helper)
- `kernel/src/arch/x86_64/syscall/mod.rs` (the device-host MMIO grant)

**Symbol:** `Mmio::map_phys` (or an ACPI-`_CRS`-Memory device-cap claim) — new
**Why it matters:** The existing `Mmio::map(&device, bar_index, len)` maps a **PCI BAR**; an LPSS controller in ACPI mode exposes its registers through a `_CRS` Memory32Fixed resource, not a BAR, so a capability-gated path to map an ACPI-described physical MMIO window is required when the controller is hidden from PCI.

**Acceptance:**
- [ ] A capability-gated device-host path grants a ring-3 driver an MMIO window from an ACPI `_CRS`-described `(phys_base, len)` (not a PCI BAR), refusing windows the caller's device cap does not authorize.
- [ ] When the controller *is* PCI-visible, the existing `Mmio::map(&device, 0, len)` BAR path is used unchanged (no regression to NVMe/e1000 mappers).
- [ ] Host/unit coverage for the new path's argument validation (rejects unauthorized base/len).

---

## Track B — HID-over-I2C Transport

> Primary reference: OpenBSD **`ihidev(4)`** (`sys/dev/i2c/ihidev.c`) + the HID-over-I2C v1.0 specification.

### B.1 — HID-over-I2C codec (host-tested)

**File:** `kernel-core/src/i2c/hid_over_i2c.rs` (new module)
**Symbol:** `I2cHidDescriptor::parse`, `build_reset`, `build_set_power`, `build_get_report`, `parse_input_report`
**Why it matters:** The HID Descriptor read from the descriptor register is what tells the driver where every other register (report-descriptor / input / output / command / data) lives and their max lengths; the command frames + the 2-byte length-prefixed input report are the whole wire contract — pinning them in host tests makes the transport falsifiable without hardware.

**Acceptance:**
- [ ] `I2cHidDescriptor::parse(&[u8])` decodes the fixed little-endian layout (`wHIDDescLength`, `bcdVersion`, `wReportDescLength`, `wReportDescRegister`, `wInputRegister`, `wMaxInputLength`, `wOutputRegister`, `wMaxOutputLength`, `wCommandRegister`, `wDataRegister`, `wVendorID`, `wProductID`, `wVersionID`); rejects a truncated/zero-length descriptor.
- [ ] `build_reset` / `build_set_power(on)` / `build_get_report(...)` emit the HID-over-I2C v1.0 command-register byte sequences (opcode + report-type/ID encoding) matching the spec.
- [ ] `parse_input_report(&[u8])` reads the 2-byte length prefix and returns the report body slice; a length of 0 is reported as "no data / reset-complete" (not an error); an over-length/short buffer does not panic.
- [ ] Host tests assert a known Elan/Precision-Touchpad HID Descriptor byte vector parses to the expected register addresses + `04F3:311C` IDs.

### B.2 — Transport bring-up (daemon)

**File:** `userspace/drivers/i2c-hid/src/transport.rs` (new)
**Symbol:** `I2cHidTransport::bring_up`, `fetch_report_descriptor`
**Why it matters:** The ordered descriptor read → report-descriptor fetch → RESET → SET_POWER(ON) sequence is what brings the device from cold to streaming; the fetched report descriptor is the input to the Phase 92b parser, the seam where I2C-HID rejoins the shared HID path.

**Acceptance:**
- [ ] Reads the HID Descriptor (its register address from ACPI for the device) via an A.2 combined write-then-read; logs the parsed vendor/product (`04F3:311C`) + report-descriptor length.
- [ ] Fetches the Report Descriptor from `wReportDescRegister` and feeds it to `kernel_core::usb::hid_report::parse_report_descriptor`, producing a non-empty `ReportField` layout (logs field count).
- [ ] Issues RESET then SET_POWER(ON) (via `build_reset`/`build_set_power`) and confirms the device is responsive (input register returns the reset-complete 0-length report).

### B.3 — GpioInt-triggered (or polled) input-report reads

**Files:**
- `userspace/drivers/i2c-hid/src/transport.rs`
- `userspace/lib/driver_runtime/src/irq.rs` (reused — `IrqNotification::subscribe` / `.wait()` / `.ack()`)

**Symbol:** `I2cHidTransport::read_input_report`, the GpioInt subscription / poll loop
**Why it matters:** The touchpad signals "report ready" by asserting its GpioInt (from `_CRS`); reading the input register on that edge is the steady-state datapath. A timed-poll fallback keeps bring-up tractable before the GPIO interrupt is routable.

**Acceptance:**
- [ ] The GpioInt resource from the device's `_CRS` (Phase 101) is resolved; the daemon subscribes to its platform interrupt via `driver_runtime::irq::IrqNotification::subscribe` and reads the input register on assertion, ack-ing the IRQ.
- [ ] A timed-poll fallback (at the device's report interval) reads the input register when the GpioInt cannot be routed — selected by a clearly-logged mode line at bind.
- [ ] An empty/0-length input read is treated as idle (no event), not an error; the loop never busy-spins a core.

---

## Track C — Multitouch Report Parse → PointerEvent

> Primary reference: OpenBSD **`imt(4)`** (`sys/dev/i2c/imt.c`) — Windows-Precision-Touchpad / HID digitizer report decode.

### C.1 — `decode_touchpad_report` (host-tested, reuses Phase 92b)

**File:** `kernel-core/src/usb/hid_report.rs`
**Symbol:** `decode_touchpad_report`, `Contact`, `TouchpadFrame`
**Why it matters:** A touchpad report uses HID Digitizer usages (Usage Page 0x0D: Tip Switch 0x42, Contact Identifier 0x51, Contact Count 0x54) plus Generic-Desktop X/Y and a clickpad Button — usages the existing `decode_pointer_report` does not handle. Reusing `ReportField` + the private `extract_bits`/`sign_extend` helpers (same module) keeps the decode falsifiable and consistent with the USB HID path.

**Acceptance:**
- [ ] `decode_touchpad_report(fields, report)` returns a `TouchpadFrame` with `contact_count`, up to N per-`Contact` `{ id, tip_down, x, y }`, and a `button` bool, decoding Digitizer usages 0x42/0x51/0x54 + Generic-Desktop X/Y + Button-page click; Report-ID handling matches `decode_pointer_report`.
- [ ] Host tests decode a captured Precision-Touchpad descriptor + a single-finger report (one contact, tip down, expected X/Y) and a two-finger report (contact_count == 2) to the expected values.
- [ ] A too-short / truncated report decodes without panicking (out-of-bounds fields read 0, mirroring the existing decode-short-report test).

### C.2 — Contact → PointerEvent mapping (daemon)

**File:** `userspace/drivers/i2c-hid/src/pointer.rs` (new)
**Symbol:** `TouchpadPipeline::ingest_frame`
**Why it matters:** A touchpad reports **absolute** contact coordinates but the `mouse_server` pipeline (and `display_server` cursor) consumes **relative** `PointerEvent` deltas; the driver must difference successive same-contact positions, scale to the logical range, and synthesize button + scroll events — the input-semantics layer.

**Acceptance:**
- [ ] A single down-contact's absolute position is differenced against the previous frame into relative `dx`/`dy` (scaled from the touchpad logical max); the first frame of a touch (no prior position) emits no motion; tip-up resets the tracked position.
- [ ] A clickpad/physical button press emits `PointerButton::Down`/`Up` (edge-tracked), and a tap (brief tip-down with negligible motion) optionally emits a left click.
- [ ] A two-contact frame emits a wheel-scroll `PointerEvent` (`wheel_dy` from the average vertical contact delta) instead of cursor motion.
- [ ] Each produced `PointerEvent` encodes via `kernel_core::input::events::PointerEvent::encode` to the 37-byte wire payload.

---

## Track D — `mouse_server` Inject + Lifecycle + Wiring

### D.1 — Inject into `mouse_server` via `MOUSE_EVENT_INJECT`

**File:** `userspace/drivers/i2c-hid/src/main.rs`
**Symbol:** `inject_pointer_event` (label `MOUSE_EVENT_INJECT = 3`)
**Why it matters:** Injecting on the exact label/path `usb-hid` uses (`mouse_server`'s `handle_mouse_inject`, gated by `ipc_peer_is_driver`) means the touchpad is just another parallel pointer producer and `display_server`/the compositor need **zero** changes.

**Acceptance:**
- [ ] Looks up the `mouse` service and `ipc_call`s `MOUSE_EVENT_INJECT` with the 37-byte `PointerEvent` bulk; `mouse_server` replies ack (`0`) and the event drains into the next `MOUSE_EVENT_PULL`.
- [ ] The daemon is recognized as a driver TCB so `ipc_peer_is_driver(reply_cap)` in `mouse_server` accepts the inject (added to the driver `exec_path` allowlist).
- [ ] A one-time startup log identifies the input source (`i2c-hid: attached to <_HID> via dwiic @ <base>, slave 0x..`).

### D.2 — Attach / detach lifecycle

**File:** `userspace/drivers/i2c-hid/src/main.rs`
**Symbol:** `program_main` service loop, `teardown`
**Why it matters:** A clean teardown (release the device cap + GpioInt subscription, stop injecting, leave `DW_IC_ENABLE` in a sane state) avoids a wedged controller or a leaked capability if the daemon is restarted or the device is reconfigured.

**Acceptance:**
- [ ] Binds on ACPI enumeration of `_HID` `DLL0945`; if the device is absent the daemon logs a skip-with-reason and idles (does not panic / busy-spin) — so the same image is correct on a machine without the touchpad.
- [ ] On teardown releases the GpioInt subscription and the device cap; a re-launch re-binds successfully.

### D.3 — Four-place new-binary wiring

**Files:**
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`bins` array in `build_userspace`; `needs_alloc = true`)
- `kernel/src/fs/ramdisk.rs` (`include_bytes!` + `BIN_ENTRIES`)
- `xtask/src/main.rs` `populate_ext2_files` + `userspace/init/src/main.rs` `KNOWN_CONFIGS` (a `services.d/i2c-hid.conf`)

**Symbol:** `main` (driver entry), `i2c-hid.conf`
**Why it matters:** Missing any of the four wiring points means the driver is not built, not embedded, or not found at runtime (per the "Adding a New Userspace Binary" rule).

**Acceptance:**
- [ ] `cargo xtask check` builds `i2c-hid`; it is embedded in the ramdisk and launched from `services.d/i2c-hid.conf`.
- [ ] Defines a `#[global_allocator]` (`syscall_lib::heap::BrkAllocator`) and enables the `alloc` feature on `syscall-lib` (`needs_alloc = true`).
- [ ] `cargo xtask clean` is run after adding the service config so the ext2 data disk is recreated with `i2c-hid.conf`.

---

## Track E — Validation

### E.1 — kernel-core host tests as the CI surface

**Files:**
- `kernel-core/src/i2c/designware.rs` (A.1 tests)
- `kernel-core/src/i2c/hid_over_i2c.rs` (B.1 tests)
- `kernel-core/src/usb/hid_report.rs` (C.1 tests)

**Symbol:** the `#[cfg(test)] mod tests` in each
**Why it matters:** QEMU models none of the I2C / GpioInt / touchpad hardware, so the only always-on CI safety net is the pure-logic transfer planning, the I2C-HID codec, and the multitouch decode — exactly the layers most likely to regress silently.

**Acceptance:**
- [ ] `cargo xtask check` runs the new `kernel-core` host tests (transfer planner, TX_ABRT decode, `I2cHidDescriptor` parse, command-frame builders, input-report parse, `decode_touchpad_report` single/two-finger + short-report).
- [ ] The tests are deterministic and require no hardware; they pass under `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`.

### E.2 — Bare-metal validation pass (Dell Tiger Lake)

**File:** `docs/appendix/bare-metal-validation.md` (results appendix for Phase 102)
**Symbol:** the recorded HW run
**Why it matters:** The phase's headline claim is a real built-in pointer on the laptop; this records the end-to-end bare-metal boot with the touchpad driving the cursor, captured per the Phase 98 Track A.5 protocol.

> **Status: operator-owned (HW-only).** Cannot be automated from the dev host — requires physical access to the Dell. Use the "Validated-on-HW (run N, date)" status convention, not a bare "Complete."

**Acceptance:**
- [ ] Following `docs/appendix/bare-metal-validation.md`: USB-boot the image on the Dell; serial (AMT SOL pre-network, network log sink post-network) shows the touchpad enumerating via `_HID` `DLL0945`, the `dwiic` controller binding, the I2C-HID descriptor + report-descriptor fetch, and the parsed `04F3:311C` IDs.
- [ ] The on-device-render arm asserts cursor displacement on finger motion (a populated-vs-baseline framebuffer diff, per the bare-metal-validation render-assertion convention) — not merely a serial log line.
- [ ] A clickpad/tap registers a button and a two-finger drag scrolls; the GUI session is usable with no external mouse (the Phase 100 interim USB mouse is no longer required).
- [ ] Recorded as "Validated-on-HW (run N, date)" in the design-doc Status + the README row.

---

## Documentation Notes

- The HID report-descriptor *language* is bus-agnostic; Track C lands `decode_touchpad_report` in `kernel_core::usb::hid_report` (reusing its `ReportField` + `extract_bits`/`sign_extend`) even though the device is on I2C, not USB — note that the `usb` module name is now slightly historical (a possible later rename, out of scope here).
- The I2C controller engine (`kernel-core/src/i2c/`) and the multitouch decode are written bus/vendor-agnostic so **Phase 108** reuses them with an AMD `AMDI0010` MMIO backend + `AMDI0030` GPIO for the HID IRQ — record the cross-reference when Phase 108 lands.
- `i2c-hid` injects on the same `MOUSE_EVENT_INJECT` (label 3) path `usb-hid` uses — record that `mouse_server` proved a second parallel pointer producer needs no dispatcher changes (it already merges USB + PS/2).
- The driver is re-expressed from BSD-licensed `dwiic`/`ihidev`/`imt`; keep the license-provenance note in the crate header (BSD source re-expressed; Linux `i2c-designware`/`hid-multitouch` facts-only), matching the `ure`/`mt792x` citation convention.
- This phase **replaces the Phase 100 interim USB mouse** as the laptop's primary pointer; the USB-mouse path remains as a fallback / external-mouse option.
- Prefer exact files/symbols over directories as these land; update this list's checkboxes as tracks complete, and convert HW-validated tracks to "Validated-on-HW (run N, date)".
