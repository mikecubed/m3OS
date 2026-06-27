# Phase 104 - Wi-Fi: Intel AX201 / CNVi (`iwx`) + Supplicant

**Status:** Planned
**Source Ref:** phase-104
**Depends on:** Phase 81 (Wi-Fi reference — mt792x driver + `wifi-core` 802.11 mgmt FSM + WPA2-PSK 4-way handshake) ✅, Phase 79 (Networking — `RemoteNic` facade + IPv4/TCP/UDP stack) ✅
**Builds on:** Reuses the Phase 79 `RemoteNic` L2 NIC facade (`net.nic` / `net.nic.ingress`) and the Phase 81 `wifi-core`/`crypto-lib` soft-MAC 802.11 management plane + WPA2-PSK 4-way handshake (built for mt792x), and adds two things mt792x left out: (1) the Intel **AX201 / CNVi** (`iwx`) driver — a second Wi-Fi chipset family, the *only* built-in NIC on the Dell Tiger Lake laptop — and (2) a **running supplicant / connect daemon**. Phase 81 shipped `wifi-core` as a *library* of building blocks (config parser, FSM, EAPOL/KDF/mgmt codecs) folded inline into the mt792x driver process with a boot-time-only static `/etc/wpa.conf` read; it is not a running supplicant with a live `connect(ssid, psk)` control surface. This phase extracts that daemon. Chartered as phase 104 of the GUI-workstation arc in [Phase 98 — Roadmap Audit & Re-Charter](./98-roadmap-audit-and-recharter.md).
**Primary Components:** `userspace/drivers/iwx/` (new — the ring-3 Intel AX201/CNVi driver), `kernel-core/src/iwx/` (new — primitive-free `iwx` hardware logic: TLV-firmware parse, context-info bootstrap, host-command/notification codec, host-testable), `kernel-core/src/nic_ids.rs` (new AX201/CNVi device-ID registry + `is_iwx`/`is_ax201`), `userspace/wifid/` (new — the running supplicant/connect daemon), `userspace/wifi-core/` (reused FSM + a new MLME↔SME seam + the live `wifi.control` surface), `userspace/crypto-lib/` (reused WPA2 PMK/PTK chain), `kernel/initrd/lib/firmware/` (operator-staged Intel `iwlwifi` AX201 firmware + PNVM blobs), `kernel/src/net/dhcp.rs` (reused — the Phase 96 in-kernel DHCP client binds the lease over Wi-Fi unchanged)

## Milestone Goal

m3OS brings up the Dell laptop's **only built-in network interface** — the Intel **AX201 CNVi** Wi-Fi part (the laptop has no Ethernet port; Phase 96 used a USB dongle as the interim NIC) — with a new `iwx`-style ring-3 driver that loads the AX201 firmware, brings the radio up, and registers as a `RemoteNic` on the same `net.nic.ingress` surface the e1000 / `ure` drivers use. In the same phase a new **running supplicant daemon** (`wifid`) drives `scan → select → associate → WPA2-PSK 4-way handshake` against a real AP using the existing `wifi-core`/`crypto-lib` host crypto, exposes a control IPC (`scan results`, `connect(ssid, psk)`, `status`), and — once the handshake installs keys and data frames flow — the in-kernel DHCP client binds a lease over Wi-Fi. The GUI network picker that consumes the daemon's control IPC is Phase 105.

## Why This Phase Exists

Phase 81 made 1.0 "honest on the laptop" with a *reference* Wi-Fi driver, but it targeted exactly one chipset family (MediaTek **mt792x**) and explicitly deferred every other family. The actual Dell Tiger Lake dev laptop's built-in NIC is an **Intel AX201**, an integrated CNVi (Connected-V-integrated) part driven on other systems by `iwlwifi` — a family m3OS cannot drive. Phase 96 worked around this by passing a Realtek USB-Ethernet dongle through to the machine; that is the interim NIC, not the built-in one. The laptop has **no Ethernet port at all**, so until the AX201 is driven the workstation has no native connectivity.

