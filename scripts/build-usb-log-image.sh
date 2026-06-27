#!/usr/bin/env bash
# build-usb-log-image.sh — assemble a single bootable USB image that carries
# BOTH the m3OS UEFI boot partition (ESP) AND a second ext2 "log" partition.
#
# Phase 96 / bare-metal log persistence. On the target laptop there is no serial
# port, so the kernel/driver log is unreadable live. m3OS's `usb-logsink` daemon
# mounts this ext2 partition at /mnt/usb0 and snapshots the kernel dmesg ring to
# /mnt/usb0/boot.log. After a boot, pull the stick, mount the SECOND partition on
# your host (it's plain ext2), and read boot.log.
#
# Usage:
#   scripts/build-usb-log-image.sh [--boot <boot-uefi-m3os.img>] [--out <img>] [--logs-mb N]
#
# Then flash the OUTPUT (not boot-uefi-m3os.img):
#   sudo dd if=<out> of=/dev/sdX bs=4M conv=fsync status=progress && sync
#
# Requires: sfdisk, mke2fs (e2fsprogs), truncate, dd — no root, operates on files.
set -euo pipefail

REL=target/x86_64-unknown-none/release
BOOT="$REL/boot-uefi-m3os.img"
OUT="$REL/m3os-usb-log.img"
LOGS_MB=128

while [ $# -gt 0 ]; do
  case "$1" in
    --boot)    BOOT="$2"; shift 2;;
    --out)     OUT="$2"; shift 2;;
    --logs-mb) LOGS_MB="$2"; shift 2;;
    -h|--help) grep '^#' "$0" | sed 's/^# \?//'; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

for t in sfdisk mke2fs truncate dd; do
  command -v "$t" >/dev/null || { echo "ERROR: missing tool '$t'" >&2; exit 1; }
done
[ -f "$BOOT" ] || { echo "ERROR: boot image not found: $BOOT (run 'cargo xtask image' first)" >&2; exit 1; }

# --- Parse the ESP partition geometry from the boot image's GPT --------------
# The boot image is a GPT disk with one ESP partition (the bootloader's output).
esp_line="$(sfdisk -d "$BOOT" | grep -E '^\S+1\s*:' | head -1)"
esp_start="$(sed -n 's/.*start=\s*\([0-9]\+\).*/\1/p' <<<"$esp_line")"
esp_size="$(sed -n 's/.*size=\s*\([0-9]\+\).*/\1/p' <<<"$esp_line")"
esp_type="$(sed -n 's/.*type=\s*\([0-9A-Fa-f-]\+\).*/\1/p' <<<"$esp_line")"
[ -n "$esp_start" ] && [ -n "$esp_size" ] || { echo "ERROR: could not parse ESP from $BOOT" >&2; exit 1; }
esp_end=$((esp_start + esp_size - 1))

# --- Layout: ESP, then a 1 MiB-aligned ext2 partition, then backup GPT -------
LINUX_FS_GUID="0FC63DAF-8483-4772-8E79-3D69D8477DE4"   # GPT type: Linux filesystem
ext2_sectors=$((LOGS_MB * 1024 * 1024 / 512))
ext2_start=$(( ((esp_end + 1 + 2047) / 2048) * 2048 ))  # 1 MiB-aligned
total_sectors=$(( ext2_start + ext2_sectors + 34 ))     # +34 for the backup GPT

echo "[build-usb-log-image] ESP: start=$esp_start size=$esp_size (end=$esp_end)"
echo "[build-usb-log-image] ext2 logs: start=$ext2_start size=$ext2_sectors (${LOGS_MB} MiB)"
echo "[build-usb-log-image] total: $total_sectors sectors ($((total_sectors / 2048)) MiB)"

# --- Assemble ----------------------------------------------------------------
cp -f "$BOOT" "$OUT"
truncate -s "$((total_sectors * 512))" "$OUT"

# Standalone ext2 image, then splice it into the partition region.
EXT2_TMP="$(mktemp --suffix=.ext2)"
trap 'rm -f "$EXT2_TMP"' EXIT
truncate -s "$((ext2_sectors * 512))" "$EXT2_TMP"
mke2fs -F -q -t ext2 -L m3os-logs "$EXT2_TMP"
dd if="$EXT2_TMP" of="$OUT" bs=512 seek="$ext2_start" conv=notrunc status=none

# Rewrite the GPT to describe both partitions (sfdisk recomputes CRCs + writes
# the primary and backup GPT for the new, larger disk).
sfdisk -q "$OUT" >/dev/null <<EOF
label: gpt
unit: sectors
first-lba: 34
last-lba: $((total_sectors - 34))
sector-size: 512

start=$esp_start, size=$esp_size, type=$esp_type, name="boot"
start=$ext2_start, size=$ext2_sectors, type=$LINUX_FS_GUID, name="m3os-logs"
EOF

echo
echo "[build-usb-log-image] wrote $OUT"
sfdisk -d "$OUT" | sed -n '/^label/,$p'
echo
echo "Flash it with:"
echo "  sudo dd if=$OUT of=/dev/sdX bs=4M conv=fsync status=progress && sync"
echo "After a boot, read the log on your host:"
echo "  sudo mount -o ro \$(sudo losetup -Pf --show $OUT)p2 /mnt && cat /mnt/boot.log   # (loopback test)"
echo "  …or mount the stick's 2nd partition (ext2) directly and 'cat boot.log'."
