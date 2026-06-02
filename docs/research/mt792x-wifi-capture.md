# mt792x Wi-Fi empirical capture (Phase 81 Track E.3)

**Status:** Driver-side work **complete**; radio validation is **hardware-only (operator
action)**. All pure logic (firmware parsers, MCU/TXD/TLV encoders, DMA-descriptor math,
802.11 FSM, WPA2 crypto chain) is host-tested via `cargo xtask check`. Real-radio
bring-up — WFDMA DMA under IOMMU, firmware download to the WM MCU, 802.11 association,
the 4-way handshake, chipset CCMP offload — can only be exercised on silicon. This build
host is **not** the user's dev laptop; the placeholder slots below are filled when the
operator runs the radio on the dev laptop following `scripts/mt792x-vfio-validate.md`.

> This is the Wi-Fi analog of `docs/research/hda-realtek-capture.md` (Phase 80):
> QEMU cannot emulate the radio path, so real-silicon behavior is recorded here once
> observed on hardware.

## Target hardware

Confirmed non-destructively on the dev laptop via `lspci` / `/sys/kernel/iommu_groups`
(no sudo, no driver unbind):

- **Wi-Fi radio:** MediaTek `[14c3:xxxx]` — raw `mt76_chip` hex: `0x____` *(fill on run)*
  PCI class `0x028000` (Other Network Controller).  Expected IDs:
  MT7921E `[14c3:7961]`, MT7922E `[14c3:0616]`, MT7925 `[14c3:7925]`.
- **PCI BDF:** `0000:__:__.0` *(fill on run)*
- **IOMMU group:** group `__` — **ISOLATED / NOT ISOLATED** *(fill on run)*;
  confirm only the radio (+ any upstream PCIe bridge class `0604`) is in the group.
  If a non-bridge endpoint shares the group, VFIO passthrough is unsafe.
- **Subsystem id:** `____ : ____` *(fill on run)*

Record the BDF + `[14c3:xxxx]` and IOMMU group membership here before proceeding.

## Firmware

The mt792x driver requires a vendor firmware blob (the chip does nothing until the WM
MCU is running). Firmware is staged under `kernel/initrd/lib/firmware/mt7961/` (or
`mt7922/`, `mt7925/` depending on the chip family) after the redistribution-license
review in `docs/legal/firmware-licenses.md`. When the blob is absent the driver logs
`MT792X_FW:absent:` and Wi-Fi stays disabled.

**Blob filenames + versions actually present on the dev laptop** *(fill on run)*:

```
/lib/firmware/mediatek/WIFI_MT79??_patch_mcu_?_?_hdr.bin   version: ____
/lib/firmware/mediatek/WIFI_RAM_CODE_MT79??_?.bin            version: ____
```

**Firmware-running poll register and value** *(fill on run)*:

The A.4 task marks this item `[UNCERTAIN]` — the value is lifted from upstream
`drivers/net/wireless/mediatek/mt76/mt7921/mcu.c` (`MT_CONN_ON_MISC` /
`MT_TOP_MISC2_FW_N9_RDY` or the equivalent for the confirmed chip variant) and must
be confirmed on silicon. After a successful download handshake the driver polls this
register; on a real chip the poll either resolves within the timeout or the firmware
is not running (possibly a blob-variant mismatch or a missing `PATCH_SEM_RELEASE`).

- **Poll register offset (BAR0-relative):** `0x____` *(confirm vs `mt7921/mcu.c`)*
- **Expected ready value / mask:** `0x____` *(confirm on run)*
- **Observed behavior:** poll resolved in `____` ms / timed out *(fill on run)*

**MCU init command sequence** *(fill on run)*:

After the firmware-running poll resolves, the driver issues WM MCU commands over
`MT_MCUQ_WM`; record the sequence of `cid` values and their responses here:

```
cid 0x____ (GET_NIC_CAPABILITY or equivalent)  → reply seq matched in ____ ms
cid 0x____ ...
```

## Association capture

All items below are operator-only on a real WPA2-PSK AP. Record the captured behavior
in the placeholder slots; if a step fails, record the serial output at the failure point.

### Scan

- **Probe-request sent on channel(s):** `____` *(2.4 GHz / 5 GHz)*
- **Probe-response(s) received:** `____` BSSes visible
- **Target AP BSSID + SSID + channel:** `__:__:__:__:__:__` / `____` / `ch ____`
- **RSN IE accepted (CCMP-pairwise + PSK-AKM):** yes / no *(fill on run)*

