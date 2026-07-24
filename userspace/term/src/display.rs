//! Phase 57 Track G.5 close-out — production display-server client
//! (extended in Phase 69c for atlas-backed glyph dispatch and the
//! 1280 × 800 / 16 × 32 cell layout; extended again in the
//! 2026-05-17 less-render-disappearance fix for double-buffered
//! publish so display_server never snapshots mid-paint).
//!
//! `DisplayClient` is the live counterpart to the `FakeFb` test
//! fixture in [`crate::render::tests`]. It owns:
//!
//! - the IPC handle for `display_server`;
//! - the `SurfaceId` term claims;
//! - **two** 1280 × 800 BGRA8888 shared-memory mappings (front +
//!   back), each with its own `BufferId`. The renderer always writes
//!   to the back mapping; on `submit` term sends
//!   `AttachSharedBuffer(back)` + `DamageSurface` + `CommitSurface`,
//!   flips back/front, and memcpys the just-committed pixels into
//!   the new back so incremental ops (scroll, partial put) have an
//!   up-to-date starting state.
//!   See [`SurfaceMapping`] for the per-mapping fields and
//!   [`DisplayClient::submit`] for the swap sequence.
//!   The `kernel_core::display::surface_buffer::SurfaceBuffer` type
//!   is no longer instantiated on this client path.
//!
//! Phase 69c replaced the in-client glyph rasteriser with the
//! `kernel-core::font` atlas: the renderer pre-resolves each glyph
//! to a [`GlyphView`](kernel_core::font::GlyphView) and
//! `DisplayClient::put_glyph` simply blits the packed-bits bitmap
//! into the backing surface — the framebuffer owner no longer
//! branches on resolution policy.
//!
//! On every [`compose`](crate::render::Renderer::compose), the
//! renderer drives `DisplayClient` through the [`FramebufferOwner`]
//! trait. `term` writes pixels directly into the SHM mapping (no
//! IPC payload) and `submit` drives the `AttachSharedBuffer` →
//! `DamageSurface` → `CommitSurface` verb sequence. The legacy
//! per-pixel `LABEL_PIXELS_CHUNK` wire is no longer the upload path.
//!
//! ## Why double-buffering
//!
//! `docs/handoffs/2026-05-17-less-render-disappearance.md` documents
//! a snapshot-during-write race: with a single SHM region,
//! `display_server`'s `pixels_snapshot` would fire while term was
//! mid-`fb.clear()` + `put_glyph` and the resulting screendump
//! captured a torn — often nearly all-black — frame. The compose-
//! timing trace probe in that handoff showed 14 of 45 snapshot
//! intervals overlapping a term compose interval. Double-buffering
//! plus the existing `pending_buffer` → `committed_buffer` move on
//! `CommitSurface` (`userspace/display_server/src/surface.rs:677`)
//! eliminates the overlap by construction: term writes only to the
//! back buffer, display_server snapshots only the committed (front)
//! buffer, and `CommitSurface` is the single atomic publication
//! point. No protocol change required.

use alloc::vec::Vec;
use kernel_core::display::pixel_chunk::cell_pixel_offset;
use kernel_core::display::protocol::{
    BufferId, CLIPBOARD_MAX_BYTES, ClientMessage, MimeTag, PROTOCOL_VERSION, Rect, ServerMessage,
    SurfaceId, SurfaceRole,
};
use kernel_core::font::GlyphView;
use syscall_lib::STDOUT_FILENO;

use crate::render::FramebufferOwner;
use crate::{DEFAULT_COLS, DEFAULT_ROWS, TermError};

/// IPC label for protocol verbs (mirrors `display_server::client::LABEL_VERB`).
const LABEL_VERB: u64 = 1;

/// Per-attempt sleep between display-server lookups (5 ms). Mirrors
/// the gfx-demo / kbd_server bounded retry shape.
const LOOKUP_BACKOFF_NS: u32 = 5_000_000;
/// Maximum lookup attempts before [`DisplayClient::connect`] gives up.
const LOOKUP_MAX_ATTEMPTS: u32 = 2000;

