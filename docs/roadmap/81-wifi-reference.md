# Phase 81 - Wi-Fi Reference Driver (MediaTek mt792x family)

**Status:** Driver-side complete; radio validation hardware-only (no QEMU mt76 model)
**Source Ref:** phase-81
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 67 (IOMMU Substrate) ✅, Phase 77 (Pre-1.0 Correctness) ✅, Phase 79 (Modern NIC — establishes the multi-NIC routing path) ✅
**Builds on:** Adds the project's first Wi-Fi driver — the MediaTek **mt792x family** (MT7921 `0x14C3:0x7961` / MT7922 `0x14C3:0x0616`, connac2, brought up first; MT7925/connac3 in the same `nic_ids` registry), matched over a bounded device-ID registry (Phase-79 style) because this build host is **not** the dev laptop and the laptop's exact chip is unconfirmed. This is a stub of a real Wi-Fi stack: one chipset family, one band, one auth method, documented as such
**Primary Components:** `userspace/drivers/mt792x/` (new — the ring-3 driver), `userspace/wifi-core/` (new — the 802.11 mgmt plane + WPA2-PSK supplicant lib, linked into the driver; depends on `crypto-lib`), the top-level **primitive-free** `kernel_core::mt792x` hardware module (regs/firmware/mcu/dma), `userspace/crypto-lib/` (the added SHA-1/HMAC-SHA1/PBKDF2/AES-Key-Wrap), `kernel/initrd/lib/firmware/mt7961/` (operator-staged vendor firmware blob)

> **Reconciled on landing (Task F.4 applied).** This design doc was written before implementation; the corrections below have now been applied to the header above and the sections that follow (the companion task list is [tasks/81-wifi-reference-tasks.md](./tasks/81-wifi-reference-tasks.md)): (1) the bring-up target is the MediaTek **mt792x family** (MT7921/MT7922 connac2 first; MT7925/connac3 in the same device-ID registry) matched over a bounded registry — Phase-79 style — rather than a single hardcoded `0x14C3:0x7925`, because this build host is **not** the user's laptop and the laptop's exact chip is unconfirmed; (2) the 802.11 mgmt plane + WPA2-PSK supplicant live in a **userspace `wifi-core` lib + `crypto-lib`** (not `kernel-core/src/net/wifi/`, which would compile policy into ring 0; `kernel-core` also cannot depend on `crypto-lib`), with only the primitive-free hardware logic in a top-level `kernel_core::mt792x`; (3) SHA-1/HMAC-SHA1/PBKDF2/AES-Key-Wrap are **absent** from the workspace and are added by Phase 81, not inherited from Phase 42; (4) Redox has no Wi-Fi stack and is not a valid reference (cite Fuchsia SME/MLME + FreeBSD `net80211` instead).

## Milestone Goal

m3OS associates with a WPA2-PSK Wi-Fi network on the dev laptop and pulls a DHCP lease over the wireless interface. The driver targets exactly one chipset (MT7925) and explicitly defers every other family. This phase exists to make 1.0 honest on the laptop the project is being developed on — not to ship a general Wi-Fi stack.

## Why This Phase Exists

The pre-1.0 audit ([`docs/appendix/audit-status/74a-pre-1.0-audit.md`](../appendix/audit-status/74a-pre-1.0-audit.md), §3 — an audit artifact, not a phase) documents the laptop reality: the dev hardware has zero ethernet. Every modern laptop ships only Wi-Fi for general-purpose connectivity. Without a Wi-Fi driver, m3OS at 1.0 is a desktop-only OS, and even then only on the diminishing set of desktops with onboard wired NICs.

