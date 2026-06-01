# RTL8125B PHY-config + firmware capture (Phase 79 → 83 reference)

Empirically captured PHY/MAC bring-up sequence that Linux's `r8169` driver
applies to a physical **RTL8125B** (`10ec:8125` rev 05, firmware `rtl8125b-2`),
for porting into the m3OS `r8125` driver. Captured by tracing the live driver's
intent-level config functions during a device rebind+link-up
(`scripts/capture-rtl8125-phy-config.sh`), so the sequence is complete even
though the PHY keeps its state across a rebind (no-op writes are still traced
because the *functions* run unconditionally).

## Access mechanisms (to implement in the m3OS driver)

* **Paged PHY MDIO** via `PHYAR` (`0x60`): write = `0x8000_0000 | (reg<<16) |
  val`, poll bit 31 clear; read = `reg<<16`, poll bit 31 set, value = low 16.
  A *paged* write is: `mdio_write(0x1f, page)`; `mdio_write(reg, val)`;
  `mdio_write(0x1f, 0)`. A *paged modify* reads, applies `(v & ~mask) | set`,
  writes back, inside the page window.
* **MAC OCP** (regs `0xC000–0xFFFF`) via the MAC-OCP register window
  (`r8168_mac_ocp_write/_modify` in Linux `r8169_main.c` — transcribe the exact
  `OCPDR/OCPAR` poke from the GPL source).
* **PHY-MCU firmware**: drive the decompressed `rtl8125b-2.fw` blob through
  `kernel_core::r8169::{parse_rtl_fw, run_phy_action}` (already built + host
  tested) over the MDIO/OCP `PhyActionSink`.

## Ordered config (one init pass)

```
W   reg=0x00 val=0x1840                 # BMCR: reset/power-down phase
OCPMOD reg=0xc0ac mask=0x1f80 set=0x0
OCPMOD reg=0xe8de mask=0x4000 set=0x0
W   reg=0x00 val=0x1040                 # BMCR
FW  (apply rtl8125b-2.fw MCU patch)
# --- 13 paged PHY modifies (page, reg, mask, set) ---
PM page=0xa44 reg=0x11 mask=0x000  set=0x800
PM page=0xac4 reg=0x13 mask=0x0f0  set=0x090
PM page=0xad3 reg=0x10 mask=0x003  set=0x001
PM page=0xbf0 reg=0x10 mask=0xe000 set=0xa000
PM page=0xbf4 reg=0x13 mask=0xf00  set=0x300
PM page=0xa4c reg=0x15 mask=0x000  set=0x040
PM page=0xbf8 reg=0x12 mask=0xe000 set=0xa000
PM page=0xa5b reg=0x12 mask=0x8000 set=0x000
PM page=0xa43 reg=0x10 mask=0x004  set=0x000
PM page=0xa6d reg=0x12 mask=0x001  set=0x000
PM page=0xa6d reg=0x14 mask=0x010  set=0x000
PM page=0xa42 reg=0x14 mask=0x080  set=0x000
PM page=0xa4a reg=0x11 mask=0x200  set=0x000
W   reg=0x00 val=0x9240                 # BMCR: autoneg enable + restart + power up
# --- 26 MAC-OCP modifies (reg, mask, set) ---
OCPMOD reg=0xe092 mask=0xff   set=0x0
OCPMOD reg=0xd40a mask=0x10   set=0x0
OCPMOD reg=0xd3e2 mask=0xfff  set=0x3a9
OCPMOD reg=0xd3e4 mask=0xff   set=0x0
OCPMOD reg=0xe860 mask=0x0    set=0x80
OCPMOD reg=0xeb58 mask=0x1    set=0x0
OCPMOD reg=0xe614 mask=0x700  set=0x200
OCPMOD reg=0xe63e mask=0xc30  set=0x0
OCPMOD reg=0xc0b4 mask=0x0    set=0xc
OCPMOD reg=0xeb6a mask=0xff   set=0x33
OCPMOD reg=0xeb50 mask=0x3e0  set=0x40
OCPMOD reg=0xe056 mask=0xf0   set=0x0
OCPMOD reg=0xe040 mask=0x1000 set=0x0
OCPMOD reg=0xea1c mask=0x3    set=0x1
OCPMOD reg=0xe0c0 mask=0x4f0f set=0x4403
OCPMOD reg=0xe052 mask=0x80   set=0x68
OCPMOD reg=0xd430 mask=0xfff  set=0x47f
OCPMOD reg=0xea1c mask=0x4    set=0x0
OCPMOD reg=0xeb54 mask=0x0    set=0x1
OCPMOD reg=0xeb54 mask=0x1    set=0x0
OCPMOD reg=0xe040 mask=0x0    set=0x3
OCPMOD reg=0xc0ac mask=0x0    set=0x1f80
OCPMOD reg=0xe094 mask=0xff00 set=0x0
OCPMOD reg=0xe092 mask=0xff   set=0x4
```

## Notes / open questions

* The OCP block is mostly MAC feature/power setup (EEE, ASPM, ALDPS, jumbo,
  LED). Basic link + `ping` may need only the BMCR power-up + autoneg-restart
  (`0x9240`) and the firmware; the OCP/PM tuning likely affects stability/perf,
  not basic link — to be confirmed on hardware.
* The firmware section (`FW_BEGIN`/`FW_END`) showed no traced MDIO writes — the
  MCU patch is applied via the MAC-OCP write path (untraced here); apply it from
  the blob via the interpreter.
* After config, link-up + `ping` still depend on the V2-interrupt RX datapath
  draining real frames — verify link state (`PHYstatus` reg `0x6C`) first to see
  whether the blocker is link (this config) or datapath.
