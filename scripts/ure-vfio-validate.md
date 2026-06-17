# URE (RTL8156) bare-metal bring-up via USB device passthrough (operator runbook)

Phase 96 Track B's last acceptance bullets — "the `ure` driver enumerates the
Anker RTL8156 dongle, brings the link up, and `ping` succeeds on real hardware"
— need the driver to run against a physical dongle. **QEMU has no RTL8156 device
model**, so emulated USB device passthrough (`--usb-passthrough 0bda:8156`) is
the only automated path; bare-metal iteration uses the same flag with a real
dongle attached to the dev machine.

> **This is an operator action, not an automated step.** It requires the QEMU
> process to have raw access to the physical USB device node (via a udev rule or
> running as root). The dongle is **claimed from the host kernel** while QEMU
> runs — any host `cdc_ncm`/`r8152` binding is released for the duration and
> automatically restored on QEMU exit. A Claude Code session can run each step
> interactively by prefixing it with `!` (e.g. `! sudo ...`).

> **Unlike PCIe VFIO, no `vfio-pci` bind is needed.** QEMU's `usb-host` device
> uses the `usbfs` / `libusb` backend to claim the matching physical device and
> presents it to the guest's emulated `qemu-xhci` controller. This is
> lower-friction than PCIe VFIO: no IOMMU group isolation check, no kernel
> module bind step, and the host NIC (if separate from the dongle) is
> unaffected.

The driver code is complete and the USB bulk endpoint, CDC-NCM framing, and
RTL8156 device-init logic is host-tested. This runbook is the hardware-only
confirmation and exists so it is reproducible and authorized.

## 0. Pre-flight (read-only, safe)

```bash
# Plug in the Anker USB-C Ethernet dongle and confirm the vendor/product ID.
# Expect: 0bda:8156 (Realtek RTL8156B — the USB 3.x model shipped in most
# Anker A82... / E8... series adapters).
lsusb | grep -i 0bda
# Expected output: Bus NNN Device NNN: ID 0bda:8156 Realtek Semiconductor Corp.
#                                          RTL8153 Gigabit Ethernet Adapter

# Find the sysfs path and note the current kernel driver binding:
lsusb -t            # locate 0bda:8156 in the tree (bus/port)
# The host kernel's r8152 driver will have claimed it if its module is loaded.
# That binding is automatically released when QEMU's usb-host backend opens it.

# Confirm udev gives your user (or the qemu group) access to the device node.
# If running as a regular user, either:
#   (a) add a udev rule:
#       SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="8156", MODE="0664", GROUP="kvm"
#   (b) or run the cargo xtask invocation below with sudo.
ls -la /dev/bus/usb/$(lsusb | awk '/0bda:8156/{print $2"/"substr($4,1,3)}')
```

## 1. In-the-loop iteration (primary bring-up workflow)

This is the main driver development loop. Each iteration: edit the `ure` driver
source, rebuild, launch QEMU with the dongle, read serial output, repeat.

```bash
cd <repo>

# Build a fresh image and launch with the dongle passed through.
# --usb-passthrough adds:
#   -device qemu-xhci,id=xhci_pt
#   -device usb-host,vendorid=0x0bda,productid=0x8156,bus=xhci_pt.0
cargo xtask run --usb-passthrough 0bda:8156 --fresh

# Or, if using KVM for faster boot:
cargo xtask run --usb-passthrough 0bda:8156 --kvm --fresh
```

> **Note:** the `--fresh` flag recreates the data disk on each run; omit it to
> preserve a persistent disk across iterations (useful once the driver reaches
> a stable state and you want to test the full userspace stack).

### Expected serial sentinels (in order)

```
xhci_driver: spawned
xhci: MSI-X configured, event ring ready
xhci: port 1 connected — slot allocation triggered
usb-core: hub: reset port 1 complete, speed USB3
usb-core: enumerated 0bda:8156 (Realtek RTL8156B), assigned address 2
ure_driver: spawned
ure: claimed 0bda:8156 at address 2
ure: device init: RTL8156B (USB 3.x path)
ure: MAC address: xx:xx:xx:xx:xx:xx
ure: link up — 2500 Mbps full-duplex
URE_SMOKE:server:READY
net: DHCP lease acquired on eth0: 192.168.1.50   # your LAN's subnet — the dongle is a real device on the real LAN, not QEMU SLIRP
```

> **Note:** with `usb-host` passthrough the dongle is a *physical* NIC on your
> physical LAN — it is **not** behind QEMU's SLIRP user-net, so it gets its
> address from the real LAN's DHCP server (or use a static IP). There is no
> `10.0.2.x` SLIRP range and no `10.0.2.2` SLIRP gateway on this interface.

If `usb-core` enumerates the device but `ure_driver` does not claim it, check
that the driver's USB device-ID table includes `0bda:8156`. If the dongle is not
seen at all, check the udev permissions / run as root.

