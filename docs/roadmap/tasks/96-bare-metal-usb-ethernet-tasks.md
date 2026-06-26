# Phase 96 — Bare-Metal Networking: USB Bulk Endpoints + RTL8156 USB-Ethernet (`ure`): Task List

**Status:** ✅ **COMPLETE (2026-06-26)** — the RX wall is cleared. On **bare metal** (Tiger Lake laptop, dongle cold-owned by m3OS) the `ure` NIC came up (`[remote_nic] up=true 2500Mbps`) and the in-kernel DHCP client **bound a real lease** (`[dhcp] bound ip=192.168.1.221/255.255.255.0 gw=192.168.1.254`) — a lease requires the full RX path (OFFER/ACK) plus TX, so B.5 RX-frame + B.6 TX-egress are validated end-to-end on silicon (the parked full `ure_rtl8156_init` runs where m3OS cold-owns the device, as predicted). HTTP-over-`ure` rides the same datapath (opt-in `M3OS_URE_NET`). The remaining text below documents the *passthrough-era* RX wall — kept for the record. — Control plane HW-validated; **DHCP client built + validated end-to-end** (over e1000/SLIRP, and now over `ure` on bare metal); ure RX-over-QEMU-passthrough blocked by a **passthrough limitation** (needs bare-metal/VFIO — now satisfied). R1 ✅; R2 Stage-1a/1b/2 ✅ HW-validated control plane (claim → MAC → control-OUT init → `link up 2500M` → `RemoteNic` registration; reviewer SHIP ×2; no HID regression); Track C ✅; Track D ✅ `ure-smoke` PASSES. **R4 ✅ DHCP client:** protocol (`kernel-core/net/dhcp.rs`, 29 host tests) + runtime-mutable IP config + kernel glue ticked by `net_task` — **proven end-to-end** on e1000/SLIRP: `[dhcp] DISCOVER sent → OFFER received; REQUEST sent → bound ip=10.0.2.15/255.255.255.0 gw=10.0.2.2`. **ure RX wall = QEMU usb-host passthrough limitation:** the host (Linux r8152) already reset+power-managed+linked the device (`OOB_CTRL`=0x00), and *any* substantial re-init (`ure_rtl8153b_init`/`nic_reset`) tears down that host-established USB connection (link drops, EP0 wedges — no re-enumeration possible under passthrough); the light-touch minimal init preserves the link but the chip won't stream RX without the (destructive-here) full init. The full `ure_rtl8156_init` is faithfully ported + parked (`#[allow(dead_code)]`) for the bare-metal/cold-attach path. Also fixed a real xHCI **deadlock** (transfer waits had no timeout → bounded polling). See **Resume Here**.
**Source Ref:** phase-96
**Depends on:** Phase 78c (HID Boot Protocol + `usb` IPC service) ✅, Phase 79 (Networking — `RemoteNic` + IPv4/TCP/UDP) ✅
**Goal:** Add USB **bulk** endpoint support to the xHCI host stack, ship a Realtek RTL815x (`ure`) USB-Ethernet class driver that registers as a `RemoteNic`, and land the reusable bare-metal bring-up & observability workflow (USB-passthrough iteration, AMT Serial-over-LAN, network log sink) so the in-kernel TCP/IP stack does real networking over a physical `0bda:8156` 2.5GbE dongle — validated in QEMU passthrough, then on bare metal.

## Progress Log