Wi-Fi drivers are large (Linux's `mt76` family is ~15k LOC of pure driver code, not counting `mac80211` and `cfg80211`), so the project pragmatically scopes Phase 81 to one chipset, one band (5 GHz preferred, fall back to 2.4 GHz), one auth method (WPA2-PSK; no enterprise, no WPA3-SAE). Everything else is post-1.0.

## Learning Goals

- Understand the layering: PCIe MMIO + firmware download (driver) → MAC firmware (chipset) → 802.11 mgmt frames (driver-supplied state machine) → IP stack (existing kernel)
- See why Wi-Fi associations involve an 802.11 mgmt-frame exchange (probe-request, probe-response, auth, assoc-request, assoc-response, 4-way handshake)
- Learn how WPA2-PSK derives the PTK from the PSK + ANonce + SNonce + MACs (the 4-way handshake)
- Understand why the kernel's existing TCP/IP stack does not change — Wi-Fi terminates at the data-link layer and produces Ethernet-shaped frames upward
- See why most of the complexity is in the proprietary firmware blob the driver downloads to the chipset

## Feature Scope

### Track A — mt792x PCIe driver shell

- **A.1** — PCI probe for the mt792x family over a bounded device-ID registry (MediaTek vendor `0x14C3`, Wi-Fi class `0x02`/`0x80`/`0x00`): MT7921 `0x7961`/`0x0608`, MT7922 `0x7922`/`0x0616`, MT7925 `0x7925`/`0x0717`. Map BAR0. Reset the WFDMA engine.
- **A.2** — Firmware download. The mt792x firmware blobs ship as `WIFI_MT7961_patch_mcu_*` / `WIFI_RAM_CODE_MT7961_*.bin` (MT7921), `WIFI_*_MT7922_*` (MT7922), etc. from the upstream `linux-firmware` tree under MediaTek's redistribution clause; the build pipeline accepts them as vendor blobs staged under `kernel/initrd/lib/firmware/mt7961/` (and `mt7922/`, `mt7925/`). The blob is mandatory (the chip does nothing until the WM MCU runs).
- **A.3** — WM MCU command ring + WFDMA TX / RX ring setup over the mt792x (connac2) descriptor format, with all addresses as `DmaBuffer` IOVAs.

### Track B — 802.11 minimal mgmt-frame state machine

- **B.1** — Scan: issue probe-request on each supported channel; collect probe-responses into a BSS list.
- **B.2** — Auth + assoc: open-system auth followed by association-request with the chosen BSS.
- **B.3** — 4-way handshake (WPA2-PSK only). PTK derivation via PBKDF2(passphrase, SSID) → PMK → PTK with the 4-way `EAPOL-Key` exchange. NOTE: SHA-1, HMAC-SHA1, PBKDF2, and AES-Key-Wrap (RFC 3394) are **not** present in the workspace (`crypto-lib` ships only the SHA-256 family); Phase 81 adds them — see the task doc Track B.1–B.3.
- **B.4** — On successful association, expose the interface as a `RemoteNic`-compatible facade. The kernel TCP/IP stack treats it exactly like an Ethernet NIC.

### Track C — Configuration surface

- **C.1** — `/etc/wpa.conf` — minimal text config: `ssid=...`, `psk=...`, optional `freq=2.4|5`. No `wpa_supplicant`-style daemon at 1.0; reload on boot only.
- **C.2** — `m3ctl wifi status` — read-only status display (associated SSID, signal strength, IP address) for diagnostic purposes.

## Important Components and How They Work

### Firmware blob

MT7925's host driver is the simple part; the hard work happens in the firmware running on the chipset's embedded MAC processor. The driver's job is to download the firmware, hand it work via a command ring, and consume completion events. Linux's `mt76` driver follows the same pattern. The blob is licensed for redistribution by MediaTek but is not open source — this is the same model as Intel's `iwlwifi`.

### 802.11 mgmt vs data frames

802.11 mgmt frames (beacon, probe, auth, assoc, deauth, disassoc) flow through a separate path from data frames. The driver assembles mgmt frames in software and pushes them to the firmware; data frames are produced by the kernel TCP/IP stack as Ethernet headers, and the driver rewrites the Ethernet header into 802.11 data-frame form before submission.

### 4-way handshake

After association the AP and the station each generate a nonce (ANonce / SNonce), exchange them via two EAPOL-Key frames, then both sides derive the PTK from `PRF-X(PMK, "Pairwise key expansion", min(AA,SA) || max(AA,SA) || min(ANonce,SNonce) || max(ANonce,SNonce))` where `AA` and `SA` are the MAC addresses. The third and fourth EAPOL-Key frames install the keys. This entire sequence is ~300 LOC of plain crypto + state machine on top of the Phase 42 primitives.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 driver-host primitives — Wi-Fi driver is just another `RemoteNic` from the kernel's perspective.
- Reuses Phase 67's `DmaBuffer<T>` for TX/RX ring allocation.
- Adds the WPA2-PSK key-derivation primitives (SHA-1, HMAC-SHA1, PBKDF2, AES-Key-Wrap) to `crypto-lib` — these are **not** present from Phase 42, which is SHA-256-family only.
- Lifts the Phase 79 `Vec<RemoteNic>` so the laptop can route over Wi-Fi when no wired link is up.

## Implementation Outline

1. Land the firmware-blob staging path in `xtask` + `populate_ext2_files`.
2. Bring up the MT7925 PCI driver shell: reset, firmware download, command-ring acknowledgment.
3. Implement scan + the BSS list display.
4. Implement open-system auth + association against a test AP (open network first).
5. Implement WPA2-PSK 4-way handshake.
6. Wire as a `RemoteNic`; verify DHCP lease over wireless on the dev laptop.
7. Bump kernel to `0.81.0`.

## Acceptance Criteria

- On the dev laptop: `cargo xtask run --kvm --on-laptop` (or equivalent bare-metal flow) brings up Wi-Fi, associates with a WPA2-PSK AP defined in `/etc/wpa.conf`, and pulls a DHCP lease.
- `m3ctl wifi status` reports SSID, signal strength, and assigned IPv4 address.
- `ping` over the wireless interface works.
- The Phase 77 DNS resolver works over the wireless interface (`getaddrinfo("github.com", ...)` returns an address).
- No regression in wired NIC drivers — Phase 79's drivers still come up; the routing default picks wired over wireless when both are available.
- The MediaTek firmware blob redistribution license is reviewed and recorded under `docs/legal/firmware-licenses.md` (or equivalent) before merge.
- Kernel bumped to `0.81.0`.

## Companion Task List

- [Phase 81 Task List](./tasks/81-wifi-reference-tasks.md)
- [Phase 81 Learning Doc](../81-wifi-reference.md) — the as-landed walkthrough (layering, soft-MAC-with-offload, the HOST-vs-CHIPSET crypto split, IOVA vs MCU addresses, where the code lives).

## How Real OS Implementations Differ

- **Wi-Fi is greenfield for a Rust microkernel — there is no peer reference.** **Redox has NO Wi-Fi stack** (wired-only over smoltcp) and must not be cited as a Wi-Fi reference; **Managarm** and **SerenityOS** are likewise wired-only. On this axis Redox is exactly where m3OS is.
- The architectural references worth borrowing are **Fuchsia's SME/MLME split** (a userspace station-management entity with the supplicant in-house) and **FreeBSD/NetBSD `net80211` + userspace `wpa_supplicant` + hardware CCMP**. m3OS makes the **Fuchsia-style** choice: the WPA2 supplicant is the crypto chain in `wifi-core` run **inside the ring-3 `mt792x` driver process** (folded in), not a separate daemon — justified by the 1.0 scope. **Genode** is the closest microkernel precedent (it ported ~215k LOC of Linux `iwlwifi`+`mac80211` via DDE-Linux and runs `wpa_supplicant` as a separate component); **Haiku** ports BSD `iwm`/`iwx`.
- Note `net80211` offloads CCMP to hardware *when available* but keeps a software CCMP path; m3OS deliberately **requires** hardware CCMP on its one target family to avoid implementing software AES-CCM at all — m3OS's own scoping choice layered on the net80211 model, not net80211's design.
- Linux ships drivers for dozens of Wi-Fi chipset families (Intel iwlwifi, Realtek rtw88/rtw89, Atheros ath9k…ath12k, Broadcom brcmfmac, MediaTek mt76, Marvell mwifiex, …) and `mac80211` handles association/roaming/power-save/key-management/channel-switching in shared code; m3OS ships one family and hand-rolls the minimal WPA2-PSK state machine.
- `wpa_supplicant` (or `iwd`) is normally a userspace daemon managing credentials, EAP, roaming policy, and concurrent networks. m3OS at 1.0 reads `/etc/wpa.conf` once at boot.
- Real OSes treat Wi-Fi power state as a first-class power-management citizen with regulatory-database integration; m3OS at 1.0 hardcodes channel 36 (5 GHz) or 6 (2.4 GHz) and trusts the firmware's regulatory enforcement.

## Deferred Until Later

- All non-mt792x chipset families (post-1.0; the gap to Linux is enormous and explicitly accepted)
- WPA3-SAE / 802.1X / EAP / OWE
- 6 GHz / MLO / Wi-Fi 7 features beyond what mt792x firmware exposes for free
- Roaming, BSS transitions, mesh
- Power-save (PS-Poll, U-APSD)
- Regulatory database
- AP / Wi-Fi Direct / hotspot modes
- Bluetooth coexistence on combo chipsets
- A real `wpa_supplicant` / `iwd` userspace daemon
