# Bare-Metal Networking: USB Bulk Endpoints, the `ure` USB-Ethernet NIC, and First-Boot Bring-Up

Kernel **v0.96.0**. Status: ✅ landed — validated on a real Dell Precision 5560
(Tiger Lake) laptop, not just QEMU.

## Overview

Phase 96 has two halves that meet on real hardware:

1. **A USB-Ethernet driver path** — the third xHCI transfer type (**bulk**) plus
   a vendor-protocol driver (`ure`) for the Realtek RTL8156 2.5 GbE USB-C dongle,
   presenting as an ordinary `RemoteNic` so the entire in-kernel IPv4/TCP/UDP
   stack lights up over USB with zero network-layer code.
2. **First-boot bring-up on a machine with no emulator and no serial port** —
   the cluster of fixes that took the reference laptop from a black-screen
   early-boot hang to a usable box: timer calibration that doesn't assume QEMU
   hardware, a PS/2 keyboard that actually receives keystrokes, a framebuffer
   fast enough to read, and a way to retrieve the boot log off a USB stick after
   the fact.

The teaching value is less in any single driver and more in the *gap between
"passes in QEMU" and "works on silicon"*. Every bug in the bring-up half was
latent in earlier phases and invisible under emulation — they only bite when the
hardware diverges from what QEMU models.

## What This Doc Covers

- **Bulk endpoints** — how a Bulk EP context differs from Control/Interrupt, why
  bulk has unbounded retry but no bandwidth reservation, and the Normal-TRB ring
  programming the live consumer added.
- **The `ure` vendor NIC** — register access tunnelled through vendor control
  requests (OCP/PLA banks) instead of a config-space BAR, per-frame hardware
  RX/TX descriptors, and **fire-and-forget TX** (the non-obvious fix for a
  driver↔kernel queueing mismatch that looked like a "QEMU wedge").
- **The `RemoteNic` facade** — why a bus-agnostic NIC seam means a USB NIC and a
  PCIe NIC are interchangeable to the TCP/IP stack.
- **Bare-metal bring-up** — unbounded-hardware-wait hangs (PIT gate bit, COM1 RX
  drain); the **PS/2 keyboard** fix (the missing piece was a *userspace daemon*,
  not the controller); **framebuffer write-combining via PAT** and the subtle
  **PAT-bit index** trap; **USB log persistence** and why `rename()` isn't
  available there; and the **stat-identity** regression (a ramdisk file shadowing
  an ext2 inode).

## Core Implementation

### Bulk endpoints — the shared substrate

xHCI has four transfer types: control, interrupt, bulk, isochronous. Phases 78a–c
shipped control + interrupt (enough to enumerate a device and read a HID boot
report). Bulk is the high-throughput, best-effort type: no reserved schedule
bandwidth (unlike isochronous), but **unbounded retry** — the controller keeps
re-driving a NAKed bulk transfer until it succeeds or the endpoint is halted.
That trade is exactly right for storage and networking, where you want every byte
delivered eventually and don't care about a fixed deadline.

Two pieces were needed:

- **EP context builders** (`kernel-core/src/usb/enumerate.rs`): the Configure
  Endpoint command needs a per-endpoint *context* describing EP Type
  (`EP_TYPE_BULK_OUT = 2`, `EP_TYPE_BULK_IN = 6`), Max Packet Size (from the
  endpoint descriptor), `CErr = 3`, and a dedicated Transfer Ring. This is pure
  logic, host-tested alongside the existing control/interrupt builders — the
  dword encoding is the falsifiable part.
- **The live consumer** (`userspace/drivers/xhci/src/server.rs`): Phase 78c
  defined the `UsbRequest::SubmitTransfer` page-grant transport but never wired a
  consumer. Track A made it real for bulk — map the `PageGrant`, enqueue **Normal
  TRBs** with the Interrupt-On-Completion bit on the last TRB, ring the endpoint
  doorbell, and complete the request off the Transfer Event TRB.

Everything Phase 92 (USB mass storage, the BOT data phase) builds on this bulk
substrate.

### The `ure` vendor-protocol USB-Ethernet NIC

A PCIe NIC has config-space BARs; you mmap a register window and poke it. A USB
NIC has none of that — its "registers" are reached by *tunnelling* reads/writes
through USB **control** requests into vendor register banks (OCP / PLA / USB
banks on the RTL815x). So `ure` splits cleanly along the two transfer types:

- **Control plane** (register access) rides the existing `ControlRequest` IPC
  path: reset the chip, read the MAC from `PLA_IDR`, bring up the PHY and
  auto-negotiation, latch `PLA_CR` `RE|TE` (RX/TX enable).
