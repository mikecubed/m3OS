# Handoff — Bare-metal boot rescue, USB log persistence, and keyboard (2026-06-25 overnight)

**Target:** Dell Precision 5560 (Tiger Lake), no serial port. Branch work sits on
top of `origin/docs/96-bare-metal-usb-ethernet` (`75808fb`). Pushed to feature
branch `feat/96-usb-log-persistence-keyboard`.

## TL;DR of the session

Started from a **black-screen early-boot hang** on bare metal; ended with a
**full boot to login + the USB mass-storage de-risk PASS**, plus log-persistence
plumbing and a keyboard fix ready to validate.

Two latent **unbounded-hardware-wait** kernel bugs were the boot hang (both only
bite when real silicon ≠ QEMU):

1. **LAPIC timer calibration** spun forever on the PIT **channel-2 gate-output
   bit (port 0x61 bit 5)**, which is dead on this laptop.
   Fix (`kernel/src/arch/x86_64/apic.rs::calibrate_lapic_timer`): poll the
   channel-2 **counter** for its terminal-count wrap instead, bounded
   (`PIT_SPIN_BUDGET=500_000`), with a sane-range clamp + 6250 ticks/ms default
   fallback.
2. **COM1 RX drain** (`kernel/src/serial.rs::drain_uart_rx_locked`) had an
   unbounded `while LSR.data_ready` loop; a port-less laptop reads `0xFF` →
   data-ready stuck on → infinite spin on the first timer tick after the APIC
   switch. Fix: bound to 64 + bail on `lsr == 0xFF` (no UART). This was the
   regression vs. the old 86e kernel (the SMP `serial_rx_backstop` that calls it
   is newer than 86e).

Diagnosis tooling added (kept, default-quiet): POST-square markers through the
post-fb-console boot region (`post_marker(6..15)` in `lib.rs`, `24..28` in
`apic.rs`), an init AHCI-retry progress `.` indicator, and a `[timer]
lapic_ticks_per_ms=N` framebuffer line just before the scheduler.

## What landed (all compile + `cargo xtask check` clean)

### 1. Log-spam reduction (perf + readability)
The uncached bare-metal framebuffer makes every log line ~0.2 s, which both
hides useful output and throttles traffic (the SSH lockups). Gated behind
default-off flags:
- `kernel/src/net/dhcp.rs`: `FB_NET_HEARTBEAT=false` gates the `[net]` fb heartbeat
  (the `log::info!` copy still reaches the dmesg ring/drive).
- `userspace/drivers/ure/src/{main,net}.rs`: `VERBOSE=false` gates `ure: hb` /
  `ure: rx` / RX-kick / TX-fail.
- `userspace/drivers/xhci/src/main.rs`: `VERBOSE_ENUM=false` gates per-step
  enumeration; **keeps** the one-line `[xhci] surfaced device vid/pid/class`.

### 2. A) USB log-storage persistence  — IMPLEMENTED + validated (not yet end-to-end on HW)
Goal: read the boot log off the drive afterward (screenshots/SSH impractical).
- **Kernel partition-aware USB mount** (`syscall/mod.rs::usb_ext2_base_lba`,
  wired into `sys_linux_mount`'s `/dev/usbN` path): probes GPT (protective MBR +
  `EFI PART`), classic MBR, and whole-disk; finds the ext2 partition's
  `base_lba` by the ext2 magic at `start+2`. The kernel's `mount_usb` already
  took a `base_lba`, so this is the only change needed.