/// Surface id term claims. Stable across the binary lifetime — only
/// one Toplevel surface per `term` instance.
///
/// Per-process surface id. Phase 72b — when multiple `term` instances
/// run concurrently (e.g. spawned via `SUPER+RETURN`), each must own a
/// distinct `SurfaceId`; otherwise the second instance's
/// `CreateSurface` is rejected as a duplicate by display_server and
/// its later commits target the *first* term's surface, producing the
/// "two terms stacked into one buffer" visual stomp.
///
/// Mirrors the display_client_ffi seed: `0x4000 + PID`. The `0x4000`
/// base reserves the low 16k for future statically-allocated surface
/// ids (today: `SurfaceId(1)` for the legacy single-term path,
/// `SurfaceId(2)` for greeter). The PID offset keeps the value
/// unique across concurrent processes.
///
/// Function form rather than `const` because PID is not known at
/// compile time. Each `term` process computes the value once at
/// startup and caches it; the input-loop split in `term::main`
/// re-derives via the same helper so the value stays single-sourced.
pub fn surface_id() -> SurfaceId {
    let pid = syscall_lib::getpid();
    if pid > 0 {
        SurfaceId(0x4000u32.wrapping_add(pid as u32))
    } else {
        // PID lookup failed — fall back to a fixed mid-range id. Two
        // such fallbacks would collide; the assumption is that a
        // userspace process always has a valid PID. Distinct from
        // display_client_ffi's 0x4001 fallback so a same-fallback DOOM
        // and term would still differ by namespace.
        SurfaceId(0x4002)
    }
}
/// Two `BufferId`s, one per SHM mapping in the double-buffered
/// publish path. Indexed `BUFFER_IDS[back_idx]` so the protocol
/// `buffer_id` always matches the SHM region term writes into and
/// display_server snapshots from.
const BUFFER_IDS: [BufferId; 2] = [BufferId(1), BufferId(2)];

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

/// Initial pixel width of the term surface, in pixels. Used at
/// `DisplayClient::connect` to size the first SHM region. Phase 72b
/// — the live surface dimensions are tracked as runtime fields on
/// `DisplayClient` (`width` / `height`) and updated when the
/// compositor sends `ServerMessage::SurfaceResized`. The constants
/// stay public because callers still use them as the canonical
/// "default cell-grid pixel area" sentinel.
pub const SURFACE_WIDTH_PX: u32 = (DEFAULT_COLS as u32) * (CELL_WIDTH as u32);
/// Initial pixel height of the term surface, in pixels. See
/// [`SURFACE_WIDTH_PX`].
pub const SURFACE_HEIGHT_PX: u32 = (DEFAULT_ROWS as u32) * (CELL_HEIGHT as u32);
/// Cell pixel width. The Phase 69c TTF atlas rasterises glyphs into
/// this cell size, so a wider cell produces more legible Nerd Font
/// glyphs without changing the 80×25 column/row contract. Phase 73
/// bumped from 16 to 24 (3× the static 8×16 fallback width) so the
/// terminal stays legible on a 1080p framebuffer; the static IBM VGA
/// bitmap still occupies a clean integer sub-rect (top-left 8×16).
/// Phase 112 Track B.1 moved the definition to the crate root so the
/// (ungated) `mouse` module can share it — see [`crate::CELL_WIDTH`].
/// Re-exported here because the renderer and the layout constants above
/// have referred to `display::CELL_WIDTH` since Phase 57.
pub use crate::{CELL_HEIGHT, CELL_WIDTH};

/// Stack-sized encode buffer for protocol verbs. The widest
/// `ClientMessage` body in Phase 57 is `SetSurfaceRole(Layer{...})`
/// at ~24 bytes; a 64-byte buffer is ample.
const VERB_ENCODE_BUF_LEN: usize = 64;
static DISPLAY_VERB_FAILURE_LOG_BUDGET: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(16);

/// Query the kernel framebuffer pixel size (`width`, `height`) so the
/// initial surface allocation never exceeds the physical panel.
///
/// Mirrors `desktop_client::output_size` (same `sys_framebuffer_info` wire
/// layout: `width u32 | height u32 | stride u32 | bpp u32 | pixel_format
/// u32`). Returns `(0, 0)` if the syscall fails; the caller then keeps the
/// default cell-grid size. term replicates this locally rather than taking a
/// `desktop_client` dependency for one helper.
fn query_output_size() -> (u32, u32) {
    let mut buf = [0u8; 20];
    if syscall_lib::framebuffer_info(&mut buf) < 0 {
        return (0, 0);
    }
    let w = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let h = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    (w, h)
}