- **Data plane** rides the new bulk path: RX is a bulk-IN completion loop, TX is a
  bulk-OUT submit. Each frame carries a **hardware RX/TX descriptor header**
  (not a bare Ethernet frame) that the driver prepends/strips.

Frames cross into the kernel via `RemoteNic::inject_rx_frame` / the
`net.nic.ingress` endpoint — *identical* to the e1000 driver — so the TCP/IP
stack never learns the NIC is on USB.

#### Fire-and-forget TX — the "QEMU wedge" that was really a queueing bug

The most instructive bug in the phase. Early bring-up *looked* like a QEMU crash
or a wedge whenever traffic picked up. The real cause was a **driver↔kernel
queueing mismatch**: the kernel held a *single shared `pending_bulk` slot*, so it
could only have **one TX frame in flight at a time**, while the driver is a
polled batch-drainer. With one-at-a-time TX, no give-up/retransmit timer could
win: holding the slot stalled for ~24 s, and dropping it meant the banner frame
never went out at all.

The fix is the lesson: **fire-and-forget TX** (`PendingSend.owned_bulk` /
`send_tx_owned`). Many frames queue at once; the driver drains a batch per poll;
each TX owns its own buffer rather than contending for one shared slot. That
single change dissolved the in-flight guard, the give-up timer, the deadlock, and
the buffer corruption *together* — they were all symptoms of serialized TX. When
a system looks like it "wedges under load," suspect a depth-1 queue before you
suspect the emulator.

### The bus-agnostic `RemoteNic` facade

Phase 79 introduced `RemoteNic` (the `net.nic.ingress` endpoint +
`inject_rx_frame`) so a ring-3 driver can present a NIC to the ring-0 stack over
IPC. Phase 96 is the payoff: a USB NIC registers through the *same* seam a PCIe
e1000/r8169 registers through, so `[remote_nic] up=true 2500Mbps` and a DHCP
lease (`[dhcp] bound ip=192.168.1.221`) come up with no changes anywhere in
IPv4/TCP/UDP. A DHCP lease is also the cheapest end-to-end **RX** proof — OFFER
and ACK must arrive — which matters because RX is the one milestone QEMU
USB-passthrough could not reach (it can drive the control plane but not a live
wire), so it had to be validated on the physical laptop.

### Bare-metal bring-up: from black screen to login

A machine with no serial port and no QEMU model exposes every assumption earlier
phases baked in. Two were *unbounded hardware waits* — loops that spin forever
when a register never reaches the value QEMU always produces:

- **LAPIC timer calibration** spun on the PIT channel-2 **gate-output bit** (port
  `0x61` bit 5), which is dead on this laptop. Fix: poll the channel-2 *counter*
  for its terminal-count wrap instead, bounded by a spin budget with a sane-range
  clamp and a default ticks/ms fallback (`apic.rs::calibrate_lapic_timer`).
- **COM1 RX drain** had an unbounded `while LSR.data_ready` loop; a port-less
  machine reads `0xFF`, so data-ready is *stuck on* and the first timer tick after
  the APIC switch spins forever. Fix: bound the drain and bail on `lsr == 0xFF`
  (no UART present) (`serial.rs::drain_uart_rx_locked`).

The general pattern: **every poll of real hardware needs a bound and a "this
device isn't here" exit.** QEMU never gives you `0xFF`-forever, so these survive
until first silicon.

#### PS/2 keyboard — the fix was a userspace daemon, not the controller

The built-in keyboard is plain **PS/2** (`"AT Translated Set 2 keyboard"` on
`i8042`/`serio0`), *not* I2C-HID — the I2C-HID device on this laptop is the
touchpad. So no new driver was needed; `ps2.rs::init_keyboard()` just enables the
first 8042 port, clears `KBD_DISABLE`, sets `KBD_IRQ`, and sends `0xF4` (enable
scanning), preserving the firmware translation bit.

But enabling the controller wasn't enough — keystrokes still didn't reach the
shell. The actual gap: **`stdin_feeder`** (the daemon that pumps scancodes from
`kbd_server` into the console TTY) and **`usbhub`** were only in the data-disk
`KNOWN_CONFIGS`, not in the bare-metal `BUILTIN_CONFIGS` (the no-data-disk boot
path). The `[ps2] kbd cfg` diagnostic proved the controller side was already
perfect (`xlate=1 irq=1 dis=0 ack=0xfa`); the missing piece was the *pump*. The
lesson: on the bare-metal default-config path, every daemon the data-disk path
relies on must be mirrored into `BUILTIN_CONFIGS` or it silently never starts.

#### Framebuffer write-combining via PAT — and the PAT-bit index trap

