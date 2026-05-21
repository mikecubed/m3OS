//! Phase 73 Track E — notification daemon.
//!
//! Listens on AF_UNIX `/run/notifyd.sock`. Clients connect, write a
//! framed notification message (4-byte LE length prefix + UTF-8 JSON
//! body), and disconnect. Each notification is rendered as a panel
//! anchored at the top-right of the primary output for `timeout_ms`
//! milliseconds, then dismissed.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use core::alloc::Layout;

use desktop_client::{
    DisplayConnection, SharedSurface, anchor, draw_text, fill, fill_rect, stroke_rect,
};
use kernel_core::display::protocol::{BufferId, KeyboardInteractivity, Layer};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "notifyd: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "notifyd: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point!(program_main);

const SOCKET_PATH: &str = "/run/notifyd.sock";
const BUFFER_ID: BufferId = BufferId(1);
const WIDTH_PX: u32 = 360;
const HEIGHT_PX: u32 = 420;
const PANEL_HEIGHT: u32 = 80;
const PANEL_GAP: u32 = 8;
const SERVICE_NAME: &str = "notifyd";

const BG_COLOR: u32 = 0x00_00_00_00; // transparent surface fill
const PANEL_BG: u32 = 0xFF_22_22_2A;
const PANEL_FG: u32 = 0xFF_E8_E8_E8;
const PANEL_TITLE: u32 = 0xFF_FF_C8_60;
const PANEL_BORDER: u32 = 0xFF_4A_4A_4A;

struct Notification {
    title: String,
    body: String,
    remaining_ms: u32,
}

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "notifyd: starting (Phase 73)\n");

    let ep = syscall_lib::create_endpoint();
    if ep != u64::MAX
        && let Ok(ep_u32) = u32::try_from(ep)
    {
        let _ = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
    }

    let listen_fd = match open_listener() {
        Some(fd) => fd,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "notifyd: failed to bind socket\n");
            return 2;
        }
    };

    let conn = match DisplayConnection::connect_auto() {
        Some(c) => c,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "notifyd: display_server unavailable\n");
            return 3;
        }
    };
    if !conn.set_layer_role(
        Layer::Overlay,
        anchor::ANCHOR_TOP | anchor::ANCHOR_RIGHT,
        0,
        KeyboardInteractivity::None,
    ) {
        return 4;
    }

    let surface = match SharedSurface::allocate(WIDTH_PX, HEIGHT_PX) {
        Some(s) => s,
        None => return 4,
    };
    let pixels = surface.pixels_mut();
    fill(pixels, BG_COLOR);
    let _ = conn.attach_damage_commit(BUFFER_ID, surface.shm_id, WIDTH_PX, HEIGHT_PX);

    let mut notifications: VecDeque<Notification> = VecDeque::new();
    let tick_ms: u32 = 100;
    let mut dirty = true; // first paint always commits
    loop {
        // Drain any pending connections (non-blocking).
        loop {
            let fd = syscall_lib::accept(listen_fd, None);
            if fd < 0 {
                break;
            }
            if let Some(note) = read_notification(fd as i32) {
                syscall_lib::write_str(STDOUT_FILENO, "notifyd: notification accepted: '");
                let _ = syscall_lib::write(STDOUT_FILENO, note.title.as_bytes());
                syscall_lib::write_str(STDOUT_FILENO, "'\n");
                notifications.push_back(note);
                dirty = true;
            }
            let _ = syscall_lib::close(fd as i32);
        }

        if dirty {
            if notifications.is_empty() {
                fill(pixels, BG_COLOR);
            } else {
                render(pixels, &notifications);
            }
            let _ = conn.attach_damage_commit(BUFFER_ID, surface.shm_id, WIDTH_PX, HEIGHT_PX);
            dirty = false;
        }

        let _ = syscall_lib::nanosleep_for(0, (tick_ms as u32) * 1_000_000);

        // Decrement timers and pop expired notifications. A repaint
        // is needed whenever a notification dismisses (so the panel
        // disappears from the framebuffer).
        let before = notifications.len();
        for note in notifications.iter_mut() {
            note.remaining_ms = note.remaining_ms.saturating_sub(tick_ms);
        }
        while let Some(front) = notifications.front()
            && front.remaining_ms == 0
        {
            notifications.pop_front();
        }
        if notifications.len() != before {
            dirty = true;
        }
    }
}

