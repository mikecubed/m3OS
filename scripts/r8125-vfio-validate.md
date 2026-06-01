# r8125 real-hardware validation via VFIO passthrough (operator runbook)

Phase 79 Track D.1's last acceptance bullet — "on a real RTL8125 card, `ping`
succeeds" — needs the driver to run against physical silicon. This dev host
*has* the exact part (`0b:00.0 [10ec:8125]`, RTL8125B), so it can be validated
here via **VFIO passthrough** into the m3OS QEMU guest.

> **This is an operator action, not an automated step.** It requires `root`,
> and it **unbinds the host's active default-route NIC (`enp11s0`) — host
> networking on that interface drops for the duration** and must be restored
> afterward. Run it only from a console / a second link (Wi-Fi `wlan*`) where
> losing `enp11s0` will not cut you off. A Claude Code session can run each
> step interactively by prefixing it with `!` (e.g. `! sudo ...`).

The driver code is complete and already corroborated against this chip
non-destructively (device ID `0x8125`, Ethernet class `0x02`, BAR2 MMIO, INTx
pin A → IRQ 39, Linux `r8169` binding, `rtl8125b-2` firmware). This runbook is
the only remaining step and exists so it is reproducible and authorized.

## 0. Pre-flight (read-only, safe)

```bash
lspci -nnk -s 0b:00.0                      # expect: 10ec:8125, driver r8169
ip route get 1.1.1.1                        # confirm whether enp11s0 is your only link
grpdir=$(readlink -f /sys/bus/pci/devices/0000:0b:00.0/iommu_group)
ls "$grpdir/devices"                        # IOMMU group 21: 03:0a.0 (bridge) + 0b:00.0
dmesg | grep -iE "DMAR|IOMMU" | head        # confirm IOMMU is enabled
```

The group also lists `0000:03:0a.0` (an upstream PCIe bridge). Bridges do not
need a `vfio-pci` binding, but the group must contain no *other* active
endpoint device. If `03:0a.0` is a bridge (class `0604`), proceed; if it is a
second endpoint with a host driver, stop — passthrough is unsafe on this host.

## 1. Bind the RTL8125 to vfio-pci  (host net on enp11s0 drops here)

```bash
sudo modprobe vfio-pci
echo 0000:0b:00.0 | sudo tee /sys/bus/pci/devices/0000:0b:00.0/driver/unbind
echo 10ec 8125    | sudo tee /sys/bus/pci/drivers/vfio-pci/new_id
lspci -nnk -s 0b:00.0                       # expect: driver vfio-pci
```

## 2. Boot m3OS with the card passed through

```bash
cd <repo>
cargo xtask image                            # ensure target/.../boot-uefi-m3os.img is current
IMG=target/x86_64-unknown-none/release/boot-uefi-m3os.img
DISK=target/x86_64-unknown-none/release/disk.img
sudo qemu-system-x86_64 -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=$IMG -serial stdio -m 2048 -smp 4 \
  -cpu host -enable-kvm -display none \
  -device vfio-pci,host=0000:0b:00.0 \
  -drive file=$DISK,format=raw,if=virtio \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04
```

Expected on serial: `r8125_driver: spawned` → it claims `10ec:8125`, computes a
non-`Unknown` `mac_version` from the live TxConfig XID, loads/validates the
`rtl8125b-2` firmware (or prints the degraded-link warning if absent), reaches
link, and `R8125_SMOKE:server:READY`. Then from the m3OS shell:

```
/bin/ping            # expect: Reply from 10.0.2.2 ... (if NAT'd) — or DHCP/link on the real LAN
```

(For LAN traffic rather than slirp, drop the virtio user-net and let the
passed-through card carry real traffic; `ping` the LAN gateway.)

## 3. Restore the host NIC  (do this every time)

```bash
echo 0000:0b:00.0 | sudo tee /sys/bus/pci/drivers/vfio-pci/unbind
echo 10ec 8125    | sudo tee /sys/bus/pci/drivers/vfio-pci/remove_id
echo 0000:0b:00.0 | sudo tee /sys/bus/pci/drivers/r8169/bind
sudo dhclient enp11s0 || sudo systemctl restart NetworkManager
ip route get 1.1.1.1                          # confirm enp11s0 is back
```

If anything wedges, a reboot fully restores the host's `r8169` binding.

## What this validates

Running m3OS's `r8125_driver` against the physical RTL8125B exercises, on real
silicon, the entire Track C/D hot path that QEMU cannot model: the runtime XID →
`mac_version` dispatch, the per-version soft reset (ChipCmd `RST` self-clear),
the OWN-bit/`TxPoll` ring under real DMA + IOMMU translation, the 32-bit V2
interrupt block, and the `rtl_nic` firmware load — closing the D.1 (and, by the
shared HAL, much of the C.1/C.2) hardware-only acceptance.