/// One SHM region paired with the `BufferId` term publishes it
/// under. Created twice per [`DisplayClient`] for the double-buffered
/// publish path.
///
/// `Drop` unmaps the region from term's address space and releases
/// the creator-reference, returning the frames to the buddy
/// allocator. Without the release, the region would remain pinned
/// in the kernel's SHM registry until reboot — display_server's
/// `CommittedBuffer::Drop` only releases its own holder reference.
struct SurfaceMapping {
    /// User-virtual base of the mapping. `0` means uninitialised
    /// (alloc failed); checked by `Drop` to skip the unmap syscall.
    surface_va: u64,
    /// Page-aligned byte length of the mapping. Same on both
    /// mappings; tracked per-mapping so `Drop` doesn't need to read
    /// outer state.
    surface_len: usize,
    /// SHM id assigned by the kernel registry. Travels in
    /// `AttachSharedBuffer` so `display_server` can map the same
    /// frames into its own address space.
    shm_id: u32,
    /// Protocol `BufferId` for this mapping. Stable across the
    /// surface's lifetime; the same id is re-attached every time
    /// the renderer publishes through this slot.
    buffer_id: BufferId,
}

impl SurfaceMapping {
    /// Allocate + map one SHM region of `byte_len` bytes, tag it
    /// with `buffer_id`, and return the bundle. Returns `None` on
    /// `shm_create` or `shm_map` failure; the caller surfaces the
    /// failure as `TermError::DisplayServerUnavailable`.
    fn allocate(byte_len: usize, buffer_id: BufferId) -> Option<Self> {
        let shm_id = syscall_lib::shm_create(byte_len);
        if shm_id == 0 {
            syscall_lib::write_str(STDOUT_FILENO, "term: shm_create failed\n");
            return None;
        }
        let surface_va = syscall_lib::shm_map(shm_id);
        if surface_va == 0 {
            syscall_lib::write_str(STDOUT_FILENO, "term: shm_map failed\n");
            // Release the creator's +1 reference so the region's
            // frames return to the buddy instead of leaking.
            let _ = syscall_lib::shm_destroy(shm_id);
            return None;
        }
        let surface_len = byte_len.div_ceil(4096) * 4096;
        Some(Self {
            surface_va,
            surface_len,
            shm_id,
            buffer_id,
        })
    }
}

impl Drop for SurfaceMapping {
    fn drop(&mut self) {
        if self.surface_va != 0 {
            let _ = syscall_lib::shm_unmap(self.surface_va);
        }
        if self.shm_id != 0 {
            let _ = syscall_lib::shm_destroy(self.shm_id);
        }
    }
}

/// Production [`FramebufferOwner`] for the `term` graphical client.
///
/// Phase 57d gave term a single SHM mapping; the 2026-05-17 less-
/// render-disappearance fix split that into two so display_server's
/// `pixels_snapshot` and term's compose writes never touch the same
/// region simultaneously. See the module-level docs and
/// [`SurfaceMapping`] for the per-mapping fields.
pub struct DisplayClient {
    server_handle: u32,
    /// Two SHM mappings indexed by `back_idx` / `1 - back_idx`. The
    /// renderer always writes into `surfaces[back_idx]`; display_
    /// server snapshots the *front* — i.e. the buffer named by the
    /// most recent `CommitSurface`. The buffers swap roles on every
    /// successful submit. See [`DisplayClient::submit`].
    surfaces: [SurfaceMapping; 2],
    /// Index (0 or 1) of the back buffer — the one the renderer
    /// currently writes into. Flipped at the end of every successful
    /// submit so the just-published buffer becomes the new front and
    /// the previously-front buffer becomes the new back.
    back_idx: usize,
    /// Phase 72b — per-instance surface id (PID-derived). Cached at
    /// connect time so every protocol verb references the same value.
    /// See [`surface_id`] for the derivation.
    surface_id: SurfaceId,
    /// Phase 112 Track B.2 — the PID-derived `client_token` sent in
    /// `Hello`. Cached because `SetClipboard` carries it too: the
    /// compositor scopes offer ownership to this token so the offer drops
    /// on our `Goodbye`.
    client_token: u32,
    /// Phase 72b — current surface pixel width. Initialised from
    /// [`SURFACE_WIDTH_PX`]; updated by [`DisplayClient::resize`] when
    /// the compositor sends `ServerMessage::SurfaceResized`. Cached
    /// here so every per-frame `AttachSharedBuffer` / `DamageSurface`
    /// uses the live dimensions instead of compile-time constants.
    width: u32,
    /// Phase 72b — current surface pixel height. See [`width`].
    height: u32,
}

