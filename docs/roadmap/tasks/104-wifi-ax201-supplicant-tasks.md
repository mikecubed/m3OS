# Phase 104 — Wi-Fi: Intel AX201 / CNVi (`iwx`) + Supplicant: Task List

**Status:** Planned
**Source Ref:** phase-104
**Depends on:** Phase 81 (Wi-Fi reference — mt792x + `wifi-core` 802.11 mgmt FSM + WPA2-PSK 4-way handshake) ✅, Phase 79 (Networking — `RemoteNic` facade + IPv4/TCP/UDP) ✅
**Goal:** Bring up the Dell Tiger Lake laptop's only built-in NIC — the Intel **AX201 CNVi** Wi-Fi part — with a new `iwx`-style ring-3 driver that loads firmware, brings the radio up, and registers as a `RemoteNic`, plus a new **running supplicant/connect daemon** (`wifid`) that drives `scan → select → associate → WPA2-PSK 4-way handshake` over the existing `wifi-core`/`crypto-lib` host crypto and exposes a `scan`/`connect(ssid, psk)`/`status` control IPC, so the in-kernel DHCP client binds a lease over Wi-Fi. The GUI network picker that consumes the control IPC is Phase 105. QEMU models no `iwlwifi`/CNVi device, so the radio is validated on bare metal per [`docs/appendix/bare-metal-validation.md`](../../appendix/bare-metal-validation.md) (Phase 98 Track A.5) with the **Validated-on-HW (run N, date)** convention; pure-logic halves are host-tested.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Intel AX201/CNVi (`iwx`) driver: device-ID registry, `kernel-core/src/iwx` codec, BAR0/APM init + reset, TLV firmware + context-info `ALIVE`, NVM/MAC + RF-kill, host-command + RX rings, soft-MAC mgmt bridge + CCMP key install, `RemoteNic` registration | — | ⬜ Planned |
| B | Running supplicant/connect daemon (`wifid`): the `wifi.mlme` driver seam, the FSM driver loop, the live `wifi.control` (`scan`/`connect`/`status`) surface | A | ⬜ Planned |
| C | Config, persistence, reconnect: `/etc/wpa.conf` + `/etc/wpa/` store, `connect` persistence (0600, PMK-only), reconnect/backoff, wired-over-wireless default route + `m3ctl wifi status` | A, B | ⬜ Planned |
| D | Validation: host tests (iwx codec + supplicant orchestration), `iwx-smoke` gate (skip-with-reason), bare-metal pass, `wifi.control` contract doc | A, B, C | ⬜ Planned |

---

## Track A — Intel AX201 / CNVi (`iwx`) Driver

> Primary reference: **OpenBSD `iwx(4)`** (`sys/dev/pci/if_iwx.c`, `if_iwxreg.h`, ISC/BSD — re-expressed in Rust; supports AX201). Linux `iwlwifi` used only as a fact cross-check (GPL → register constants / firmware-section ordering only). Mirrors the Phase 81 mt792x layout (`kernel_core::mt792x` + `userspace/drivers/mt792x`).

### A.1 — AX201 / CNVi device-ID registry + `is_iwx` / `is_ax201`

**File:** `kernel-core/src/nic_ids.rs`
**Symbol:** `AX201_CNVI_IDS`, `IWX_AX201_SUBSYS`, `is_iwx`, `is_ax201` (new — alongside the existing `MT792X_FAMILIES` / `is_mt792x` table)
**Why it matters:** The driver must match the AX201 CNVi integrated device by Intel vendor `0x8086` + the CNVi device-ID set (disambiguated by the AX201 subsystem ID) without colliding with the existing Intel **Ethernet** families (e1000/e1000e/igb/igc all share vendor `0x8086`).

