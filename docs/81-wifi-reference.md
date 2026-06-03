# Phase 81 — Wi-Fi Reference Driver (MediaTek mt792x family)

**Status:** Driver-side complete; radio validation hardware-only (no QEMU mt76 model)
**Source Ref:** phase-81
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 67 (IOMMU Substrate) ✅, Phase 74 (IPC Capability Grants) ✅, Phase 77 (Pre-1.0 Correctness — DNS/`getaddrinfo`) ✅, Phase 79 (Modern NIC — multi-NIC registry + routing) ✅
**Builds on:** Adds m3OS's first Wi-Fi driver as a ring-3 device-host process for the MediaTek **mt792x** PCIe family (MT7921/MT7922 connac2 first; MT7925/connac3 in the same device-ID registry). Reuses the Phase 55b/67/79 device-host substrate wholesale and presents upward as an Ethernet-shaped L2 NIC through `RemoteNic`, so the kernel TCP/IP stack does not change.
**Primary Components:** `kernel_core::mt792x` (host-tested hardware logic: regs/firmware/mcu/dma + `nic_ids` Wi-Fi family + route helper), `userspace/wifi-core/` (802.11 mgmt plane + WPA2-PSK supplicant), `userspace/crypto-lib/` (SHA-1/HMAC-SHA1/PBKDF2/AES-Key-Wrap), `userspace/drivers/mt792x/` (ring-3 driver), `m3ctl wifi status`.

## Milestone Goal

m3OS associates with a WPA2-PSK Wi-Fi network on the dev laptop and pulls a DHCP
lease over the wireless interface, with `m3ctl wifi status` reporting the SSID,
RSSI, and assigned IPv4. The driver targets exactly one chipset family and
explicitly defers everything else. This phase makes the 1.0 connectivity story
honest on the laptops the project actually runs on (which ship only Wi-Fi).

## Why This Phase Exists

The pre-1.0 audit (`docs/appendix/audit-status/74a-pre-1.0-audit.md`, §3)
documents the laptop reality: the dev hardware has zero ethernet. Without a Wi-Fi
driver, m3OS at 1.0 is desktop-only, and only on the diminishing set of desktops
with onboard wired NICs. Wi-Fi drivers are large (Linux's `mt76` is ~15k LOC, not
counting `mac80211`/`cfg80211`), so Phase 81 is scoped to one chipset family, one
band (5 GHz preferred, 2.4 GHz fallback), one auth method (WPA2-PSK).

## Learning Goals

- The Wi-Fi layering: PCIe MMIO + firmware download (driver) → WM MCU (chipset) →
  802.11 management frames + supplicant (host, in the ring-3 driver) → IP stack
  (unchanged kernel).
- Why mt792x is **soft-MAC-with-offload**, not full-MAC: the host runs the entire
  MLME (scan/auth/assoc) **and** the WPA2 key management, while the chipset does
  per-packet CCMP in hardware.
- The precise **HOST-vs-CHIPSET crypto split** and why the host needs only
  HMAC-SHA1, PBKDF2, and AES-128 key-wrap — never software AES-CCM.
- The three distinct address spaces a driver juggles: **IOVA** (device DMA
  addresses), **host-physical**, and **chip-internal MCU addresses**.
- Why the kernel TCP/IP stack is untouched: Wi-Fi terminates at L2 and emits
  Ethernet-shaped frames through `RemoteNic`.

## Feature Scope

### mt792x PCIe driver shell (Track A)

Wi-Fi-class (`0x02`/`0x80`/`0x00`) device-ID registry over the bounded multi-NIC
registry (`kernel_core::nic_ids`, MediaTek vendor `0x14C3`); BAR0 map + WFDMA
reset; firmware ROM-patch (big-endian) + RAM-code (trailer-based little-endian)
parsers and the download handshake; the WM MCU command ring; WFDMA TX/RX data
rings with IOVA from `DmaBuffer`. All bit/descriptor/firmware-parser/MCU-encoder
math lives in host-tested `kernel_core::mt792x`.

### 802.11 mgmt plane + WPA2-PSK supplicant (Track B)

In `userspace/wifi-core/` (`#![no_std] + alloc`, host-tested): mgmt-frame
builders + the exact CCMP+PSK RSN IE, the scan→auth→assoc→handshake→connected
FSM, the EAPOL-Key codec, and the WPA2 key derivation. The four missing crypto
primitives — SHA-1, HMAC-SHA1, PBKDF2-HMAC-SHA1, and AES Key-Wrap (RFC 3394) —
are added to `crypto-lib` (they were **not** present from Phase 42, which is
SHA-256-family only). SHA-1/HMAC-SHA1 are introduced **solely** for the
legacy-mandated WPA2-PSK KDF (PBKDF2/PRF-384) and EAPOL-Key MIC; SHA-1 is
collision-broken and must not be used for any new security-sensitive purpose
(this mirrors the `crypto-lib/src/sha1.rs` header and the task-list
Documentation Notes).

