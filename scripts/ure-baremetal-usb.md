# Booting m3OS bare-metal from USB to test the RTL8156 (`ure`) USB-Ethernet + DHCP

Operator runbook (Phase 96). This is the **bare-metal** path that closes the loop
QEMU can't: an actual Ethernet frame across the `ure` bulk-IN endpoint and a DHCP
lease bound from the real LAN.

> **Why bare-metal?** Under QEMU `usb-host` passthrough the host kernel (`r8152`)
> already owns + initialized + linked the dongle, and re-running the chip's vendor
> reset tears down that host-established USB connection (the guest can't
> re-enumerate). So **TX works under QEMU but the bulk-IN RX stream is never
> delivered** — DHCP sends DISCOVER but no OFFER comes back. On bare metal m3OS
> **cold-owns** the device from power-on, so the driver runs the full vendor init
> and RX flows. See `scripts/ure-vfio-validate.md` for the QEMU loop and
> `docs/roadmap/tasks/96-bare-metal-usb-ethernet-tasks.md` (R4 findings) for the
> full analysis.

The driver **auto-detects** which init to run from `PLA_OOB_CTRL.NOW_IS_OOB`
(cold device → full init; host-pre-initialized → minimal). **No special build is
needed** — the same image is correct on QEMU and bare metal.

---

## 0. What you need

- **Target machine:** a UEFI x86-64 PC. (The Phase 96 reference is a Dell
  Precision 5560 / Tiger Lake, which also has Intel AMT for headless serial
  capture — see §4.)
