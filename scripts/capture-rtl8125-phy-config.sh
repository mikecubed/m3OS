#!/usr/bin/env bash
# Phase 79 v2 — capture the RTL8125B PHY-config + firmware write sequence Linux
# applies to THIS chip. Linux runs the PHY config + firmware on interface *up*
# (rtl_open), so we trace while forcing a full re-init: unbind/rebind + link up.
# Read-only tracing; enp11s0 (.222) drops briefly — run from the Wi-Fi (.210)
# shell. Usage:  sudo bash capture-phy-config.sh
set -u
DEV=0000:0b:00.0
IFACE=enp11s0
OUT=${OUT:-/tmp/rtl8125_phy_capture.txt}
log(){ echo "[capture] $*"; }

if [ "$(id -u)" -ne 0 ]; then echo "must run as root: sudo bash $0"; exit 1; fi
command -v bpftrace >/dev/null || { echo "bpftrace not found"; exit 1; }

: > "$OUT"
log "starting bpftrace (PHY writes via two paths + firmware boundary + OCP)..."
# Intent-level tracing — these functions are called unconditionally for every
# register in the config, so the sequence is COMPLETE even when the inner MDIO
# write is skipped as a no-op (the PHY keeps its config across rebind).
#   phy_write_paged(phydev, page, reg, val):           arg1=page arg2=reg arg3=val
#   phy_modify_paged(phydev, page, reg, mask, set):    arg1=page arg2=reg arg3=mask arg4=set
#   __phy_modify(phydev, reg, mask, set):              arg1=reg arg2=mask arg3=set (unpaged)
#   r8168_mac_ocp_modify(tp, reg, mask, set):          arg1=reg arg2=mask arg3=set
#   r8169_mdio_write_reg(bus, phyaddr, reg, val):      arg2=reg arg3=val (raw, page-0 BMCR etc.)
bpftrace -e '
kprobe:phy_write_paged       { printf("PW page=0x%x reg=0x%x val=0x%x\n", arg1, arg2, arg3); }
kprobe:phy_modify_paged      { printf("PM page=0x%x reg=0x%x mask=0x%x set=0x%x\n", arg1, arg2, arg3, arg4); }
kprobe:__phy_modify          { printf("M reg=0x%x mask=0x%x set=0x%x\n", arg1, arg2, arg3); }
kprobe:r8168_mac_ocp_modify  { printf("OCPMOD reg=0x%x mask=0x%x set=0x%x\n", arg1, arg2, arg3); }
kprobe:r8169_mdio_write_reg  { printf("W reg=0x%x val=0x%x\n", arg2, arg3); }
kprobe:rtl_fw_write_firmware { printf("FW_BEGIN\n"); }
kretprobe:rtl_fw_write_firmware { printf("FW_END\n"); }
' >> "$OUT" 2>>"$OUT" &
BT=$!
sleep 3   # let probes attach

log "down/unbind/rebind/up to force a full PHY re-init (enp11s0 drops now)..."
ip link set "$IFACE" down 2>/dev/null || true
sleep 1
echo "$DEV" > /sys/bus/pci/drivers/r8169/unbind 2>/dev/null || true
sleep 2
echo "$DEV" > /sys/bus/pci/drivers/r8169/bind 2>/dev/null || true
sleep 3
# Explicitly bring the link up — this is what triggers rtl_open -> phy config + fw.
ip link set "$IFACE" up 2>/dev/null || true
sleep 8

kill "$BT" 2>/dev/null
wait "$BT" 2>/dev/null

networkctl reconfigure "$IFACE" 2>/dev/null || true
sleep 2

log "capture complete -> $OUT"
log "PHY writes (mdio_write_reg): $(grep -c '^W ' "$OUT")"
log "PHY writes (phy_write_paged): $(grep -c '^PW ' "$OUT")"
log "OCP modifies:                $(grep -c '^OCPMOD ' "$OUT")"
log "firmware sections (FW_BEGIN):$(grep -c FW_BEGIN "$OUT")"
echo "----- first 30 lines -----"
head -30 "$OUT"
echo "----- enp11s0 now -----"
ip -br addr show "$IFACE" 2>/dev/null