**Acceptance:**
- [ ] `AX201_CNVI_IDS` contains the cross-verified CNVi device IDs (`0xA0F0` Tiger-Lake-LP, `0x02F0`, `0x4DF0`, `0x43F0`, `0x3DF0`, …) and `is_ax201` additionally checks `IWX_AX201_SUBSYS`.
- [ ] `is_iwx` reuses the existing `WIFI_CLASS`/`WIFI_SUBCLASS`/`WIFI_PROG_IF` (`0x02`/`0x80`/`0x00`) triple.
- [ ] Host tests assert the CNVi set is pairwise-disjoint from `E1000_IDS`/`E1000E_IDS`/`IGB_IDS`/`IGC_IDS` and from the mt792x sets, has no duplicates, and that `is_iwx(0x100E)` (classic e1000) and `is_mt792x(0xA0F0)` are both `false`.

### A.2 — `iwx` crate scaffold + four-place new-binary wiring

**Files:**
- `userspace/drivers/iwx/Cargo.toml`, `userspace/drivers/iwx/src/main.rs`
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`bins` array in `build_userspace`)
- `kernel/src/fs/ramdisk.rs` (`include_bytes!` + `BIN_ENTRIES`)
- `xtask/src/main.rs` `populate_ext2_files` + `userspace/init/src/main.rs` `KNOWN_CONFIGS` (a `services.d/iwx.conf`)

**Symbol:** `program_main` (driver entry), `iwx.conf`
**Why it matters:** Missing any of the four wiring points means the driver is not built, not embedded, or not found at runtime (per the "Adding a New Userspace Binary" rule). `needs_alloc = true` (uses `kernel-core`/`Vec`); mirrors `userspace/drivers/mt792x`.

**Acceptance:**
- [ ] `cargo xtask check` builds `iwx`; it is embedded in the ramdisk and launched from `services.d/iwx.conf`.
- [ ] Defines a `#[global_allocator]` (`syscall_lib::heap::BrkAllocator`) + `alloc_error_handler` + `panic_handler`, and enables the `alloc` feature on `syscall-lib`.
- [ ] With no AX201 present the driver enumerates, finds no match via `is_ax201`, and exits cleanly (exit code 0) — no panic on machines without the chip (mirrors mt792x `no mt792x device present — exiting cleanly`).

### A.3 — BAR0 MMIO + APM power-up + card-reset

**File:** `userspace/drivers/iwx/src/init.rs`
**Symbol:** `iwx_prepare_card_hw`, `iwx_apm_init`, `iwx_reset` (the CSR/`HBUS_TARG` register sequence), `Iwx::bring_up`
**Why it matters:** The CNVi MAC's CSR window in BAR0 must be powered up (APM) and reset before any firmware load; without the documented power-up sequence the device CSRs read back garbage and the context-info load never starts. Mirrors `Mt792x::bring_up`.

**Acceptance:**
- [ ] Claims the device, maps BAR0, and runs the `prepare-card-hw` → APM-init → reset sequence; a known CSR (e.g. `CSR_HW_REV` / `CSR_HW_RF_ID`) reads back a plausible AX201 revision and is logged.
- [ ] A reset-timeout returns a distinct error (logged sentinel) rather than hanging, mirroring mt792x `BringUpError::ResetTimeout`.

### A.4 — TLV firmware parse + context-info bootstrap + `ALIVE`

**Files:**
- `kernel-core/src/iwx/firmware.rs` (new — host-testable TLV parse + context-info encode)
- `userspace/drivers/iwx/src/fw.rs` (the hardware loader driving the parse output)
- `xtask/src/main.rs` + `userspace/init` (firmware staging under `kernel/initrd/lib/firmware/`)

**Symbol:** `parse_ucode_tlv`, `UcodeSections` (UMAC/LMAC/PNVM), `IwxCtxtInfo`, `wait_alive`, `firmware_blob`
**Why it matters:** A gen2 `iwx`/`iwlwifi` device boots only via a context-info structure pointing at firmware sections DMA'd from the TLV `.ucode` (+ the PNVM blob); the firmware signals readiness with an `ALIVE` notification. This is the AX201 analog of mt792x's WM-MCU patch+ram-code download.