- **`usb-logsink` daemon** (`userspace/usb-logsink/`, new crate, wired into
  workspace + xtask bins + ramdisk `/bin/usb-logsink` + `init::add_builtin_defaults`):
  waits for `usb0.block`, mounts `/mnt/usb0`, snapshots `/proc/kmsg` →
  `/mnt/usb0/boot.log` every 3 s with `fsync`. Separate process from
  `usb-storage` on purpose (a block server can't mount its own device).
- **Combined image builder** (`scripts/build-usb-log-image.sh`): assembles a
  single GPT image = `[ESP boot] + [ext2 m3os-logs]` from `boot-uefi-m3os.img`
  using `sfdisk`/`mke2fs`/`dd` (no root). Output: `m3os-usb-log.img`.

**Validated:** `usb-mount-smoke` PASS (whole-disk `base_lba=0`, no regression);
the GPT-parse offsets validated byte-for-byte against the real `m3os-usb-log.img`
(finds ext2 @ LBA 32768). **Not yet validated:** the end-to-end `usb-logsink`
run on the builtin-defaults (no-data-disk) path — needs a bare-metal reflash or
a new no-data-disk QEMU harness.

### 3. B) USB keyboard
Bare-metal-only (works in QEMU; usb-hid claim path is robust for boot + report
protocol). Made it **diagnosable**: spam reduction means the always-on
`[xhci] surfaced device …` + usb-hid bind lines are now readable. Need the
morning boot log to pinpoint (did it enumerate? class=3? bound?). Likely behind
the dock/hub since all USB ports are full → check tier-2/`usbhub`.

### 4. C) Built-in keyboard — ROOT-CAUSED on this hardware
**It is PLAIN PS/2, not I2C.** Local probe (`/proc/bus/input/devices`,
`/sys/bus/serio`, `/proc/interrupts`): `"AT Translated Set 2 keyboard"` on
`isa0060/serio0` → `/devices/platform/i8042/serio0` (atkbd, PNP0303), IRQ1 on
I/O-APIC edge (2253 ints under Linux), **no i8042 quirks** in the kernel cmdline.
(The I2C-HID `DLL0945`/Elan `04F3:311C` on `i2c_designware.1` is the TOUCHPAD.)
So **no I2C-HID keyboard driver is needed.** The kernel only ever ran
`init_mouse` and assumed firmware left the keyboard enabled. Fix added:
`kernel/src/arch/x86_64/ps2.rs::init_keyboard()` — enable the 1st 8042 port,
clear `KBD_DISABLE`, set `KBD_IRQ`, send `0xF4` (enable scanning), preserving the
firmware translation bit (→ set-1 scancodes). Wired into `lib.rs` before
`init_mouse`. IRQ1 is already routed to vector 33 by `apic::init`.

## MORNING STEPS (bare-metal validation)

1. `cargo xtask image` (already built tonight, but rebuild to be safe).
2. `scripts/build-usb-log-image.sh --logs-mb 128` → produces
   `target/x86_64-unknown-none/release/m3os-usb-log.img`.
3. Flash the **combined** image (NOT boot-uefi): `sudo dd if=…/m3os-usb-log.img
   of=/dev/sdX bs=4M conv=fsync status=progress && sync`.
4. Boot. Expect:
   - **Keyboard works now** (PS/2 init). If it does → built-in keyboard DONE.
   - `usb-logsink: /mnt/usb0 mounted` then `boot.log written` on screen.
5. Power off, pull the stick, on the host:
   `sudo mount -o ro /dev/sdX2 /mnt && cat /mnt/boot.log`  (partition 2 = ext2)
   — that's the full kernel dmesg, captured off the drive.
6. Read the now-visible `[timer] lapic_ticks_per_ms=N` (couldn't be seen tonight,
   scrolled off). Sane ≈ 1.5k–60k; `6250` = the default fallback (means PIT ch2
   didn't count → calibration is approximate; revisit if timing feels off).

## Open / caveats

- **`cargo xtask smoke-test` FAILS** on `SMOKE:stat-identity:FAIL fstat st_ino is
  zero` + `ext2-coherence`. **Not touched by this work** (no ext2/`fill_stat`
  changes; the static stat-assembly gate passes; `usb-mount-smoke` ext2 ops pass).
  Likely a stale data disk / pre-existing environmental flake on this box —
  verify with `cargo xtask clean` + a clean baseline before blaming this branch.
- **SSH stability/perf** still unsolved (uncached framebuffer / W^X). The spam
  reduction + on-drive logging should reduce reliance on SSH for debugging.
  Root lever remains write-combining (PAT) for the framebuffer (still deferred).
- Diagnostic markers + the AHCI-retry dots + the `[timer]` line are still in;
  clean them up (or gate) once the boot is confirmed stable.
- `usb-logsink` is only in `add_builtin_defaults` (bare-metal path), deliberately
  NOT in `KNOWN_CONFIGS`, so it doesn't perturb the QEMU smoke gates. If you want
  it on data-disk boots too, add `/etc/services.d/usb-logsink.conf`.

## ✅ COMPLETED (2026-06-26) — bare-metal bring-up landed; Phase 96 closed

Validated on the real laptop and merged to `docs/96-bare-metal-usb-ethernet`
(commits `fc93b7d`→`30657e1`→`ae01ed4`→`7c77288`):

- **Built-in (PS/2) keyboard WORKS** — root cause was `stdin_feeder` (the
  scancode→TTY pump) **and** `usbhub` missing from `add_builtin_defaults`
  `BUILTIN_CONFIGS` (they were only in the data-disk `KNOWN_CONFIGS`). Added both.
  The `[ps2] kbd cfg` diagnostic confirmed the controller side was already perfect
  (`xlate=1 irq=1 dis=0 ack=0xfa`); the gap was purely the missing pump.
- **USB log persistence WORKS end-to-end** — `flasher.sh` had been flashing the
  plain `boot-uefi-m3os.img` (no ext2 partition); the combined `m3os-usb-log.img`
  mounts at `/mnt/usb0` and `usb-logsink` persists `boot.log`. Read off the drive
  repeatedly this session. The original goal — read the boot log off the drive — is met.
- **Network validated on bare metal** — `[remote_nic] up=true 2500Mbps` +
  `[dhcp] bound ip=192.168.1.221`. **Closes Phase 96's RX milestone** (the
  passthrough-blocked datapath; a DHCP lease requires RX). `lapic_ticks_per_ms=2411`
  (sane — PIT-ch2 calibration solid on this CPU).
- **Framebuffer write-combining** (new `kernel/src/arch/x86_64/pat.rs`) — the
  "Root lever" deferred above is **done**: PAT index 2 → WC, the FB remapped, +
  per-core PAT + an SFENCE on console writes. Console is now fast on bare metal.
- **stat-identity smoke failure FIXED** (`ae01ed4`) — was NOT a stale disk; a real
  Phase 96 regression (ramdisk `/etc/passwd`/`group`/`shadow` shadowing the ext2
  root). `ramdisk_lookup` now defers those to a mounted ext2 root.

**Remaining (now tracked elsewhere, not Phase 96 blockers):** USB keyboard in
text mode (`stdin_feeder` to also drain usb-hid's `KBD_EVENT_PULL` events); the
`usb-hid`/`usbhub` CPU-hog busy-poll; bring-up-diagnostic cleanup (POST markers /
AHCI dots / `[timer]` line); GUI mode (needs a pointer — the I2C-HID touchpad
driver, a future phase). The `dlopen-test-smoke` TCG stall is **Phase 97**
(`2026-06-26-dlopen-smoke-tcg-stall.md`). The GUI-on-real-hardware roadmap
(trackpad, Wi-Fi, …) + a phase-quality audit are proposed as **Phase 98**.
