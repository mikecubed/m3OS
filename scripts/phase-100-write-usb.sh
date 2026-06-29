#!/usr/bin/env bash
#
# phase-100-write-usb.sh — write the m3OS bootable UEFI image to a USB key.
#
# Usage:
#   scripts/phase-100-write-usb.sh                # list candidate disks, then exit
#   scripts/phase-100-write-usb.sh /dev/sdX       # write default image to /dev/sdX (asks to confirm)
#   scripts/phase-100-write-usb.sh -y /dev/sdX    # skip the interactive confirmation
#   scripts/phase-100-write-usb.sh --image PATH /dev/sdX   # use a non-default image
#   scripts/phase-100-write-usb.sh --force /dev/sdX        # allow a non-removable target (DANGEROUS)
#
# Writes target/x86_64-unknown-none/release/boot-uefi-m3os.img (the UEFI boot
# image produced by `cargo xtask image`) to the given WHOLE-DISK device.
# Refuses partitions, the disk hosting / , and (without --force) non-removable
# disks. dd to the wrong device destroys data — read the confirmation prompt.
#
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DEFAULT_IMAGE="$REPO_ROOT/target/x86_64-unknown-none/release/boot-uefi-m3os.img"

IMAGE="${M3OS_IMAGE:-$DEFAULT_IMAGE}"
ASSUME_YES=0
FORCE_NONREMOVABLE=0
DEVICE=""

die()  { printf 'error: %s\n' "$*" >&2; exit 1; }
warn() { printf 'warning: %s\n' "$*" >&2; }

list_devices() {
  echo "Whole-disk block devices (pick the USB key — the WHOLE disk, e.g. /dev/sdb,"
  echo "not a partition like /dev/sdb1).  RM=1 / HOTPLUG=1 / TRAN=usb usually = removable:"
  echo
  lsblk -d -o NAME,SIZE,TYPE,RM,HOTPLUG,MODEL,TRAN | awk 'NR==1 || $3=="disk"'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -y|--yes)   ASSUME_YES=1; shift ;;
    --force)    FORCE_NONREMOVABLE=1; shift ;;
    --image)    IMAGE="${2:?--image needs a path}"; shift 2 ;;
    -l|--list)  list_devices; exit 0 ;;
    -h|--help)  sed -n '2,16p' "$0"; exit 0 ;;
    -*)         die "unknown option: $1 (try --help)" ;;
    *)          [[ -z "$DEVICE" ]] || die "unexpected extra argument: $1"; DEVICE="$1"; shift ;;
  esac
done

# No device given: show candidates and exit (safe default).
if [[ -z "$DEVICE" ]]; then
  list_devices
  echo
  echo "Then run:  $0 /dev/sdX"
  exit 0
fi

# Validate the image.
[[ -e "$IMAGE" ]] || die "image not found: $IMAGE
  Build it first with:  cargo xtask image"
[[ -s "$IMAGE" ]] || die "image is empty: $IMAGE"

# Validate the device.
[[ -b "$DEVICE" ]] || die "not a block device: $DEVICE  (run '$0 --list')"
DEV_BASE="$(basename -- "$DEVICE")"

DEV_TYPE="$(lsblk -dno TYPE "$DEVICE" 2>/dev/null || true)"
[[ "$DEV_TYPE" == "disk" ]] || die "$DEVICE is type '${DEV_TYPE:-unknown}', not a whole disk.
  Pass the whole disk (e.g. /dev/sdb), not a partition (e.g. /dev/sdb1)."

# Refuse the disk that hosts the running root filesystem.
ROOT_SRC="$(findmnt -no SOURCE / 2>/dev/null || true)"
if [[ -n "$ROOT_SRC" ]]; then
  ROOT_DISK="$(lsblk -no PKNAME "$ROOT_SRC" 2>/dev/null | head -n1 || true)"
  [[ -n "$ROOT_DISK" ]] || ROOT_DISK="$(basename -- "$ROOT_SRC")"
  [[ "$DEV_BASE" != "$ROOT_DISK" ]] || die "$DEVICE hosts the running root filesystem (/). Refusing."
fi

# Removable check (override with --force).
REMOVABLE=0
[[ -r "/sys/block/$DEV_BASE/removable" ]] && REMOVABLE="$(cat "/sys/block/$DEV_BASE/removable")"
if [[ "$REMOVABLE" != "1" && "$FORCE_NONREMOVABLE" != "1" ]]; then
  die "$DEVICE is not marked removable — it may be an internal disk.
  If you are certain this is your USB key, re-run with --force."
fi

# Show the plan and require confirmation.
echo "About to OVERWRITE this device — ALL DATA ON IT WILL BE LOST:"
echo
lsblk -o NAME,SIZE,TYPE,RM,HOTPLUG,MODEL,TRAN,MOUNTPOINTS "$DEVICE"
echo
echo "  image : $IMAGE  ($(du -h "$IMAGE" | cut -f1))"
echo "  target: $DEVICE"
echo
if [[ "$ASSUME_YES" != "1" ]]; then
  read -r -p "Type the device name ($DEVICE) to confirm: " CONFIRM
  [[ "$CONFIRM" == "$DEVICE" ]] || die "confirmation did not match; aborting (nothing written)."
fi

# Unmount any mounted partitions of the target first.
while read -r part mnt; do
  [[ -n "$mnt" ]] || continue
  echo "unmounting /dev/$part ($mnt)..."
  sudo umount "/dev/$part" || warn "could not unmount /dev/$part (continuing)"
done < <(lsblk -lno NAME,MOUNTPOINT "$DEVICE" | awk 'NR>0 && $2!="" {print $1, $2}')

# Write.
echo "writing (sudo dd) — this can take a few minutes..."
sudo dd if="$IMAGE" of="$DEVICE" bs=4M conv=fsync status=progress
echo "syncing buffers..."
sync
echo
echo "done. Boot the Dell from this USB key: power on → F12 → UEFI USB entry"
echo "(Secure Boot must be Disabled in BIOS, since this image is unsigned)."