- **The dongle:** the Realtek **RTL8156** USB-Ethernet adapter (`0bda:8156`),
  and an **Ethernet cable into a LAN that runs a DHCP server with egress** (a
  normal home/office router). The link must come up (the dongle's LEDs light).
- **A USB stick** (any size ≥ ~256 MB) you can erase — this becomes the boot
  drive. A **second USB port** (or USB hub) for the dongle.
- **The build host** (this dev machine) with the m3OS toolchain (the same one
  that runs `cargo xtask`).
- *(Optional, for headless log capture)* a **second computer** on the same LAN
  and either Intel AMT provisioned on the target (§4a) or just a camera to read
  the target's screen (§4b).

---

## 1. Build the bootable image

```bash
cd <repo>                      # the m3OS workspace root
cargo xtask fetch-fonts        # one-time: pulls the Nerd Font the image embeds
cargo xtask image              # builds the UEFI-bootable disk image
```

Output (raw, dd-able):

```
target/x86_64-unknown-none/release/boot-uefi-m3os.img
```

`ure_driver` is bundled in the default image and `init` auto-starts it
(`/etc/services.d/ure_driver.conf`, with a built-in fallback), so nothing extra
is required to bring the dongle up.

**Tip — easier log reading.** The default image boots into the graphical
compositor, which scrolls the boot log away. To keep the kernel/driver log on the
console (so the `ure:` / `[dhcp]` lines stay visible), build with:

```bash
cargo xtask image --skip-login        # stay on the serial/framebuffer console
```

**Secure Boot.** Most firmwares will refuse an unsigned EFI binary unless Secure
Boot is disabled (§3). If you must keep Secure Boot on, sign the image:

```bash
./scripts/gen-secure-boot-keys.sh     # one-time: generate enrollment keys
cargo xtask image --sign              # produces a signed EFI binary
# then enroll the generated cert in the firmware's Secure Boot key DB (§3).
```

---

## 2. Write the image to the USB stick

> ⚠️ **`dd` writes raw to a whole disk — the wrong device name destroys data.**
> Identify the USB stick carefully and double-check before running `dd`.

**Linux:**

```bash
# Plug in the USB stick (NOT the dongle). Find its device node:
lsblk -dno NAME,SIZE,MODEL,TRAN | grep usb
#   e.g.  sdb   28.9G  SanDisk Ultra  usb     →  the device is /dev/sdb
# (Confirm by size/model. Make sure it is the STICK, not your system disk.)

# Unmount any auto-mounted partitions on it:
sudo umount /dev/sdX*        2>/dev/null || true   # replace sdX with your device

# Write the image (replace /dev/sdX with the confirmed device, NOT a partition):
sudo dd if=target/x86_64-unknown-none/release/boot-uefi-m3os.img \
        of=/dev/sdX bs=4M conv=fsync status=progress
sync
```

**macOS:** `diskutil list` → identify `/dev/diskN` → `diskutil unmountDisk
/dev/diskN` → `sudo dd if=boot-uefi-m3os.img of=/dev/rdiskN bs=4m` → `sync`.

**Windows:** use [Rufus](https://rufus.ie) in **DD image** mode (not ISO mode)
with `boot-uefi-m3os.img`, or `balenaEtcher`.

The image is a GPT disk with a UEFI System Partition holding the bootloader +
kernel (which embeds the ramdisk of drivers). It is self-contained — the separate
`disk.img` ext2 data disk used by the QEMU harness is **not** required for the
`ure`/DHCP test (`init` uses its built-in service-config fallback).

---

## 3. Boot the target from USB

1. Insert the **boot USB** and the **RTL8156 dongle** (separate USB ports), and
   the **Ethernet cable** into a live LAN with DHCP.
2. Power on and enter the firmware boot menu / setup:
   - Dell (incl. Precision 5560): tap **F12** at the logo for the one-time boot
     menu; **F2** for BIOS setup.
   - Generic: usually **F12 / F11 / F9 / Esc** for the boot menu.
3. In **BIOS setup**, if needed:
   - **Secure Boot → Disabled** (unless you signed the image in §1 and enrolled
     the cert), and ensure **UEFI** (not Legacy/CSM) boot mode.
   - *(Optional)* enable the **internal serial / AMT SOL** if you'll capture logs
     over AMT (§4a).
4. From the boot menu, select the **UEFI: <USB stick>** entry. m3OS boots.

---

## 4. Capture the boot log (so you can read the `ure` / `[dhcp]` lines)

The driver's progress is on the kernel serial console (16550 UART, COM1 `0x3F8`).
Pick whichever capture fits the machine:

### 4a. Headless via Intel AMT Serial-over-LAN (best, if AMT is provisioned)

From a **second machine on the same LAN** (mirrors `scripts/ure-vfio-validate.md`
§2):

```bash
# Install amtterm (Arch: yay -S amtterm; Debian/Ubuntu: sudo apt install amtterm)
amtterm <target-amt-ip> | tee m3os-sol.log
# enter the AMT password; you now see COM1 in real time + a saved log.
```

### 4b. Read the screen directly (no setup)

If you built with `--skip-login` (§1), the boot log stays on the laptop's own
display. Watch (or video) the screen — the `ure:` and `[dhcp]` lines scroll past
during/after boot. A phone video lets you pause on the sentinels.

### 4c. Post-network log sink (confirms connectivity *after* DHCP binds)

Once DHCP has bound (so m3OS has working networking), point its `syslogd` at a
listener on a second machine to tail logs over the wire — see
`scripts/m3os-logsink.sh`. (This captures *post*-network logs; the DHCP-bind
moment itself is best seen via 4a/4b.)

---

## 5. What success looks like

Watch the serial/console log for this sequence (the `ure: cold device — full
vendor init` line confirms the bare-metal full-init path was taken):

```
ure: spawned
ure: claimed 0bda:8156 slot=...
ure: MAC aa:bb:cc:dd:ee:ff
URE_STAGE1A:OK
ure: cold device — full vendor init          ← bare-metal path (full ure_rtl8156_init)
ure: PLA_CR=0x0c                              ← RE|TE latched
ure: link up 1000M   (or 2500M)
URE_STAGE1B:OK
URE_STAGE2:NIC-UP
[remote_nic] ... registered ring-3 NIC driver ... mac=aa:bb:cc:dd:ee:ff
ure: rx len=0x.... etype=0x....               ← *** REAL RX FRAMES — the milestone ***
[dhcp] DISCOVER sent
[dhcp] OFFER received; REQUEST sent
[dhcp] bound ip=A.B.C.D/M.M.M.M gw=G.G.G.G    ← *** DHCP lease from the real LAN ***
```

The two starred lines are the headline result:
- **`ure: rx len=…`** — Ethernet frames are crossing the bulk-IN endpoint (the
  thing QEMU passthrough could not do). EtherTypes like `0x0806` (ARP),
  `0x0800` (IPv4), `0x86dd` (IPv6) are normal LAN broadcast/multicast traffic.
- **`[dhcp] bound …`** — the in-kernel DHCP client completed a full
  DISCOVER→OFFER→REQUEST→ACK handshake over `ure` and installed the lease. The
  bound IP/gateway are your LAN's, not `10.0.2.x`.

### Optional: exercise the link from the shell
At the m3OS shell, `ping` defaults to `10.0.2.2` (the QEMU gateway) — on a real
LAN that target won't exist, so a bare `ping` will time out. The meaningful proof
is the `ure: rx` + `[dhcp] bound` lines above; pinging a *specific* LAN host needs
a `ping`/HTTP client that takes a target argument (a small follow-up — today's
`ping` has a hardcoded target).

---

## 6. Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| USB entry not in the boot menu | Secure Boot still on (disable, or sign + enroll §1/§3); ensure UEFI (not CSM/Legacy) mode; re-seat the stick; try `dd` again with `conv=fsync` + `sync`. |
| Boots but `ure: no RTL8156 found` | Dongle not enumerated — try a different USB port (prefer a direct port over a hub), confirm its LEDs, and that it's `0bda:8156`. |
| `ure: cold device` but `link down` and no RX | Cable not in a live switch/router, or the full init needs a tweak on your chip rev — capture the full log (§4a) and compare the register writes to OpenBSD `ure(4)`; the parked init is faithful but this is its first bare-metal run. |
| RX frames appear but `[dhcp]` keeps retransmitting | The LAN has no DHCP server, or DHCP replies aren't reaching the client — confirm another device gets DHCP on that port; check the OFFER is broadcast. |
| Shows `ure: pre-initialized device — minimal init` on bare metal | Something already claimed the MAC before m3OS (rare on cold boot). Power the machine fully off (not just reboot) and unplug/replug the dongle so it's truly cold. |
| Black screen / no console output | Build with `--skip-login` (§1) so the log stays on-console; or capture via AMT SOL (§4a). |

---

## 7. Recording the result

Per Phase 96 Track D.3, capture the boot log (SOL or screen video) showing the
sentinels in §5 and attach it / note it in
`docs/roadmap/tasks/96-bare-metal-usb-ethernet-tasks.md`. The headline claim —
"real networking over a physical USB-Ethernet dongle" — is proven once
`ure: rx len=…` and `[dhcp] bound …` appear on real hardware.