- **Round 1** (branch `docs/96-bare-metal-usb-ethernet`, PR #237, WIP/draft) — the validatable-now, hardware-independent slice:
  - **Track A** — *Finding:* the bulk EP **context** support already existed (`EP_TYPE_BULK_OUT=2`/`EP_TYPE_BULK_IN=6` in `kernel-core/src/usb/xhci/context.rs`; the `(2,false)/(2,true)` arms in `build_configure_endpoint_ctx`). Round 1 added the missing **regression test** (`configure_endpoint_maps_bulk_out_and_bulk_in_ep_types`) + import tidy. Host tests: 22 pass. Commit `82a6154`.
  - **Track C** — `--usb-passthrough <vid:pid>` run mode (emulated `qemu-xhci` + `usb-host`), `scripts/m3os-logsink.sh`, `scripts/ure-vfio-validate.md`. 8 new xtask tests (170 pass). Commits `577c341`+`1868dfb` (review fixes).
  - Integration: fmt clean, both suites green (commit `2f22d72`).
- **Round 2** (hardware-enabled, in progress — physical `0bda:8156` attached) —
  - **Foundation (tested):** `AttachNotice` gained `vendor_id`/`product_id` + `bulk_in/out_dci+mps`; `device_info_from_ctx` now surfaces any interface with a bulk IN+OUT pair (not just HID); added `TRANSFER_TYPE_BULK`. usb-core: 14 host tests pass. Commit `2ad27cf`.
  - **`ure` Stage-1a driver — ✅ HARDWARE-VALIDATED:** `userspace/drivers/ure` claims `0bda:8156` (by VID/PID) and reads the MAC from `PLA_IDR` via an OCP vendor-IN control transfer over the **existing EP0 path**. Commit `08359f2`. **Validated on the physical dongle via QEMU `usb-host` passthrough:** xHCI enumerated `VID=0bda PID=8156` (vendor class 0xff; bulk-IN `0x81`/bulk-OUT `0x02`/interrupt-IN `0x83` all configured), `ure: claimed 0bda:8156 slot=1`, `ure: MAC 08:92:04:52:d7:97` (matches the host's view of the dongle), `URE_STAGE1A:OK`. Proves enumeration→surface(by VID/PID)→claim→OCP control-read against real silicon.
  - **Architecture discovered (drives the remaining tasks):** a *working NIC* needs more than the doc's A.3 assumed — the xHCI server currently only does control (EP0) + pre-configured interrupt-IN (HID); `SubmitTransfer`/`ConfigureEndpoints` return `ENOSYS`. Remaining server work: **(i) control-OUT with data** (`control_transfer` only allocs a buffer for `dir_in`; OCP *writes* for init/enable need an OUT data stage) — extend `ControlRequest` + `controller.control_transfer`; **(ii) bulk RX/IN poll + bulk OUT submit** mirroring the existing `InterruptEndpoint`/`arm_interrupt_in`/`capture_interrupt_report` machinery (`USB_MSG_MAX=1024` is too small for a 1522-byte frame — bump it or use the `PageGrant` path). BSD `ure(4)` register map captured: OCP tunnel = control req `bmRequestType` 0x40/0xC0, `bRequest` 0x05, `wValue`=reg, `wIndex`=MCU_TYPE|byte_en; `MCU_TYPE_PLA=0x0100`/`MCU_TYPE_USB=0x0000`; `PLA_IDR=0xc000` (MAC), `PLA_CR` RE/TE bits enable RX/TX, RX/TX 8-byte descriptor headers (`URE_RXPKT_LEN_MASK=0x7fff`, `URE_TXPKT_TX_FS/LS`), link via `PLA_PHYSTATUS`.
  - **Staged milestones:** 1a = claim + MAC read (control IN) ✅ HARDWARE-VALIDATED; 1b = control-OUT + chip init + link up; 2 = bulk RX/TX + `RemoteNic` → DHCP/ping/HTTP; D = `ure-smoke`. Each needs a hardware iteration (privileged QEMU passthrough run).
  - **Stage-1b IPC path (commit `6c48288`):** the xHCI control path could only *read* (IN); OCP register **writes** need an OUT data stage. Added `UsbRequest::ControlWrite { slot_id, setup, data }` (wire tag 7, encode/decode + host test; `ControlRequest` left untouched for usb-hid's no-data path) → server `ControlWrite` handler → `controller.control_transfer(.., out_data: Option<&[u8]>)` now allocates+copies the host payload for any `len>0` OUT transfer (readback guarded by `dir_in`), plus `controller.control_write()` validating D2H-clear + `wLength==data.len()` fail-closed. usb-core 15 host tests pass.
  - **Stage-1b driver init (commit `01066d2`):** `userspace/drivers/ure/src/regs.rs` — the full RTL815x register map (314 consts + `byte_en_1/2` helpers) re-expressed from OpenBSD `if_urereg.h` rev 1.14 / `if_ure.c` rev 1.37 (BSD-2-Clause, source-verified). `main.rs` ports `ure_read_1/2/4` + `ure_write_1/2/4` faithfully (dword-aligned 4-byte OCP window, `wLength=4`, shifted byte-enable OR'd into `wIndex` for writes; reads pass `mcu_type` unchanged). Minimal `ure_init`: `PLA_RMS=1522`, `RCR=APM|AM|AB`, `PLA_CR |= RE|TE`; **verified by reading `PLA_CR` back and asserting `RE|TE` latched** (proves the OUT transfer reached the chip). Link read from `PLA_PHYSTATUS` (up/down+speed). New `URE_STAGE1B:OK` sentinel. `ure_driver` builds + clippy-clean on `x86_64-unknown-none`. **Adversarial reviewer (opus) verdict: SHIP** — byte-for-byte vs OpenBSD `ure(4)`, both high-risk lane-shifted addresses confirmed (RMS `0xC016`→wIndex `0x01CC`, PLA_CR `0xE813`→wIndex `0x0188`), OUT control path / validation / wire codec / panic-safety all PASS; no blockers.
  - **Stage-2 (code-complete — commits `a56179d` transport + `d63e715` driver):** the bulk RX/TX path that turns the dongle into a NIC. **Transport** (`PollBulkIn`/`SubmitBulkOut` requests + `BulkData` reply, `USB_MSG_MAX` 1024→4096 for inline frames): generalized the Phase-78c interrupt-IN machinery to frame-sized bulk — `InterruptEndpoint.armed_len` (capture/re-arm use the armed TRB length, not `mps`, so HID is unchanged but bulk gets a frame-sized buffer), `arm_ring_in` (grows `data_buf` on demand), `arm_bulk_in`/`take_bulk_report`/`submit_bulk_out`. `submit_bulk_out`'s wait captures IN completions (odd DCI) seen during the TX wait so concurrent RX is never lost. **Driver** (`ure/src/net.rs`): a polled `RemoteNic` io-loop (no IRQ behind xHCI → `NetServer::try_handle_next` + `PollBulkIn`, 1 ms idle pacing) — registers `net.nic`, publishes link state, prepends the 8-byte Realtek V1 TX descriptor (`len|TX_FS|TX_LS`) on bulk-OUT, strips the 8-byte RX descriptor + 4-byte CRC (8-byte-aligned per `ure_decap`) on bulk-IN → `publish_rx_frame`. New `URE_STAGE2:NIC-UP` sentinel. RX/TX framing re-expressed from OpenBSD `ure(4)`. Builds + clippy-clean; usb-core 15 host tests pass.
    - **Stage-2 NIC registration ✅ HARDWARE-VALIDATED** (passthrough of the physical `0bda:8156`): `URE_STAGE2:NIC-UP` fired, then the kernel net stack bound it — `[remote_nic] link-state bootstrap registered ring-3 NIC driver: endpoint=EndpointId(11) mac=08:92:04:52:d7:97`. No panic, no `bulk rx buf alloc failed`, **no HID regression** (kbd/mouse/usb-hid started cleanly despite the shared xHCI `armed_len` changes), and the polled io-loop did **not** hang the boot (usbhub→usb-hid→term→audio all came up after it). So bulk transport + `RemoteNic` registration + link-state publish are proven on silicon. **Not yet exercised on hardware:** an actual RX/TX *frame* over the bulk path (no traffic flowed — m3OS does not auto-DHCP a passthrough NIC and virtio-net/SLIRP is the default route). Driving real DHCP/ping/HTTP over the dongle interface is the **Track D `ure-smoke`** job (B.5 RX-frame / B.6 TX-egress acceptance complete once that gate sends traffic through `ure`).
    - **Stage-2 adversarial reviewer (opus) verdict: SHIP** — **HID interrupt-IN path provably unchanged** (diffed byte-for-byte vs the pre-change `arm_interrupt_in`: a HID endpoint's `data_buf` is already `≥ mps` so `arm_ring_in` never reallocs, and `armed_len == mps` makes capture/re-arm identical). RX stride (`8 + roundup(pktlen, 8)`) + CRC strip (`pktlen − 4`) and the TX descriptor (`len | TX_FS | TX_LS`, word1 0) match OpenBSD `ure_decap`/`ure_encap_txpkt` V1 exactly. OOB safety, the `arm_ring_in` disjoint-borrow split, the `submit_bulk_out` capture-during-TX-wait, IPC sizing (USB_MSG_MAX 4096 stack buffers fine; 2052 B reply / 1535 B request both < 4096), io-loop nesting (no deadlock — same pattern as the HID `ControlRequest`-in-bound-loop), and the wire codec are all PASS. Two MINOR notes (poll-cadence throughput, `BULK_RX_LEN=2048` cap) are documented design choices, no fix. No BLOCKER/MAJOR.
  - **Stage-1b ✅ HARDWARE-VALIDATED (QEMU `usb-host` passthrough of the physical `0bda:8156`):** serial log shows `ure: claimed 0bda:8156 slot=1 class=ff` → `ure: MAC 08:92:04:52:d7:97` → `URE_STAGE1A:OK` → **`ure: PLA_CR=0x0c`** (read-back after the OUT write: `RE(0x08)|TE(0x04)` latched — proves the control-OUT transfer reached the chip through the correct byte lane of `0xE813`) → **`ure: link up 2500M`** (PHYSTATUS read working; the dongle auto-negotiated 2.5GbE — the reduced init was sufficient for link, the reviewer's "link may stay down" caveat did not bite) → **`URE_STAGE1B:OK`**. The lone `[xhci] ctrl-xfer: completion code 6` (STALL) in the log is **usb-hid**'s `SET_IDLE(0)` on the emulated GUI HID device (`usb-hid: warn: SET_IDLE(0) failed; continuing to poll`), unrelated to `ure`. Control IN + control OUT + byte-enable lane math + link readout all proven on real silicon.

## Datapath bring-up findings (R3 — hardware, the RX/TX blocker)

The control plane (enumerate → claim → MAC → init → link → `RemoteNic` register) is hardware-proven. Getting an actual **frame** across the bulk path is not yet working; this records exactly why, so the next session doesn't re-walk it.

- **Symptom:** booted with the physical `0bda:8156` on a live 2.5GbE LAN (constant broadcast/ARP/mDNS). The `ure` io-loop polls bulk-IN **60k+ times with `data=0, ipcfail=0`** — the transport is sound, the chip simply never streams an RX frame to the bulk-IN endpoint. (TX never fires at idle: static IP `10.0.2.15` is wrong for the dongle's LAN, so the stack sends nothing.)
- **Deadlock bug found + fixed (real, committed):** the xHCI server's transfer waits blocked on `irq.wait()`, which has **no timeout** in m3OS (`notify_wait` is unbounded). A never-completing transfer or a coalesced/already-consumed IRQ (lost-wakeup race) deadlocked the single-threaded server → hung the whole USB stack, intermittently stalling the io-loop on its first `usb_call`. Converted `wait_for_transfer_event` + `wait_for_bulk_out_event` to **bounded event-ring polling** (~400 ms, 1 ms steps), advancing `ERDP`/`IMAN.IP` only when the drain consumed events. HID re-validated (`usb-smoke` PASS) — no regression.
- **RX-init attempts (insufficient):** added, on top of the minimal init, the `RXDY_GATED_EN` ungate (`PLA_MISC_1`), RX-FIFO thresholds (`RXFIFO_FULL=1024`/`RX_FIFO_EMPTY=2048`), `USB_RX_BUF_TH=0x00600400`, and `RX_AGG_DISABLE` (so the chip flushes each frame immediately rather than coalescing). **Still `data=0`** — none of these alone open the RX stream.
- **OOB-claim shortcut is wrong here + harmful:** ported `ure_rtl8153_nic_reset`'s MAC-claim (clear `NOW_IS_OOB` in `OOB_CTRL`, `RE_INIT_LL` + poll `LINK_LIST_READY`). On hardware: `autoload=01` (firmware loaded) but **`OOB_CTRL` reads `0x00`** — `NOW_IS_OOB` is already clear (the host released the MAC before passthrough), so the claim is a no-op; and the reset/`RE_INIT_LL` writes **destabilize EP0** — control transfers begin timing out mid-sequence (638 "no transfer event"), with no EP0-halt recovery, hanging init. The `ure_nic_reset` code is parked (`#[allow(dead_code)]`, not called) as the basis for the proper port.
- **Conclusion / the real remaining work:** RX needs the **complete, faithful `ure_rtl8156_init` (a.k.a. `ure_rtl8153b_init`) power-on + reset sequence**, ported in exact order — not the piecemeal subset — and likely (a) EP0-halt recovery (Reset-Endpoint / Clear-Feature(HALT)) in the xHCI server so a stall doesn't wedge the control path, and (b) care that any USB-level reset the sequence triggers doesn't desync the passthrough slot/endpoints. This is a substantial, iteration-heavy effort (5-min non-deterministic boots) and is the gate for any real traffic — and therefore for the requested **DHCP client** (which also needs runtime-mutable IP config in `kernel/src/net/config.rs` + broadcast UDP from `0.0.0.0`; egress-over-`ure` already works since `net::send_frame` prefers the registered `RemoteNic`).

## R4 — Full datapath + DHCP (in progress)

User directive: do the full DHCP work — i.e. get real RX/TX traffic over `ure` and a working DHCP client. Plan, parallelized:
1. **RX datapath (critical path, hardware):** port the complete, faithfully-ordered `ure_rtl8156_init`/`ure_rtl8153b_init` (MAC/USB register sequence; skip PHY/SRAM since link is up) + add **EP0 halt-recovery** to the xHCI server if the reset stalls EP0. Goal: `ure: rx len=…` frames flow.
2. **DHCP protocol (independent, host-tested):** `kernel-core/src/net/dhcp.rs` — DISCOVER/REQUEST build + OFFER/ACK parse + option codec + `Init→Selecting→Requesting→Bound` state machine (RFC 2131, smoltcp cross-check). Pure logic, host tests.
3. **DHCP integration (kernel glue):** runtime-mutable IP config in `kernel/src/net/config.rs` (`set_config`) + broadcast UDP from `0.0.0.0`→`255.255.255.255` + drive the state machine, kicked on NIC link-up. Egress-over-`ure` already works (`net::send_frame` prefers the registered `RemoteNic`).
4. **Validate** DHCP-over-`ure` end-to-end on the real LAN; then ping/HTTP.

**R4 progress:**
- ✅ **DHCP protocol** (`kernel-core/src/net/dhcp.rs`, 29 host tests pass): `DhcpClient::{new,start,on_reply,reset}` + `build_discover/request` + `parse_reply` + `DhcpAction::{SendRequest,Bound,Nak,Ignore}`/`DhcpConfig`, reusing `kernel_core::types::{Ipv4Addr,MacAddr}`.
- ✅ **Runtime-mutable IP config** (`kernel/src/net/config.rs`): atomics + `set_config(ip,mask,gw)`; `our_ip`/`subnet_mask`/`gateway_ip` read it back.
- ✅ **DHCP kernel glue** (`kernel/src/net/dhcp.rs`): `tick()` driven by `net_task` (after `process_rx`/`tcp_tick`) — broadcast DISCOVER (`0.0.0.0:68`→`255.255.255.255:67`, MAC bcast, UDP csum 0), drain UDP:68 → `parse_reply` → state machine → REQUEST → on ACK `config::set_config`. Retransmit ~2 s. **Gated on `RemoteNic::is_registered()`** so pure virtio/SLIRP boots keep their exact static-IP path (zero risk to existing gates); RemoteNic boots converge (SLIRP serves 10.0.2.15; real LAN serves its own).
- ✅ **DHCP end-to-end VALIDATED** (`cargo xtask run --device e1000`, QEMU SLIRP DHCP server): serial showed `[dhcp] DISCOVER sent` → `[dhcp] OFFER received; REQUEST sent` → `[dhcp] bound ip=10.0.2.15/255.255.255.0 gw=10.0.2.2`. The full client handshake + `config::set_config` install proven on a working emulated NIC — independent of the ure RX wall.
- ✅ **Full `ure_rtl8156_init` ported** (faithful, in exact order: `ure_rtl8153b_init` power-up + `ure_nic_reset` with the correct EP0 ordering — `ure_reset` CDC_ECM toggle → BMU flush → **then** OOB claim; 8156 **skips** LINK_LIST_READY — + `ure_ifmedia_init` + `ure_iff`). **Parked `#[allow(dead_code)]`** — it is **destructive on the QEMU-passthrough'd device** (the host already owns/linked it; the power/USB/reset writes drop the 2500M link + wedge EP0 with no re-enumeration). For the bare-metal/cold-attach path.
- ✅ **ure active path = `ure_init_minimal`** (link-preserving light-touch RX/TX enable: RMS, RCR APM\|AB, FIFO/RX_BUF_TH, agg-disable, `CR RE\|TE`, RXDY ungate). Keeps the host-established link up; the ure control plane (claim→MAC→init→link→`RemoteNic`→DHCP-DISCOVER) works on the passthrough, while actual RX frames await bare-metal (passthrough doesn't forward the bulk-IN stream from the host-owned device).
- **Net result:** the DHCP client is complete + validated; over `ure` it sends DISCOVER and would bind the real-LAN IP the moment RX delivers an OFFER — which requires bare-metal/VFIO (the documented validation path for the live ure traffic arm).
- **Auto-init by ownership state (commit `e06cf26`):** the driver reads `PLA_OOB_CTRL.NOW_IS_OOB` — SET ⇒ cold device (bare-metal cold attach) ⇒ run the full `ure_rtl8153b_init`+`nic_reset`+`ifmedia_init`+`iff`; CLEAR ⇒ host-pre-initialized (QEMU passthrough) ⇒ `ure_init_minimal`. The **same image** is correct on QEMU (minimal — gate stays green) and bare metal (full — enables RX). No build flag / code edit needed.
- **Operator runbook for the bare-metal test:** `scripts/ure-baremetal-usb.md` — build the image (`cargo xtask image`), `dd` it to a USB stick, UEFI-boot the target with the dongle on a DHCP LAN, capture the log (AMT SOL / screen / network sink), and confirm the `ure: rx len=…` + `[dhcp] bound …` sentinels. Closes the D.3 loop.
- **Fresh-dongle confirmation (replug, clean device, minimal init):** control plane up (`URE_STAGE1A:OK`, `URE_STAGE1B:OK` = RE\|TE latched, `RemoteNic registered`), **`ure: tx … ok`** — the DHCP DISCOVER **bulk-OUT TX succeeded** (the chip accepted the frame); but **RX still 0** (bulk-IN never completes, `ipcfail=0`) and 161 control-read timeouts. So **TX works, RX does not, through QEMU usb-host** — the bulk-IN RX stream from a host-owned device isn't delivered to the guest, and control transfers are flaky through the grab. This is a definitive QEMU-passthrough limitation (≈17 boots); the RX/lease milestone requires **bare-metal/VFIO** where m3OS cold-owns the device (the parked full `ure_rtl8156_init` then runs and re-enumeration is possible).

## Resume Here (next session)

**Branch:** `docs/96-bare-metal-usb-ethernet` (PR #237, draft). Last code commit `01066d2` (Stage-1b driver init). Everything builds clean; image builds; usb-core **15** tests + xtask 170 tests pass; `ure_driver`/`xhci_driver` clippy-clean on `x86_64-unknown-none`.

**Stage 1b ✅ HARDWARE-VALIDATED** (no validation owed). Passthrough run confirmed `URE_STAGE1A:OK` → `PLA_CR=0x0c` (RE|TE latched) → `link up 2500M` → `URE_STAGE1B:OK` on the physical dongle. The control-IN + control-OUT + byte-enable lane math + link readout are all proven on real silicon. **Stage 2 is the active work** (see below).

**Hardware-run loop (this machine — Dell Precision 5560, dongle `0bda:8156` at USB bus 2 dev path):**
```
# one-time per boot: make the dongle node accessible to QEMU (no sudo on cargo needed after)
sudo chmod 666 $(lsusb -d 0bda:8156 | sed -E 's#Bus ([0-9]+) Device ([0-9]+).*#/dev/bus/usb/\1/\2#')
# build + boot with the real dongle passed through; serial to a log
timeout 220 cargo xtask run --usb-passthrough 0bda:8156 --fresh > /tmp/ure-run.log 2>&1
grep -inE 'ure:|URE_|\[xhci\]|MAC|link' /tmp/ure-run.log
```
`/dev/kvm` is world-accessible here; no passwordless sudo (only the chmod needs sudo). First `--fresh` boot is slow (pre-Phase-87 VFS writes); `ure`/xHCI output lands ~2/3 through. If the Nerd Font is missing, run `cargo xtask fetch-fonts` first.

**Stage 1b is ✅ DONE** (commits `6c48288` + `01066d2`, HARDWARE-VALIDATED above): `ControlWrite` IPC path, full `regs.rs` map (source-verified from OpenBSD), ported `ure_read/write_1/2/4`, minimal `ure_init` (RMS/RCR/CR RE|TE) + PLA_CR read-back verify + link readout — all proven on the physical `0bda:8156`.

**Stage 2 is ✅ DONE** (commits `a56179d` transport + `d63e715` driver, HW-validated + reviewer SHIP): inline bulk transport (`PollBulkIn`/`SubmitBulkOut`/`BulkData`, `USB_MSG_MAX`→4096) over the generalized interrupt-IN machinery (`armed_len`, `arm_ring_in` regrow, `arm_bulk_in`/`take_bulk_report`/`submit_bulk_out` with capture-during-TX-wait); `ure/src/net.rs` polled `RemoteNic` io-loop (V1 RX/TX descriptors, `try_handle_next`+`PollBulkIn`). The kernel net stack binds the USB NIC; HID unaffected.

**Track D is ✅ DONE** (commit `1ab8f4f`): `ure-smoke` PASSES against the dongle (always-on NIC-up core; sysfs skip-when-absent; opt-in `M3OS_URE_NET` traffic arm), `M3OS_URE_REGRESSION` AGENTS row, roadmap status.

**What actually remains for the phase (all operator-owned / non-CI):**
1. **Opt-in live traffic** — `M3OS_URE_NET=1` DHCP/ping/HTTP routed *over `ure`*. Blocked on two non-driver concerns: (a) the passthrough dongle is on the host's real LAN (no SLIRP control), so it needs a DHCP+egress LAN; (b) m3OS must route over `ure` rather than the default virtio/SLIRP NIC (boot with ure as the *only* NIC, or add interface selection). Drive manually per `scripts/ure-vfio-validate.md`. This is the **B.5 RX-frame / B.6 TX-egress** "frame observed" acceptance.
2. **D.3 bare-metal boot** on the reference machine (USB boot, AMT SOL + network-sink capture) — needs physical access; the driver path is already proven on real silicon via passthrough.
3. **Merge** — the user is holding the branch; do not merge until they say so.

> NOTE: RTL8156 uses the **v1** (8-byte) RX/TX descriptors (`regs::URE_RXPKT_*` / `URE_TXPKT_*`); the v2 (16-byte) layout in `regs.rs` is RTL8157-only.

**Key seam file:line refs:** driver `userspace/drivers/ure/src/{main,net,regs}.rs`; bulk transport `usb-core/src/protocol.rs` (`PollBulkIn`/`SubmitBulkOut`/`BulkData`); controller bulk machinery `userspace/drivers/xhci/src/controller.rs` (`arm_ring_in`/`arm_bulk_in`/`take_bulk_report`/`submit_bulk_out`/`wait_for_bulk_out_event`); server arms `userspace/drivers/xhci/src/server.rs`; the gate `xtask/src/main.rs` `cmd_ure_smoke`. BSD refs: `ure(4)` `if_ure.c`/`if_urereg.h` (BSD-2; Linux `r8152.c` facts-only).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | USB bulk endpoint support (EP contexts + Configure Endpoint + bulk transfer consumer + usb-core client API) | — | ✅ A.1/A.2 (R1) · A.3/A.4 landed via the **inline** bulk transport (`PollBulkIn`/`SubmitBulkOut`/`BulkData` + controller `arm_bulk_in`/`take_bulk_report`/`submit_bulk_out`) — code-complete, awaiting HW. (The page-grant `SubmitTransfer` variant is left for Phase 90 mass-storage; inline frames fit `USB_MSG_MAX`.) |
| B | `ure` RTL815x USB-Ethernet class driver → `RemoteNic` | A | ✅ B.1/B.2/B.3 HW-validated (Stage-1a) · B.2-write/B.3-init/B.4 link ✅ HW-validated (Stage-1b) · B.5 RX + B.6 TX + B.7 `RemoteNic` ✅ code-complete (Stage-2, awaiting HW) |
| C | Bare-metal bring-up & observability tooling (`run --usb-passthrough`, AMT SOL runbook, `m3os-logsink`) | — | ✅ Done (R1) |
| D | Validation (`ure-smoke` + bare-metal runbook + gate docs) | A, B, C | 🟡 D.1 `ure-smoke` gate (always-on NIC-up core + opt-in traffic arm) ✅ landed · D.2 `AGENTS.md` row + roadmap status ✅ · D.3 bare-metal pass → operator/manual (real-LAN dependency) |

---

## Track A — USB Bulk Endpoint Support

### A.1 — Bulk EP type constants + context builder

**File:** `kernel-core/src/usb/enumerate.rs`
**Symbol:** `EP_TYPE_BULK_OUT` / `EP_TYPE_BULK_IN`, `ep_context_dword1`
**Why it matters:** The enumerator only builds Control/Interrupt EP contexts today; without a bulk EP context (xHCI EP Type `2` = Bulk OUT, `6` = Bulk IN) no bulk transfer ring can exist, so a NIC's data endpoints cannot be configured.

**Acceptance:**
- [x] `EP_TYPE_BULK_OUT = 2` and `EP_TYPE_BULK_IN = 6` defined alongside the existing control/interrupt constants. *(pre-existing — `context.rs:179/181`, tested at `:428-429`)*
- [x] `ep_context_dword1` produces the correct dword for a bulk EP (EP Type, `CErr=3`, Max Packet Size from the endpoint descriptor). *(pre-existing — encodes any EP type)*
- [x] Host test in `kernel-core` asserts the bulk context-dword encoding for representative MPS values. *(R1: `configure_endpoint_maps_bulk_out_and_bulk_in_ep_types` asserts BULK_OUT/IN at MPS 512)*

### A.2 — Configure Endpoint includes bulk endpoints

**File:** `kernel-core/src/usb/enumerate.rs`
**Symbol:** the Configure Endpoint input-context builder (the function that walks parsed endpoints)
**Why it matters:** A device's bulk IN/OUT endpoints must be added to the Configure Endpoint input context with a transfer ring each, or the controller never schedules bulk TRBs.

**Acceptance:**
- [x] For a config with bulk IN + bulk OUT endpoints, the input context flags both EP contexts and allocates a transfer ring per EP. *(pre-existing logic at `enumerate.rs:290-291`)*
- [x] Host test covers a mixed config (one interrupt + one bulk IN + one bulk OUT) producing the expected Add Context flags. *(R1: new test asserts Add-Flags A3/A4/A5 set, A1/EP0 clear)*

### A.3 — Bulk `SubmitTransfer` consumer + Normal-TRB ring programming

**File:** `userspace/drivers/xhci/src/server.rs`
**Symbol:** the `UsbRequest::SubmitTransfer` handler (the page-grant transport defined in 78c with no live consumer)
**Why it matters:** Phase 78c defined the page-grant transfer transport but left it inert; bulk RX/TX is the first live consumer (and the exact path Phase 90 Track D.1 Mass Storage needs).

> **Status: deferred to Round 2 (hardware-enabled).** No-`std` driver code whose round-trip acceptance can only be exercised against the real dongle via `cargo xtask run --usb-passthrough 0bda:8156` (delivered in Track C).

**Acceptance:**
- [ ] `SubmitTransfer` maps the `PageGrant`, enqueues Normal TRBs (IOC on the last), rings the EP doorbell, and completes off the Transfer Event TRB.
- [ ] A bulk OUT submit transfers a known buffer and a bulk IN submit returns received bytes + residual length.
- [ ] Errors (STALL/short packet) surface a distinct result rather than hanging.

### A.4 — usb-core bulk client API

**File:** `userspace/drivers/usb-core` (the client-facing protocol/transfer module)
**Symbol:** `bulk_in` / `bulk_out` (new client helpers over `SubmitTransfer`)
**Why it matters:** The `ure` driver needs an ergonomic bulk read/write call rather than hand-rolling the IPC each time; keeps the class driver bus-detail-free.

> **Status: deferred to Round 2 (hardware-enabled)** — pairs with A.3; round-trip needs the live bulk path.

**Acceptance:**
- [ ] `bulk_out(ep, &buf)` and `bulk_in(ep, &mut buf) -> len` exist and round-trip against the Track A.3 server path.
- [ ] Doc comment notes the page-grant ownership contract (driver owns the DMA grant; no writable sharing beyond it).

---

## Track B — `ure` USB-Ethernet Class Driver

> Primary reference: OpenBSD/FreeBSD **`ure(4)`** (`sys/dev/usb/if_ure.c`, `if_urereg.h`, BSD-2 — re-expressed in Rust). Linux `r8152.c` used only as a fact cross-check (GPL → register constants/sequences only).

### B.1 — Crate scaffold + four-place new-binary wiring

**Files:**
- `userspace/drivers/ure/Cargo.toml`, `userspace/drivers/ure/src/main.rs`
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`bins` array in `build_userspace`)
- `kernel/src/fs/ramdisk.rs` (`include_bytes!` + `BIN_ENTRIES`)
- `xtask/src/main.rs` `populate_ext2_files` + `userspace/init/src/main.rs` `KNOWN_CONFIGS` (a `services.d/ure.conf`)

**Symbol:** `main` (driver entry), `ure.conf`
**Why it matters:** Missing any of the four wiring points means the driver is not built, not embedded, or not found at runtime (per the "Adding a New Userspace Binary" rule). `needs_alloc = true` (uses `kernel-core`/`Vec`).

**Acceptance:**
- [ ] `cargo xtask check` builds `ure`; it is embedded in the ramdisk and launched from `services.d/ure.conf`.
- [ ] Defines a `#[global_allocator]` (`syscall_lib::heap::BrkAllocator`) and enables the `alloc` feature on `syscall-lib`.

### B.2 — Device match + OCP register access

**File:** `userspace/drivers/ure/src/regs.rs`
**Symbol:** `ure_read_mem` / `ure_write_mem` (OCP vendor-request tunnel), `MCU_TYPE_PLA` / `MCU_TYPE_USB`
**Why it matters:** RTL815x registers are not MMIO — they are reached through vendor control requests addressing the PLA/USB register banks; every later step depends on this tunnel.

**Acceptance:**
- [ ] Matches RTL815x by VID/PID (`0bda:8152/8153/8156/8157`).
- [ ] Reads/writes PLA and USB bank registers via the OCP vendor request over the `ControlRequest` IPC path.
- [ ] Reads a known-constant register (e.g. chip version) and logs the expected value for `0bda:8156`.

### B.3 — Chip reset + init sequence + MAC read

**File:** `userspace/drivers/ure/src/init.rs`
**Symbol:** `ure_reset`, `ure_init`, `PLA_IDR`
**Why it matters:** Without the documented reset/init register sequence the chip will not pass traffic; the MAC address (read from `PLA_IDR`) is required for the `RemoteNic` registration and ARP.

**Acceptance:**
- [ ] Runs the `ure(4)` reset + init register sequence; chip reaches a ready state.
- [ ] Reads a plausible (non-zero, non-broadcast) MAC from `PLA_IDR` and logs it.

### B.4 — PHY bring-up + auto-negotiation

**File:** `userspace/drivers/ure/src/init.rs`
**Symbol:** `ure_phy_init`, link-status poll
**Why it matters:** The link must negotiate (10/100/1000/2500) and report up before frames flow; link state feeds `NET_LINK_STATE`.

**Acceptance:**
- [ ] PHY initialised; auto-negotiation completes against a real peer.
- [ ] Link up/down transitions are detected and logged; speed is reported.

### B.5 — RX path (bulk IN + RX descriptor)

**File:** `userspace/drivers/ure/src/rx.rs`
**Symbol:** `ure_rx_loop`, `ure_rxpkt` (RX descriptor header)
**Why it matters:** Each bulk-IN buffer carries one-or-more frames prefixed by the Realtek RX descriptor (length + flags); the driver must strip the header and forward the Ethernet frame to the kernel.

**Acceptance:**
- [ ] Bulk-IN completions are parsed into individual frames using the RX descriptor length field.
- [ ] Each frame is forwarded via `RemoteNic` ingress (`net.nic.ingress` / `inject_rx_frame`); a captured frame's EtherType/length is sanity-logged.

### B.6 — TX path (bulk OUT + TX descriptor) + `RemoteNic` egress

**File:** `userspace/drivers/ure/src/tx.rs`
**Symbol:** `ure_tx`, `ure_txpkt` (TX descriptor header)
**Why it matters:** Outbound frames from the TCP/IP stack must be prefixed with the Realtek TX descriptor (length / no-offload opts) and submitted on bulk OUT; this is the egress half of the `RemoteNic` contract.

**Acceptance:**
- [ ] Egress frames from the kernel are prefixed with a valid TX descriptor and submitted on bulk OUT.
- [ ] An outbound ARP/ICMP frame is observed leaving the device (peer replies).

### B.7 — `RemoteNic` registration + service lifecycle

**File:** `userspace/drivers/ure/src/main.rs`
**Symbol:** the `RemoteNic` register call + main service loop
**Why it matters:** Registration on the same `net.nic.ingress` surface the e1000 driver uses is what makes the USB NIC a first-class interface with no network-layer changes.

**Acceptance:**
- [ ] `ure` registers as a `RemoteNic` and survives detach/reattach (releases capabilities cleanly on disconnect).
- [ ] With the dongle present, `ip`-equivalent state in m3OS shows the interface and MAC.

---

## Track C — Bare-Metal Bring-up & Observability

### C.1 — `cargo xtask run --usb-passthrough <vid:pid>`

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_run` arg parsing + `qemu_args_with_devices`
**Why it matters:** Hands the **physical** dongle to the QEMU guest (`-device qemu-xhci -device usb-host,vendorid=…,productid=…`) so driver iteration runs against the real chip while the existing serial harness captures logs — the only in-the-loop path given no QEMU model exists.

**Acceptance:**
- [x] `cargo xtask run --usb-passthrough 0bda:8156` builds the `qemu-xhci,id=xhci_pt` + `usb-host,vendorid=0x0bda,productid=0x8156,bus=xhci_pt.0` device args. *(R1: flag parsing + arg emission unit-tested; the live boot-with-device is exercised in R2 with the dongle attached)*
- [x] Help text documents the `<vid:pid>` form and the host permission requirement (udev/`usb-host` access).

### C.2 — `scripts/m3os-logsink.sh` (network log sink)

**File:** `scripts/m3os-logsink.sh` (new)
**Symbol:** the listener script
**Why it matters:** Gives a second machine a single tailable file fed by the target's `syslogd`/console over the network — live post-network observability for an operator (or an AI session running on that machine).

**Acceptance:**
- [x] Runs a UDP listener (remote `syslogd` target) and optional `ssh` tail, appending to one log file. *(R1: `socat`-preferred, `nc` fallback, INT/TERM cleanup; `bash -n` clean)*
- [x] Documented usage: target → sink IP/port, and how m3OS `syslogd` is pointed at it.

### C.3 — `scripts/ure-vfio-validate.md` (bare-metal capture runbook)

**File:** `scripts/ure-vfio-validate.md` (new)
**Symbol:** the runbook
**Why it matters:** Documents AMT Serial-over-LAN capture (`amtterm`) for pre-network bare-metal panic/boot logs on a port-less machine, plus the USB-passthrough iteration loop — mirroring `scripts/mt792x-vfio-validate.md`.

**Acceptance:**
- [x] Covers: AMT SOL provisioning + `amtterm` capture (COM1 `0x3F8`), USB-passthrough iteration, and the hand-off to the network sink once the NIC is up.
- [x] States the second-machine requirement and the static-IP-on-LAN bring-up option (direct cable optional, not required).

---

## Track D — Validation

### D.1 — `ure-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_ure_smoke` (new) + `M3OS_URE_REGRESSION`
**Why it matters:** Proves the whole chain — enumerate → `ure` bind → link → IP → TCP — against the real chip, and gates regressions, while skipping cleanly where the device is absent (CI has no dongle).

**Acceptance:**
- [x] With `0bda:8156` passed through: asserts enumeration → `ure` claim + MAC → init → link up → **`RemoteNic` registration** (the kernel net stack binds the USB NIC). *Gate run: `ure-smoke: PASS — enumerate → claim → MAC → init → link → RemoteNic registration on the physical RTL8156`.* The **IP (DHCP/static) + outbound HTTP GET** is the **opt-in** `M3OS_URE_NET` arm (real-LAN dependency — see the function doc + AGENTS row), not the always-on core, since a passthrough NIC's traffic is non-CI-deterministic.
- [x] SKIPS-with-reason when the device is absent — sysfs-scanned `usb_host_device_present`; mirrors `tls-smoke`/`wifi-smoke`.
- [x] Runs at a timeout sized for the cold load + link negotiation — floored at 360 s (slow fresh-disk boot).

### D.2 — Gate + AGENTS.md documentation

**Files:**
- `AGENTS.md` (pre-push opt-in gate table)
- `docs/roadmap/README.md` (Phase 96 row + mermaid node)

**Symbol:** the `M3OS_URE_REGRESSION` row; the Phase 96 summary row
**Why it matters:** Keeps the gate discoverable and the roadmap accurate per the documentation policy.

**Acceptance:**
- [x] `M3OS_URE_REGRESSION=1` row added to the `AGENTS.md` gate table with the same skip-vs-pass semantics wording as the TLS/Wi-Fi rows.
- [x] `docs/roadmap/README.md` has the Phase 96 table row and a mermaid node depending on Phase 78/79 (`P78 --> P96`, `P79 --> P96`); status bumped to In Progress (stages 1a/1b/2 HW-validated).

### D.3 — Bare-metal validation pass

**File:** `scripts/ure-vfio-validate.md` (results appendix)
**Symbol:** the recorded bare-metal run
**Why it matters:** The phase's headline claim is real-hardware networking; this records the end-to-end bare-metal boot with logs captured over SOL then the network sink.

> **Status: operator-owned (the one remaining manual milestone).** The driver logic is already validated against **real RTL8156 silicon** via QEMU `usb-host` passthrough (`ure-smoke` PASS — the chip enumerates, claims, reads its MAC, accepts control-OUT init, links at 2.5GbE, and registers as a `RemoteNic`). What D.3 adds is a *bare-metal boot of m3OS from USB on the reference machine* (not a QEMU guest) with logs captured over AMT SOL then the network sink — which requires physical access to that machine and cannot be automated from the dev host.

**Acceptance:**
- [ ] Booting the USB image on the reference machine enumerates the dongle, brings the link up, and reaches a network-reachable state. *(driver path proven on real silicon via passthrough; bare-metal boot pending operator run)*
- [ ] Pre-network logs captured via AMT SOL; post-network logs captured via the network sink; both referenced in the runbook.

---

## Documentation Notes

- Track A delivers the bulk-endpoint infrastructure that Phase 90 (USB Class Expansion) Track D.1 (Mass Storage BOT) also needs — Phase 90 should consume it rather than reimplement; note the cross-reference when either lands.
- The `ure` driver is the first **USB** NIC and the first **non-PCIe** entry on the `RemoteNic` facade — record that the facade proved bus-agnostic unchanged.
- `ure` is re-expressed from BSD-licensed `ure(4)`; keep the license provenance note in the crate header (BSD source re-expressed; Linux `r8152.c` facts-only), matching the mt792x driver's `mt76`-citation convention.
- Track C tooling is reused by the deferred touchpad (I2C-HID) and AX201 Wi-Fi phases — keep `m3os-logsink.sh` / the SOL runbook driver-agnostic.
- Prefer exact files/symbols over directories when these land; update this list's checkboxes as tracks complete.