impl DisplayClient {
    /// Look up `display_server`, send the `Hello` + `CreateSurface`
    /// + `SetSurfaceRole(Toplevel)` round-trip, allocate **two**
    /// ~4 MiB shared-memory regions (front + back) sized for the
    /// 1280 × 800 BGRA8888 surface, and return a ready-to-submit
    /// `DisplayClient`.
    /// Returns a typed error if the lookup, encode, `ipc_call_buf`,
    /// or SHM allocation fails.
    pub fn connect() -> Result<Self, TermError> {
        let server_handle = match Self::lookup_with_backoff() {
            Some(h) => h,
            None => return Err(TermError::DisplayServerUnavailable),
        };
        let mut buf = [0u8; VERB_ENCODE_BUF_LEN];

        // Phase 72b Track K.7 — use the process PID as `client_token`
        // so the compositor can scope Goodbye teardown to this term's
        // surfaces only.
        let client_token: u32 = {
            let pid = syscall_lib::getpid();
            if pid > 0 { pid as u32 } else { 0x7e8e0001 }
        };
        // Phase 72b — per-instance surface id so two concurrent terms
        // (e.g. a SUPER+RETURN spawn while a first term is running)
        // do not collide on `SurfaceId(1)`. Without this the second
        // term's CreateSurface is rejected as duplicate and its later
        // commits target the *first* term's surface — the visible
        // "two terms stacked into one buffer" symptom.
        let sid = surface_id();

        // 1. Hello.
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
            client_token,
        };
        if !Self::send_verb(server_handle, &hello, &mut buf, "Hello") {
            return Err(TermError::DisplayServerUnavailable);
        }

        // 2. CreateSurface.
        let create = ClientMessage::CreateSurface {
            surface_id: sid,
            client_token,
        };
        if !Self::send_verb(server_handle, &create, &mut buf, "CreateSurface") {
            return Err(TermError::DisplayServerUnavailable);
        }

        // 3. SetSurfaceRole(Toplevel).
        let role = ClientMessage::SetSurfaceRole {
            surface_id: sid,
            role: SurfaceRole::Toplevel,
        };
        if !Self::send_verb(server_handle, &role, &mut buf, "SetSurfaceRole") {
            return Err(TermError::DisplayServerUnavailable);
        }

        // 4. Allocate the two shared-memory regions, sized to the smaller of
        //    the default cell-grid area and the actual framebuffer. The
        //    default grid is 80×25 cells at CELL_WIDTH×CELL_HEIGHT
        //    (= 1920×1200 px). On a panel shorter than 1200 px (e.g. a 1080p
        //    laptop) an un-clamped 1200-tall buffer would exceed the panel,
        //    forcing the compositor to nearest-neighbour downscale every
        //    frame until the first SurfaceResized. Clamping to the real
        //    framebuffer here keeps the surface ≤ panel; term::main re-derives
        //    the initial cell grid from these clamped dims, and the compositor
        //    resizes the surface to its assigned tile shortly after map via
        //    ServerMessage::SurfaceResized. At 1920×1080 each region is
        //    8,294,400 B (2025 pages → order-11 8 MiB block).
        let (out_w, out_h) = query_output_size();
        let init_w = if out_w > 0 {
            SURFACE_WIDTH_PX.min(out_w)
        } else {
            SURFACE_WIDTH_PX
        };
        let init_h = if out_h > 0 {
            SURFACE_HEIGHT_PX.min(out_h)
        } else {
            SURFACE_HEIGHT_PX
        };
        let byte_len = (init_w as usize)
            .saturating_mul(init_h as usize)
            .saturating_mul(4);
        let front = SurfaceMapping::allocate(byte_len, BUFFER_IDS[0])
            .ok_or(TermError::DisplayServerUnavailable)?;
        let back = SurfaceMapping::allocate(byte_len, BUFFER_IDS[1])
            .ok_or(TermError::DisplayServerUnavailable)?;