**Acceptance:**
- [ ] `parse_ucode_tlv` extracts the UMAC/LMAC firmware sections + the PNVM TLV from a representative TLV fixture (host test) and rejects a truncated/garbage TLV without panicking.
- [ ] `IwxCtxtInfo` encodes to the documented gen2 context-info layout (host test asserts field offsets/sizes).
- [ ] On HW: the firmware loads via the context-info path and the driver observes the `ALIVE` notification (logged sentinel); a missing/short firmware blob emits a degraded sentinel and the driver exits cleanly (mirrors mt792x `FW_ABSENT_SENTINEL`).

### A.5 — Host-command queue + RX (RBD) ring + notification demux

**Files:**
- `kernel-core/src/iwx/mcu.rs` (new — host-testable command-header build + notification-ID demux)
- `userspace/drivers/iwx/src/rings.rs` (the gen2 TX/host-command queue + RX ring)

**Symbol:** `HostCmd`, `build_host_cmd`, `parse_notif`, `NotifId`, `RxRing`, `CmdQueue`
**Why it matters:** All chip control after `ALIVE` (NVM read, PHY config, station add, key install, scan) rides the host-command queue, and all events (`ALIVE`, scan results, EAPOL/mgmt RX, link state) arrive on the RX ring; without the codec the soft-MAC bridge has no transport.

**Acceptance:**
- [ ] `build_host_cmd` produces the documented command header (group/id/flags/length) and `parse_notif` demuxes a representative notification by ID — host tests round-trip both.
- [ ] On HW: a host command (e.g. NVM-access) completes and its response notification is parsed; the RX ring delivers received frames without dropping on wrap.

### A.6 — NVM/OTP MAC read + RF-kill state

**File:** `userspace/drivers/iwx/src/init.rs` (NVM section)
**Symbol:** `iwx_nvm_read`, `mac()`, `rfkill_state`
**Why it matters:** The MAC address (from NVM/OTP) is required for `RemoteNic` registration, ARP, and as the 802.11 source address; RF-kill must be read so the driver does not attempt to associate with the radio hardware-killed.

**Acceptance:**
- [ ] Reads a plausible (non-zero, non-broadcast) MAC from NVM and logs it.
- [ ] Reads the RF-kill state; if hardware-killed the driver registers as a passive L2 NIC and logs the kill rather than spinning on association.

### A.7 — Soft-MAC mgmt bridge + CCMP key install (firmware offload)

**File:** `userspace/drivers/iwx/src/net.rs`
**Symbol:** `eth_to_80211` / `classify_and_rewrite_rx` (reused pattern), `send_mgmt`, `deliver_rx`, `install_key`
**Why it matters:** `wifi-core` produces 802.11 mgmt/EAPOL frames in software; the driver must submit them to the firmware and surface received mgmt/EAPOL frames upward, and must install the CCMP pairwise/group keys for **firmware CCMP offload** (m3OS requires HW CCMP — no software AES-CCM, the Phase 81 scoping choice). Data frames are rewritten Ethernet⇄802.11 as in `mt792x_hal::io`.

**Acceptance:**
- [ ] A `wifi-core`-assembled mgmt/EAPOL frame is submitted to the firmware via a host command, and a received mgmt/EAPOL frame is surfaced as a `WifiEvent`-shaped message to the MLME seam (Track B).
- [ ] `install_key` posts the CCMP key to the firmware (the AX201 key-offload command); after key install, encrypted data frames pass (proven by the DHCP exchange in D.3).
- [ ] Data-plane Ethernet⇄802.11 rewrite reuses the `eth_to_80211`/`classify_and_rewrite_rx` host-tested helpers.

### A.8 — `RemoteNic` registration + service lifecycle

**File:** `userspace/drivers/iwx/src/main.rs`
**Symbol:** `ipc_register_service` for `net.nic` + `net.nic.wireless`, `INGRESS_SERVICE_NAME` lookup, the io loop
**Why it matters:** Registration on the same `net.nic.ingress` surface the mt792x/e1000 drivers use is what makes the AX201 a first-class interface with no network-layer changes, and the `net.nic.wireless` marker is what lets the kernel prefer a wired link over it.

