# RTL8125B PHY-config + firmware capture (Phase 79 → 83 reference)

> **RESOLVED (2026-06): the literal `ping` works over the physical RTL8125B,
> without firmware.** `R8125_LIVE: PASS — ICMP reply from 192.168.1.254 over the
> REAL RTL8125`, reproduced across runs. The blocker was never the firmware (a
> PHY-MCU *tuning* patch — the card links/RXes without it): it was two MAC
> register-sequencing bugs found by reading the chip's registers back —
> **(1)** the 8125 drops `ChipCmd RxEnb|TxEnb` if asserted while the link is down,
> so the engines must be **re-enabled after link-up**; **(2)** the 8125 TX
> doorbell is a 16-bit register at **`0x90`**, not the classic `TxPoll` (`0x38`),
> so posted TX frames were never transmitted. Fixed in
> `feat(r8125): real-silicon ping WORKS`. The firmware-loader material below
> remains a valid Phase-83 reference for *enabling the firmware path* (perf/EEE
> tuning), but it is **not** required for basic link/RX/ping.

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

## Empirical finding (real RTL8125B, 2026-06): link is the blocker, and PHYAR is the *wrong* MDIO window

Tested against the physical card under VFIO. After full m3OS bring-up
(claim → BAR map → soft reset → ring program → RX/TX enable) plus a classic
`PHYAR` (`0x60`) **BMCR autoneg-restart** (`0x9240`), the MAC's `PHYstatus`
(`0x6C`) still reads **link-down**, and `ping` gets no reply. Two conclusions:

1. **The blocker is link, not the datapath.** With no link there are no RX
   frames to drain, so the V2-interrupt RX path can't be the *first* thing to
   fix — the PHY must negotiate first.
2. **The RTL8125 does not use the `PHYAR` MDIO window.** The 8168 reaches its
   PHY through `PHYAR` (`0x60`); the 8125 reaches its PHY through a **GPHY-OCP**
   window (the same MAC-OCP mechanism used for the `0xC000–0xFFFF` MAC regs,
   addressed into the PHY/GPHY range). The autoneg-restart above was therefore a
   no-op on the 8125 — it never reached the PHY. This is the concrete missing
   mechanism for the port.

### Remaining Phase-83 work to get a real `ping`

1. Implement the **GPHY-OCP MDIO** accessor for the 8125 (transcribe
   `r8168g_mdio_write` / `r8168g_mdio_read` from Linux `r8169_main.c` — they
   encode the PHY register into an OCP address and poke `OCPDR`/`PHYOCP`). The
   captured `PM page/reg` and `W reg` entries above are addressed *through this
   window*, not raw `PHYAR`.
2. Replay the ordered config (the 13 paged PHY modifies + 26 MAC-OCP modifies)
   over that accessor.
3. Apply `rtl8125b-2.fw` via `kernel_core::r8169::{parse_rtl_fw, run_phy_action}`
   over the OCP `PhyActionSink`.
4. Issue BMCR `0x9240` **over the OCP window**, wait ~2–5 s for autoneg, then
   re-check `PHYstatus` (`0x6C`) for link-up before expecting `ping` to reply.
5. Only after link-up does the V2-interrupt RX datapath become the next thing
   to verify.

## Empirical finding #2 (real RTL8125B, 2026-06): bring-up + TX work; RX needs the MCU-patch protocol

A second live VFIO session implemented all of items 1–5 above and pushed the
datapath much further. What now works on the physical card (serial-confirmed):

* **GPHY-OCP + MAC-OCP accessors** reach the PHY: `phy_id(ocp)=0x001cc840` (a
  real Realtek PHY ID). Implemented in `r8169_hal::init` over the host-tested
  `kernel_core::r8169` OCP command-word encoder; PHYAR is confirmed a no-op.
* **Link up** via a GPHY-OCP `BMCR 0x9240` (no firmware) — the minimal path.
* **Service registration**: the driver wins the single-holder `net.nic` race
  (after making bring-up non-blocking so it registers before any emulated NIC).
* **Kernel binding with the real MAC**: the driver reads `MAC0`
  (`34:5a:60:16:77:c6`) and publishes `NET_LINK_STATE`, which bootstraps the
  kernel `RemoteNic` registration — without this the kernel had
  `00:00:00:00:00:00` and never routed TX out to the driver.
* **TX to the wire**: the kernel pushes the ARP request (42 B) and the driver
  transmits it (`first TX frame sent, len=42`).
