//! Phase 106 Track C — the installer syscall ABI (`0x117x`).
//!
//! Declared here (single source of truth, the `spectre::SYS_*` /
//! `power::syscalls::SYS_*` pattern) so the kernel dispatcher and the
//! ring-3 `installer` share the same numbers. The installer copies raw
//! sectors from the boot medium to a target disk (USB → NVMe); the
//! per-`dev_id` block I/O already lives in `kernel/src/blk`, but raw
//! cross-device reads/writes are too destructive to be ambient, so
//! these syscalls are gated on the installer's unforgeable exec path
//! ([`INSTALLER_EXEC_PATH`], the `/drivers/`-gate shape).

/// Resolve a block-device service name (e.g. `"nvme.block"`) to a
/// `dev_id` usable with the raw read/write syscalls, registering a
/// secondary block slot if needed. `sys_blk_resolve_dev(name_ptr,
/// name_len) -> isize` (the `dev_id`, or a negative errno). `dev_id 0`
/// is the boot/root device and needs no resolution.
pub const SYS_BLK_RESOLVE_DEV: u64 = 0x1170;

/// Read `count` sectors from `dev_id` into a user buffer:
/// `sys_blk_raw_read(dev_id, start_lba, count, buf_ptr) -> isize`
/// (bytes read, or a negative errno). `dev_id 0` = the root device.
pub const SYS_BLK_RAW_READ: u64 = 0x1171;

/// Write `count` sectors from a user buffer to `dev_id`:
/// `sys_blk_raw_write(dev_id, start_lba, count, buf_ptr) -> isize`
/// (bytes written, or a negative errno). Destructive — access-checked
/// against [`INSTALLER_EXEC_PATH`].
pub const SYS_BLK_RAW_WRITE: u64 = 0x1172;

/// Flush `dev_id`'s write-back cache: `sys_blk_raw_flush(dev_id) ->
/// isize` (0 on success, negative errno). The installer flushes the
/// **target** device (a secondary `dev_id`) before rebooting — the
/// reboot path only flushes the root device (slot 0).
pub const SYS_BLK_RAW_FLUSH: u64 = 0x1173;

/// The unforgeable exec path the raw block syscalls are gated on. The
/// kernel writes `exec_path` during `execve`, so a ring-3 process
/// cannot spoof it (identical trust model to the `/drivers/` device-host
/// gate). Only a process launched from exactly this path may issue the
/// raw read/write/resolve syscalls.
pub const INSTALLER_EXEC_PATH: &str = "/sbin/installer";

/// Logical sector size assumed by the raw syscalls (matches every m3OS
/// block backend and the QEMU nvme/usb defaults).
pub const SECTOR_BYTES: u64 = 512;

/// Upper bound on sectors per raw request — matches the block-IPC
/// `MAX_SECTORS_PER_REQUEST` (256 = 128 KiB), the largest a single
/// `read_sectors_dev`/`write_sectors_dev` accepts. The installer's copy
/// loop chunks to this to minimize round-trips over a multi-hundred-MB
/// image.
pub const MAX_SECTORS_PER_RAW_REQUEST: u64 = 256;

/// Validate a raw request's sector count against
/// [`MAX_SECTORS_PER_RAW_REQUEST`]. Pure so both sides (kernel bounds
/// check + installer chunking) agree; host-tested.
pub fn raw_count_ok(count: u64) -> bool {
    count > 0 && count <= MAX_SECTORS_PER_RAW_REQUEST
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_numbers_are_pinned() {
        // Renumbering breaks every compiled installer binary.
        assert_eq!(SYS_BLK_RESOLVE_DEV, 0x1170);
        assert_eq!(SYS_BLK_RAW_READ, 0x1171);
        assert_eq!(SYS_BLK_RAW_WRITE, 0x1172);
        assert_eq!(SYS_BLK_RAW_FLUSH, 0x1173);
        assert_eq!(INSTALLER_EXEC_PATH, "/sbin/installer");
    }

    #[test]
    fn raw_count_bounds() {
        assert!(!raw_count_ok(0));
        assert!(raw_count_ok(1));
        assert!(raw_count_ok(MAX_SECTORS_PER_RAW_REQUEST));
        assert!(!raw_count_ok(MAX_SECTORS_PER_RAW_REQUEST + 1));
        assert!(!raw_count_ok(u64::MAX));
        // Must not exceed the block-IPC per-request ceiling.
        assert_eq!(MAX_SECTORS_PER_RAW_REQUEST, 256);
    }
}