An AX201 driver *alone* still does not let a user join a network. Phase 81's "supplicant" is the `wifi-core` FSM *folded into the mt792x driver process*, reading `/etc/wpa.conf` once at boot (`load_supplicant` in `userspace/drivers/mt792x/src/main.rs`); the live scan/auth/assoc orchestration against the radio was itself deferred (mt792x Track E.4). There is no standalone running daemon, no runtime `connect(ssid, psk)`, and no live control surface a UI could drive. Both halves — a driver for the real chip **and** a running supplicant — are prerequisites before Wi-Fi is usable from the workstation, and before the Phase 105 settings/network-picker UI has anything to talk to.

## Learning Goals

- Understand Intel's **CNVi** split: the RF die (Companion RF / CRF) sits on the internal **CNVio** bus, fronted by an integrated MAC in the PCH, but presents to the OS as a single PCI function with a CSR/HBUS register window in BAR0 — so the driver structure mirrors a discrete PCIe Wi-Fi part even though the radio is integrated.
- See how a `iwlwifi`/`iwx`-class **gen2** device boots: a TLV-format `.ucode` blob parsed into UMAC/LMAC sections + a separate platform-NVM (`PNVM`) blob, loaded via a **context-info** structure (not a sequential loader), with the firmware signalling readiness via an `ALIVE` notification — contrasted with mt792x's WM-MCU patch+ram-code download.
- Learn the **MLME ↔ SME split** (the Fuchsia-style layering Phase 81's doc cites but did not implement): a thin in-driver MLME seam (send/recv mgmt frame, scan, install key) versus a separate Station-Management-Entity supplicant daemon that owns the association FSM and credentials — and why extracting the daemon is what makes a runtime `connect(ssid, psk)` possible.
- Understand why a second chipset family lights up the entire IP stack for free: the `RemoteNic` facade and the `wifi-core` FSM are bus- and chipset-agnostic, so the AX201 reuses the unchanged kernel networking path (including the Phase 96 DHCP client) the way the second/third Ethernet families did in Phase 79.

## Feature Scope

### Track A — Intel AX201 / CNVi (`iwx`) driver

A new ring-3 driver `userspace/drivers/iwx`, re-expressed from **OpenBSD `iwx(4)`** (`sys/dev/pci/if_iwx.c` / `if_iwxreg.h`, ISC/BSD-licensed — supports AX201, re-expressed in Rust, not copied), with Linux `iwlwifi` used only as a fact cross-check (GPL → register constants / firmware-section ordering only). It matches the AX201 CNVi integrated device by PCI vendor `0x8086` + the CNVi device-ID set (Tiger Lake-LP `0xA0F0`, plus the Comet/Ice/Jasper/Tiger-Lake-H CNVi IDs) disambiguated by the AX201 subsystem ID, over a bounded device-ID registry in `kernel-core/src/nic_ids.rs` mirroring the existing `is_mt792x` table. It maps BAR0, runs the APM / `prepare-card-hw` power-up + reset, parses the TLV `.ucode` + PNVM blobs, bootstraps firmware via the gen2 `iwx_ctxt_info` context-info path and waits for `ALIVE`, reads the MAC from NVM/OTP, sets up the host-command queue + RX ring, runs the soft-MAC mgmt bridge (forwarding `wifi-core`-assembled mgmt/EAPOL frames to the firmware and surfacing received mgmt frames), installs the CCMP pairwise/group keys for **firmware CCMP offload**, and registers as a `RemoteNic` on `net.nic` + the `net.nic.wireless` marker — exactly the surface the mt792x driver registers through.

### Track B — Running supplicant / connect daemon (`wifid`)

A new userspace service `userspace/wifid` that is the project's first **running** Wi-Fi supplicant (Phase 81's was a boot-time library call). It owns a `wifi_core::fsm::WifiFsm` per association and drives it through `scan → select → associate → WPA2-PSK 4-way handshake`, talking to the driver over a new **MLME seam** (`wifi.mlme`: scan-request → BSS list, send-mgmt/send-eapol, deliver-rx-mgmt/deliver-eapol, install-key, link-up/down events) and exposing a **public control IPC** (`wifi.control`) built on the existing `wifi_core::control` labels (`WIFI_SCAN_REQ` / `WIFI_SCAN_RESULT` / `WIFI_CONNECT_REQ` / `WIFI_STATUS`) — now served *live* rather than only round-tripped in host tests. `connect(ssid, psk)` derives the PMK via `crypto_lib::hash::wpa_pmk`, draws a fresh SNonce from kernel entropy (`getrandom`), starts the FSM, and dispatches the resulting `WifiAction`s (`SendMgmt`/`SendEapol`/`InstallKey`/`PurgeKeys`/`Emit`) onto the MLME seam. The daemon is the single consumer the Phase 105 network picker will drive.