### RemoteNic integration + config (Tracks C/D)

The driver registers on `net.nic`, rewrites Ethernet ⇄ 802.11 data frames,
demuxes EAPOL to the supplicant FSM, emits `NET_LINK_STATE` on association, and
adds a link/medium-aware default-route helper (wired preferred over wireless).
Config is a single static `/etc/wpa.conf`; `m3ctl wifi status` is a read-only
diagnostic.

## Important Components and How They Work

### Firmware blob and the WM MCU

mt792x does nothing until the WM MCU is running, so firmware is **mandatory**
(unlike the *optional* r8169 PHY firmware). The driver downloads a ROM-patch then
a RAM-code image to the chip, hands it work via a DMA command ring, and consumes
asynchronous completion events matched by sequence number. The patch header is
**big-endian**; the RAM image is **trailer-based little-endian** with each region
loading to its own `region.addr`; the patch-semaphore must skip re-download on
`PATCH_IS_DL` or the MCU wedges. The parsers are host-tested against synthetic
crafted fixtures (the `r8169::parse_rtl_fw` precedent); the real vendor blob is
license-gated (see `docs/legal/firmware-licenses.md`) and parsed against shipping
firmware only on hardware.

### Soft-MAC-with-offload and the HOST-vs-CHIPSET crypto split

mt792x runs the lower-MAC + PHY in firmware but leaves the **management plane**
and **key management** to the host. So `wifi-core`, executing **inside the ring-3
driver process** (the Fuchsia-style folded-in supplicant, not a separate
daemon), computes:

- **PMK** = `PBKDF2(HMAC-SHA1, passphrase, SSID, 4096, 32)`,
- a random **SNonce** (kernel `getrandom`),
- **PTK** = `PRF-512(PMK, "Pairwise key expansion", min/max-ordered MACs ‖ nonces)`,
- the **EAPOL-Key MIC** = `HMAC-SHA1-128(KCK, body-with-zeroed-MIC)`,
- the **GTK unwrap** = RFC 3394 AES-128 key-unwrap under the KEK.

The **chipset** does per-packet **CCMP** (AES-CCM) encrypt/decrypt + replay in
hardware once the 16-byte TK is installed in the WTBL via an MCU `STA_REC_KEY`
command. The only AES the host needs is the raw AES-128 block cipher for the
RFC-3394 GTK unwrap — **there is no software AES-CCM**. The TK is installed only
after EAPOL M3's MIC verifies, which the FSM enforces.

### IOVA vs host-phys vs MCU-address

Every descriptor `buf0`/`buf1`, every ring `desc_base`, and every firmware-scatter
buffer is a **device DMA address** and under VT-d/AMD-Vi must be
`DmaBuffer::iova()` — never host-physical. The WFDMA register offsets
(`0xD4000 + …`) are **CPU MMIO** into BAR0 (host VA via `Mmio::map`). The firmware
load addresses (`0x200000`, per-region `region.addr`) are **chip-internal MCU
addresses** passed opaquely inside MCU payloads — neither host-phys nor IOVA.
This is the #1 first-driver hazard; the host test proves the descriptor argument
is plumbed through, and the IOMMU fault ISR staying silent confirms it is the
IOVA on hardware.

### Where the code lives (and why)

- `kernel_core::mt792x` + `kernel_core::nic_ids` — **primitive-free** hardware
  logic only (register offsets, descriptor/token math, firmware parsers, MCU/TXD
  TLV encoders, the `STA_REC_KEY` packer, the route helper). The convention
  `r8169`/`hda` follow.
- `userspace/wifi-core/` — the 802.11 MLME + WPA2 supplicant. It depends on
  `crypto-lib`, so it **cannot** live in `kernel-core` (which cannot depend on
  `crypto-lib`); and housing the supplicant under `kernel_core::net::*` would
  compile policy into ring 0, violating the userspace-first rule.
- `userspace/crypto-lib/` — the four added primitives.

The kernel TCP/IP stack is **unchanged**: the driver presents Ethernet-shaped
frames through the existing `driver_ipc::net` seam, so Wi-Fi terminates at the
data-link layer exactly like a wired NIC.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 driver-host primitives — the Wi-Fi driver is just
  another `RemoteNic` from the kernel's perspective.
