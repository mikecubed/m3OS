# Phase 96 - Bare-Metal Networking: USB Bulk Endpoints + RTL8156 USB-Ethernet (`ure`)

**Status:** ✅ **Complete (2026-06-26)** — the `ure` control plane + bulk transport + `RemoteNic` registration were HW-validated on the physical `0bda:8156` via QEMU passthrough (Stages 1a/1b/2), and the **RX datapath — the one milestone QEMU passthrough could not reach — is now validated on bare metal**: the real Tiger Lake laptop brought the dongle up (`[remote_nic] up=true 2500Mbps`) and **bound a DHCP lease** (`[dhcp] bound ip=192.168.1.221/255.255.255.0 gw=192.168.1.254`), which requires RX (OFFER/ACK) as well as TX. `ure-smoke` + DHCP client landed. HTTP-over-`ure` rides the same proven RX/TX datapath (opt-in `M3OS_URE_NET`, physical-dongle-only). The bare-metal bring-up follow-on (boot rescue, USB log persistence, PS/2 keyboard, framebuffer write-combining) landed on this branch — see [the bring-up handoff](../handoffs/2026-06-25-usb-log-persistence-and-keyboard.md). Remaining items (I2C-HID touchpad, AX201 Wi-Fi, CDC-ECM, offloads) are **future phases** in *Deferred Until Later*, not Phase 96 gaps.
**Source Ref:** phase-96
**Depends on:** Phase 78c (HID Boot Protocol + `usb` IPC service) ✅, Phase 79 (Networking & GitHub — `RemoteNic` facade + IPv4/TCP/UDP stack) ✅
**Builds on:** Extends the Phase 78 USB foundation (xHCI host driver + enumeration + `usb` IPC service) with **bulk** endpoints — the transfer type 78c deferred — and reuses the Phase 79 `RemoteNic` facade that the ring-3 e1000 / r8169 drivers register through, so a USB NIC becomes a first-class network interface with no new kernel network code. The bulk-endpoint groundwork is the same infrastructure Phase 90 (USB Class Expansion) Track D.1 needs for Mass Storage.
**Primary Components:** `kernel-core/src/usb/enumerate.rs` (bulk EP context type + Configure Endpoint), `userspace/drivers/xhci/src/server.rs` (inline bulk transport — `PollBulkIn`/`SubmitBulkOut`/`BulkData` with `USB_MSG_MAX=4096`, plus `ControlWrite` for control-OUT — as built, not the originally-planned `SubmitTransfer` page-grant consumer), `userspace/drivers/usb-core` (bulk/control IPC protocol), `userspace/drivers/ure` (new — RTL8152/8153/**8156** USB-Ethernet class driver → `RemoteNic`), `xtask/src/main.rs` (`run --usb-passthrough` + `ure-smoke`), `scripts/ure-vfio-validate.md` + `scripts/m3os-logsink.sh` (new — bare-metal bring-up & observability)

## Milestone Goal

m3OS gets its **first real-hardware network interface**: a Realtek RTL8156-based USB 2.5GbE dongle (the common Anker / generic `0bda:8156` class) enumerates on the existing xHCI stack, the new `ure` class driver brings up the chip and pumps frames over bulk endpoints into the existing `RemoteNic` facade, and the in-kernel TCP/IP stack does DHCP/ping/HTTP over it — all validated against the **physical chip** passed through to QEMU, then on bare metal. The phase also lands the reusable **bare-metal bring-up workflow** (USB-passthrough iteration, AMT Serial-over-LAN capture, and network log shipping to a second machine) that future bare-metal driver phases (touchpad, Wi-Fi) reuse.

## Why This Phase Exists

Every NIC m3OS supports today (e1000, e1000e/igb/igc, r8169, the VirtIO baseline) is **PCIe**, and the only wireless driver is MediaTek mt792x. On a real modern laptop the built-in NIC is frequently an Intel CNVi Wi-Fi part (iwlwifi) that m3OS cannot drive, and there is often **no Ethernet port at all**. The cheapest, lowest-risk path to real-hardware networking on such a machine is a **USB Ethernet dongle** — but the USB host stack stops at HID Boot Protocol (control + interrupt endpoints only; Phase 78c explicitly deferred bulk). A NIC needs bulk IN/OUT. This phase adds bulk endpoints and a USB-NIC class driver, turning the existing USB host stack + the existing TCP/IP stack into working bare-metal networking, and establishes the observability workflow that makes bare-metal bring-up tractable at all (no QEMU model exists for any of this hardware).

## Learning Goals

- Understand the xHCI **bulk** transfer type: how a Bulk EP context differs from Control/Interrupt, how Normal TRBs are queued on a bulk ring, and why bulk has no bandwidth reservation but unbounded retry (the opposite trade from the isochronous endpoints Phase 90 Track E adds).
- See how a **vendor-protocol** USB NIC (Realtek `r8152`-family) differs from a pure CDC-ECM class device: register access is tunnelled through vendor control requests (OCP/PLA/USB register banks), and each frame carries a hardware RX/TX descriptor header rather than a bare Ethernet frame.
- Learn how a bus-agnostic driver facade pays off: the same `RemoteNic` IPC surface the PCIe e1000 driver registers through accepts a USB NIC unchanged, so the entire IPv4/TCP/UDP stack lights up with zero network-layer code.
- Understand the practical reality of **bare-metal bring-up with no emulator**: USB/VFIO passthrough into QEMU for in-the-loop iteration, Serial-over-LAN for pre-network panic capture, and network log shipping for post-network observability.

## Feature Scope

### Track A — USB bulk endpoint support (shared infrastructure)

The xHCI enumerator currently configures only `EP_TYPE_CONTROL` / `EP_TYPE_INTERRUPT_IN` / `EP_TYPE_INTERRUPT_OUT` (`kernel-core/src/usb/enumerate.rs`); the descriptor parser *recognises* bulk (`bmAttributes` bits `1:0 == 2`) but nothing builds a bulk EP context or a transfer path. Track A adds bulk IN/OUT EP contexts (xHCI EP Type `2` = Bulk OUT, `6` = Bulk IN), a Configure Endpoint that includes them, and a bulk `SubmitTransfer` consumer in the xHCI server that programs Normal TRBs against a `PageGrant` data buffer and completes off the event ring. This is deliberately a **shared** deliverable: Phase 90 Track D.1 (USB Mass Storage BOT) needs exactly this and should consume it rather than reimplement it.

### Track B — `ure` USB-Ethernet class driver

A new ring-3 driver `userspace/drivers/ure` modelled on OpenBSD/FreeBSD **`ure(4)`** (`if_ure.c` / `if_urereg.h`, BSD-2 licensed — re-expressed in Rust, not copied), with Linux `r8152.c` used only as a fact cross-check (GPL → register constants/sequences only). It matches the RTL815x family by USB VID/PID (`0bda:8152/8153/8156/8157`), reads/writes chip registers through the OCP vendor-request tunnel (`MCU_TYPE_PLA` / `MCU_TYPE_USB` banks), runs the documented reset + init sequence, reads the MAC from `PLA_IDR`, brings the PHY up with 10/100/1000/2500 auto-negotiation, then services bulk IN (RX) and bulk OUT (TX) with the Realtek RX/TX descriptor headers. It registers as a `RemoteNic` (the same `net.nic.ingress` surface the e1000 driver uses), forwarding `NET_RX_FRAME` ingress and accepting egress frames, and reports link state via `NET_LINK_STATE`.

### Track C — Bare-metal bring-up & observability workflow

Reusable tooling so the iteration loop survives the fact that **none of this hardware has a QEMU model**:

- **USB-passthrough run mode** — `cargo xtask run --usb-passthrough <vid:pid>` adds `-device qemu-xhci -device usb-host,vendorid=…,productid=…` so the **physical** dongle is handed to the guest; the existing serial-capture harness reads logs exactly as the smoke tests do. This is the primary in-the-loop iteration path (mirrors the existing mt792x/HDA VFIO pattern).
- **Serial-over-LAN capture runbook** — `scripts/ure-vfio-validate.md` documents AMT SOL capture from a second machine (`amtterm`) for **pre-network** bare-metal panic/boot logs on machines with no physical COM port (the 16550 COM1 @ `0x3F8` that m3OS already logs to is redirected over Ethernet by the ME).
- **Network log sink** — `scripts/m3os-logsink.sh` runs on a second machine: a UDP listener (remote `syslogd` target) + optional `ssh` tail that appends the target's console/`syslogd` stream to a single tailable file, giving live **post-network** observability over the dongle.

### Track D — Validation

- `ure-smoke` (opt-in, `M3OS_URE_REGRESSION=1`): with the real `0bda:8156` passed through, asserts enumeration → `ure` bind → link up → DHCP/static-IP → an outbound TCP/HTTP GET over the in-kernel stack succeeds. Skip-with-reason when the device is absent (mirroring `tls-smoke`/`wifi-smoke`).
- A bare-metal validation runbook capturing the AMT-SOL + network-log-sink procedure end-to-end.

## Important Components and How They Work

### `kernel-core/src/usb/enumerate.rs` — bulk EP contexts

Adds `EP_TYPE_BULK_OUT` (`2`) / `EP_TYPE_BULK_IN` (`6`) and extends the Configure Endpoint path so a device's bulk endpoints get EP contexts (Max Packet Size from the endpoint descriptor, `CErr=3`, a transfer ring per EP). Host-tested like the existing control/interrupt context builders. Pure-logic; no `unsafe`.

### `userspace/drivers/xhci/src/server.rs` — bulk transfer consumer

Phase 78c already defined the `UsbRequest::SubmitTransfer` page-grant transport but left it without a live consumer. Track A makes it real for bulk: map the `PageGrant`, enqueue Normal TRBs (with the IOC bit on the last), ring the doorbell, and complete the request off the Transfer Event TRB. This is the exact code path the Phase 90 D.1 note ("program bulk-endpoint TRBs") points at.

### `userspace/drivers/ure` — the NIC

Control-plane (register access) rides the existing `ControlRequest` IPC path; data-plane rides the new bulk path. The driver owns no writable shared memory beyond its DMA grants and never blocks in an interrupt context — RX is a bulk-IN completion loop, TX is a bulk-OUT submit. Frames cross to the kernel via `RemoteNic::inject_rx_frame` / the `net.nic.ingress` endpoint, identical to the e1000 driver, so the TCP/IP stack is unaware the NIC is on USB.

### Bring-up tooling

`xtask run --usb-passthrough` is a thin QEMU-args addition; `m3os-logsink.sh` and `ure-vfio-validate.md` are host-side operator aids, not m3OS code. They exist so a human + a second machine can keep an AI/operator in the loop during bare-metal runs.

## How This Builds on Earlier Phases

- **Extends Phase 78 (USB)** by adding the bulk transfer type deferred from 78c — the third of the four xHCI transfer types (control/interrupt shipped; isochronous is Phase 90 Track E).
- **Reuses Phase 79's `RemoteNic` facade** (`net.nic.ingress`, `inject_rx_frame`) unchanged — the bus-agnostic NIC seam the e1000/r8169 drivers already register through.
- **Coordinates with Phase 90 (USB Class Expansion)**: Track A here *is* the bulk-EP groundwork Phase 90 Track D.1 (Mass Storage BOT) lists; Phase 90 should consume it. HID Report Protocol (Phase 90 Track B) remains the home for the I2C/USB touchpad work, separate from this phase.
- **Reuses the existing serial-capture harness** (`qemu_args_with_devices` / the smoke `Wait` plumbing) for passthrough iteration, and the existing `syslogd` / `sshd` for network observability.

## Implementation Outline

1. **Track A** — add bulk EP type constants + context builders in `kernel-core/src/usb/enumerate.rs` (host tests); extend Configure Endpoint to include bulk EPs; implement the bulk `SubmitTransfer` consumer + Normal-TRB ring programming in `userspace/drivers/xhci/src/server.rs`; add a bulk read/write client API in `userspace/drivers/usb-core`.
2. **Track B** — scaffold `userspace/drivers/ure` (the four-place new-binary wiring: workspace member, xtask `bins`, ramdisk entry, `services.d` config); implement OCP register access, chip reset/init, MAC read, PHY/auto-neg, RX/TX descriptor framing; register `RemoteNic` and forward frames; report `NET_LINK_STATE`.
3. **Track C** — add `cargo xtask run --usb-passthrough <vid:pid>`; write `scripts/m3os-logsink.sh` (UDP/syslog listener + ssh tail → tailable file) and `scripts/ure-vfio-validate.md` (AMT SOL runbook).
4. **Track D** — add the opt-in `ure-smoke` gate (passthrough `0bda:8156`, assert enumerate → link → DHCP/static → TCP GET); write the bare-metal validation runbook; add the `M3OS_URE_REGRESSION` row to the pre-push gate table in `AGENTS.md`.

## Acceptance Criteria

- A bulk EP context is built and a Configure Endpoint including bulk endpoints succeeds; host tests in `kernel-core` cover the bulk context-dword encoding (EP Type 2/6, MPS, CErr).
- With `0bda:8156` passed through (`cargo xtask run --usb-passthrough 0bda:8156`), serial shows the device enumerating, `ure` binding, the MAC read from `PLA_IDR`, and the link negotiating up.
- The in-kernel TCP/IP stack completes an outbound HTTP GET over the dongle (DHCP or static IP), proven by `ure-smoke` asserting a known sentinel from a host-served endpoint.
- `ure-smoke` PASSES with the device present and SKIPS-with-reason when absent; the `M3OS_URE_REGRESSION` gate is documented in `AGENTS.md`.
- `cargo xtask run --usb-passthrough <vid:pid>` injects the `usb-host` device and the existing serial harness captures its log.
- `scripts/m3os-logsink.sh` on a second machine appends the target's `syslogd`/console stream to a single tailable file; `scripts/ure-vfio-validate.md` documents the AMT-SOL pre-network capture end-to-end.
- Bare-metal: booting the USB image on the reference machine enumerates the dongle and reaches a network-reachable state with logs captured over SOL (pre-network) then the network sink (post-network).

## Companion Task List

- [Phase 96 Task List](./tasks/96-bare-metal-usb-ethernet-tasks.md)

## How Real OS Implementations Differ

- **Linux `r8152.c`** is ~9,000 lines with NAPI, runtime PM, firmware patching, RSS, and the full RTL815x matrix; `ure` here targets the bring-up subset (polled-ish RX completion, no PM, no firmware patch) — closer to OpenBSD `ure(4)` or u-boot's `r8152`.
- Real drivers expose **CDC-ECM/NCM fallback**: many RTL8156 dongles also enumerate a standards CDC interface. Production stacks pick the vendor protocol for performance; a teaching OS could alternatively implement only CDC-ECM (simpler, no vendor registers) at a throughput cost. This phase takes the vendor path because it's what the chip presents by default and what BSD `ure` documents.
- Production USB stacks stream RX with multiple in-flight bulk URBs and zero-copy DMA; `ure` uses a simple submit/complete loop sized for correctness over throughput.
- Real bring-up uses JTAG / a hardware USB analyzer / a vendor BIOS debug UART; this phase substitutes QEMU USB-passthrough + AMT Serial-over-LAN because that is what the reference hardware exposes.

## Deferred Until Later

- **CDC-ECM/NCM generic USB-NIC class driver** — a vendor-neutral path for non-Realtek dongles; deferred, reuses Track A bulk endpoints.
- **I2C-HID touchpad** (Intel LPSS DesignWare I2C + I2C-HID multitouch, OpenBSD `dwiic` + `imt` references) — a separate future bare-metal phase that reuses Track C's bring-up tooling; the HID Report Protocol home is Phase 90 Track B.
- **Intel AX201 / CNVi Wi-Fi** (OpenBSD `iwx(4)` reference — supports AX201, BSD-licensed) — a much larger future phase; deferred, reuses Track C's observability workflow.
- **USB NIC offloads** (checksum/TSO/RSS), runtime power management, and multi-URB RX pipelining — deferred throughput work.
- **DHCP client** if not already present in the Phase 79 stack — bring-up uses static IP; a DHCP client can ride a later networking phase.