**Acceptance:**
- [ ] `iwx` registers `net.nic` + `net.nic.wireless` and resolves `net.nic.ingress`; the kernel logs `[remote_nic] … registered ring-3 NIC driver … mac=…`.
- [ ] RX frames are published to `net.nic.ingress` and link-state (up/down + band) is reported; on driver exit / device loss the service registration + capabilities are released cleanly.

---

## Track B — Running Supplicant / Connect Daemon (`wifid`)

> Phase 81 folded the `wifi-core` FSM inline into the mt792x driver as a boot-time `/etc/wpa.conf` read (`load_supplicant` in `userspace/drivers/mt792x/src/main.rs`). Track B extracts a **running** SME daemon with a live control surface — the Fuchsia-style SME/MLME split Phase 81 cited but deferred.

### B.1 — `wifid` crate scaffold + four-place wiring

**Files:**
- `userspace/wifid/Cargo.toml`, `userspace/wifid/src/main.rs`
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`bins` array) + `kernel/src/fs/ramdisk.rs` (`include_bytes!` + `BIN_ENTRIES`)
- `xtask/src/main.rs` `populate_ext2_files` + `userspace/init` `KNOWN_CONFIGS` (a `services.d/wifid.conf`)

**Symbol:** `program_main` (daemon entry), `wifid.conf`
**Why it matters:** `wifid` is a new userspace service; missing any wiring point means it is not built, embedded, or started. `needs_alloc = true` (links `wifi-core`/`crypto-lib`).

**Acceptance:**
- [ ] `cargo xtask check` builds `wifid`; it is embedded and started from `services.d/wifid.conf`; depends on `wifi-core` + `crypto-lib`.
- [ ] With no Wi-Fi NIC registered, `wifid` starts, serves `wifi.control` (returning "no interface"), and does not busy-spin or exit.

### B.2 — `wifi.mlme` driver seam (MLME protocol + driver responder)

**Files:**
- `userspace/wifi-core/src/control.rs` (new MLME label constants + encode/decode, alongside the existing `WIFI_*` control labels)
- `userspace/drivers/iwx/src/net.rs` (the driver-side `wifi.mlme` responder)

**Symbol:** `WIFI_MLME_SCAN`, `WIFI_MLME_TX_MGMT`, `WIFI_MLME_RX_MGMT`, `WIFI_MLME_INSTALL_KEY`, `WIFI_MLME_LINK` (new), and the driver responder
**Why it matters:** The daemon (SME) and the driver (MLME) need a defined seam: scan-request → BSS list, send mgmt/EAPOL, deliver received mgmt/EAPOL, install key, and link up/down events. Keeping it in `wifi-core` makes the codec host-testable and chipset-agnostic (a future mt792x rework can adopt the same seam).

**Acceptance:**
- [ ] The MLME label constants are distinct from the existing `WIFI_SCAN_REQ`/`WIFI_SCAN_RESULT`/`WIFI_CONNECT_REQ`/`WIFI_STATUS` labels (host test, mirroring `labels_distinct`).
- [ ] Each MLME message type round-trips through encode/decode (host tests), including a BSS-list scan reply and an RX-mgmt frame carrying an EAPOL payload.
- [ ] The `iwx` driver registers a `wifi.mlme` service and answers a scan request with a `BssInfo` list built from received probe-responses.

### B.3 — Supplicant FSM driver loop

**File:** `userspace/wifid/src/main.rs` (or `userspace/wifid/src/sme.rs`)
**Symbol:** the FSM driver loop over `wifi_core::fsm::WifiFsm` (`new_with_snonce`, `on_event`, `WifiAction` dispatch)
**Why it matters:** This is the running supplicant Phase 81 lacked: it pumps `WifiEvent`s (`ProbeResp`/`AuthResp`/`AssocResp`/`Eapol`/`Timeout`/`Deauth`) from the MLME seam into `on_event` and dispatches the returned `WifiAction`s (`SendMgmt`/`SendEapol`/`InstallKey`/`PurgeKeys`/`Emit`) back over `wifi.mlme`, driving `Init → Scanning → Authenticating → Associating → Handshake → Connected`.

