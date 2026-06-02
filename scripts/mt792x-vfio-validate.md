# mt792x real-hardware validation via VFIO passthrough (operator runbook)

Phase 81 Track E.3's last acceptance bullet — "on the dev laptop, the `mt792x_driver`
completes bring-up (claims the radio, downloads firmware, brings the WM MCU up, and
associates with the WPA2-PSK AP in `/etc/wpa.conf`)" — needs the driver to run against
physical silicon. **QEMU has no `mt76` device model**, so this is the *only* path to
validate the radio path. The host-tested pure logic in `kernel-core`/`wifi-core`/`crypto-lib`
proves the firmware parsers, MCU/TXD/TLV encoders, 802.11 FSM, and WPA2 crypto chain, but
nothing from "submit a descriptor to WFDMA" onward can be reached without a real radio.

> **This is an operator action, not an automated step.** It requires `root`, and it
> **unbinds the host's active Wi-Fi radio — host wireless connectivity drops** for the
> duration and must be restored afterward. Run it only from a wired link or a console
> where losing the Wi-Fi adapter will not cut you off. A Claude Code session can run
> each step interactively by prefixing it with `!` (e.g. `! sudo ...`).

> **This build host is NOT the user's dev laptop.** This session can author and
> host-test all pure logic and this runbook, but cannot bind `vfio-pci`, pass the
> radio through, or reach a real AP. The association, DHCP, and `ping` steps below
> are **operator root actions** on the dev laptop against a real WPA2-PSK AP.

The driver code is complete and its firmware-parser, MCU-command, DMA-descriptor,
802.11-FSM, and WPA2-crypto logic is already host-tested via `cargo xtask check`.
This runbook is the hardware-only confirmation and exists so it is reproducible and
authorized.

## 0. Pre-flight (read-only, safe)

```bash
# Identify the Wi-Fi radio and confirm its vendor/device ID.
# Expect a MediaTek PCI ID in the [14c3:79xx] range:
#   MT7921E: [14c3:7961]   MT7922E: [14c3:0616]   MT7925: [14c3:7925]
lspci -nn | grep -i network

# Record the BDF (e.g. 03:00.0) and the [14c3:xxxx] device ID from above.
wifi=$(lspci -Dn | awk '/14c3:/{print $1; exit}')
echo "Wi-Fi radio at $wifi"
lspci -nnks "$wifi"

# Confirm IOMMU group isolation.
# All devices in the group must be passed through together; if a non-bridge
# endpoint shares the group, passthrough is unsafe — stop and investigate.
grpdir=$(readlink -f /sys/bus/pci/devices/0000:${wifi}/iommu_group)
echo "IOMMU group: $grpdir"
ls "$grpdir/devices"        # expect only the radio + any upstream PCIe bridge
dmesg | grep -iE "DMAR|IOMMU" | head   # confirm IOMMU is enabled
```

If the IOMMU group contains only the radio (and a PCIe bridge with class `0604`),
proceed. An unreserved second endpoint in the group means passthrough is unsafe.

## 1. Bind the radio to vfio-pci

**NOTE: this step drops host Wi-Fi connectivity for the duration.**

```bash
sudo modprobe vfio-pci

# Unbind from the kernel mt7921e / mt7922e driver:
echo "0000:${wifi}" | sudo tee /sys/bus/pci/devices/0000:${wifi}/driver/unbind

# Register the device ID with vfio-pci.  Replace <devid> with the hex
# device ID from step 0 (e.g. "7961" for MT7921E, "0616" for MT7922E):
echo "14c3 <devid>" | sudo tee /sys/bus/pci/drivers/vfio-pci/new_id

# Confirm the binding:
lspci -nnks "$wifi"         # expect: Kernel driver in use: vfio-pci
```

## 2. Boot m3OS with the radio passed through

Build the image, then add VFIO passthrough.

```bash
cd <repo>
cargo xtask image           # ensure target/.../boot-uefi-m3os.img is current

IMG=target/x86_64-unknown-none/release/boot-uefi-m3os.img
DISK=target/x86_64-unknown-none/release/disk.img

sudo qemu-system-x86_64 -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=$IMG -serial stdio -m 2048 -smp 4 \
  -cpu host -enable-kvm -display none \
  -drive file=$DISK,format=raw,if=virtio \
  -device vfio-pci,host=${wifi},addr=0x0a \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04
```