- Reuses Phase 67's `DmaBuffer<T>` (IOVA-routed) for the firmware-scatter, MCU,
  and WFDMA rings.
- Extends the Phase 79 `Vec<NicEntry>` multi-NIC registry with a link/medium-aware
  default-route helper so the laptop prefers a wired link when both are up.
- Adds the WPA2 key-derivation primitives to `crypto-lib` — **not** inherited from
  Phase 42 (which is SHA-256-family only).

## Implementation Outline

1. Land the host-tested `kernel_core::mt792x` hardware logic + the `nic_ids`
   Wi-Fi family + the route helper.
2. Add the four crypto primitives to `crypto-lib` against published vectors.
3. Build the `wifi-core` mgmt plane + association FSM + WPA2 KDF/MIC/GTK.
4. Build the `mt792x` driver shell: select, BAR0/WFDMA reset, firmware download,
   MCU ring, WFDMA data rings, graceful firmware-absent degradation.
5. Wire the net.nic data path: Ethernet ⇄ 802.11 rewrite, EAPOL demux to the FSM,
   key install, link-state, and the four-place build wiring.
6. Kernel route plumbing + `m3ctl wifi status` + the `wifi-smoke` skip-with-reason
   gate.
7. Bump kernel to `0.81.0`; record the firmware-redistribution license.

## Acceptance Criteria

- Host tests cover all Track-A/B logic (firmware parsers, MCU/TXD/TLV encoders,
  ring/descriptor/token math, mgmt builders + RSN IE, the association FSM, and
  the entire WPA2 crypto chain against published vectors); `cargo xtask check`
  passes with `wifi-core` + `mt792x_driver` added.
- `cargo xtask wifi-smoke` is a skip-with-reason gate (no QEMU mt76 model) that
  points at the host tests + the VFIO runbook.
- *(Hardware-only.)* On the dev laptop the driver claims the radio, downloads the
  operator-supplied firmware, associates with the WPA2-PSK AP in `/etc/wpa.conf`,
  installs the TK/GTK, pulls a DHCP lease, and `ping` + `getaddrinfo` succeed over
  the wireless link; with a wired NIC also up the default route is the wired NIC.
- The MediaTek firmware redistribution license is recorded under
  `docs/legal/firmware-licenses.md` before any blob is committed.
- Kernel bumped to `0.81.0`.

## Companion Task List

- [Phase 81 Task List](./roadmap/tasks/81-wifi-reference-tasks.md)

## How Real OS Implementations Differ

- **Wi-Fi is greenfield for a Rust microkernel — there is no peer reference.**
  **Redox has no Wi-Fi stack** (wired-only over smoltcp); **Managarm** and
  **SerenityOS** are likewise wired-only, so none can be cited as a Wi-Fi peer.
- The borrowed architectural references are **Fuchsia's SME/MLME split** (a
  userspace management entity with the supplicant in-house) and **FreeBSD/NetBSD
  `net80211` + userspace `wpa_supplicant` + hardware CCMP**. m3OS makes the
  Fuchsia-style choice: the WPA2 supplicant is the crypto chain in `wifi-core`
  run **inside the ring-3 driver process** (folded in), not a separate daemon —
  justified by the 1.0 scope (single static config, no EAP). This still honors
  "the supplicant belongs in userspace, not the kernel" because the driver is
  ring 3, distinct from the kernel TCP/IP stack.
- **Genode** is the closest microkernel precedent: it ported ~215k LOC of Linux
  `iwlwifi`+`mac80211` via DDE-Linux and runs `wpa_supplicant` as a separate
  component. **Haiku** ports BSD `iwm`/`iwx`.
- `net80211` offloads CCMP to hardware *when available* but keeps a software CCMP
  path; m3OS deliberately **requires** hardware CCMP on its one target family to
  avoid implementing software AES-CCM at all — m3OS's own scoping choice layered
  on the net80211 model.
- Linux ships dozens of Wi-Fi chipset families and `mac80211` handles
  association/roaming/power-save/key-management in shared code; m3OS ships one
  family and hand-rolls the minimal WPA2-PSK state machine.

## Deferred Until Later

- All non-mt792x chipset families (the gap to Linux is enormous and accepted).
- WPA3-SAE / 802.1X / EAP / OWE.
- 6 GHz / MLO / Wi-Fi 7 features beyond what mt792x firmware exposes for free.
- Roaming, BSS transitions, mesh; power-save (PS-Poll, U-APSD); regulatory
  database; AP / Wi-Fi-Direct modes; Bluetooth coexistence on combo chips.
- A real `wpa_supplicant` / `iwd` userspace daemon (m3OS reads `/etc/wpa.conf`
  once at boot).
