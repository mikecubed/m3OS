//! Phase 72 Track E — Border rendering.
//!
//! Each tiled / floating Toplevel is wrapped by a 1–4 px coloured
//! border so the focused window is visually distinct from unfocused
//! ones. Borders are painted *after* the surface-blit pass so they
//! always sit on top of the surface pixels.
//!
//! The pixel-format-aware fill loop reuses [`bytes_per_pixel`] so the
//! same code paints into BGRA8888 and RGBA8888 framebuffers correctly.

use kernel_core::display::fb_owner::{FbError, FramebufferOwner, bytes_per_pixel};
use kernel_core::display::protocol::Rect;

/// Border styling: per-edge pixel width plus active / inactive colors.
/// Sourced from `/etc/compositor.conf [borders]` at startup and on
/// `m3ctl reload`.
#[derive(Clone, Copy, Debug)]
pub struct BorderConfig {
    /// Edge thickness in pixels. `0` disables border painting entirely.
    pub width: u8,
    /// Encoded colour for the focused window's border (matches the
    /// framebuffer's native pixel layout — BGRA8888 / RGBA8888).
    pub active_color: u32,
    /// Encoded colour for unfocused windows' borders.
    pub inactive_color: u32,
}

impl BorderConfig {
    /// Sensible Phase 72 defaults: 2 px border, active = vivid blue,
    /// inactive = neutral grey. Override via TOML config + hot reload.
    pub const fn defaults() -> Self {
        Self {
            width: 2,
            active_color: 0x00_5F_AF_FFu32,
            inactive_color: 0x00_44_44_44u32,
        }
    }

    /// Disabled-border config — width zero, colours zero. Used when
    /// the config file explicitly sets `width = 0`.
    pub const fn disabled() -> Self {
        Self {
            width: 0,
            active_color: 0,
            inactive_color: 0,
        }
    }
}

impl Default for BorderConfig {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Paint a border rectangle around `rect` with thickness `width` and
/// colour `color`. The border is painted *inside* `rect` (so the
/// useful interior shrinks by `width`); this matches the
/// omarchy/Hyprland convention where the tile's reported geometry
/// includes its border chrome.
///
/// Returns the number of `write_pixels` calls issued so the composer
/// can fold the count into its frame-stats sample.
pub fn paint_border<O: FramebufferOwner>(
    owner: &mut O,
    rect: Rect,
    width: u8,
    color: u32,
) -> Result<usize, FbError> {
    if width == 0 || rect.w == 0 || rect.h == 0 {
        return Ok(0);
    }
    let bpp = bytes_per_pixel(owner.metadata().pixel_format) as usize;
    let mut writes = 0usize;
    let bytes = color.to_le_bytes();
    let mut pixel = [0u8; 8];
    let copy_len = bpp.min(bytes.len());
    pixel[..copy_len].copy_from_slice(&bytes[..copy_len]);

    let w = u32::from(width)
        .min(rect.w.div_ceil(2))
        .min(rect.h.div_ceil(2));
    if w == 0 {
        return Ok(0);
    }

    // Top + bottom strips: full width × `w` height.
    let top = Rect {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: w,
    };
    let bottom = Rect {
        x: rect.x,
        y: rect.y.saturating_add(rect.h as i32 - w as i32),
        w: rect.w,
        h: w,
    };
    writes += fill_solid(owner, top, &pixel[..bpp])?;
    writes += fill_solid(owner, bottom, &pixel[..bpp])?;

    // Left + right strips: skip the corners (already painted by top /
    // bottom) so we don't double-write.
    let interior_h = rect.h.saturating_sub(2 * w);
    if interior_h > 0 {
        let left = Rect {
            x: rect.x,
            y: rect.y.saturating_add(w as i32),
            w,
            h: interior_h,
        };
        let right = Rect {
            x: rect.x.saturating_add(rect.w as i32 - w as i32),
            y: rect.y.saturating_add(w as i32),
            w,
            h: interior_h,
        };
        writes += fill_solid(owner, left, &pixel[..bpp])?;
        writes += fill_solid(owner, right, &pixel[..bpp])?;
    }
    Ok(writes)
}

fn fill_solid<O: FramebufferOwner>(
    owner: &mut O,
    rect: Rect,
    pixel: &[u8],
) -> Result<usize, FbError> {
    if rect.w == 0 || rect.h == 0 {
        return Ok(0);
    }
    let bpp = pixel.len();
    let pixel_count = (rect.w as usize).saturating_mul(rect.h as usize);
    let total = pixel_count.saturating_mul(bpp);
    let mut buf: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(total);
    for _ in 0..pixel_count {
        buf.extend_from_slice(pixel);
    }
    let stride = (rect.w as u32).saturating_mul(bpp as u32);
    owner.write_pixels(rect, &buf, stride)?;
    Ok(1)
}