### Track C — Config, persistence, and reconnect

- Boot-time `/etc/wpa.conf` autoconnect via the existing `wifi_core::config::parse_wpa_conf` (PMK derived at parse time, plaintext PSK volatile-zeroed), generalized to a small **known-networks** store (`/etc/wpa/`) so more than one network can be remembered.
- `connect(ssid, psk)` from the control IPC persists a `0600` credential (PMK + SSID, never plaintext PSK) so the network is remembered across reboots.
- **Reconnect / retry**: on a `WifiState::Failed` (deauth, handshake timeout, link-down) the daemon re-scans and re-associates with bounded backoff rather than wedging.
- Default-route preference reuses `kernel_core::nic_ids::default_route_index_by_link` (wired-over-wireless), so a present Ethernet/`ure` link still wins; `m3ctl wifi status` reads the live `WifiStatus`.

### Track D — Validation (host tests + bare-metal)

QEMU models **no** `iwlwifi`/CNVi device, so — like `wifi-smoke` (mt792x) and `ure-smoke` — there is no always-on CI arm for the radio. Validation is:

- **Host tests** for the falsifiable pure-logic halves: the `iwx` TLV-firmware parse + context-info encode + host-command/notification codec in `kernel-core/src/iwx/`, the AX201/CNVi device-ID registry in `nic_ids.rs`, and the supplicant-daemon orchestration over a mock MLME (driving `WifiFsm` through `scan → connected` and the `Failed → reconnect` path).
- A `wifi-supplicant-smoke` / `iwx-smoke` gate that **skips-with-reason** when no AX201 is present (sysfs/PCI-scanned), mirroring `tls-smoke`/`wifi-smoke`.
- A **bare-metal validation pass** following the protocol in [`docs/appendix/bare-metal-validation.md`](../appendix/bare-metal-validation.md) (the Phase 98 Track A.5 deliverable): on the Dell, firmware loads → `ALIVE` → MAC read → `RemoteNic` registered → `wifid` scans → associates → WPA2-PSK 4-way handshake completes → the in-kernel DHCP client binds a lease over Wi-Fi → ping/HTTP. Recorded as **Validated-on-HW (run N, date)**, never a bare "Complete".

## Important Components and How They Work

### `kernel-core/src/nic_ids.rs` — AX201 / CNVi device-ID registry (new)

Adds the Intel CNVi Wi-Fi family alongside the existing `MT792X_FAMILIES` table: an `AX201_CNVI_IDS` device-ID set (`0xA0F0` Tiger-Lake-LP, `0x02F0`, `0x4DF0`, `0x43F0`, `0x3DF0`, … cross-verified against the `iwx(4)`/`iwlwifi` PCI tables), the AX201 subsystem-ID discriminator, and `is_iwx`/`is_ax201` predicates with the same pairwise-disjoint + no-duplicate host tests the Intel-Ethernet and mt792x families already carry. Reuses the existing `WIFI_CLASS`/`WIFI_SUBCLASS`/`WIFI_PROG_IF` triple (class `0x02` / subclass `0x80`) — a CNVi part presents as an "Other Network Controller" like mt792x.