### From the m3OS shell

```
m3ctl nic list          # expect: eth0 (ure), link up
/bin/ping 192.168.1.1   # your LAN gateway (or the m3os-logsink machine) — expect ICMP echo replies
```

## 2. Pre-network bare-metal capture (AMT Serial-over-LAN)

On a physical machine that has no serial port (no COM1 header), m3OS logs to
the 16550 UART at `0x3F8` (COM1). Intel ME / AMT can redirect that UART over
Ethernet using the Serial-over-LAN (SOL) protocol. This lets you capture
boot-time and pre-network logs from a second machine without a physical serial
cable.

> **Requirement:** Intel AMT must be provisioned and enabled in the platform
> BIOS (Intel Management Engine BIOS Extension — MEBx). The host must be on the
> same LAN segment as the second machine you capture from.

```bash
# On the SECOND machine (log collector):
# Install amtterm (Debian/Ubuntu: sudo apt install amtterm;
#                  Arch: yay -S amtterm):
amtterm <m3os-host-ip>
# Enter the AMT password when prompted.
# The terminal now shows COM1 output from the m3OS host in real time.

# Optionally tee to a file:
amtterm <m3os-host-ip> | tee m3os-sol.log
```

This is how panic/boot logs are read before the NIC driver brings the network
up. Even if the `ure` driver panics early, the crash log appears on SOL.

## 3. Post-network hand-off (live syslog capture)

Once the `ure` driver links and m3OS acquires an IP address, you can switch
from SOL to a UDP syslog stream for lower-latency, persistent log capture.

**On the second (log collector) machine** — run `scripts/m3os-logsink.sh`:

```bash
# Default: listen on UDP 514, append to ./m3os-console.log
sudo scripts/m3os-logsink.sh --port 514 --log ./m3os-ure-run.log

# Or use an unprivileged port (no sudo needed):
scripts/m3os-logsink.sh --port 5140 --log ./m3os-ure-run.log
```

**On m3OS** (once the shell is reachable):

```
# Point syslogd at the collector machine:
syslogd -R <collector-ip>:514
# or: syslogd -R <collector-ip>:5140
```

From this point, every kernel log and userspace message appears on the collector
machine in real time.

**SSH tail (optional — once m3OS sshd is up):**

```bash
scripts/m3os-logsink.sh --ssh root@<m3os-ip> --log ./m3os-ure-ssh.log
# or: --remote-cmd "dmesg -w" for kernel messages only
```

## 4. What this validates

Running m3OS's `ure` driver + `xhci_driver` against the physical RTL8156 dongle
exercises, on real hardware, the entire path that QEMU cannot model:

- **USB device passthrough** — that `xhci_driver`'s enumeration path handles a
  real USB 3.x device, including the SuperSpeed port reset sequence and the
  SET_ADDRESS / GET_DESCRIPTOR control transfers on real silicon.
- **`ure` bulk endpoint setup** — that the driver's IN/OUT bulk endpoint
  allocation (`usb-core` allocates, `ure` configures the CDC-NCM data interface)
  works end-to-end: zero-length packet handling, wMaxPacketSize negotiation, and
  the RTL8156-specific `URE_SET_MCAST`/link-state commands over the control
  endpoint.
- **RTL8156 device init** — the chip-specific register sequence (reset, MAC
  address read, link negotiation) under real USB bus timing, not a model.
- **Link up and DHCP** — that the driver correctly delivers Ethernet frames to
  the m3OS IP stack and that DHCP completes over the real link.
- **End-to-end TCP** — that `ping` and TCP connections (to a LAN peer such as
  the `m3os-logsink` machine, or the LAN gateway) work through the `RemoteNic`
  facade. (The dongle is a real LAN device — there is no QEMU SLIRP gateway on
  this interface.)

These close the hardware-only acceptance items in Phase 96 Track B that the
host-unit-tests and the QEMU smoke gates cannot reach.

## 5. Requirements and notes

- **A second machine is required** for SOL / network-capture observability.
  The dongle and the collector just need to be reachable from the m3OS guest
  (same LAN with a static or DHCP IP on m3OS). **A direct cable is optional**;
  any LAN with a DHCP server works.
- **The dongle does not need to be your dev machine's active NIC.** If your
  dev machine has a separate wired or Wi-Fi connection, plugging in the dongle
  and running `cargo xtask run --usb-passthrough 0bda:8156` is safe — the
  dongle is simply claimed by QEMU for the session and released on exit.
- **No IOMMU group isolation check** is required (unlike PCIe VFIO). The
  `usb-host` backend operates via `usbfs`, not a PCIe IOMMU domain.
- **Restore after use** is automatic: QEMU releases the `usb-host` device when
  it exits. The host kernel's `r8152`/`cdc_ncm` module will rebind if the
  dongle remains plugged in. No manual unbind/rebind step is needed.