        Ok(Self {
            server_handle,
            // Initial layout: index 0 holds `front`, index 1 holds
            // `back`, and `back_idx == 1` selects the back for writes.
            // The first submit will commit `surfaces[1]`, flip
            // `back_idx` to 0, and begin writing the next frame into
            // `surfaces[0]`.
            surfaces: [front, back],
            back_idx: 1,
            surface_id: sid,
            client_token,
            width: init_w,
            height: init_h,
        })
    }

    /// Phase 72b — accessor for the per-instance surface id so callers
    /// outside this struct (e.g. the input-loop split in `term::main`)
    /// can read the same value without redundantly calling `getpid`.
    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }

    /// Phase 112 Track B.2 — publish `text` as the compositor clipboard
    /// offer (`text/plain;charset=utf-8`). Returns `false` when the text
    /// exceeds [`CLIPBOARD_MAX_BYTES`] or the IPC send fails.
    ///
    /// This mirrors `desktop_client::set_clipboard`, but is implemented
    /// inline and rides the **same** `"display"` handle `term` already
    /// holds. `term` keeps exactly one display connection and one client
    /// library; pulling in `desktop_client` purely for two verbs would
    /// have added a second connection and a second surface-management
    /// model to a binary that deliberately has neither.
    ///
    /// Over-long input is **rejected, not truncated** — silently copying
    /// half a selection would be worse than copying nothing, because the
    /// user cannot see the cut.
    pub fn set_clipboard(&self, text: &str) -> bool {
        let bytes = text.as_bytes();
        if bytes.len() > CLIPBOARD_MAX_BYTES {
            return false;
        }
        let msg = ClientMessage::SetClipboard {
            mime_tag: MimeTag::TextPlainUtf8,
            len: bytes.len() as u32,
            client_token: self.client_token,
        };
        // The offer bytes follow the frame in the same IPC bulk; the
        // compositor reads them after decoding the header.
        let mut frame = [0u8; VERB_ENCODE_BUF_LEN];
        let n = match msg.encode(&mut frame) {
            Ok(n) => n,
            Err(_) => {
                Self::log_verb_failure("term: display verb encode failed: ", "SetClipboard");
                return false;
            }
        };
        let mut combined: Vec<u8> = Vec::with_capacity(n + bytes.len());
        combined.extend_from_slice(&frame[..n]);
        combined.extend_from_slice(bytes);
        let reply = syscall_lib::ipc_call_buf(self.server_handle, LABEL_VERB, 0, &combined);
        if reply == u64::MAX {
            Self::log_verb_failure("term: display verb ipc_call_buf failed: ", "SetClipboard");
            return false;
        }
        true
    }

    /// Phase 112 Track B.2 — fetch the current clipboard offer's bytes.
    ///
    /// Returns `Some(bytes)` on success — including `Some(vec![])` when
    /// the clipboard is legitimately **empty**, which is distinct from
    /// `None` (the request failed or the reply was malformed). Pasting an
    /// empty clipboard is a no-op, not an error, and the caller should not
    /// have to guess which happened.
    pub fn get_clipboard(&self) -> Option<Vec<u8>> {
        let msg = ClientMessage::RequestClipboard {
            mime_tag: MimeTag::TextPlainUtf8,
        };
        let mut frame = [0u8; VERB_ENCODE_BUF_LEN];
        let n = msg.encode(&mut frame).ok()?;
        let reply = syscall_lib::ipc_call_buf(self.server_handle, LABEL_VERB, 0, &frame[..n]);
        if reply == u64::MAX {
            return None;
        }
        let mut buf = [0u8; CLIPBOARD_MAX_BYTES + 16];
        let got = syscall_lib::ipc_take_pending_bulk(&mut buf);
        if got == u64::MAX {
            return None;
        }
        let got = (got as usize).min(buf.len());
        let (hdr, consumed) = ServerMessage::decode(&buf[..got]).ok()?;
        let len = match hdr {
            ServerMessage::ClipboardData { len, .. } => len as usize,
            _ => return None,
        };
        if len == 0 {
            return Some(Vec::new());
        }
        // Clamp to what actually arrived: a truncated bulk must not be
        // read past its end.
        let end = (consumed + len).min(got);
        if end <= consumed {
            return None;
        }
        Some(buf[consumed..end].to_vec())
    }

    /// Current surface pixel width. Reflects either the initial
    /// [`SURFACE_WIDTH_PX`] or the value passed to the most recent
    /// successful [`DisplayClient::resize`].
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Current surface pixel height. See [`width`].
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Phase 72b — reallocate the SHM front + back buffers at
    /// `new_width × new_height` so the surface buffer matches the
    /// compositor's assigned tile rect. Called from the term main
    /// loop's `PulledEvent::SurfaceResized` handler.
    ///
    /// The Phase 56 protocol's `AttachSharedBuffer` validates the
    /// `shm_id` byte length against `width * height * 4`; we cannot
    /// reuse the old SHM region under new dimensions because the
    /// server-side check would reject it. So `resize` allocates two
    /// fresh `SurfaceMapping`s, replaces the existing pair, and lets
    /// the old `SurfaceMapping::Drop` impls call `shm_destroy` on the
    /// way out.
    ///
    /// On any allocation failure the existing buffers are kept and
    /// the function returns `false`; the caller's compose loop will
    /// continue rendering into the old dimensions and the next tile
    /// change will retry.
    pub fn resize(&mut self, new_width: u32, new_height: u32) -> bool {
        if new_width == self.width && new_height == self.height {
            return true;
        }
        if new_width == 0 || new_height == 0 {
            return false;
        }
        let byte_len = (new_width as usize)
            .saturating_mul(new_height as usize)
            .saturating_mul(4);
        let new_front = match SurfaceMapping::allocate(byte_len, BUFFER_IDS[0]) {
            Some(m) => m,
            None => return false,
        };
        let new_back = match SurfaceMapping::allocate(byte_len, BUFFER_IDS[1]) {
            Some(m) => m,
            None => {
                // new_front drops here, releasing its SHM cleanly.
                return false;
            }
        };
        // Replace the pair atomically. The previous `surfaces[..]`
        // values are dropped on assignment; their `SurfaceMapping::Drop`
        // releases the old SHM regions.
        self.surfaces = [new_front, new_back];
        self.back_idx = 1;
        self.width = new_width;
        self.height = new_height;
        true
    }

    /// Mutable access to the back-buffer shared-memory mapping —
    /// used by the `FramebufferOwner` impl below. The mapping
    /// returned is always the buffer term currently writes into,
    /// which is *not* the buffer display_server has committed for
    /// snapshot. The two-mapping invariant is established at
    /// `connect` and maintained by `submit`'s `back_idx` flip.
    ///
    /// # Safety
    ///
    /// This API hands out an `&mut [u8]` aliased with another
    /// process's mapping (`display_server`'s read-only view), which
    /// strictly speaking violates Rust's aliasing model: the compiler
    /// is entitled to assume that the bytes referenced by an
    /// `&mut [u8]` are not observed by anyone else. With double-
    /// buffering the back buffer is *not* the buffer display_server
    /// snapshots from (the front buffer is the one referenced by
    /// `committed_buffer` server-side), so the concrete races the
    /// single-buffer path documented at this site are now impossible.
    /// The remaining LLVM-level hazard — reordering of writes by
    /// this process — is unaffected; a future hardening pass can
    /// replace this with `*mut u8` + `core::ptr` volatile writes.
    ///
    /// Callers must not retain the borrow across syscalls that may
    /// observe SHM bytes (e.g. `CommitSurface`); the `pixels_mut`
    /// users in this file all consume the slice within a single call.
    unsafe fn pixels_mut(&mut self) -> &mut [u8] {
        let mapping = &self.surfaces[self.back_idx];
        unsafe {
            core::slice::from_raw_parts_mut(mapping.surface_va as *mut u8, mapping.surface_len)
        }
    }

    /// Publish the current back buffer via `AttachSharedBuffer` +
    /// `DamageSurface` + `CommitSurface`. After a successful publish
    /// the back/front buffers swap roles: the next call to
    /// `pixels_mut` returns the buffer display_server no longer holds
    /// the commit reference to. Returns `true` on success.
    ///
    /// Re-attaching every frame is intentional. Display_server's
    /// existing `CommitSurface` handler moves `pending_buffer` into
    /// `committed_buffer` and drops the old committed buffer
    /// (`userspace/display_server/src/surface.rs:646-678`); that's
    /// the atomic publication point the snapshot-during-write race
    /// was missing.
    fn attach_damage_commit_back(&mut self) -> bool {
        let mapping = &self.surfaces[self.back_idx];
        let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
        let attach = ClientMessage::AttachSharedBuffer {
            surface_id: self.surface_id,
            buffer_id: mapping.buffer_id,
            shm_id: mapping.shm_id,
            width: self.width,
            height: self.height,
        };
        if !Self::send_verb(self.server_handle, &attach, &mut buf, "AttachSharedBuffer") {
            return false;
        }
        let damage = ClientMessage::DamageSurface {
            surface_id: self.surface_id,
            rect: Rect {
                x: 0,
                y: 0,
                w: self.width,
                h: self.height,
            },
        };
        if !Self::send_verb(self.server_handle, &damage, &mut buf, "DamageSurface") {
            return false;
        }
        let commit = ClientMessage::CommitSurface {
            surface_id: self.surface_id,
        };
        if !Self::send_verb(self.server_handle, &commit, &mut buf, "CommitSurface") {
            return false;
        }
        true
    }

    /// Copy the just-published front buffer's pixels into the new
    /// back buffer so incremental render commands (scroll, partial
    /// `put_glyph`) have an up-to-date starting state. Without this
    /// the new back would still hold pixels from *two* frames ago —
    /// e.g. a `RenderCommand::Scroll { amount: 1 }` would scroll
    /// stale content instead of the current screen.
    ///
    /// Both reads (display_server snapshot of front; term memcpy
    /// from front) are reads, so they cannot race. The `front_idx`
    /// expression below is `1 - back_idx` (the buffer that is no
    /// longer the back after `submit`'s flip).
    fn refresh_back_from_front(&mut self) {
        let front_idx = 1 - self.back_idx;
        let len = self.surfaces[self.back_idx].surface_len;
        let src = self.surfaces[front_idx].surface_va as *const u8;
        let dst = self.surfaces[self.back_idx].surface_va as *mut u8;
        // SAFETY: both surfaces are mapped by this process for at
        // least `len` bytes. `src` and `dst` cover disjoint
        // mappings (different `shm_id`s, different virtual ranges),
        // so the copy is non-overlapping. No live `&` / `&mut`
        // borrows alias either region at this point — `submit`
        // is the only caller and all `pixels_mut` borrows from the
        // prior compose pass have been dropped.
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, len);
        }
    }
}