**Acceptance:**
- [ ] Given a target SSID + PMK, the loop drives the FSM from `Scanning` to `WifiState::Connected` against the MLME seam, emitting `SendMgmt(auth)` → `SendMgmt(assoc)` → `SendEapol(M2)` → `SendEapol(M4)` + `InstallKey` in order.
- [ ] An `InstallKey` action is forwarded as a `WIFI_MLME_INSTALL_KEY` to the driver; a `PurgeKeys` (deauth) is forwarded as a key purge.
- [ ] A handshake `Timeout` transitions to `Failed(HandshakeTimeout)` and triggers the Track C reconnect path (does not wedge the daemon).

### B.4 — Live `wifi.control` surface (`scan` / `connect` / `status`)

**Files:**
- `userspace/wifid/src/main.rs` (the control responder)
- `userspace/wifi-core/src/control.rs` (reused `ScanResult` / `WifiStatus` / labels)

**Symbol:** `WIFI_SCAN_REQ` → `WIFI_SCAN_RESULT`, `WIFI_CONNECT_REQ`, `WIFI_STATUS`, `crypto_lib::hash::wpa_pmk`
**Why it matters:** This is the surface the Phase 105 network picker consumes. Phase 81's `control.rs` defined the labels + codecs but only round-tripped them in host tests; `wifid` serves them **live** — `scan` returns the BSS list, `connect(ssid, psk)` derives the PMK and starts an association, and `status` reports the live connection state.

**Acceptance:**
- [ ] `scan` returns a list of `ScanResult` entries (BSSID/SSID/RSSI/channel) from the MLME scan.
- [ ] `connect(ssid, psk)` validates the PSK (8..=63 printable ASCII via the `wifi_core::config` rules), derives the PMK with `crypto_lib::hash::wpa_pmk`, draws a fresh SNonce via `getrandom`, and starts the FSM; the plaintext PSK is volatile-zeroed after PMK derivation.
- [ ] `status` returns a `WifiStatus` (SSID/RSSI/IPv4) that round-trips on the wire; not-associated reports an empty SSID + `0.0.0.0`.

---

## Track C — Config, Persistence, and Reconnect

### C.1 — Boot-time `/etc/wpa.conf` autoconnect + `/etc/wpa/` known-networks store

**Files:**
- `userspace/wifid/src/main.rs` (boot autoconnect)
- `userspace/wifi-core/src/config.rs` (reused `parse_wpa_conf`, `WpaConfig`)

