//! Phase 57 Track G.5 close-out — production display-server client.
//!
//! `DisplayClient` is the live counterpart to the `FakeFb` test
//! fixture in [`crate::render::tests`]. It owns:
//!
//! - the IPC handle for `display_server`;
//! - the `SurfaceId` and `BufferId` term claims;
//! - a 640 × 400 BGRA8888 [`SurfaceBuffer`] that backs the term grid
//!   (80 × 25 cells × 8 × 16 px);
//! - the [`BasicBitmapFont`] used to rasterise glyphs into the
//!   buffer.
//!
//! On every [`compose`](crate::render::Renderer::compose), the
//! renderer drives `DisplayClient` through the [`FramebufferOwner`]
//! trait. `submit` chunks the local 1 MB buffer through the
//! `LABEL_PIXELS_CHUNK` wire (added in the same Phase 57 PR) and
//! drives the `AttachBuffer` → `DamageSurface` → `CommitSurface`
//! verb sequence.
//!
//! ## Why one BufferId per frame is not used
//!
//! Phase 56's `AttachBuffer` consumes a `pending_bulk` slot keyed by
//! `BufferId`. Re-using the same id every frame works because each
//! commit drains the slot. A future tracking phase may grow per-
//! frame ids for double-buffering; today the single-id pattern keeps
//! the protocol footprint minimal.

use alloc::vec;
use alloc::vec::Vec;

use kernel_core::display::pixel_chunk::{
    CHUNK_HEADER_LEN, PixelChunkHeader, cell_pixel_offset, chunk_plan,
};
use kernel_core::display::protocol::{
    BufferId, ClientMessage, PROTOCOL_VERSION, Rect, SurfaceId, SurfaceRole,
};
use kernel_core::session::font::{BasicBitmapFont, FontProvider};
use surface_buffer::{PixelFormat, SurfaceBuffer, SurfaceBufferId};
use syscall_lib::STDOUT_FILENO;

use crate::render::FramebufferOwner;
use crate::{DEFAULT_COLS, DEFAULT_ROWS, TermError};

/// IPC label for protocol verbs (mirrors `display_server::client::LABEL_VERB`).
const LABEL_VERB: u64 = 1;
/// IPC label for chunked pixel uploads (mirrors
/// `display_server::client::LABEL_PIXELS_CHUNK`).
const LABEL_PIXELS_CHUNK: u64 = 5;

/// Per-attempt sleep between display-server lookups (5 ms). Mirrors
/// the gfx-demo / kbd_server bounded retry shape.
const LOOKUP_BACKOFF_NS: u32 = 5_000_000;
/// Maximum lookup attempts before [`DisplayClient::connect`] gives up.
const LOOKUP_MAX_ATTEMPTS: u32 = 8;

/// Surface id term claims. Stable across the binary lifetime — only
/// one Toplevel surface per `term` instance.
const SURFACE_ID: SurfaceId = SurfaceId(1);
/// Buffer id term re-uses each frame. See module-level docs.
const BUFFER_ID: BufferId = BufferId(1);

/// Background colour used by [`FramebufferOwner::clear`] and by
/// [`FramebufferOwner::scroll`] when blanking the new bottom row.
/// Black `0x00000000` matches the screen state machine's default
/// background and avoids the framebuffer flashing teal between
/// frames before the first PutGlyph paints over it.
const DEFAULT_BG_BGRA: u32 = 0x0000_0000;
/// Foreground colour used by [`FramebufferOwner::put_glyph`] when
/// the screen-supplied `fg`/`bg` are both zero (e.g. in early-boot
/// frames before any SGR has fired). White-on-black is the screen's
/// default, but the screen always passes explicit colours; this
/// constant just protects against zero-zero pairs producing an
/// invisible glyph.
const FALLBACK_FG_BGRA: u32 = 0x00FF_FFFF;

/// Pixel width of the term surface, in pixels.
pub const SURFACE_WIDTH_PX: u32 = (DEFAULT_COLS as u32) * (CELL_WIDTH as u32);
/// Pixel height of the term surface, in pixels.
pub const SURFACE_HEIGHT_PX: u32 = (DEFAULT_ROWS as u32) * (CELL_HEIGHT as u32);
/// Cell pixel width — pinned to the bundled font's cell size.
pub const CELL_WIDTH: u8 = 8;
/// Cell pixel height — pinned to the bundled font's cell size.
pub const CELL_HEIGHT: u8 = 16;

