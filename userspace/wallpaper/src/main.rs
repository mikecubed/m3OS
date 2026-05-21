//! Phase 73 Track G — desktop background Layer-shell client.
//!
//! Reads `/etc/compositor.conf` `[wallpaper]` for a raw RGBA image
//! path + fallback colour. Maps a full-output Layer surface at the
//! Background layer so the picture sits behind every tiled window.
//!
//! On `SIGHUP` the loader rescans the config and reloads the image.
//! `SIGTERM` exits cleanly.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::{AtomicBool, Ordering};

use desktop_client::{DisplayConnection, SharedSurface, anchor, fill};
use kernel_core::display::protocol::{BufferId, KeyboardInteractivity, Layer};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "wallpaper: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "wallpaper: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const CONFIG_PATH: &[u8] = b"/etc/compositor.conf\0";
const BUFFER_ID: BufferId = BufferId(1);
const SURFACE_WIDTH_PX: u32 = 1280;
const SURFACE_HEIGHT_PX: u32 = 800;
const SERVICE_NAME: &str = "wallpaper";
const POLL_IDLE_NS: u32 = 200_000_000;

static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sighup(_: i32) {
    RELOAD_REQUESTED.store(true, Ordering::Relaxed);
}

extern "C" fn handle_sigterm(_: i32) {
    EXIT_REQUESTED.store(true, Ordering::Relaxed);
}

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "wallpaper: starting (Phase 73)\n");

    let ep = syscall_lib::create_endpoint();
    if ep != u64::MAX
        && let Ok(ep_u32) = u32::try_from(ep)
    {
        let _ = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
    }

    let _ = syscall_lib::rt_sigaction_simple(syscall_lib::SIGHUP as usize, handle_sighup);
    let _ = syscall_lib::rt_sigaction_simple(syscall_lib::SIGTERM as usize, handle_sigterm);

    let conn = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "wallpaper: display_server unavailable\n");
            return 2;
        }
    };
    if !conn.set_layer_role(
        Layer::Background,
        anchor::ANCHOR_TOP | anchor::ANCHOR_BOTTOM | anchor::ANCHOR_LEFT | anchor::ANCHOR_RIGHT,
        0,
        KeyboardInteractivity::None,
    ) {
        syscall_lib::write_str(STDOUT_FILENO, "wallpaper: SetSurfaceRole failed\n");
        return 3;
    }

    let surface = match SharedSurface::allocate(SURFACE_WIDTH_PX, SURFACE_HEIGHT_PX) {
        Some(s) => s,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "wallpaper: SHM allocation failed\n");
            return 3;
        }
    };
    let pixels = surface.pixels_mut();

    paint(pixels);
    if !conn.attach_damage_commit(
        BUFFER_ID,
        surface.shm_id,
        SURFACE_WIDTH_PX,
        SURFACE_HEIGHT_PX,
    ) {
        surface.release();
        return 5;
    }

    loop {
        if EXIT_REQUESTED.load(Ordering::Relaxed) {
            break;
        }
        if RELOAD_REQUESTED.swap(false, Ordering::Relaxed) {
            paint(pixels);
            let _ = conn.attach_damage_commit(
                BUFFER_ID,
                surface.shm_id,
                SURFACE_WIDTH_PX,
                SURFACE_HEIGHT_PX,
            );
            syscall_lib::write_str(STDOUT_FILENO, "wallpaper: reloaded background\n");
        }
        let _ = syscall_lib::nanosleep_for(0, POLL_IDLE_NS);
    }

    conn.goodbye();
    surface.release();
    0
}

fn paint(pixels: &mut [u32]) {
    let cfg = load_config();
    if let Some(path) = &cfg.path
        && let Some((w, h, image)) = load_rgba_file(path)
        && !image.is_empty()
    {
        blit_scaled(pixels, SURFACE_WIDTH_PX, SURFACE_HEIGHT_PX, &image, w, h);
        return;
    }
    fill(pixels, cfg.fallback_color);
}

struct WallpaperConfig {
    path: Option<String>,
    fallback_color: u32,
}

impl WallpaperConfig {
    fn defaults() -> Self {
        Self {
            path: None,
            fallback_color: 0x002B_5A4B,
        }
    }
}

fn load_config() -> WallpaperConfig {
    let mut buf = [0u8; 8192];
    let n = read_file(CONFIG_PATH, &mut buf);
    if n == 0 {
        return WallpaperConfig::defaults();
    }
    let text = match core::str::from_utf8(&buf[..n]) {
        Ok(s) => s,
        Err(_) => return WallpaperConfig::defaults(),
    };
    let mut cfg = WallpaperConfig::defaults();
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_section = name.trim() == "wallpaper";
            continue;
        }
        if !in_section {
            continue;
        }
        let (k, v) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim().trim_matches('"').trim_matches('\'')),
            None => continue,
        };
        match k {
            "path" => {
                if !v.is_empty() {
                    cfg.path = Some(v.to_string());
                }
            }
            "fallback_color" => {
                let hex = v.trim_start_matches("0x").trim_start_matches('#');
                if let Ok(c) = u32::from_str_radix(hex, 16) {
                    cfg.fallback_color = c;
                }
            }
            _ => {}
        }
    }
    cfg
}

fn read_file(path: &[u8], buf: &mut [u8]) -> usize {
    let fd = syscall_lib::open(path, syscall_lib::O_RDONLY, 0);
    if fd < 0 {
        return 0;
    }
    let mut total = 0usize;
    loop {
        if total >= buf.len() {
            break;
        }
        let n = syscall_lib::read(fd as i32, &mut buf[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    let _ = syscall_lib::close(fd as i32);
    total
}

fn load_rgba_file(path: &str) -> Option<(u32, u32, Vec<u32>)> {
    let mut path_buf = [0u8; 256];
    if path.len() + 1 > path_buf.len() {
        return None;
    }
    path_buf[..path.len()].copy_from_slice(path.as_bytes());
    path_buf[path.len()] = 0;
    let fd = syscall_lib::open(&path_buf[..path.len() + 1], syscall_lib::O_RDONLY, 0);
    if fd < 0 {
        return None;
    }
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = syscall_lib::read(fd as i32, &mut chunk);
        if n <= 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n as usize]);
    }
    let _ = syscall_lib::close(fd as i32);
    if data.len() >= 12 && &data[0..4] == b"RGBA" {
        let w = u32::from_le_bytes(data[4..8].try_into().ok()?);
        let h = u32::from_le_bytes(data[8..12].try_into().ok()?);
        let need = (w as usize) * (h as usize) * 4;
        if data.len() < 12 + need {
            return None;
        }
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for i in 0..(w * h) as usize {
            let off = 12 + i * 4;
            let px = u32::from_le_bytes(data[off..off + 4].try_into().ok()?);
            pixels.push(px);
        }
        Some((w, h, pixels))
    } else {
        None
    }
}

fn blit_scaled(dst: &mut [u32], dst_w: u32, dst_h: u32, src: &[u32], src_w: u32, src_h: u32) {
    if src_w == 0 || src_h == 0 {
        return;
    }
    let dw = dst_w as usize;
    let dh = dst_h as usize;
    let sw = src_w as usize;
    let sh = src_h as usize;
    for y in 0..dh {
        let sy = (y * sh) / dh;
        for x in 0..dw {
            let sx = (x * sw) / dw;
            let s_idx = sy * sw + sx;
            let d_idx = y * dw + x;
            if s_idx < src.len() && d_idx < dst.len() {
                dst[d_idx] = src[s_idx];
            }
        }
    }
}