**Symbol:** `parse_wpa_conf`, `WpaConfig::{ssid,pmk,freq}`, the `/etc/wpa/` directory walk
**Why it matters:** The daemon must autoconnect to a configured network at boot (preserving Phase 81's `/etc/wpa.conf` behavior, now in `wifid` rather than the driver) and remember more than one network.

**Acceptance:**
- [ ] At boot `wifid` reads `/etc/wpa.conf` (if present) and any `/etc/wpa/*.conf` entries, derives PMKs at parse time, and attempts to associate with the first reachable known network.
- [ ] The plaintext passphrase is volatile-zeroed after PMK derivation (reusing the `config.rs` `zero_secret` volatile-wipe pattern); `WpaConfig` exposes no passphrase getter.

### C.2 — `connect` persistence (0600, PMK-only)

**File:** `userspace/wifid/src/main.rs` (credential store writer)
**Symbol:** the `/etc/wpa/<ssid>.conf` writer
**Why it matters:** A network joined at runtime via `connect(ssid, psk)` should be remembered across reboots without persisting the plaintext PSK.

**Acceptance:**
- [ ] A successful `connect(ssid, psk)` writes a `0600` credential under `/etc/wpa/` containing the SSID + derived PMK (never the plaintext PSK).
- [ ] After reboot the persisted network autoconnects via C.1; the credential file mode is asserted `-rw-------`.

### C.3 — Reconnect / backoff on `Failed`

**File:** `userspace/wifid/src/main.rs` (reconnect supervisor)
**Symbol:** the reconnect loop handling `WifiState::Failed(FailReason::{Deauthed,HandshakeTimeout,…})` + a `RemoteNic` link-down event
**Why it matters:** A real supplicant survives deauths, roaming away, and link-down without operator intervention; without this the daemon wedges in `Failed` after the first disconnect.

**Acceptance:**
- [ ] On `Failed(Deauthed)` / `Failed(HandshakeTimeout)` / link-down, the daemon re-scans and re-associates with bounded exponential backoff (capped), not a tight loop.
- [ ] A host orchestration test (Track D.1) drives `Connected → Deauth → reconnect → Connected` over a mock MLME.

### C.4 — Wired-over-wireless default route + `m3ctl wifi status`

**Files:**
- `kernel-core/src/nic_ids.rs` (reused `default_route_index_by_link`)
- `userspace/m3ctl` (the `wifi status` subcommand reading the live `WifiStatus`)

**Symbol:** `default_route_index_by_link`, `NicRoute`, the `m3ctl wifi status` reader
**Why it matters:** When a wired/`ure` link is up it must win the default route over the AX201 (the Phase 81 policy), and the operator needs a read-only status view.

**Acceptance:**
- [ ] With both a wired link-up NIC and the AX201 associated, the default route resolves to the wired NIC (`default_route_index_by_link` returns the wired index); when only the AX201 is up it is selected.
- [ ] `m3ctl wifi status` reports the associated SSID, RSSI, and DHCP-assigned IPv4 from the live `WifiStatus` (renders "not associated" on an empty SSID).

---

## Track D — Validation

### D.1 — Host tests: `iwx` codec + supplicant orchestration

**Files:**
- `kernel-core/src/iwx/{firmware,mcu}.rs` + `kernel-core/src/nic_ids.rs`
- `userspace/wifi-core/src/control.rs` (MLME codec) + `userspace/wifid` (orchestration test)

**Symbol:** `parse_ucode_tlv`, `build_host_cmd`/`parse_notif`, `is_iwx`/`is_ax201`, the mock-MLME supplicant driver
**Why it matters:** These are the falsifiable pure-logic halves that *can* be CI-tested even though the radio cannot; they guard the codec + state machine against regressions the bare-metal pass cannot catch deterministically.

**Acceptance:**
- [ ] `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` covers the `iwx` TLV parse, the context-info encode, the host-command/notification codec, and the AX201/CNVi registry disjointness.
- [ ] A `wifid` (or `wifi-core`) host test drives the supplicant over a mock MLME from `scan → Connected` and through `Connected → Deauth → reconnect → Connected`.
- [ ] `cargo xtask check` runs the new `kernel-core` `iwx` + `wifi-core` MLME tests.

### D.2 — `iwx-smoke` / `wifi-supplicant-smoke` gate + `AGENTS.md` + README

**Files:**
- `xtask/src/main.rs` (`cmd_iwx_smoke` / `cmd_wifi_supplicant_smoke`, new) + `M3OS_AX201_REGRESSION`
- `AGENTS.md` (pre-push opt-in gate table)
- `docs/roadmap/README.md` (Phase 104 row + mermaid node)

**Symbol:** the gate function, the `M3OS_AX201_REGRESSION` row, the Phase 104 summary row
**Why it matters:** Keeps the gate discoverable with honest skip-vs-pass semantics (QEMU has no `iwlwifi`/CNVi model — CI never has the device), mirroring `wifi-smoke`/`ure-smoke`.

**Acceptance:**
- [ ] The gate SKIPS-with-reason when no AX201 is present (PCI/sysfs-scanned), mirroring `tls-smoke`/`wifi-smoke`; when the device is present it asserts the bring-up chain (firmware → `ALIVE` → MAC → `RemoteNic` registration) + a `wifid` scan/associate.
- [ ] The `M3OS_AX201_REGRESSION=1` row is added to the `AGENTS.md` gate table with the same skip-vs-pass wording as the Wi-Fi/`ure` rows.
- [ ] `docs/roadmap/README.md` has the Phase 104 row and a mermaid node depending on Phase 79 + Phase 81 (`P79 --> P104`, `P81 --> P104`).

### D.3 — Bare-metal validation pass (Dell AX201)

**Files:**
- `docs/appendix/bare-metal-validation.md` (the Phase 98 Track A.5 protocol — followed here; results recorded)
- `scripts/` (an `iwx`/AX201 bring-up runbook, mirroring `scripts/ure-vfio-validate.md` / `scripts/mt792x-vfio-validate.md`)

**Symbol:** the recorded bare-metal run; the **Validated-on-HW (run N, date)** status entry
**Why it matters:** The phase's headline claim is real-hardware Wi-Fi on the laptop's only built-in NIC; this is the only place the radio datapath is exercised (no emulator path exists). Per the Phase 98 convention, HW-only phases land as Validated-on-HW with a recorded run, not a bare "Complete".

**Acceptance:**
- [ ] On the Dell, captured per `docs/appendix/bare-metal-validation.md` (SOL pre-network, network sink post-network): firmware loads → `ALIVE` → MAC read → `RemoteNic` registered (`[remote_nic] … mac=…` + `net.nic.wireless`).
- [ ] `wifid` scans (target BSS listed), associates, and **completes the WPA2-PSK 4-way handshake** (`WifiState::Connected`, keys installed via firmware CCMP offload).
- [ ] The in-kernel DHCP client binds a lease over Wi-Fi (`[dhcp] bound ip=… gw=…`) and an outbound `ping`/HTTP GET over the AX201 succeeds; `m3ctl wifi status` shows the SSID/RSSI/IPv4.
- [ ] An induced deauth triggers automatic reconnect (Track C.3) on HW.
- [ ] The run is recorded as **Validated-on-HW (run N, date)** in the design-doc Status and the runbook results appendix; the Intel `iwlwifi`/PNVM redistribution license is recorded under `docs/legal/firmware-licenses.md`.

### D.4 — `wifi.control` contract documentation (for Phase 105)

**File:** `docs/roadmap/104-wifi-ax201-supplicant.md` (a "Control IPC contract" appendix) and/or `docs/appendix/`
**Symbol:** the `wifi.control` label/semantics table (`WIFI_SCAN_REQ`/`WIFI_SCAN_RESULT`/`WIFI_CONNECT_REQ`/`WIFI_STATUS`)
**Why it matters:** Phase 105's network picker is the consumer; the control surface must be documented as a stable contract (labels, request/reply shapes, error cases) before that UI is built.

**Acceptance:**
- [ ] The `scan`/`connect(ssid,psk)`/`status` request/reply wire shapes + error cases (`NotAssociated`, `BadRequest`) are documented with the `wifi_core::control` label values.
- [ ] The doc states the security contract (PSK never persisted in plaintext; PMK-only credentials at `0600`) and is linked from the Phase 105 design doc when it lands.

---

## Documentation Notes

- The `iwx` driver is the project's **second** Wi-Fi chipset family (after Phase 81 mt792x) and the first **Intel/CNVi** entry on the `RemoteNic` facade — record that the facade + `wifi-core` FSM proved chipset-agnostic, reused unchanged.
- `wifid` is the first **running** Wi-Fi supplicant; Phase 81's supplicant was the `wifi-core` FSM folded inline into the mt792x driver with a boot-time-only `/etc/wpa.conf` read (`load_supplicant`). Note the extraction (Fuchsia-style SME/MLME split) when this lands, and that the mt792x driver could later adopt the same `wifi.mlme` seam.
- `iwx` is re-expressed from ISC/BSD-licensed OpenBSD `iwx(4)`; keep the license-provenance note in the crate header (BSD source re-expressed; Linux `iwlwifi` facts-only), matching the mt792x `mt76`-citation convention.
- The `wifi.control` IPC is consumed by the **Phase 105** network picker — keep the labels stable and the contract doc current.
- This is a **HW-only** phase (no QEMU `iwlwifi`/CNVi model): use **Validated-on-HW (run N, date)**, not "Complete", per `docs/appendix/bare-metal-validation.md`.
- Prefer exact files/symbols over directories as these land; update the checkboxes and the Track Layout status column as tracks complete.