/// Maximum payload bytes per chunk = MAX_BULK_LEN (4096) minus the
/// 24-byte chunk header. Each [`FramebufferOwner::submit`] uploads
/// `ceil(SURFACE_WIDTH_PX * SURFACE_HEIGHT_PX * 4 / CHUNK_PAYLOAD_LEN)`
/// chunks per frame.
const CHUNK_PAYLOAD_LEN: usize = 4096 - CHUNK_HEADER_LEN;

/// Stack-sized encode buffer for protocol verbs. The widest
/// `ClientMessage` body in Phase 57 is `SetSurfaceRole(Layer{...})`
/// at ~24 bytes; a 64-byte buffer is ample.
const VERB_ENCODE_BUF_LEN: usize = 64;

/// Production [`FramebufferOwner`] for the `term` graphical client.
pub struct DisplayClient {
    server_handle: u32,
    surface: SurfaceBuffer,
    chunk_buf: Vec<u8>,
}

impl DisplayClient {
    /// Look up `display_server`, send the `Hello` + `CreateSurface`
    /// + `SetSurfaceRole(Toplevel)` round-trip, allocate the local
    /// 640 × 400 BGRA buffer, and return a ready-to-submit
    /// `DisplayClient`. Returns a typed error if the lookup, encode,
    /// or `ipc_call_buf` fails.
    pub fn connect() -> Result<Self, TermError> {
        let server_handle = match Self::lookup_with_backoff() {
            Some(h) => h,
            None => return Err(TermError::DisplayServerUnavailable),
        };
        let mut buf = [0u8; VERB_ENCODE_BUF_LEN];

        // 1. Hello.
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
        };
        if !Self::send_verb(server_handle, &hello, &mut buf, "Hello") {
            return Err(TermError::DisplayServerUnavailable);
        }

        // 2. CreateSurface.
        let create = ClientMessage::CreateSurface {
            surface_id: SURFACE_ID,
        };
        if !Self::send_verb(server_handle, &create, &mut buf, "CreateSurface") {
            return Err(TermError::DisplayServerUnavailable);
        }

        // 3. SetSurfaceRole(Toplevel).
        let role = ClientMessage::SetSurfaceRole {
            surface_id: SURFACE_ID,
            role: SurfaceRole::Toplevel,
        };
        if !Self::send_verb(server_handle, &role, &mut buf, "SetSurfaceRole") {
            return Err(TermError::DisplayServerUnavailable);
        }

        // 4. Allocate the local pixel store. 640 × 400 × 4 = 1 MiB,
        //    well under the 4 MiB chunked-transport cap.
        let surface = SurfaceBuffer::new(
            SurfaceBufferId(BUFFER_ID.0),
            SURFACE_WIDTH_PX,
            SURFACE_HEIGHT_PX,
            PixelFormat::Bgra8888,
        )
        .map_err(|_| TermError::DisplayServerUnavailable)?;

        let chunk_buf = vec![0u8; CHUNK_HEADER_LEN + CHUNK_PAYLOAD_LEN];

