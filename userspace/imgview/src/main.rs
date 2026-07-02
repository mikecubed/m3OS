//! `imgview` — an image-viewer Toplevel on `m3ui` + `imagefmt`
//! (Phase 105 Track D.1).
//!
//! Given one or more file paths, it decodes each (PNG / BMP / JPEG,
//! auto-detected by magic bytes) via `imagefmt`, renders the current one
//! scaled-to-fit (or 1:1) into a `desktop_client` Toplevel surface, and
//! draws an `m3ui` toolbar: the filename, a Fit/1:1 toggle, and Prev/Next.
//! A file that fails to decode shows an error label instead of crashing.
//!
//! Usage: `imgview <path> [<path>...]`.
//!
//! On startup it decodes every argument and prints one greppable serial
//! line per file — the `imgview-smoke` oracle:
//!
//! ```text
//! IMGVIEW:ok    fmt=<png|bmp|jpeg> name=<basename> dim=<w>x<h> nonblank=<N>
//! IMGVIEW:blank fmt=<png|bmp|jpeg> name=<basename>
//! IMGVIEW:error name=<basename> reason=<why>
//! ```
//!
//! `nonblank` is the count of non-black pixels produced by a scaled render
//! of the decoded image; `ok` fires only when it is positive, so a serial
//! gate can prove each format decoded to real content without a framebuffer
//! capture (the compositor's own gates cover on-screen display).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

use desktop_client::{DisplayConnection, SharedSurface};
use imagefmt::{ImageError, decode_bmp, decode_jpeg, decode_png};
use kernel_core::display::protocol::{BufferId, ServerMessage};
use m3ui::{Focus, InputState, Item, Rect, SurfacePainter, Theme, Ui};
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{STDOUT_FILENO, write_str};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "imgview: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "imgview: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const BUFFER_ID: BufferId = BufferId(1);
const WIN_W: u32 = 680;
const WIN_H: u32 = 520;
const SCALE: u32 = 1;
const TOOLBAR_H: i32 = 40;
/// Fixed content size used for the startup non-blank probe render, so the
/// `nonblank` count is deterministic regardless of the live window size.
const PROBE_W: u32 = 512;
const PROBE_H: u32 = 384;

fn log(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    syscall_lib::serial_print(msg);
}

/// A successfully decoded image plus its display metadata.
struct Image {
    name: String,
    fmt: &'static str,
    w: u32,
    h: u32,
    pixels: Vec<u32>,
}

/// Last path component (after the final `/`), for the toolbar + sentinels.
fn basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Detect the format by magic bytes and decode to BGRA8888.
fn detect_and_decode(bytes: &[u8]) -> Result<(&'static str, u32, u32, Vec<u32>), ImageError> {
    const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    if bytes.len() >= 8 && bytes[0..8] == PNG_SIG {
        let (w, h, px) = decode_png(bytes)?;
        Ok(("png", w, h, px))
    } else if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let (w, h, px) = decode_jpeg(bytes)?;
        Ok(("jpeg", w, h, px))
    } else if bytes.len() >= 2 && &bytes[0..2] == b"BM" {
        let (w, h, px) = decode_bmp(bytes)?;
        Ok(("bmp", w, h, px))
    } else {
        Err(ImageError::BadSignature)
    }
}

/// Human-readable reason for an [`ImageError`], for the error label + sentinel.
fn err_reason(e: ImageError) -> &'static str {
    match e {
        ImageError::Truncated => "truncated",
        ImageError::BadSignature => "bad-signature",
        ImageError::Unsupported => "unsupported",
        ImageError::GeometryOverflow => "geometry-overflow",
        ImageError::Corrupt => "corrupt",
    }
}

