//! Phase 73 — shared Layer-shell / Toplevel boilerplate.
//!
//! Five new Phase 73 clients (`wallpaper`, `bar`, `launcher`,
//! `notifyd`, `lockscreen`) all walk the same handshake with
//! `display_server`. This crate consolidates the common path so each
//! client only carries its own UI / event-loop logic.
//!
//! ## What this crate is *not*
//!
//! A toolkit. The compose loop renders BGRA8888 pixels directly into
//! a shared-memory surface; there is no widget tree, no layout
//! engine. The bitmap-font helpers exist purely so the four text
//! clients do not each reimplement glyph blitting.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::display::protocol::{
    BufferId, ClientMessage, KeyboardInteractivity, Layer, LayerConfig, MimeTag, PROTOCOL_VERSION,
    Rect, ServerMessage, SurfaceId, SurfaceRole,
};
use kernel_core::input::events::KeyEvent;
use kernel_core::session::font::{BasicBitmapFont, FontProvider, Glyph};

pub use kernel_core::input::events::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

const LABEL_VERB: u64 = 1;
const LABEL_CLIENT_EVENT_PULL: u64 = 3;
const VERB_ENCODE_BUF_LEN: usize = 128;

/// Phase 105 Track B — the largest clipboard offer carried in one IPC
/// bulk. The frame + bytes must fit under the protocol's
/// `MAX_FRAME_BODY_LEN` (4096) `decode_message` guard; 3900 leaves room
/// for the 13-byte frame and a safety margin. Text clipboards are far
/// smaller in practice; multi-frame transfer for larger blobs is a
/// documented follow-up.
pub const CLIPBOARD_MAX_BYTES: usize = 3900;

/// Connection to `display_server`. Wraps an IPC handle plus a single
/// surface id; clients that need more than one surface hold multiple
/// connections.
pub struct DisplayConnection {
    handle: u32,
    token: u32,
    surface_id: SurfaceId,
}

impl DisplayConnection {
    /// Convenience: connect using [`auto_surface_id`] so callers do
    /// not have to remember the Phase 73 reservation table. This is
    /// the canonical entry point for new desktop clients.
    pub fn connect_auto() -> Option<Self> {
        Self::connect(auto_surface_id())
    }