* **Polled RX datapath**: the V2 loop now non-blocking-polls the command
  endpoint (`IpcBackend::try_recv` / `NetServer::try_handle_next`) and drains
  the RX ring every iteration, so RX no longer depends on INTx delivery — which
  is unreliable for a VFIO passthrough device.
* **8125 RX/TX engine config**: `RxConfig` fetch-default + DMA-burst, `TxConfig`
  burst/IFG, the **RXDV gate** clear (`MISC` bit 19), and the 26-entry MAC-OCP
  block — none of which the classic 8169 bring-up does.

What still blocks RX (and therefore `ping`):

* **The PHY-MCU firmware does not relink the PHY.** The firmware loader runs the
  full 200-instruction patch (`firmware applied, steps=0x…`, varying with the
  reads it branches on) and demonstrably reaches the PHY — applying it flips the
  link from **up** to **down** and the captured 13 PHY modifies + `BMCR 0x9240`
  do not bring it back. The missing piece is the **8125 MCU-patch load
  protocol**: the enable/disable bracketing around `rtl_fw_write_firmware` that
  Linux performs via MAC-OCP writes which this capture never traced (the
  `FW_BEGIN/FW_END` window showed no MDIO writes). Without that bracket the patch
  is loaded into an MCU that is not in patch-accept state, so it misapplies and
  the PHY cannot relink.