/// Read an entire file into a `Vec`, or `None` on any I/O error.
fn read_file(path: &str) -> Option<Vec<u8>> {
    let mut cpath: Vec<u8> = Vec::with_capacity(path.len() + 1);
    cpath.extend_from_slice(path.as_bytes());
    cpath.push(0);
    let fd = syscall_lib::open(&cpath, syscall_lib::O_RDONLY, 0);
    if fd < 0 {
        return None;
    }
    let fd = fd as i32;
    let mut out: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = syscall_lib::read(fd, &mut chunk);
        if n < 0 {
            let _ = syscall_lib::close(fd);
            return None;
        }
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n as usize]);
    }
    let _ = syscall_lib::close(fd);
    Some(out)
}

fn write_u32_dec(n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    let mut v = n;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    log(core::str::from_utf8(&buf[i..]).unwrap_or("?"));
}

/// Count non-black pixels in a scaled-to-fit render of `img` into a
/// `PROBE_W × PROBE_H` scratch — the deterministic content oracle.
fn nonblank_probe(img: &Image) -> u32 {
    let mut scratch = vec![0u32; (PROBE_W * PROBE_H) as usize];
    imagefmt::blit_scale_to_fit(&img.pixels, img.w, img.h, &mut scratch, PROBE_W, PROBE_H);
    scratch.iter().filter(|&&p| (p & 0x00FF_FFFF) != 0).count() as u32
}

/// Fill `pixels` (a `w × h` surface) with a solid color.
fn fill(pixels: &mut [u32], color: u32) {
    for p in pixels.iter_mut() {
        *p = color;
    }
}

/// Blit `img` into the surface's content region. `fit` scales-to-fit
/// (letterboxed); otherwise the native pixels are drawn top-left-anchored
/// and clipped. Both stay inside `region`.
fn draw_image(img: &Image, surface: &mut [u32], surf_w: u32, region: Rect, fit: bool) {
    let (rw, rh) = (region.w.max(0) as u32, region.h.max(0) as u32);
    if rw == 0 || rh == 0 {
        return;
    }
    if fit {
        let mut scratch = vec![0u32; (rw * rh) as usize];
        imagefmt::blit_scale_to_fit(&img.pixels, img.w, img.h, &mut scratch, rw, rh);
        for row in 0..rh as usize {
            let dst = ((region.y as usize + row) * surf_w as usize) + region.x as usize;
            let src = row * rw as usize;
            surface[dst..dst + rw as usize].copy_from_slice(&scratch[src..src + rw as usize]);
        }
    } else {
        // 1:1 — copy native pixels, clipped to the region.
        let copy_w = (img.w).min(rw) as usize;
        let copy_h = (img.h).min(rh) as usize;
        for row in 0..copy_h {
            let dst = ((region.y as usize + row) * surf_w as usize) + region.x as usize;
            let src = row * img.w as usize;
            surface[dst..dst + copy_w].copy_from_slice(&img.pixels[src..src + copy_w]);
        }
    }
}