On real hardware the bootloader maps the framebuffer **uncacheable**, so every
pixel write is a bus transaction — ~0.2 s per scrolled line, which both hides
output and throttles anything that logs (the SSH lockups). The fix is a true
**Write-Combining** memory type, which lets the CPU batch pixel writes into burst
transactions.

x86 has no WC memory type in the *default* PAT — the eight slots decode as
`[WB, WT, UC-, UC, WB, WT, UC-, UC]`. So `pat.rs` **reprograms PAT index 2**
(the slot selected by PCD-alone) from UC- to WC via the `IA32_PAT` MSR, then
remaps the framebuffer range to select it. PAT is **per-core** (the SDM requires
every CPU mapping a shared page to agree on its type), so `init()` runs on the
BSP and every AP; and WC is weakly ordered, so console writes need an `SFENCE` to
become visible.

The non-obvious part — surfaced during review — is **how a PAT index is
selected**. The index is a 3-bit number `(PAT << 2) | (PCD << 1) | PWT` built
from three *page-table-entry* bits. Setting PCD and clearing PWT only gets you
the low two bits; if the existing mapping already has the **PAT bit** set, the
index becomes 6 (UC-), not 2 (WC), and the upgrade silently no-ops. Worse, the
PAT bit's *position is leaf-size dependent*: **bit 7 in a 4 KiB PTE** but **bit
12 in a 2 MiB PDE**. The framebuffer is 4 KiB-mapped, where bit 7 is what the
`x86_64` crate calls `HUGE_PAGE`, so the remap clears it to force index 2. On a
2 MiB leaf bit 12 sits *inside* the crate's frame-address mask and
`update_flags` can't reach it — documented as relying on the (real) precondition
that the callers map with PAT=0. Takeaway: **selecting a PAT memory type means
pinning all three selector bits, and one of them moves with the page size.**

#### USB log persistence — and why `rename()` isn't available there

With no serial port, the boot log is unreadable live and scrolls away. The
solution ships it to a USB stick for post-mortem reading on the host:

- The boot stick is a **GPT image** = `[ESP boot] + [ext2 logs]`. The kernel
  mount path is partition-aware (`usb_ext2_base_lba`): it probes GPT (protective
  MBR + `EFI PART`), classic MBR, and whole-disk, finding the ext2 partition by
  the ext2 magic at `start + 2` so `/dev/usb0` resolves wherever the table places
  it.
- The **`usb-logsink`** daemon waits for `usb0.block`, mounts `/mnt/usb0`, and
  snapshots `/proc/kmsg` → `/mnt/usb0/boot.log` every few seconds with `fsync`.
  It's a *separate* process from `usb-storage` on purpose — a block server can't
  mount its own device (it would block in the mount waiting for itself).

A reliability subtlety (surfaced during review): a snapshot must never destroy
the *previous* good log. The ideal is write-temp-then-`rename` for an atomic
replace — but **`rename()` is not routed for `/mnt/usbN` mounts** (the
`sys_linux_rename` dispatcher only handles the ext2 *root* via `vfs_server` or
tmpfs; a USB-mount path falls through to `EROFS`). Verifying that *before*
depending on it mattered — a rename-based design would have silently broken log
persistence entirely. The workable fix uses only the syscalls that do work on
the mount: **read the whole bounded ring into memory first**, and only
`O_TRUNC`+rewrite `boot.log` once the complete snapshot is in hand — so a
`/proc/kmsg` read failure (the dominant failure on a flaky bare-metal USB/VFS)
leaves the prior snapshot untouched. The lesson: **confirm a syscall is actually
serviced on the target mount before architecting around it** — VFS routing is
per-mount, not universal.

#### stat-identity — a ramdisk file shadowing an ext2 inode

Embedding `/etc/passwd`/`group`/`shadow` in the ramdisk (so bare-metal login
works with no data disk) introduced a regression: the open path checks the
ramdisk *before* the ext2 root, so on a data-disk boot those three files shadowed
the real ext2 inodes and `fstat` returned `st_ino = 0`. Fix: `ramdisk_lookup`
**defers** those fallback paths to a mounted ext2 root when `ext2::is_mounted()`.
The lesson: a ramdisk floor that exists for one boot mode can silently override a
real filesystem in another — fallbacks must yield to the authoritative mount when
it's present.

## Key Files