impl DisplayClient {
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
                Self::log_verb_failure("term: display verb encode failed: ", step);
                return false;
            }
        };
        let reply = syscall_lib::ipc_call_buf(handle, LABEL_VERB, 0, &buf[..len]);
        if reply == u64::MAX {
            Self::log_verb_failure("term: display verb ipc_call_buf failed: ", step);
            return false;
        }
        true
    }

    fn log_verb_failure(prefix: &str, step: &str) {
        if DISPLAY_VERB_FAILURE_LOG_BUDGET
            .fetch_update(
                core::sync::atomic::Ordering::Relaxed,
                core::sync::atomic::Ordering::Relaxed,
                |remaining| remaining.checked_sub(1),
            )
            .is_err()
        {
            return;
        }
        syscall_lib::write_str(STDOUT_FILENO, prefix);
        syscall_lib::write_str(STDOUT_FILENO, step);
        syscall_lib::write_str(STDOUT_FILENO, "\n");
    }
}

impl FramebufferOwner for DisplayClient {
    fn put_glyph(
        &mut self,
        row: u16,
        col: u16,
        _codepoint: u32,
        glyph: &GlyphView<'_>,
        fg: u32,
        bg: u32,
    ) {
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
        let stride_pixels = self.width as usize;
        let cell_offset =
            match cell_pixel_offset(row, col, CELL_WIDTH, CELL_HEIGHT, self.width, self.height) {
                Some(o) => o,
                None => return,
            };

        // SAFETY: see `pixels_mut` — the borrow stays inside this
        // call and the compositor snapshots before reading.
        let pixels = unsafe { self.pixels_mut() };
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

        // Phase 69c Track E.2 — the renderer pre-resolved the
        // codepoint to a `GlyphView`, so we paint whatever bitmap
        // was handed in. Always bg-fill the full cell first so a
        // glyph smaller than the cell (e.g. the static IBM VGA 8×16
        // fallback in a 16×32 cell when TTF load fails) doesn't
        // leave stale pixels in the uncovered area. Only blank
        // glyphs (control codepoints `U+0000..=U+001F`, `U+007F`,
        // the C1 range, NBSP, and the static-table blanks) need no
        // further work; uncovered codepoints come back as the
        // visible centred-dot fallback (non-blank).
        fill_cell_bg(
            cell_view,
            stride_pixels,
            CELL_WIDTH as usize,
            CELL_HEIGHT as usize,
            bg,
        );
        if !glyph.bitmap.iter().all(|&b| b == 0) {
            blit_glyph_view(glyph, cell_view, stride_pixels, fg, bg);
        }
    }