    /// Block until `display_server` is reachable, then send the Phase
    /// 56 `Hello` + `CreateSurface` handshake. Returns `None` if the
    /// service never appears or any step fails.
    pub fn connect(surface_id: SurfaceId) -> Option<Self> {
        let handle = lookup_display_with_backoff()?;
        let token = client_token();
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
            client_token: token,
        };
        if !send_verb(handle, &hello) {
            return None;
        }
        let create = ClientMessage::CreateSurface {
            surface_id,
            client_token: token,
        };
        if !send_verb(handle, &create) {
            return None;
        }
        Some(Self {
            handle,
            token,
            surface_id,
        })
    }

    pub fn handle(&self) -> u32 {
        self.handle
    }

    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }

    pub fn token(&self) -> u32 {
        self.token
    }

    /// Send a `SetSurfaceRole` verb. Returns `true` on success.
    pub fn set_role(&self, role: SurfaceRole) -> bool {
        send_verb(
            self.handle,
            &ClientMessage::SetSurfaceRole {
                surface_id: self.surface_id,
                role,
            },
        )
    }

    /// Convenience: declare a Layer surface at a single layer / anchor
    /// combination.
    pub fn set_layer_role(
        &self,
        layer: Layer,
        anchor_mask: u8,
        exclusive_zone: u32,
        interactivity: KeyboardInteractivity,
    ) -> bool {
        self.set_role(SurfaceRole::Layer(LayerConfig {
            layer,
            anchor_mask,
            exclusive_zone,
            keyboard_interactivity: interactivity,
            margin: [0; 4],
        }))
    }

    pub fn set_toplevel_role(&self) -> bool {
        self.set_role(SurfaceRole::Toplevel)
    }

    /// Send `AttachSharedBuffer`, full-surface `DamageSurface`, and
    /// `CommitSurface` in sequence.
    pub fn attach_damage_commit(
        &self,
        buffer_id: BufferId,
        shm_id: u32,
        width: u32,
        height: u32,
    ) -> bool {
        let attach = ClientMessage::AttachSharedBuffer {
            surface_id: self.surface_id,
            buffer_id,
            shm_id,
            width,
            height,
        };
        if !send_verb(self.handle, &attach) {
            return false;
        }
        let damage = ClientMessage::DamageSurface {
            surface_id: self.surface_id,
            rect: Rect {
                x: 0,
                y: 0,
                w: width,
                h: height,
            },
        };
        if !send_verb(self.handle, &damage) {
            return false;
        }
        let commit = ClientMessage::CommitSurface {
            surface_id: self.surface_id,
        };
        send_verb(self.handle, &commit)
    }

    /// Pull one outbound event addressed at this client's surface.
    pub fn pull_event(&self) -> Option<ServerMessage> {
        let label = syscall_lib::ipc_call(
            self.handle,
            LABEL_CLIENT_EVENT_PULL,
            self.surface_id.0 as u64,
        );
        if label != LABEL_CLIENT_EVENT_PULL {
            let mut sink = [0u8; 64];
            let _ = syscall_lib::ipc_take_pending_bulk(&mut sink);
            return None;
        }
        let mut buf = [0u8; 96];
        let n = syscall_lib::ipc_take_pending_bulk(&mut buf);
        if n == 0 || n == u64::MAX {
            return None;
        }
        let len = (n as usize).min(buf.len());
        ServerMessage::decode(&buf[..len]).ok().map(|(m, _)| m)
    }

    /// Phase 105 Track B — publish `text` as the clipboard offer
    /// (`text/plain;charset=utf-8`). The compositor stores it until a
    /// later offer replaces it or this client disconnects. Returns
    /// `false` if `text` exceeds the single-IPC transport cap
    /// ([`CLIPBOARD_MAX_BYTES`]) or the IPC send fails.
    pub fn set_clipboard(&self, text: &str) -> bool {
        let bytes = text.as_bytes();
        if bytes.len() > CLIPBOARD_MAX_BYTES {
            return false;
        }
        // Frame (13 bytes: 4 header + 9 body) followed by the offer bytes,
        // in one IPC bulk. The compositor reads the trailing bytes after
        // decoding the frame.
        let msg = ClientMessage::SetClipboard {
            mime_tag: MimeTag::TextPlainUtf8,
            len: bytes.len() as u32,
            client_token: self.token,
        };
        let mut frame = [0u8; 16];
        let n = match msg.encode(&mut frame) {
            Ok(n) => n,
            Err(_) => return false,
        };
        let mut combined: Vec<u8> = Vec::with_capacity(n + bytes.len());
        combined.extend_from_slice(&frame[..n]);
        combined.extend_from_slice(bytes);
        let reply = syscall_lib::ipc_call_buf(self.handle, LABEL_VERB, 0, &combined);
        reply != u64::MAX
    }

    /// Phase 105 Track B — fetch the current clipboard offer's bytes, or
    /// `None` when the clipboard is empty or the request fails. The
    /// compositor answers synchronously with `[ClipboardData frame][bytes]`
    /// staged as the reply bulk.
    pub fn get_clipboard(&self) -> Option<Vec<u8>> {
        let msg = ClientMessage::RequestClipboard {
            mime_tag: MimeTag::TextPlainUtf8,
        };
        let mut frame = [0u8; 16];
        let n = msg.encode(&mut frame).ok()?;
        let reply = syscall_lib::ipc_call_buf(self.handle, LABEL_VERB, 0, &frame[..n]);
        if reply == u64::MAX {
            return None;
        }
        let mut buf = [0u8; CLIPBOARD_MAX_BYTES + 16];
        let got = syscall_lib::ipc_take_pending_bulk(&mut buf);
        if got == 0 || got == u64::MAX {
            return None;
        }
        let got = (got as usize).min(buf.len());
        // Decode the ClipboardData frame; the offer bytes follow it.
        let (data_hdr, consumed) = ServerMessage::decode(&buf[..got]).ok()?;
        let len = match data_hdr {
            ServerMessage::ClipboardData { len, .. } => len as usize,
            _ => return None,
        };
        if len == 0 {
            return None; // empty clipboard
        }
        let end = (consumed + len).min(got);
        if end <= consumed {
            return None;
        }
        Some(buf[consumed..end].to_vec())
    }

    /// Politely tell `display_server` we are done.
    pub fn goodbye(&self) {
        let _ = send_verb(
            self.handle,
            &ClientMessage::Goodbye {
                client_token: self.token,
            },
        );
    }
}

/// Shared-memory surface backing.
pub struct SharedSurface {
    pub shm_id: u32,
    pub va: u64,
    pub width: u32,
    pub height: u32,
    pub byte_len: usize,
}