### `kernel-core/src/iwx/` — primitive-free `iwx` hardware logic (new)

The host-testable analog of `kernel_core::mt792x`: TLV `.ucode` section parsing (UMAC/LMAC/PNVM TLV walk), the gen2 `iwx_ctxt_info` structure encode, and the host-command / notification ring codec (command-header build, notification-ID demux, `ALIVE`/`NVM`/`MVM` notification parse). No `unsafe`, no MMIO — pure byte logic so the firmware-bootstrap and command path are unit-tested off-target, exactly as `kernel-core/src/mt792x/{firmware,mcu}.rs` are.

### `userspace/drivers/iwx/` — the radio driver (new)

Mirrors the mt792x layout (`init.rs` / `fw.rs` / `mcu.rs` / `rings.rs` / `net.rs` / `main.rs`). The control plane runs the APM power-up + reset, context-info firmware load, and `ALIVE` wait; the data plane is the gen2 TX queue + RX (RBD) ring, with the firmware doing CCMP offload (the host installs keys, never software-AES-CCMs — the same scoping choice Phase 81 made). Data frames are rewritten Ethernet⇄802.11 the way `mt792x_hal::io::eth_to_80211` does. The driver registers `net.nic` + `net.nic.wireless` and publishes RX frames + link-state to `net.nic.ingress`, so the kernel TCP/IP stack and the Phase 96 DHCP client treat it as just another NIC.

### `userspace/wifid/` — the supplicant daemon (new)

The first *running* supplicant. It is the SME (Station Management Entity); the `iwx` driver provides the MLME seam. `wifid` loads remembered networks at boot, serves `wifi.control` for runtime `scan`/`connect`/`status`, and per association instantiates `WifiFsm::new_with_snonce(pmk, ssid, sta_mac, snonce)` and pumps `WifiEvent`s from the MLME into `on_event`, dispatching `WifiAction`s back over the MLME. This is the architectural move Phase 81's "How Real OS Implementations Differ" anticipated (Fuchsia SME/MLME) but deferred — extracted here so a UI can drive it.

### Reused: `wifi-core`, `crypto-lib`, `RemoteNic`, the DHCP client

`wifi_core::fsm`/`eapol`/`kdf`/`mgmt`/`config`/`control` and the `crypto-lib` WPA2 chain (`wpa_pmk`, PBKDF2-HMAC-SHA1, AES-Key-Wrap, `mic_sha1_128`) are reused unchanged — they were written chipset-agnostic. The `RemoteNic` facade (`kernel/src/net/remote.rs`) and the Phase 96 in-kernel DHCP client (`kernel/src/net/dhcp.rs`, gated on `RemoteNic::is_registered()`) need no changes: once `iwx` registers and keys are installed, DHCP DISCOVER/OFFER/REQUEST/ACK runs over Wi-Fi automatically.

## How This Builds on Earlier Phases

- **Extends Phase 81** by adding a *second* Wi-Fi chipset family (Intel AX201/CNVi alongside MediaTek mt792x) in the same `nic_ids` registry pattern, and by **extracting** the folded-in boot-time supplicant into a standalone running daemon with a live control surface.
- **Reuses Phase 79's `RemoteNic` facade** unchanged — the AX201 is the first **Intel** and first **CNVi** entry on the bus-agnostic NIC seam; no kernel network-layer code changes.
- **Reuses the Phase 96 in-kernel DHCP client** (`kernel/src/net/dhcp.rs`) and the Phase 96 bare-metal bring-up workflow (USB-log persistence, SOL/network-sink capture) — 96 proved the workflow on this exact Dell; 104 reuses it for a non-passthrough (cold-owned) PCI/CNVi device.
- **Reuses the Phase 81 firmware-staging path** (`xtask` + `populate_ext2_files` + `kernel/initrd/lib/firmware/`) for the Intel `iwlwifi` AX201 `.ucode` + PNVM blobs, and the `docs/legal/firmware-licenses.md` redistribution-record convention.
- **Feeds Phase 105** — the `wifi.control` IPC (`scan`/`connect`/`status`) is the surface the Phase 105 settings/network-picker UI consumes; this phase documents it as a stable contract.