* Because the firmware misapplies and link drops, RX never receives — the polled
  RX path is correct but has nothing to drain. With **no** firmware the link is
  up and TX works, but the 8125 RX engine stays inert (the documented "8125
  requires the signed PHY blob"), so RX is dead either way until the firmware
  applies *correctly*.

### Precise remaining work (the firmware path is built; only this is missing)

1. Transcribe the **8125 MCU-patch load wrapper** from Linux
   (`rtl8125_hw_mac_mcu_config` / the `rtl_fw_write_firmware` call site for the
   8125): the MAC-OCP register pokes that put the PHY MCU into patch-accept mode
   before the blob and restore/commit it after. Wrap `Nic::apply_firmware` with
   those.
2. Re-verify on hardware that `PHYstatus` (`0x6C`) returns to link-up *after* the
   firmware (it currently does not), then that `first RX frame(s) drained`
   appears (the polled RX path is already in place), then `ping` reply.
3. Possibly fold in any remaining `rtl_hw_start_8125` register writes beyond the
   captured 26-entry OCP block (interrupt mitigation, descriptor mode) if RX is
   still dry once link holds.

Everything except step 1's MCU bracket is implemented and committed
(`r8169_hal::init`: GPHY/MAC-OCP accessors, `apply_firmware`/`FwSink`,
`phy_config_8125`, the 13-PM and 26-OCP tables; `kernel_core::r8169`: the OCP
encoders + `parse_rtl_fw`/`run_phy_action`, host-tested). The firmware blob
itself is **not** vendored (linux-firmware licensing); staging it is the
coordinator's E.2 responsibility, and `firmware_blob()` is the single seam.

## Empirical finding #3 (real RTL8125B, 2026-06): firmware runs correctly but the MCU-patch-status poll times out

The complete `rtl8125b_hw_phy_config` (firmware → `enable_gphy_10m` → paged
modifies → the 3 direct-OCP `phy_param` writes at `0xB87C`/`0xB87E` → the 10
paged `phy_param` writes in page `0xa43` → EEE/ALDPS → autoneg) is now
implemented verbatim, and a real **interpreter bug was fixed**: the `BJMPN`
back-jump was off by one (Linux's action loop is `for(index=0;…;index++)`, so the
net target is `index - regno + 1`, not `index - regno`). After the fix the
firmware executes its back-jump loops correctly (step count rose from ~240 to
**694**). Link still does **not** come up after the firmware.

Decoding `rtl8125b-2.fw`'s control flow explains why and pinpoints the gap:

```
idx   0  MDIO_CHG data=1     # switch to MAC-MCU register space
idx 1..125 WRITE …           # stream the MCU patch into RAM at 0xfc20 + reg
idx 126  MDIO_CHG data=0     # back to PHY register space
idx 127..133 WRITE/DELAY     # arm/kick the patch
idx 134  READ                # \
idx 136  COMP_EQ regno=2 d=0x40   #  poll: exit when the status reg == 0x40
idx 137  RC_EQ_SKIP d=0x64        #  timeout after count==100 reads
idx 138  BJMPN regno=5 -> 134     # /  loop back
```

The trailing loop is the MCU **patch-acceptance handshake**: load the patch, then
poll a status register until it reads `0x40` ("patch ran"), bailing after 100
reads. The 694-step run is that loop **timing out** — the status never reaches
`0x40`, i.e. the MCU never accepts/executes the streamed patch. So the remaining
gap is firmware-application *fidelity*: either the MAC-MCU patch-RAM writes
(idx 1..125, via `mac_ocp_write(ocp_base + reg)`) land at the wrong addresses, or
the arm/kick at idx 127..133 needs additional state the trace never captured.

### Update: the gap is the PHY-MCU patch-request handshake (bit 6 vs bit 7)

Deeper instrumentation localized the timeout exactly. The trailing loop is the
vendor driver's `rtl8125_set_phy_mcu_patch_request`: it writes `0xB820` bit
`0x10` (idx 128–130) and polls `0xB800` for bit `0x40` (idx 134–138, "patch RAM
ready"). On the real card the driver now:

* loads the MAC-MCU patch correctly — a scratch `0xF800` round-trip
  (`0xa5a5`/`0x5a5a`) reads back exactly, proving the MAC-OCP write path;
* starts the MAC before the firmware (Linux `rtl_hw_start` precedes `phy_start`);
* acquires the PHY-MCU patch key before the blob
  (`0xA436=0x8024; 0xA438=0x3701; 0xB82E=0x0001`).

After the key, `0xB800` reads **`0x0080`** — bit **7** set, not the bit **6**
(`0x40`) the blob's poll waits for. So the key *does* elicit a response from the
PHY MCU, but the ready bit it reports differs from what the blob polls.

The blob is **not** the wrong variant: the host carries
`/lib/firmware/rtl_nic/rtl8125b-2.fw.zst` and the `r8169` driver links this exact
card at **1000 Mbps**, so `rtl8125b-2.fw` is the blob Linux uses. The remaining
candidates are therefore (a) the patch key value/sequence, or (b) a missing
precondition before `set_phy_mcu_patch_request` (e.g. the order of the MAC-MCU
patch vs. the request, or a halt/clock step). All are settled by the reference
trace of Linux's actual write stream.

Note also that the firmware is a PHY-MCU *tuning* patch — the card links without
it (the m3OS driver reaches link via a bare `BMCR 0x9240`, and Linux links at
1G). So the firmware is likely **not** the gate for a basic `ping`: the more
probable RX gate is an incomplete `rtl_hw_start_8125` MAC datapath bring-up (the
captured 26 MAC-OCP modifies are only the `mac_ocp_modify` calls; `rtl_hw_start`
also issues many plain `RTL_W8/16/32` register writes — interrupt mitigation,
descriptor mode, MTPS, etc. — that the OCP-only trace never saw). The committed
default (`firmware_blob() = None`) already links + transmits; closing RX most
likely means transcribing the full `rtl_hw_start_8125`, which the reference trace
(extended to the plain register writes) would capture.

### Definitive next step (Phase 83)

`bpftrace` the **exact register pokes** Linux performs during *this blob's* load —
`kprobe:__r8168_mac_ocp_write { printf("MACOCP %x %x\n", arg1, arg2) }` and
`kprobe:__r8168_mac_ocp_read`, plus the GPHY-OCP `r8168g_mdio_write` — across a
single `ip link set <if> up` (which triggers `rtl_fw_write_firmware`). Diff that
authoritative `(addr, value)` stream against the driver's MAC-MCU writes
(instrument `Nic::mac_ocp_write` to log the same) to find exactly where the patch
RAM diverges. Everything upstream (link, registration, real-MAC binding, TX,
polled RX, the OCP accessors, the interpreter, the complete PHY config) is
implemented, committed, and host-tested; only this last fidelity diff remains.

## Notes / open questions

* The OCP block is mostly MAC feature/power setup (EEE, ASPM, ALDPS, jumbo,
  LED). Basic link + `ping` may need only the BMCR power-up + autoneg-restart
  (`0x9240`) and the firmware; the OCP/PM tuning likely affects stability/perf,
  not basic link — to be confirmed on hardware.
* The firmware section (`FW_BEGIN`/`FW_END`) showed no traced MDIO writes — the
  MCU patch is applied via the MAC-OCP write path (untraced here); apply it from
  the blob via the interpreter.