impl SharedSurface {
    pub fn allocate(width: u32, height: u32) -> Option<Self> {
        // Reject pathological dimensions and use checked arithmetic so
        // a caller passing huge values can never advertise width/height
        // in `AttachSharedBuffer` that overflows past the SHM mapping.
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return None;
        }
        let byte_len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        let shm_id = syscall_lib::shm_create(byte_len);
        if shm_id == 0 {
            return None;
        }
        let va = syscall_lib::shm_map(shm_id);
        if va == 0 {
            let _ = syscall_lib::shm_destroy(shm_id);
            return None;
        }
        Some(Self {
            shm_id,
            va,
            width,
            height,
            byte_len,
        })
    }

    pub fn pixels_mut(&self) -> &'static mut [u32] {
        let count = self.byte_len / 4;
        unsafe { core::slice::from_raw_parts_mut(self.va as *mut u32, count) }
    }

    pub fn release(&self) {
        let _ = syscall_lib::shm_unmap(self.va);
        let _ = syscall_lib::shm_destroy(self.shm_id);
    }
}

/// Fill the entire surface with `color`.
pub fn fill(pixels: &mut [u32], color: u32) {
    for px in pixels.iter_mut() {
        *px = color;
    }
}

/// Fill an axis-aligned rectangle.
pub fn fill_rect(
    pixels: &mut [u32],
    stride: u32,
    height: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: u32,
) {
    let stride = stride as i32;
    let height = height as i32;
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w as i32).min(stride);
    let y1 = (y + h as i32).min(height);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    for row in y0..y1 {
        for col in x0..x1 {
            let idx = (row * stride + col) as usize;
            if idx < pixels.len() {
                pixels[idx] = color;
            }
        }
    }
}

/// Draw a 1-pixel border around a rectangle.
pub fn stroke_rect(
    pixels: &mut [u32],
    stride: u32,
    height: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: u32,
) {
    if w == 0 || h == 0 {
        return;
    }
    fill_rect(pixels, stride, height, x, y, w, 1, color);
    fill_rect(pixels, stride, height, x, y + (h as i32 - 1), w, 1, color);
    fill_rect(pixels, stride, height, x, y, 1, h, color);
    fill_rect(pixels, stride, height, x + (w as i32 - 1), y, 1, h, color);
}

/// Draw an ASCII string with the bundled 8×16 bitmap font at native
/// scale. Returns the rendered width in pixels. Codepoints outside
/// ASCII fall back to the centred-dot glyph.
pub fn draw_text(
    pixels: &mut [u32],
    stride: u32,
    height: u32,
    x: i32,
    y: i32,
    text: &str,
    fg: u32,
    bg: u32,
) -> i32 {
    draw_text_scaled(pixels, stride, height, x, y, text, fg, bg, 1)
}

/// Draw an ASCII string with the bundled 8×16 bitmap font, scaled
/// by `scale` (each source pixel becomes a `scale × scale` block).
/// Used by HiDPI surfaces (1080p+) so the text matches the
/// framebuffer's higher pixel density. Returns the rendered width
/// in pixels (`8 * scale * text.len()` if nothing clipped).
pub fn draw_text_scaled(
    pixels: &mut [u32],
    stride: u32,
    height: u32,
    x: i32,
    y: i32,
    text: &str,
    fg: u32,
    bg: u32,
    scale: u32,
) -> i32 {
    let font = BasicBitmapFont::new();
    let (cw, ch) = font.cell_size();
    let scale_i = scale.max(1) as i32;
    let cw_i = cw as i32 * scale_i;
    let mut cx = x;
    for ch_byte in text.bytes() {
        if cx + cw_i > stride as i32 {
            break;
        }
        let g: &Glyph = font.glyph_or_fallback(ch_byte as u32);
        draw_glyph_alpha(pixels, stride, height, cx, y, g, fg, bg, scale.max(1));
        cx += cw_i;
    }
    let _ = ch;
    cx - x
}

fn draw_glyph_alpha(
    pixels: &mut [u32],
    stride: u32,
    height: u32,
    x: i32,
    y: i32,
    g: &Glyph,
    fg: u32,
    bg: u32,
    scale: u32,
) {
    let w = g.width as usize;
    let h = g.height as usize;
    let s = scale.max(1) as i32;
    let bytes_per_row = w.div_ceil(8);
    for row in 0..h {
        let row_start = row * bytes_per_row;
        for col in 0..w {
            let byte_idx = row_start + (col / 8);
            if byte_idx >= g.bitmap.len() {
                continue;
            }
            let bit_idx = 7 - (col % 8);
            let bit_set = (g.bitmap[byte_idx] >> bit_idx) & 1 == 1;
            let color = if bit_set { fg } else { bg };
            // Emit a `s × s` block in the destination for this source
            // pixel. Pixel-doubling stays sharp for monospace bitmap
            // fonts; bilinear filtering would make small glyphs
            // blurry.
            for dy in 0..s {
                let py = y + row as i32 * s + dy;
                if py < 0 || py >= height as i32 {
                    continue;
                }
                let row_off = (py as usize) * (stride as usize);
                for dx in 0..s {
                    let px = x + col as i32 * s + dx;
                    if px < 0 || px >= stride as i32 {
                        continue;
                    }
                    let idx = row_off + px as usize;
                    if idx < pixels.len() {
                        pixels[idx] = color;
                    }
                }
            }
        }
    }
}