## Implementation Outline

1. **Track A** — add the AX201/CNVi ID set + `is_iwx`/`is_ax201` (host tests) to `kernel-core/src/nic_ids.rs`; scaffold `kernel-core/src/iwx/` (TLV-firmware parse, context-info encode, host-command/notification codec — host-tested); scaffold the `iwx` driver crate (four-place new-binary wiring + `services.d/iwx.conf`); implement BAR0/APM init + reset, context-info firmware load + `ALIVE`, NVM/MAC read + RF-kill, host-command + RX rings, the soft-MAC mgmt bridge + CCMP key install, and `RemoteNic` registration.
2. **Track B** — define the `wifi.mlme` seam (driver responder + the `wifi-core` MLME label/codec); scaffold `userspace/wifid` (four-place wiring + `services.d/wifid.conf`); implement the FSM driver loop (events ← MLME, actions → MLME) and the live `wifi.control` surface (`scan`/`connect(ssid,psk)`/`status`) reusing `wifi_core::control`.
3. **Track C** — boot-time `/etc/wpa.conf` autoconnect + the `/etc/wpa/` known-networks store; `connect` persistence (`0600`, PMK-only); reconnect/backoff on `Failed`; wire `m3ctl wifi status` to the live `WifiStatus`.
4. **Track D** — host tests (iwx codec + supplicant orchestration over a mock MLME); the `wifi-supplicant-smoke`/`iwx-smoke` gate (skip-with-reason); the bare-metal validation pass per `docs/appendix/bare-metal-validation.md`; the `M3OS_AX201_REGRESSION` AGENTS row + README; the `wifi.control` contract doc for Phase 105.

## Acceptance Criteria

- **Host tests pass** in `kernel-core` and `wifi-core`: the AX201/CNVi `is_iwx`/`is_ax201` registry is pairwise-disjoint from the Intel-Ethernet and mt792x families and rejects a foreign Intel Ethernet ID (e.g. `0x100E`); the `iwx` TLV-firmware parse extracts the UMAC/LMAC/PNVM sections from a representative TLV fixture; the host-command/notification codec round-trips an `ALIVE` notification; and the supplicant-daemon orchestration drives a mock MLME from `scan → WifiState::Connected` and exercises the `Failed → reconnect` path.
- **Bare-metal (Dell AX201), per `docs/appendix/bare-metal-validation.md`** — captured over SOL (pre-network) then the network sink (post-network), recorded as **Validated-on-HW (run N, date)**:
  - the `iwx` driver matches the AX201 by device + subsystem ID, loads the firmware, the firmware signals `ALIVE`, the MAC is read from NVM (non-zero, non-broadcast), and the driver registers a `RemoteNic` (kernel logs `[remote_nic] … registered ring-3 NIC driver … mac=…` + the `net.nic.wireless` marker);
  - `wifid` scans and lists at least the target BSS, `connect(ssid, psk)` (or boot-time `/etc/wpa.conf`) associates and **completes the WPA2-PSK 4-way handshake** (`WifiState::Connected`, keys installed via firmware CCMP offload);
  - the **in-kernel DHCP client binds a lease over Wi-Fi** (`[dhcp] bound ip=… gw=…`), and an outbound `ping`/HTTP GET over the AX201 succeeds;
  - `m3ctl wifi status` reports the associated SSID, RSSI, and the DHCP-assigned IPv4.