### Open-system auth + association

- **Auth frame (seq=1, algorithm=0 Open-System) sent:** yes / no
- **Auth response (seq=2, status=0):** yes / no / status `____`
- **Assoc-Request sent (carrying RSN IE):** yes / no
- **Assoc-Response status code:** `0` (success) / `____` (failure) *(fill on run)*

### 4-way handshake (EAPOL M1..M4)

- **EAPOL M1 received (ANonce):** yes / no
- **EAPOL M2 sent (SNonce + MIC, RSN IE byte-matched to Assoc-Request):** yes / no
- **EAPOL M3 received (GTK wrapped, MIC verified):** yes / no
  - MIC verify result: **pass** / fail (if fail: frame dropped, `InstallKey` not emitted)
- **EAPOL M4 sent:** yes / no
- **TK installed via `STA_REC_UPDATE` MCU command:** yes / no
  - MCU reply seq matched: yes / no
- **GTK unwrapped (AES-Key-Wrap / RFC 3394):** yes / no

### DHCP lease

- **`NET_LINK_STATE{up:true}` emitted on association:** yes / no
- **DHCP lease acquired:** yes / no
- **Assigned IPv4:** `___.___.___._____` *(fill on run)*
- **Lease time:** `____` s

### Ping + DNS

- **`ping <gateway>` over wireless interface:** replies / no reply
  - Packet loss (N packets): `___`%
- **`getaddrinfo("github.com", …)` returns ≥1 A record:** yes / no
- **With wired NIC also up: default route is wired (C.3 `default_route_index_by_link`):**
  yes / no / no wired NIC present to test *(fill on run)*

### `m3ctl wifi status`

```
SSID:   ____
RSSI:   ____ dBm
IPv4:   ___.___.___._____
```

### Any driver bug uncovered (cf. Phase 79/80 VFIO findings)

*(fill on run — describe the symptom, the fix, and the commit ref)*

____

## Notes

### Host-vs-chipset crypto split

The entire WPA2-PSK key management runs on the **host** (in the ring-3 `mt792x_driver`
process via `wifi-core` + `crypto-lib`):

- **Host computes:** PMK (`PBKDF2-HMAC-SHA1(passphrase, SSID, 4096, 32)`), SNonce
  (CSPRNG), PTK (`PRF-512(PMK, …)` over HMAC-SHA1), EAPOL-Key MIC
  (`HMAC-SHA1-128(KCK, frame_with_zeroed_mic)`), GTK unwrap (RFC 3394 AES-Key-Wrap
  under KEK). The only AES primitive the host executes is raw AES-128 ECB for the
  key-wrap/unwrap — **no software AES-CCM is implemented**.
- **Chipset does:** per-packet CCMP (AES-CCM) encrypt and decrypt + replay-counter
  enforcement, entirely in hardware, once the 16-byte TK (and GTK) are installed via
  the `STA_REC_UPDATE` MCU command. The host hands the chip plaintext frames
  (Ethernet-shaped, with 802.11 header rewrite done by the driver) after key install;
  CCMP framing and MIC generation happen in the radio hardware, not in software.

This split is why no software AES-CCM is needed and why the EAPOL demux (intercepting
LLC/SNAP ethertype `0x888E` before `NET_RX_FRAME` is emitted) is load-bearing — the
4-way-handshake frames must reach the supplicant FSM in the driver, not the kernel
IP stack.

### Host-test coverage

All of the above host-side logic is covered by the `kernel-core`/`wifi-core`/`crypto-lib`
host tests that run under `cargo xtask check` — firmware-parser tests against synthetic
crafted fixtures, MCU-TXD/TLV encoding and `seq`-matching tests, DMA-descriptor IOVA
plumbing tests, 802.11 mgmt-frame builder + RSN IE tests, the full FSM happy-path and
failure-edge tests, and WPA2 vector tests (PTK, EAPOL-Key MIC, GTK-unwrap). What the
host tests **cannot** prove is that the IOVA argument plumbed into a descriptor is
actually `DmaBuffer::iova()` (not `user_ptr()`) — that is confirmed hardware-only by
the IOMMU fault ISR staying silent across sustained DMA. Similarly, the firmware-running
poll register/value and the MCU init command sequence are confirmed here once observed
on hardware, resolving the A.4 `[UNCERTAIN]` item.