    fn clear(&mut self) {
        // Fill the back-buffer mapping byte-wise with the BG colour.
        // The pixels are 4 bytes wide; we splat the same little-
        // endian BGRA value across the whole buffer.
        let len = self.surfaces[self.back_idx].surface_len;
        // SAFETY: see `pixels_mut` — borrow scoped to this call.
        let pixels = unsafe { self.pixels_mut() };
        let bg_bytes = DEFAULT_BG_BGRA.to_le_bytes();
        let mut offset = 0;
        while offset + 4 <= len {
            pixels[offset..offset + 4].copy_from_slice(&bg_bytes);
            offset += 4;
        }
    }

    fn scroll(&mut self, amount: i16) {
        if amount == 0 {
            return;
        }
        let stride = (self.width as usize) * 4;
        let row_bytes = stride * (CELL_HEIGHT as usize);
        let buf_len = self.surfaces[self.back_idx].surface_len;
        // SAFETY: see `pixels_mut` — borrow scoped to this call.
        let pixels = unsafe { self.pixels_mut() };
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

    fn submit(&mut self) -> bool {
        // Double-buffered publish sequence (2026-05-17 less-render-
        // disappearance fix; 2026-05-18 follow-up). The renderer
        // drained its queue into `surfaces[self.back_idx]`; we now
        // hand that buffer to display_server via `AttachSharedBuffer`
        // + `DamageSurface` + `CommitSurface`, then flip `back_idx`
        // so the next compose pass writes into the buffer display_
        // server no longer holds a commit reference to. The flip
        // *must* happen before `refresh_back_from_front` so the
        // memcpy's source is the just-published front (now at
        // `1 - back_idx` after the flip) and the destination is the
        // new back (`back_idx` after the flip).
        //
        // The refresh is unconditional. The 2026-05-17 fix tried to
        // skip the first-frame memcpy as an optimisation (the new
        // back is SHM-create-zeroed, so copying zeros into zeros is a
        // no-op), but that left the new back at a *stale* zero state
        // on the second publish: the second compose would then drain
        // its incremental Puts into a zero buffer instead of into a
        // copy of the previous front, dropping all content that
        // hadn't been re-painted this frame. The trace probe from the
        // first follow-up session caught this directly — 33 of 76
        // snapshots showed `nz=0/4096000` even though no Clear-only
        // queue had drained. Always copying is ~4 MiB of memcpy per
        // frame, well within budget.
        if !self.attach_damage_commit_back() {
            return false;
        }
        self.back_idx = 1 - self.back_idx;
        self.refresh_back_from_front();
        true
    }
}

/// Phase 72b — send a `Goodbye` to display_server on drop so the
/// compositor scopes K.7 surface teardown to *this* term's surfaces
/// only. Covers every exit path: normal return after shell exits,
/// `CloseRequest` break, panic. The `client_token` is rebuilt from
/// PID — the value is stable across the process lifetime so it
/// matches what `Hello` sent at connect time.
impl Drop for DisplayClient {
    fn drop(&mut self) {
        let pid = syscall_lib::getpid();
        let client_token: u32 = if pid > 0 { pid as u32 } else { 0x7e8e0001 };
        let mut buf = [0u8; VERB_ENCODE_BUF_LEN];
        let goodbye = ClientMessage::Goodbye { client_token };
        // Best-effort — the process is exiting; if the IPC call fails
        // we can't do anything about it. `send_verb` already swallows
        // encode failures.
        let _ = Self::send_verb(self.server_handle, &goodbye, &mut buf, "Goodbye(drop)");
    }
}

/// Phase 69c Track E.2 — blit a [`GlyphView`] into the cell.
/// Mirrors the layout `Glyph::render_into` enforced before the
/// renderer started resolving glyphs itself: packed bits row-major,
/// MSB-first per byte. Atlas-rasterized bitmaps share the same
/// layout so both paths use this helper.
fn blit_glyph_view(
    glyph: &GlyphView<'_>,
    cell_view: &mut [u32],
    stride_pixels: usize,
    fg: u32,
    _bg: u32,
) {
    let w = glyph.width as usize;
    let h = glyph.height as usize;
    if w == 0 || h == 0 {
        return;
    }
    // Scale a smaller-than-cell glyph up to fill the cell. The static IBM VGA
    // fallback is 8×16; in the Phase 73 24×48 cell it would otherwise be
    // stranded in the top-left corner, leaving a wide background gap after
    // every character (the "spaces between characters" seen on diskless boots
    // where the TTF atlas asset is absent). Integer nearest-neighbour: 8×16
    // scales 3× to exactly fill 24×48. An atlas glyph already rasterised at
    // cell size scales 1× (unchanged). The cell background was already painted
    // by `fill_cell_bg`, so only set ("on") bits are written here.
    let cw = CELL_WIDTH as usize;
    let ch = CELL_HEIGHT as usize;
    let scale = core::cmp::max(1, core::cmp::min(cw / w, ch / h));
    let off_x = cw.saturating_sub(w * scale) / 2;
    let off_y = ch.saturating_sub(h * scale) / 2;
    let bytes_per_row = w.div_ceil(8);
    for row in 0..h {
        let row_start = row * bytes_per_row;
        for col in 0..w {
            let byte_idx = row_start + col / 8;
            if byte_idx >= glyph.bitmap.len() {
                break;
            }
            let bit_idx = 7 - (col % 8);
            if (glyph.bitmap[byte_idx] >> bit_idx) & 1 != 1 {
                continue; // background already filled
            }
            // Paint a `scale × scale` block for this glyph pixel.
            for dy in 0..scale {
                let py = off_y + row * scale + dy;
                let row_off = py * stride_pixels + off_x + col * scale;
                for dx in 0..scale {
                    let dst = row_off + dx;
                    if dst >= cell_view.len() {
                        break;
                    }
                    cell_view[dst] = fg;
                }
            }
        }
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