- **Reconnect:** after an induced deauth / link-down the daemon re-associates without operator intervention (bounded backoff), proven on HW and in the host orchestration test.
- **`wifi-supplicant-smoke`/`iwx-smoke` SKIPS-with-reason** when no AX201 is present (CI has no CNVi device); the `M3OS_AX201_REGRESSION` gate and its skip-vs-pass semantics are documented in `AGENTS.md`, and the Phase 104 README row + mermaid node (depending on Phase 79 + Phase 81) are added.
- **The `wifi.control` IPC contract is documented** (labels, `scan`/`connect(ssid,psk)`/`status` semantics, error cases) for the Phase 105 network picker.
- **No regression** in existing NICs: the mt792x, e1000-family, r8169/r8125, and `ure` drivers still come up, and the wired-over-wireless default-route preference (`default_route_index_by_link`) still picks a link-up wired NIC over the AX201.
- **The Intel `iwlwifi` AX201 firmware/PNVM redistribution license is reviewed and recorded** under `docs/legal/firmware-licenses.md` before merge (mirroring the Phase 81 MediaTek blob record).

## Companion Task List

- [Phase 104 Task List](./tasks/104-wifi-ax201-supplicant-tasks.md)

## How Real OS Implementations Differ

- **Linux `iwlwifi`** is tens of thousands of lines spanning the `dvm`/`mvm`/`fw`/`pcie` opmodes, the full 22000/Qu/So/Bz device matrix, debug-fw, runtime PM, RSS, TX aggregation/BA sessions, and regulatory-domain handling, riding `mac80211`+`cfg80211`; `iwx` here targets the bring-up subset for one device (AX201/Qu) — closer to OpenBSD `iwx(4)` (the chosen reference).
- **AX210 / So-family and the gen3 context-info path (`ctxt_info_gen3`) + mandatory PNVM-for-newer-firmware** are deferred; this phase implements only the gen2 `iwx_ctxt_info` path the Qu/QuZ AX201 uses.
- Production stacks run a full **`wpa_supplicant`/`iwd`** managing EAP/802.1X, WPA3-SAE/OWE, PMKSA caching, roaming/BSS-transition, P2P, and concurrent interfaces; `wifid` is a minimal WPA2-PSK SME with a `scan`/`connect`/`status` surface — WPA3-SAE, EAP, and roaming are explicitly deferred.
- Real soft-MAC drivers stream RX with many in-flight RBDs and zero-copy DMA and offload aggregation/rate-control to firmware; `iwx` uses a correctness-first submit/complete ring loop sized for bring-up, like the mt792x reference.
- Bring-up on real silicon normally uses a hardware sniffer, the vendor's firmware-debug UART, and JTAG; m3OS substitutes the Phase 96 bare-metal workflow (cold-owned PCI device on the Dell, AMT Serial-over-LAN pre-network, network log sink post-network) because that is what the reference hardware exposes — and because **QEMU models no `iwlwifi`/CNVi device** there is no emulator iteration path at all.

## Deferred Until Later

- **WPA3-SAE / 802.1X-EAP / OWE / PMKSA caching** — WPA2-PSK is the AX201 milestone; SAE rides a later supplicant phase.
- **Roaming / BSS transition / band steering / power-save (PS-Poll, U-APSD)** — backlog.
- **AX210 / So-family devices + the gen3 context-info + PNVM-mandatory path**, 6 GHz / MLO / Wi-Fi 7 features, and the MT7925 connac3 bring-up (the latter is Phase 108's AMD-laptop work).
- **TX aggregation / Block-Ack sessions, checksum/TSO offloads, RSS, and runtime power management** — deferred throughput/power work.
- **A regulatory database** — the firmware's own regulatory enforcement is trusted, as in Phase 81; the channel set is constrained to the configured band.
- **The GUI network picker** that consumes `wifi.control` — that is **Phase 105** (Native GUI Toolkit & Core Desktop Apps); this phase ships only the daemon + its documented control IPC.