        Ok(Self {
            server_handle,
            surface,
            chunk_buf,
        })
    }

    /// Mutable access to the underlying pixel buffer — used by the
    /// `FramebufferOwner` impl below and (in the future) by
    /// rendering paths that need to write directly without going
    /// through a glyph.
    fn pixels_mut(&mut self) -> &mut [u8] {
        self.surface.pixels_mut()
    }

    /// `display_server` lookup with bounded retry. Mirrors
    /// `gfx-demo::lookup_display_with_backoff`.
    fn lookup_with_backoff() -> Option<u32> {
        for attempt in 0..LOOKUP_MAX_ATTEMPTS {
            let raw = syscall_lib::ipc_lookup_service("display");
            if raw != u64::MAX {
                return Some(raw as u32);
            }
            if attempt + 1 == LOOKUP_MAX_ATTEMPTS {
                return None;
            }
            let _ = syscall_lib::nanosleep_for(0, LOOKUP_BACKOFF_NS);
        }
        None
    }

    /// Encode + send one `ClientMessage` via `ipc_call_buf`. Returns
    /// `true` on success. The `step` string is for log messages.
    fn send_verb(handle: u32, msg: &ClientMessage, buf: &mut [u8], step: &str) -> bool {
        let len = match msg.encode(buf) {
            Ok(n) => n,
            Err(_) => {
                syscall_lib::write_str(STDOUT_FILENO, "term: display verb encode failed: ");
                syscall_lib::write_str(STDOUT_FILENO, step);
                syscall_lib::write_str(STDOUT_FILENO, "\n");
                return false;
            }
        };
        let reply = syscall_lib::ipc_call_buf(handle, LABEL_VERB, 0, &buf[..len]);
        if reply == u64::MAX {
            syscall_lib::write_str(STDOUT_FILENO, "term: display verb ipc_call_buf failed: ");
            syscall_lib::write_str(STDOUT_FILENO, step);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
            return false;
        }
        true
    }

    /// Upload the entire local surface to `display_server` via the
    /// chunked-bulk wire. Each chunk is 4 KiB (24-byte header + up
    /// to 4072 pixel bytes); total chunk count is
    /// `ceil(byte_len() / CHUNK_PAYLOAD_LEN)` ≈ 257 for the default
    /// 1 MiB buffer.
    fn upload_chunked(&mut self) -> bool {
        let byte_len = self.surface.byte_len() as u32;
        // `chunk_plan` produces the (offset, chunk_len) sequence and
        // is host-tested in `kernel_core::display::pixel_chunk` —
        // the production loop here just translates each pair into
        // an IPC bulk call.
        for (offset, chunk_len) in chunk_plan(byte_len, CHUNK_PAYLOAD_LEN as u32) {
            let header = PixelChunkHeader {
                buffer_id: BUFFER_ID.0,
                width: SURFACE_WIDTH_PX,
                height: SURFACE_HEIGHT_PX,
                total_bytes: byte_len,
                offset,
                chunk_len,
            };
            if header
                .encode(&mut self.chunk_buf[..CHUNK_HEADER_LEN])
                .is_err()
            {
                syscall_lib::write_str(STDOUT_FILENO, "term: chunk header encode failed\n");
                return false;
            }
            let src_start = offset as usize;
            let src_end = src_start + chunk_len as usize;
            self.chunk_buf[CHUNK_HEADER_LEN..CHUNK_HEADER_LEN + chunk_len as usize]
                .copy_from_slice(&self.surface.pixels()[src_start..src_end]);
            let payload = &self.chunk_buf[..CHUNK_HEADER_LEN + chunk_len as usize];
            let reply = syscall_lib::ipc_call_buf(
                self.server_handle,
                LABEL_PIXELS_CHUNK,
                BUFFER_ID.0 as u64,
                payload,
            );
            if reply == u64::MAX {
                syscall_lib::write_str(STDOUT_FILENO, "term: chunked upload ipc_call_buf failed\n");
                return false;
            }
        }
        true
    }

    /// Send the post-chunked `AttachBuffer` + `DamageSurface(full)` +
    /// `CommitSurface` verbs.
    fn finalise_frame(&mut self) -> bool {
        let mut buf = [0u8; VERB_ENCODE_BUF_LEN];

        let attach = ClientMessage::AttachBuffer {
            surface_id: SURFACE_ID,
            buffer_id: BUFFER_ID,
        };
        if !Self::send_verb(self.server_handle, &attach, &mut buf, "AttachBuffer") {
            return false;
        }

        let damage = ClientMessage::DamageSurface {
            surface_id: SURFACE_ID,
            rect: Rect {
                x: 0,
                y: 0,
                w: SURFACE_WIDTH_PX,
                h: SURFACE_HEIGHT_PX,
            },
        };
        if !Self::send_verb(self.server_handle, &damage, &mut buf, "DamageSurface") {
            return false;
        }

        let commit = ClientMessage::CommitSurface {
            surface_id: SURFACE_ID,
        };
        if !Self::send_verb(self.server_handle, &commit, &mut buf, "CommitSurface") {
            return false;
        }
        true
    }
}