/// Re-export anchor constants so clients don't import directly from
/// kernel-core in two places.
pub mod anchor {
    pub use kernel_core::display::protocol::{
        ANCHOR_BOTTOM, ANCHOR_CENTER, ANCHOR_LEFT, ANCHOR_RIGHT, ANCHOR_TOP,
    };
}

/// Re-export of [`kernel_core::input::events::KeyEvent`] for clients
/// that drain events.
pub type Key = KeyEvent;

fn send_verb(handle: u32, msg: &ClientMessage) -> bool {
    let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
    let len = match msg.encode(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let reply = syscall_lib::ipc_call_buf(handle, LABEL_VERB, 0, &buf[..len]);
    reply != u64::MAX
}

fn lookup_display_with_backoff() -> Option<u32> {
    for attempt in 0..2000u32 {
        let raw = syscall_lib::ipc_lookup_service("display");
        if raw != u64::MAX {
            return Some(raw as u32);
        }
        if attempt + 1 == 2000 {
            return None;
        }
        let _ = syscall_lib::nanosleep_for(0, 5_000_000);
    }
    None
}

fn client_token() -> u32 {
    let pid = syscall_lib::getpid();
    if pid > 0 { pid as u32 } else { 0xD0E5_0001 }
}

/// Derive a per-process surface id that does not collide with
/// well-known fixed ids used by other compositor clients.
///
/// The compositor's surface registry is keyed by `SurfaceId` globally
/// (not per-client), so two processes that both pick `SurfaceId(1)`
/// stomp on each other: the second `CreateSurface` is rejected as a
/// duplicate, and all subsequent verbs (`SetSurfaceRole`,
/// `AttachSharedBuffer`, ...) silently target the *first* process's
/// surface. The visible symptom is that a Phase 73 daemon (bar /
/// wallpaper / ...) starting after greeter mutates greeter's role
/// from `Toplevel` to `Layer::Top`, turning the login page into a 24
/// px strip at the top of the screen until greeter's session
/// manager respawns it.
///
/// Reserved ranges:
/// * `SurfaceId(1..=0x3FFF)` — fixed ids (greeter = 1, future
///   well-known clients).
/// * `SurfaceId(0x4000..=0x7FFF)` — `term` (`0x4000 + pid`).
/// * `SurfaceId(0x8000..=0xFFFF)` — Phase 73 desktop clients
///   (`0x8000 + pid`, this function).
///
/// Each `DisplayConnection::connect_auto` call uses this helper.
pub fn auto_surface_id() -> SurfaceId {
    let pid = syscall_lib::getpid();
    if pid > 0 {
        SurfaceId(0x8000u32.wrapping_add(pid as u32))
    } else {
        // PID lookup failed — fall back to a fixed mid-range id. Two
        // such fallbacks would collide, but a userspace process
        // without a valid PID is already in an unrecoverable state.
        SurfaceId(0x8001)
    }
}

/// Query the kernel framebuffer dimensions. Used by clients that
/// allocate full-output surfaces (wallpaper, lockscreen, greeter, the
/// bar's full-width Layer surface) so they size to the running
/// hardware instead of a hardcoded resolution. Falls back to a
/// conservative 1280×800 if the syscall fails — keeps a misconfigured
/// boot bootable instead of crashing the client.
pub fn output_size() -> (u32, u32) {
    // Wire layout: `width u32 | height u32 | stride u32 | bpp u32 |
    // pixel_format u32` — 20 bytes total, defined in
    // `kernel::arch::x86_64::syscall::sys_framebuffer_info`.
    let mut buf = [0u8; 20];
    let rc = syscall_lib::framebuffer_info(&mut buf);
    if rc < 0 {
        return (1280, 800);
    }
    let width = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let height = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if width == 0 || height == 0 {
        (1280, 800)
    } else {
        (width, height)
    }
}

/// Helper: collect a `Vec<KeyEvent>` from the connection until it
/// produces no more events. Useful for the launcher's "drain all
/// pending keystrokes before re-rendering" pattern.
pub fn drain_keys(conn: &DisplayConnection) -> Vec<KeyEvent> {
    let mut out = Vec::new();
    for _ in 0..32 {
        match conn.pull_event() {
            Some(ServerMessage::Key(ev)) => out.push(ev),
            Some(_) => continue,
            None => break,
        }
    }
    out
}
