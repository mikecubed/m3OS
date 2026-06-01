#!/usr/bin/env bash
# Phase 79 → 83: capture the EXACT MAC-OCP / GPHY-OCP register pokes Linux's
# r8169 driver performs while loading the RTL8125B PHY-MCU firmware, so they can
# be diffed against the m3OS driver's MAC-MCU writes.
#
# Why: the m3OS r8125 driver now runs the full firmware interpreter against the
# real PHY, but the firmware's MCU patch-acceptance poll (wait for status==0x40)
# times out — the streamed patch never takes effect. The authoritative
# (addr,value) write stream below is the reference to diff against the driver's
# `Nic::mac_ocp_write` output (instrument it to log "MACOCP <addr> <val>") to find
# exactly where the patch RAM diverges. See
# docs/research/r8125-phy-config-capture.md "Empirical finding #3".
#
# The firmware is (re)loaded on interface UP (rtl_open -> rtl8125b_hw_phy_config
# -> r8169_apply_firmware), so we trace while forcing a full re-init. enp11s0
# drops briefly — run from the Wi-Fi/.210 shell.  Usage:
#   sudo bash scripts/capture-rtl8125-firmware-writes.sh
set -u
DEV=0000:0b:00.0
IFACE=enp11s0
OUT=/tmp/rtl8125-firmware-writes.txt
log(){ echo "[fw-capture] $*"; }

if [ "$(id -u)" -ne 0 ]; then echo "must run as root: sudo bash $0"; exit 1; fi
command -v bpftrace >/dev/null || { echo "bpftrace not found"; exit 1; }

: > "$OUT"
log "attaching bpftrace to the raw OCP write/read path + firmware boundary..."
# __r8168_mac_ocp_write(tp, reg, data)  : arg1=reg(OCP addr) arg2=data  — the MCU patch RAM stream
# __r8168_mac_ocp_read (tp, reg)        : arg1=reg                       — patch-status polls
# r8168g_mdio_write   (tp, reg, val)    : arg1=reg arg2=val             — PHY-mode writes
# rtl_fw_write_firmware                  : brackets the whole apply
bpftrace -e '
kprobe:__r8168_mac_ocp_write { printf("MACOCP_W reg=0x%x data=0x%x\n", arg1, arg2); }
kprobe:__r8168_mac_ocp_read  { printf("MACOCP_R reg=0x%x\n", arg1); }
kprobe:r8168g_mdio_write     { printf("PHY_W reg=0x%x val=0x%x\n", arg1, arg2); }
kprobe:rtl_fw_write_firmware { printf("=== FW_BEGIN ===\n"); }
kretprobe:rtl_fw_write_firmware { printf("=== FW_END ===\n"); }
' >> "$OUT" 2>>"$OUT" &
BT=$!
sleep 3   # let probes attach

log "down/unbind/rebind/up to force a full firmware re-apply (enp11s0 drops now)..."
ip link set "$IFACE" down 2>/dev/null || true
sleep 1
echo "$DEV" > /sys/bus/pci/drivers/r8169/unbind 2>/dev/null || true
sleep 2
echo "$DEV" > /sys/bus/pci/drivers/r8169/bind 2>/dev/null || true
sleep 2
ip link set "$IFACE" up 2>/dev/null || true   # triggers rtl_open -> firmware load
sleep 6

kill "$BT" 2>/dev/null
wait "$BT" 2>/dev/null
networkctl reconfigure "$IFACE" 2>/dev/null || true
sleep 2

log "capture complete -> $OUT"
log "MAC-OCP writes:        $(grep -c '^MACOCP_W ' "$OUT")"
log "MAC-OCP reads:         $(grep -c '^MACOCP_R ' "$OUT")"
log "PHY writes:            $(grep -c '^PHY_W ' "$OUT")"
log "firmware windows:      $(grep -c FW_BEGIN "$OUT")"
echo "----- the MAC-OCP writes *inside* the firmware window (the MCU patch stream) -----"
awk '/FW_BEGIN/{f=1} f&&/^MACOCP_W /{print} /FW_END/{f=0}' "$OUT" | head -60
echo "----- enp11s0 now -----"
ip -br addr show "$IFACE" 2>/dev/null