fn open_listener() -> Option<i32> {
    // Best-effort: remove a stale socket if present.
    let mut path_z: [u8; 64] = [0u8; 64];
    let path = SOCKET_PATH.as_bytes();
    if path.len() + 1 > path_z.len() {
        return None;
    }
    path_z[..path.len()].copy_from_slice(path);
    let _ = syscall_lib::unlink(&path_z[..path.len() + 1]);

    let fd = syscall_lib::socket(
        syscall_lib::AF_UNIX as i32,
        syscall_lib::SOCK_STREAM as i32,
        0,
    );
    if fd < 0 {
        return None;
    }
    let addr = syscall_lib::SockaddrUn::new(SOCKET_PATH);
    if syscall_lib::bind_unix(fd as i32, &addr) < 0 {
        let _ = syscall_lib::close(fd as i32);
        return None;
    }
    if syscall_lib::listen(fd as i32, 8) < 0 {
        let _ = syscall_lib::close(fd as i32);
        return None;
    }
    let _ = syscall_lib::set_nonblocking(fd as i32);
    Some(fd as i32)
}

fn read_notification(fd: i32) -> Option<Notification> {
    let mut len_buf = [0u8; 4];
    let mut read = 0usize;
    while read < 4 {
        let n = syscall_lib::read(fd, &mut len_buf[read..]);
        if n <= 0 {
            return None;
        }
        read += n as usize;
    }
    let body_len = u32::from_le_bytes(len_buf) as usize;
    if body_len == 0 || body_len > 8192 {
        return None;
    }
    let mut body = alloc::vec![0u8; body_len];
    let mut total = 0usize;
    while total < body_len {
        let n = syscall_lib::read(fd, &mut body[total..]);
        if n <= 0 {
            return None;
        }
        total += n as usize;
    }
    let text = core::str::from_utf8(&body).ok()?;
    parse_notification(text)
}

fn parse_notification(text: &str) -> Option<Notification> {
    // Minimal JSON-ish extractor: looks for "title": "...", "body":
    // "...", "timeout_ms": <int>. Whitespace-tolerant. Does not
    // honour escape sequences except `\"`.
    let title = extract_string(text, "title")?;
    let body = extract_string(text, "body")?;
    let timeout = extract_int(text, "timeout_ms").unwrap_or(5000) as u32;
    Some(Notification {
        title,
        body,
        remaining_ms: timeout,
    })
}

fn extract_string(text: &str, key: &str) -> Option<String> {
    let needle = alloc::format!("\"{key}\"");
    let pos = text.find(&needle)?;
    let rest = &text[pos + needle.len()..];
    let colon = rest.find(':')?;
    let after_colon = &rest[colon + 1..].trim_start();
    let quote = after_colon.find('"')?;
    let body = &after_colon[quote + 1..];
    let end = body.find('"')?;
    Some(body[..end].to_string())
}

fn extract_int(text: &str, key: &str) -> Option<i64> {
    let needle = alloc::format!("\"{key}\"");
    let pos = text.find(&needle)?;
    let rest = &text[pos + needle.len()..];
    let colon = rest.find(':')?;
    let after_colon = &rest[colon + 1..].trim_start();
    let mut end = 0;
    for (i, c) in after_colon.char_indices() {
        if c.is_ascii_digit() || c == '-' {
            end = i + c.len_utf8();
            continue;
        }
        break;
    }
    if end == 0 {
        return None;
    }
    after_colon[..end].parse::<i64>().ok()
}

fn render(pixels: &mut [u32], notifications: &VecDeque<Notification>) {
    fill(pixels, BG_COLOR);
    let mut y: i32 = 8;
    for note in notifications.iter().take(5) {
        fill_rect(
            pixels,
            WIDTH_PX,
            HEIGHT_PX,
            4,
            y,
            WIDTH_PX - 8,
            PANEL_HEIGHT,
            PANEL_BG,
        );
        stroke_rect(
            pixels,
            WIDTH_PX,
            HEIGHT_PX,
            4,
            y,
            WIDTH_PX - 8,
            PANEL_HEIGHT,
            PANEL_BORDER,
        );
        draw_text(
            pixels,
            WIDTH_PX,
            HEIGHT_PX,
            12,
            y + 8,
            &note.title,
            PANEL_TITLE,
            PANEL_BG,
        );
        // Wrap long body lines crudely at 40 chars.
        let mut line_y = y + 30;
        let body_bytes = note.body.as_bytes();
        let mut start = 0;
        while start < body_bytes.len() {
            let end = (start + 40).min(body_bytes.len());
            if let Ok(s) = core::str::from_utf8(&body_bytes[start..end]) {
                draw_text(
                    pixels, WIDTH_PX, HEIGHT_PX, 12, line_y, s, PANEL_FG, PANEL_BG,
                );
            }
            line_y += 16;
            if line_y > y + (PANEL_HEIGHT as i32) - 12 {
                break;
            }
            start = end;
        }
        y += PANEL_HEIGHT as i32 + PANEL_GAP as i32;
        if y > HEIGHT_PX as i32 {
            break;
        }
    }
}