impl FramebufferOwner for DisplayClient {
    fn put_glyph(&mut self, row: u16, col: u16, codepoint: u32, fg: u32, bg: u32) {
        // Resolve fg/bg fallbacks. The screen always passes explicit
        // colours, but defending against the all-zero pair keeps a
        // future caller from rendering invisible glyphs.
        let fg = if fg == 0 && bg == 0 {
            FALLBACK_FG_BGRA
        } else {
            fg
        };
        let bg = if fg == bg { DEFAULT_BG_BGRA } else { bg };

        // Resolve the cell's u32-pixel offset within the surface
        // buffer. Out-of-grid requests are silently dropped — the
        // helper is host-tested in
        // `kernel_core::display::pixel_chunk::cell_pixel_offset_*`.
        let stride_pixels = SURFACE_WIDTH_PX as usize;
        let cell_offset = match cell_pixel_offset(
            row,
            col,
            CELL_WIDTH,
            CELL_HEIGHT,
            SURFACE_WIDTH_PX,
            SURFACE_HEIGHT_PX,
        ) {
            Some(o) => o,
            None => return,
        };

        // The font's `render_into` writes BGRA8888 pixels into a
        // caller-supplied `&mut [u32]` row-major buffer. We carve
        // a sub-slice of the cell's pixels and pass the surface
        // stride (in u32 pixels) so glyph rows index correctly into
        // the larger surface buffer.
        let pixels = self.pixels_mut();
        let pixel_count = pixels.len() / 4;
        // SAFETY: SurfaceBuffer allocates `width * height * 4` bytes
        // with default alignment. `Vec<u8>` is aligned to at least 1
        // byte; reinterpreting as `[u32]` requires 4-byte alignment.
        // The surface_buffer crate's allocator is the global heap
        // (`BrkAllocator`) which honours requested alignment up to
        // pointer width. The cast is sound on x86_64 where
        // `align_of::<u32>() = 4`.
        let pixels_u32: &mut [u32] = unsafe {
            core::slice::from_raw_parts_mut(pixels.as_mut_ptr() as *mut u32, pixel_count)
        };
        let cell_view = &mut pixels_u32[cell_offset..];

        // Look up the glyph; if missing, paint the cell with bg only.
        let font = BasicBitmapFont::new();
        match font.glyph(codepoint) {
            Some(glyph) => {
                let _ = glyph.render_into(cell_view, stride_pixels, fg, bg);
            }
            None => {
                fill_cell_bg(
                    cell_view,
                    stride_pixels,
                    CELL_WIDTH as usize,
                    CELL_HEIGHT as usize,
                    bg,
                );
            }
        }
    }

    fn clear(&mut self) {
        self.surface.fill(DEFAULT_BG_BGRA);
    }

    fn scroll(&mut self, amount: i16) {
        if amount == 0 {
            return;
        }
        let stride = (SURFACE_WIDTH_PX as usize) * 4;
        let row_bytes = stride * (CELL_HEIGHT as usize);
        let buf_len = self.surface.byte_len();
        let pixels = self.pixels_mut();
        if amount > 0 {
            // Scroll up: shift everything up by `amount * row_bytes`,
            // blank the bottom `amount` rows.
            let shift = (amount as usize).saturating_mul(row_bytes).min(buf_len);
            if shift >= buf_len {
                pixels.fill(0);
                return;
            }
            pixels.copy_within(shift.., 0);
            for byte in &mut pixels[buf_len - shift..] {
                *byte = 0;
            }
        } else {
            // Scroll down: shift everything down, blank the top.
            let mag = (-(amount as i32)) as usize;
            let shift = mag.saturating_mul(row_bytes).min(buf_len);
            if shift >= buf_len {
                pixels.fill(0);
                return;
            }
            pixels.copy_within(0..buf_len - shift, shift);
            for byte in &mut pixels[..shift] {
                *byte = 0;
            }
        }
    }

    fn submit(&mut self) {
        if !self.upload_chunked() {
            return;
        }
        let _ = self.finalise_frame();
    }
}

/// Paint a `cw × ch` cell with the background colour. Used as the
/// missing-glyph fallback so a private-use codepoint produces a
/// solid bg cell rather than a stale image of the previous tenant.
fn fill_cell_bg(cell_view: &mut [u32], stride_pixels: usize, cw: usize, ch: usize, bg: u32) {
    for row in 0..ch {
        let row_start = row * stride_pixels;
        for col in 0..cw {
            let i = row_start + col;
            if i >= cell_view.len() {
                return;
            }
            cell_view[i] = bg;
        }
    }
}
