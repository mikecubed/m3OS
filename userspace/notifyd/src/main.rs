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

// The compositor surface-blit path ignores BGRA alpha, so the
// "transparent" semantic we'd prefer collapses to whatever the buffer
// holds. We render the surface with an intentional opaque "stack
// surround" colour so the inter-panel + below-panel region matches a
// notification-tray look rather than accidentally displaying as
// solid black.
const BG_COLOR: u32 = 0xFF_18_18_1A;
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
    // Phase 73 — do NOT pre-commit an empty buffer. The compositor
    // does not honour BGRA alpha during the surface blit, so an
    // all-zero (transparent) backing buffer reads back as solid
    // BLACK on screen — covering the right 360 px × 420 px of every
    // surface beneath this `Layer::Overlay`. The surface only
    // becomes visible the first time a real notification arrives;
    // once the queue drains the last attach-commit stays in place
    // (a tiny visual quirk, not a glitch) until the next arrival
    // refreshes it.
    let mut notifications: VecDeque<Notification> = VecDeque::new();
    let tick_ms: u32 = 100;
    // `dirty=false` here means: nothing to render yet, don't attach
    // a buffer. Flipped to `true` only when a real notification is
    // accepted from the socket.
    let mut dirty = false;
    loop {
        // Drain any pending connections (listener is non-blocking).
        loop {
            let fd = syscall_lib::accept(listen_fd, None);
            if fd < 0 {
                break;
            }
            // Set accepted fd non-blocking so a misbehaving client that
            // connects but never sends (or sends only a partial frame)
            // cannot wedge the notifyd loop — `read_notification` does
            // bounded retries with a total budget on EAGAIN.
            if syscall_lib::set_nonblocking(fd as i32) < 0 {
                let _ = syscall_lib::close(fd as i32);
                continue;
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
                // No fresh paint when the queue empties: committing
                // an "empty" frame just reproduces the surround
                // backdrop without panels, which still shows as a
                // 360×420 opaque rectangle since compositor blits
                // opaque. Leaving the previously-attached buffer in
                // place is a smaller visual quirk (the last panels
                // linger briefly) than reintroducing the top-right
                // black/grey overlay on every drain.
            } else {
                render(pixels, &notifications);
                let _ = conn.attach_damage_commit(BUFFER_ID, surface.shm_id, WIDTH_PX, HEIGHT_PX);
            }
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

/// `-EAGAIN` on a non-blocking fd with no data ready. Local
/// `syscall_lib` does not export the constant; the kernel uses the
/// Linux convention.
const NEG_EAGAIN: isize = -11;
/// Per-attempt sleep when the fd is non-blocking but the client hasn't
/// finished writing yet. Granular enough that a well-behaved client
/// (which writes immediately after `connect`) hits at most one or two
/// sleeps; coarse enough that the bounded retry budget below remains
/// short.
const READ_RETRY_NS: u32 = 5_000_000; // 5 ms
/// Maximum retries per `read_exact` call. 5 ms × 20 = 100 ms total —
/// bounds the daemon's accept-drain loop even when a peer connects and
/// stalls indefinitely.
const READ_RETRY_MAX: u32 = 20;

fn read_notification(fd: i32) -> Option<Notification> {
    let mut len_buf = [0u8; 4];
    if !read_exact(fd, &mut len_buf) {
        return None;
    }
    let body_len = u32::from_le_bytes(len_buf) as usize;
    if body_len == 0 || body_len > 8192 {
        return None;
    }
    let mut body = alloc::vec![0u8; body_len];
    if !read_exact(fd, &mut body) {
        return None;
    }
    let text = core::str::from_utf8(&body).ok()?;
    parse_notification(text)
}

/// Read exactly `buf.len()` bytes from a non-blocking fd, sleeping
/// between attempts when the kernel reports EAGAIN. Returns `false` on
/// EOF, hard error, or after `READ_RETRY_MAX` consecutive EAGAINs —
/// the caller drops the connection in that case so a stalled client
/// cannot wedge the daemon.
fn read_exact(fd: i32, buf: &mut [u8]) -> bool {
    let mut got = 0usize;
    let mut idle = 0u32;
    while got < buf.len() {
        let n = syscall_lib::read(fd, &mut buf[got..]);
        if n > 0 {
            got += n as usize;
            idle = 0;
            continue;
        }
        if n == NEG_EAGAIN {
            if idle >= READ_RETRY_MAX {
                return false;
            }
            idle += 1;
            let _ = syscall_lib::nanosleep_for(0, READ_RETRY_NS);
            continue;
        }
        // 0 = EOF, any other negative = hard error.
        return false;
    }
    true
}

fn parse_notification(text: &str) -> Option<Notification> {
    // Minimal JSON-ish extractor: looks for "title": "...", "body":
    // "...", "timeout_ms": <int>. Whitespace-tolerant. Honours `\"`
    // and `\\` so notify-send payloads with embedded quotes survive
    // the round-trip — see `notify_send::json_escape`.
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
    let after_colon = rest[colon + 1..].trim_start();
    let mut chars = after_colon.chars();
    if chars.next()? != '"' {
        return None;
    }
    // Walk the value, decoding `\"` and `\\` so an escaped quote
    // inside the value does not terminate the string early. Other
    // backslash escapes pass through with their leading `\` preserved;
    // notify-send only emits `\"`, `\\`, `\n`, `\r`, `\t`, so the
    // round-trip preserves whatever the user supplied. Char-based
    // iteration keeps multi-byte UTF-8 codepoints intact.
    let mut out = String::with_capacity(after_colon.len());
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => return None,
            }
            continue;
        }
        if c == '"' {
            return Some(out);
        }
        out.push(c);
    }
    None
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
        // Wrap long body lines at 40 characters per row. Iterate over
        // `char_indices` so multi-byte UTF-8 codepoints are never
        // split — a naive 40-byte chunk would corrupt non-ASCII
        // bodies and silently drop lines via `from_utf8`.
        let mut line_y = y + 30;
        let body = note.body.as_str();
        let mut line_start_byte = 0usize;
        let mut chars_in_line = 0u32;
        let mut iter = body.char_indices().peekable();
        while let Some((idx, ch)) = iter.next() {
            chars_in_line += 1;
            let next_byte = idx + ch.len_utf8();
            if chars_in_line >= 40 || iter.peek().is_none() {
                let slice = &body[line_start_byte..next_byte];
                draw_text(
                    pixels, WIDTH_PX, HEIGHT_PX, 12, line_y, slice, PANEL_FG, PANEL_BG,
                );
                line_y += 16;
                if line_y > y + (PANEL_HEIGHT as i32) - 12 {
                    break;
                }
                line_start_byte = next_byte;
                chars_in_line = 0;
            }
        }
        y += PANEL_HEIGHT as i32 + PANEL_GAP as i32;
        if y > HEIGHT_PX as i32 {
            break;
        }
    }
}