| File | Role |
|---|---|
| `kernel-core/src/usb/enumerate.rs` | Bulk EP type constants + Configure-Endpoint context builders (host-tested) |
| `userspace/drivers/xhci/src/server.rs` | Live bulk `SubmitTransfer` consumer — Normal-TRB ring programming |
| `userspace/drivers/ure/` | The RTL8156 vendor NIC: OCP/PLA control plane, bulk RX/TX, `RemoteNic` registration |
| `kernel/src/arch/x86_64/apic.rs` | `calibrate_lapic_timer` — bounded PIT ch2 counter poll (no gate-bit assumption) |
| `kernel/src/serial.rs` | `drain_uart_rx_locked` — bounded RX drain, bail on `0xFF` (no UART) |
| `kernel/src/arch/x86_64/ps2.rs` | `init_keyboard` — enable the 8042 port + scanning for the built-in PS/2 keyboard |
| `kernel/src/arch/x86_64/pat.rs` | PAT-index-2 → WC reprogramming + per-leaf framebuffer remap (the PAT-bit-clear) |
| `userspace/init/src/main.rs` | `BUILTIN_CONFIGS` — mirrors `stdin_feeder`/`usbhub`/`usb-logsink` onto the bare-metal path |
| `userspace/usb-logsink/` | Post-mortem log persistence: mount `/mnt/usb0`, snapshot `/proc/kmsg` → `boot.log` |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `usb_ext2_base_lba` GPT/MBR partition probe; `sys_linux_rename` routing |
| `kernel/src/fs/ramdisk.rs` | `ramdisk_lookup` defers the `/etc` user-db fallback to a mounted ext2 root |

## How This Phase Differs From Production OSes

- **Linux `r8152.c`** is ~9,000 lines (NAPI, runtime PM, firmware patching, RSS,
  the full RTL815x matrix); `ure` targets the bring-up subset (submit/complete
  RX, no PM, no firmware patch) — closer to OpenBSD `ure(4)`.
- Production USB stacks stream RX with **multiple in-flight bulk URBs** and
  zero-copy DMA; `ure` uses a correctness-first submit/complete loop (the
  fire-and-forget TX fix is the minimum needed to not serialize to depth 1).
- Many RTL8156 dongles also expose a standards **CDC-ECM/NCM** interface; a
  teaching OS could implement only CDC-ECM (no vendor registers) at a throughput
  cost. This phase takes the vendor path because it's the chip's default and what
  BSD `ure` documents. (CDC-ECM is its own deferred generalization.)
- Real bring-up uses JTAG / a USB analyzer / a vendor debug UART; with none of
  those on the reference laptop, this phase substitutes **QEMU USB-passthrough**
  for in-the-loop control-plane iteration and **on-drive log persistence** for
  post-network observability — and accepts that the live RX datapath can only be
  proven on physical silicon.
- Production framebuffer drivers get WC from the firmware/GOP or a dedicated MTRR;
  reprogramming a PAT slot at runtime is the lightweight equivalent for a kernel
  that owns its own page tables.

## Related Roadmap Docs

- [Phase 96 design](./roadmap/96-bare-metal-usb-ethernet.md) — milestone goal,
  track layout, acceptance criteria.
- [Phase 96 task list](./roadmap/tasks/96-bare-metal-usb-ethernet-tasks.md).
- [Phase 96 session handoff](./96-bare-metal-network-handoff.md) — the raw
  working notes from the bare-metal bring-up sessions.
- [Phase 78 — USB host foundation](./78-usb-host-foundation.md) — the xHCI stack
  bulk extends.
- [Phase 79 — Modern NIC](./79-modern-nic.md) — the `RemoteNic` facade `ure`
  reuses.
- [Phase 92 — USB Class Expansion](./92-usb-class-expansion.md) — consumes this
  phase's bulk-endpoint substrate (mass storage, CDC-ECM).

## Deferred or Later-Phase Topics

- **CDC-ECM/NCM generic USB-NIC class driver** — a vendor-neutral path for
  non-Realtek dongles (lands in Phase 92e, on this bulk substrate).
- **USB keyboard in text mode** — `stdin_feeder` also draining `usb-hid`'s
  `KBD_EVENT_PULL` events (PS/2 works today; USB-HID text-mode input is the gap).
- **I2C-HID touchpad** (Intel LPSS DesignWare I2C + I2C-HID multitouch) — the
  pointer device GUI-on-real-hardware needs; a future bare-metal phase reusing the
  Track C bring-up tooling.
- **Intel AX201 / CNVi Wi-Fi** (OpenBSD `iwx(4)` reference) — a much larger future
  phase.
- **USB NIC offloads** (checksum/TSO/RSS), runtime power management, multi-URB RX
  pipelining — deferred throughput work.
- **Bring-up diagnostic cleanup** — the POST-square markers, AHCI-retry dots, and
  `[timer] lapic_ticks_per_ms` line are gated default-off (`BRINGUP_DIAG`), to be
  removed once boot stability is long-settled.