fn program_main(args: &[&str]) -> i32 {
    // ---- Decode every argument up front; emit one sentinel per file. ----
    let mut images: Vec<Image> = Vec::new();
    for path in args.iter().skip(1) {
        let name = String::from(basename(path));
        let Some(bytes) = read_file(path) else {
            log("IMGVIEW:error name=");
            log(&name);
            log(" reason=open\n");
            continue;
        };
        match detect_and_decode(&bytes) {
            Ok((fmt, w, h, pixels)) => {
                let img = Image {
                    name: name.clone(),
                    fmt,
                    w,
                    h,
                    pixels,
                };
                let nonblank = nonblank_probe(&img);
                // `ok` only when the scaled render produced real content; an
                // all-black decode is a `blank` failure, not a success.
                if nonblank > 0 {
                    log("IMGVIEW:ok fmt=");
                    log(fmt);
                    log(" name=");
                    log(&name);
                    log(" dim=");
                    write_u32_dec(w);
                    log("x");
                    write_u32_dec(h);
                    log(" nonblank=");
                    write_u32_dec(nonblank);
                    log("\n");
                } else {
                    log("IMGVIEW:blank fmt=");
                    log(fmt);
                    log(" name=");
                    log(&name);
                    log("\n");
                }
                images.push(img);
            }
            Err(e) => {
                log("IMGVIEW:error name=");
                log(&name);
                log(" reason=");
                log(err_reason(e));
                log("\n");
            }
        }
    }

    // ---- Toplevel window + interactive loop. ----
    let conn = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => {
            log("imgview: cannot connect to display_server\n");
            return 1;
        }
    };
    conn.set_toplevel_role();

    let mut win_w = WIN_W;
    let mut win_h = WIN_H;
    let mut surface = match SharedSurface::allocate(win_w, win_h) {
        Some(s) => s,
        None => {
            log("imgview: surface allocate failed\n");
            return 1;
        }
    };

    let mut input = InputState::new();
    let mut focus = Focus::new();
    let theme = Theme::dark();
    let mut index = 0usize;
    let mut fit = true;
    let mut announced_ready = false;
    let bg = theme.window_bg.0;

    loop {
        input.begin_frame();
        focus.begin_frame();

        let mut running = true;
        let mut resized: Option<(u32, u32)> = None;
        for _ in 0..64 {
            match conn.pull_event() {
                Some(ServerMessage::Key(ev)) => {
                    if let Some(kp) = m3ui::decode_key(&ev) {
                        input.set_mods(kp.mods);
                        input.push_key(kp);
                    }
                }
                Some(ServerMessage::Pointer(ev)) => m3ui::apply_pointer(&mut input, &ev),
                Some(ServerMessage::SurfaceResized { width, height, .. }) => {
                    resized = Some((width.max(120), height.max(80)));
                }
                Some(ServerMessage::CloseRequest { .. }) => running = false,
                Some(_) => {}
                None => break,
            }
        }
        if !running {
            break;
        }
        if let Some((w, h)) = resized
            && (w != win_w || h != win_h)
        {
            surface.release();
            win_w = w;
            win_h = h;
            surface = match SharedSurface::allocate(win_w, win_h) {
                Some(s) => s,
                None => {
                    log("imgview: realloc failed\n");
                    return 1;
                }
            };
        }

        // ---- paint the frame ----
        let n = images.len();
        {
            let pixels = surface.pixels_mut();
            fill(pixels, bg);
            if let Some(img) = images.get(index) {
                let region = Rect::new(0, TOOLBAR_H, win_w as i32, win_h as i32 - TOOLBAR_H);
                draw_image(img, pixels, win_w, region, fit);
            }
        }

        // Toolbar chrome (drawn over the top strip via m3ui).
        let mut next_index = index;
        {
            let pixels = surface.pixels_mut();
            let mut painter = SurfacePainter::new(pixels, win_w, win_h, SCALE);
            let bounds = Rect::new(0, 0, win_w as i32, TOOLBAR_H);
            let mut ui = Ui::new(&mut painter, &input, &mut focus, &theme, bounds);
            let cells = ui.split_row(
                theme.row_height,
                &[
                    Item::flex(1),
                    Item::fixed(70),
                    Item::fixed(70),
                    Item::fixed(70),
                ],
            );
            // Filename (or an error hint when nothing decoded).
            let title = match images.get(index) {
                Some(img) => img.name.as_str(),
                None => "no image",
            };
            ui.label_at(cells[0], title);
            if ui
                .button_at(cells[1], if fit { "1:1" } else { "Fit" })
                .clicked
            {
                fit = !fit;
            }
            if ui.button_at(cells[2], "Prev").clicked && n > 0 {
                next_index = (index + n - 1) % n;
            }
            if ui.button_at(cells[3], "Next").clicked && n > 0 {
                next_index = (index + 1) % n;
            }
            ui.end();
        }
        index = next_index;

        conn.attach_damage_commit(BUFFER_ID, surface.shm_id, win_w, win_h);
        if !announced_ready {
            announced_ready = true;
            log("IMGVIEW:ready\n");
        }

        let _ = syscall_lib::nanosleep_for(0, 33_000_000); // ~30 Hz
    }

    surface.release();
    conn.goodbye();
    0
}
