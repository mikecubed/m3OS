//! Minimal QEMU `fw_cfg` reader for the launch-time boot-mode signal.
//!
//! `cargo xtask run` / `run-gui` pass the desired boot mode via QEMU's `fw_cfg`
//! interface (`-fw_cfg name=opt/m3os/boot-mode,string=graphical|serial`) instead
//! of baking it into the persistent data disk. This module reads that entry once
//! at boot and exposes it through `/proc/m3os-boot-mode`, which `init` consults —
//! falling back to the on-disk `/etc/m3os-graphical-only` marker when the entry
//! is absent (real hardware, or a standalone `cargo xtask image`). This is what
//! lets a developer boot the SAME data disk into serial or GUI mode per launch
//! without regenerating it, while tests/CI pick their mode explicitly.
//!
//! The classic port interface is used: write a u16 selector to 0x510, then read
//! the selected item's bytes sequentially from 0x511. The `"QEMU"` signature
//! check makes the whole thing a safe no-op when `fw_cfg` is absent.

use core::sync::atomic::{AtomicU8, Ordering};
use x86_64::instructions::port::Port;

const FW_CFG_PORT_SEL: u16 = 0x510;
const FW_CFG_PORT_DATA: u16 = 0x511;
const FW_CFG_SIGNATURE: u16 = 0x0000;
const FW_CFG_FILE_DIR: u16 = 0x0019;

/// No launch-time override present — callers fall back to the disk marker.
pub const BOOT_MODE_AUTO: u8 = 0;
/// Force the serial / autologin boot path this launch.
pub const BOOT_MODE_SERIAL: u8 = 1;
/// Force the graphical greeter boot path this launch.
pub const BOOT_MODE_GRAPHICAL: u8 = 2;

static BOOT_MODE: AtomicU8 = AtomicU8::new(BOOT_MODE_AUTO);

/// The launch-time boot mode read from `fw_cfg` at boot, or [`BOOT_MODE_AUTO`].
pub fn boot_mode() -> u8 {
    BOOT_MODE.load(Ordering::Relaxed)
}

/// Read the `opt/m3os/boot-mode` `fw_cfg` entry (if any) and record it. A safe
/// no-op when `fw_cfg` is absent or the key is missing — `BOOT_MODE` stays
/// [`BOOT_MODE_AUTO`]. Call once, early in kernel boot.
pub fn init() {
    // SAFETY: hardware boundary — reads the QEMU fw_cfg I/O ports. The signature
    // check inside bails out (leaving BOOT_MODE_AUTO) when fw_cfg is not present,
    // so this is safe even on real hardware where the ports are unimplemented.
    let mode = unsafe { read_boot_mode() };
    BOOT_MODE.store(mode, Ordering::Relaxed);
}

unsafe fn select(sel: u16) {
    unsafe { Port::<u16>::new(FW_CFG_PORT_SEL).write(sel) };
}

unsafe fn read_into(buf: &mut [u8]) {
    let mut data = Port::<u8>::new(FW_CFG_PORT_DATA);
    for b in buf.iter_mut() {
        *b = unsafe { data.read() };
    }
}

unsafe fn read_boot_mode() -> u8 {
    // 1. Signature gate — reads "QEMU" only when fw_cfg is present.
    unsafe { select(FW_CFG_SIGNATURE) };
    let mut sig = [0u8; 4];
    unsafe { read_into(&mut sig) };
    if &sig != b"QEMU" {
        return BOOT_MODE_AUTO;
    }

    // 2. Walk the file directory for `opt/m3os/boot-mode`.
    //    Layout: be32 count, then `count` entries of
    //    { be32 size, be16 select, be16 reserved, char name[56] }.
    unsafe { select(FW_CFG_FILE_DIR) };
    let mut count_be = [0u8; 4];
    unsafe { read_into(&mut count_be) };
    let count = u32::from_be_bytes(count_be).min(1024); // defensive bound
    let target = b"opt/m3os/boot-mode";
    let mut found: Option<(u16, u32)> = None;
    for _ in 0..count {
        let mut size_be = [0u8; 4];
        unsafe { read_into(&mut size_be) };
        let mut sel_be = [0u8; 2];
        unsafe { read_into(&mut sel_be) };
        let mut _reserved = [0u8; 2];
        unsafe { read_into(&mut _reserved) };
        let mut name = [0u8; 56];
        unsafe { read_into(&mut name) };
        let name_len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        if &name[..name_len] == target {
            found = Some((u16::from_be_bytes(sel_be), u32::from_be_bytes(size_be)));
            // Selecting the item below resets the read pointer, so there is no
            // need to drain the rest of the directory.
            break;
        }
    }
    let Some((sel, size)) = found else {
        return BOOT_MODE_AUTO;
    };

    // 3. Read the item's bytes and classify.
    unsafe { select(sel) };
    let mut content = [0u8; 16];
    let n = (size as usize).min(content.len());
    unsafe { read_into(&mut content[..n]) };
    let s = &content[..n];
    if s.starts_with(b"graphical") {
        BOOT_MODE_GRAPHICAL
    } else if s.starts_with(b"serial") {
        BOOT_MODE_SERIAL
    } else {
        BOOT_MODE_AUTO
    }
}