> **CRITICAL — pin the radio to a PCI slot clear of the fixed-BDF driver
> sentinels.** The m3OS device-host infrastructure uses fixed guest PCI slots
> as claim sentinels: e1000 = slot 3 (`0x03`), NVMe = slot 4 (`0x04`), AC97 =
> slot 5 (`0x05`), xHCI = slot 6 (`0x06`). If the passed-through radio lands on
> one of those slots by default, the sentinel driver claims it (wrong device),
> fails, and its restart churn starves `mt792x_driver`. **Always use `addr=0x0a`
> (slot 10) or `addr=0x0b` (slot 11)**, which are clear of all sentinels.
> This is the same fix applied to the Phase 80 HDA VFIO run — see
> `scripts/hda-vfio-validate.md` for the precedent.

### Expected serial sentinels (in order)

```
mt792x_driver: spawned
mt792x: IOMMU fault ISR armed
mt792x: firmware ROM-patch download: PATCH_NOT_DL_SEM_SUCCESS, N sections
mt792x: firmware RAM-code download: M regions
mt792x: WFDMA TX/RX DMA enabled
mt792x: WM MCU ready
MT792X_SMOKE:server:READY
mt792x: scan complete, N BSSes
mt792x: associating with <SSID>
mt792x: 4-way handshake complete, TK installed
net: DHCP lease acquired on wlan0: <IP>
```

If the firmware blob is absent (not yet staged under
`kernel/initrd/lib/firmware/mt7961/`) the driver logs
`MT792X_FW:absent: firmware blob absent — Wi-Fi disabled, see docs/legal/firmware-licenses.md`
and Wi-Fi stays disabled. Stage the operator-supplied blob at the path above
(after the F.3 license review — see `docs/legal/firmware-licenses.md`) and
rebuild before running this runbook.

### From the m3OS shell

```
m3ctl wifi status   # expect: SSID, RSSI, and DHCP-assigned IPv4
/bin/ping <gateway> # expect: ICMP echo replies (0% loss)
```

## 3. Restore the host Wi-Fi

Run this **every time**, whether the guest succeeded or not.

```bash
echo "0000:${wifi}" | sudo tee /sys/bus/pci/drivers/vfio-pci/unbind
echo "14c3 <devid>"  | sudo tee /sys/bus/pci/drivers/vfio-pci/remove_id

# Rebind to the kernel driver (use mt7921e or mt7922e as appropriate):
echo "0000:${wifi}" | sudo tee /sys/bus/pci/drivers/mt7921e/bind \
  || echo "0000:${wifi}" | sudo tee /sys/bus/pci/drivers/mt7922e/bind

# Restart NetworkManager / wpa_supplicant to bring the interface back:
sudo systemctl restart NetworkManager \
  || sudo wpa_supplicant -B -i wlan0 -c /etc/wpa_supplicant/wpa_supplicant.conf
```

If anything wedges, a reboot fully restores the kernel's `mt7921e`/`mt7922e`
binding.

## What this validates

Running m3OS's `mt792x_driver` against the physical silicon exercises, on real
radio hardware, the entire hot path that QEMU cannot model:

- **WFDMA DMA under IOMMU translation** — that every descriptor `buf0`/`buf1`
  and every ring `desc_base` carries `DmaBuffer::iova()`, not host-physical, and
  the IOMMU fault ISR stays silent across sustained DMA (the host test in
  `kernel_core::mt792x::dma` proves only that the argument is plumbed into the
  descriptor; the IOVA-vs-host-phys distinction is hardware-only).
- **Firmware download to the WM MCU** — ROM-patch big-endian section semaphore
  (`PATCH_IS_DL` branch, chunked 4096-byte `FW_SCATTER`) + per-region RAM-code
  download to each region's own load address; the **firmware-running poll
  register/value** (the A.4 `[UNCERTAIN]` item, lifted from `mt7921/mcu.c`)
  confirmed against real MCU behavior.
- **WM MCU command ring** — `GET_NIC_CAPABILITY` (or equivalent init query) on
  `MT_MCUQ_WM` returning a matched reply on `MT_RXQ_MCU`, proving the TXD/TLV
  encoding and `seq` matching on real silicon.
- **802.11 association** — the host-driven open-system auth + assoc-request +
  probe/scan state machine, which the chip's firmware does **not** run (soft-MAC).
- **WPA2-PSK 4-way handshake** — the EAPOL-Key M1..M4 exchange, PTK derivation
  (PRF-512 / HMAC-SHA1), EAPOL-Key MIC verify, GTK unwrap (AES-Key-Wrap), and
  TK install via `STA_REC_UPDATE` MCU command — all executing on the path the
  host-unit-tests cover, now confirmed against a real AP.
- **Chipset CCMP offload** — that data frames flow after TK install with CCMP
  encrypt/decrypt done entirely in hardware; the host never encrypts a data frame.

These close the hardware-only acceptance items in E.3/E.4 that the
`kernel-core`/`wifi-core`/`crypto-lib` host tests and the `wifi-smoke`
skip-with-reason gate cannot reach.
