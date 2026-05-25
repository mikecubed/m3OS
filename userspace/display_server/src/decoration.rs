//! Phase 73 Track B — Window decorations (pre-computed buffers only).
//!
//! Two CPU-side decoration buffers intended for the compose loop's
//! post-blit pass. The buffers themselves land in this phase; the
//! per-frame application pass (mask blend + shadow blit against the
//! framebuffer) is deferred and documented in the PR description.
//!
//! * [`RoundedCornerMask`] — pre-computed alpha ramp for the four
//!   corners of a Toplevel surface, suitable for rounding them off
//!   without GPU shaders once the apply pass lands.
//! * [`DropShadow`] — pre-computed alpha falloff buffer that will be
//!   drawn behind a Toplevel for a sense of depth.
//!
//! Both structures are configured from `[decorations]` in
//! `/etc/compositor.conf`. A zero radius / zero blur disables the pass
//! entirely so an operator who wants raw tile pixels pays no cost.

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::display::protocol::Rect;

/// `[decorations]` section parsed out of `/etc/compositor.conf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecorationConfig {
    /// Pixel radius of the rounded-corner mask. `0` disables the pass.
    pub corner_radius: u32,
    /// Shadow blur radius (pixels). `0` disables the shadow pass.
    pub shadow_blur: u32,
    /// Horizontal shadow offset (px). Positive moves the shadow right.
    pub shadow_offset_x: i32,
    /// Vertical shadow offset. Positive moves the shadow down.
    pub shadow_offset_y: i32,
    /// Shadow tint in BGRA8888.
    pub shadow_color: u32,
}

impl DecorationConfig {
    /// Default omarchy-ish look: slight rounding, soft drop shadow.
    pub fn defaults() -> Self {
        Self {
            corner_radius: 8,
            shadow_blur: 12,
            shadow_offset_x: 0,
            shadow_offset_y: 4,
            // Translucent black, straight-alpha BGRA. The apply pass
            // multiplies by the per-pixel alpha at blend time.
            shadow_color: 0x80_00_00_00,
        }
    }
}

impl Default for DecorationConfig {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Pre-computed alpha ramp for a single quadrant of a rounded corner.
///
/// We sample a quarter-circle into a `radius × radius` byte grid where
/// each cell stores the alpha coverage of the surface pixel at that
/// offset from the corner. `0` ⇒ fully outside the circle (corner is
/// transparent); `255` ⇒ fully inside (paint as-is). The compose loop
/// uses this to know which corner pixels to clear back to background.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundedCornerMask {
    radius: u32,
    alpha: Vec<u8>,
}

impl RoundedCornerMask {
    /// Build a mask of the given radius. `radius == 0` returns an empty
    /// mask that disables the pass.
    pub fn new(radius: u32) -> Self {
        if radius == 0 {
            return Self {
                radius: 0,
                alpha: Vec::new(),
            };
        }
        let r = radius as i64;
        let mut alpha = Vec::with_capacity((radius as usize) * (radius as usize));
        // Sample at the centre of each cell. dx, dy are distance from
        // the corner's circle centre; alpha falls off with distance.
        for y in 0..radius {
            for x in 0..radius {
                let dx = (x as i64) - (r - 1);
                let dy = (y as i64) - (r - 1);
                let dist_sq = dx * dx + dy * dy;
                let r_sq = r * r;
                let cov = if dist_sq <= (r - 1) * (r - 1) {
                    255
                } else if dist_sq >= r_sq {
                    0
                } else {
                    // Linear ramp in [r-1, r] gives a soft 1-pixel edge.
                    let inside = r * r - dist_sq;
                    let band = r * r - (r - 1) * (r - 1);
                    ((inside * 255) / band).clamp(0, 255) as u8
                };
                alpha.push(cov);
            }
        }
        Self { radius, alpha }
    }

    /// Radius the mask was constructed with.
    pub fn radius(&self) -> u32 {
        self.radius
    }

    /// `true` when the mask is disabled (radius zero) — callers can
    /// skip the corner pass entirely.
    pub fn is_disabled(&self) -> bool {
        self.radius == 0
    }

    /// Alpha coverage at `(x, y)` within the corner square. The four
    /// corners share the same ramp by mirroring through the centre.
    pub fn sample(&self, x: u32, y: u32) -> u8 {
        if self.radius == 0 || x >= self.radius || y >= self.radius {
            return 255;
        }
        let idx = (y as usize) * (self.radius as usize) + (x as usize);
        self.alpha[idx]
    }

    /// Apply the mask to `surface_rect` by blending the four corners
    /// of `pixels` (BGRA8888) towards `background`. Pixels are read
    /// row-by-row, `pixels.len() == surface_rect.w * surface_rect.h *
    /// 4`. Returns the number of bytes modified (useful for tests).
    pub fn apply(&self, pixels: &mut [u8], surface_rect: Rect, background: u32) -> usize {
        if self.is_disabled() {
            return 0;
        }
        let w = surface_rect.w as usize;
        let h = surface_rect.h as usize;
        let r = self.radius as usize;
        if w < 2 || h < 2 || r == 0 {
            return 0;
        }
        let r_eff = r.min(w / 2).min(h / 2);
        let stride = w * 4;
        let bg = background.to_le_bytes();
        let mut writes = 0usize;
        for cy in 0..r_eff {
            for cx in 0..r_eff {
                let cov = self.sample(cx as u32, cy as u32);
                if cov == 255 {
                    continue;
                }
                // Four corners: TL, TR, BL, BR. Mirror cx/cy.
                let positions = [
                    (cx, cy),
                    (w - 1 - cx, cy),
                    (cx, h - 1 - cy),
                    (w - 1 - cx, h - 1 - cy),
                ];
                for (px, py) in positions.iter() {
                    let off = py * stride + px * 4;
                    if off + 4 > pixels.len() {
                        continue;
                    }
                    if cov == 0 {
                        pixels[off..off + 4].copy_from_slice(&bg);
                    } else {
                        // Linear blend between background and the
                        // surface pixel.
                        for c in 0..4 {
                            let s = pixels[off + c] as u32;
                            let b = bg[c] as u32;
                            let alpha = cov as u32;
                            pixels[off + c] = ((s * alpha + b * (255 - alpha)) / 255) as u8;
                        }
                    }
                    writes += 4;
                }
            }
        }
        writes
    }
}

/// Pre-computed drop-shadow alpha buffer.
///
/// The shadow has the same `(w, h)` as its owning window plus a `2 *
/// blur_radius` border on each side. Each cell stores the shadow's
/// alpha contribution at that offset. The compose loop blits the
/// shadow before the window itself, offset by `shadow_offset_x/y`.
#[derive(Clone, Debug, PartialEq)]
pub struct DropShadow {
    pub width: u32,
    pub height: u32,
    pub blur_radius: u32,
    pub color: u32,
    /// Straight-alpha BGRA8888 pixels (width × height). The RGB
    /// channels stay at the configured `color`'s RGB and only the
    /// alpha channel varies with the falloff. The (future) apply
    /// pass is responsible for multiplying by alpha before blending
    /// into the framebuffer.
    pub pixels: Vec<u32>,
}

impl DropShadow {
    /// Pre-compute the shadow for a window of `(w, h)` pixels.
    ///
    /// `blur_radius == 0` returns a zero-sized shadow that callers can
    /// skip painting.
    pub fn compute(width: u32, height: u32, blur_radius: u32, color: u32) -> Self {
        if width == 0 || height == 0 || blur_radius == 0 {
            return Self {
                width: 0,
                height: 0,
                blur_radius,
                color,
                pixels: Vec::new(),
            };
        }
        let pad = blur_radius as usize;
        let buf_w = width as usize + 2 * pad;
        let buf_h = height as usize + 2 * pad;
        let mut pixels = alloc::vec![0u32; buf_w * buf_h];
        let inner_l = pad;
        let inner_r = pad + width as usize - 1;
        let inner_t = pad;
        let inner_b = pad + height as usize - 1;
        let max_dist = blur_radius as i64;
        let color_a = ((color >> 24) & 0xFF) as u32;
        let color_rgb = color & 0x00FF_FFFF;
        for y in 0..buf_h {
            for x in 0..buf_w {
                let dx = if x < inner_l {
                    (inner_l - x) as i64
                } else if x > inner_r {
                    (x - inner_r) as i64
                } else {
                    0
                };
                let dy = if y < inner_t {
                    (inner_t - y) as i64
                } else if y > inner_b {
                    (y - inner_b) as i64
                } else {
                    0
                };
                let dist_sq = dx * dx + dy * dy;
                let max_sq = max_dist * max_dist;
                let alpha = if dist_sq == 0 {
                    color_a
                } else if dist_sq >= max_sq {
                    0
                } else {
                    // Squared-distance falloff. Cheap (no sqrt) and
                    // gives a soft Gaussian-ish profile at the radii
                    // the spec calls for.
                    ((color_a as i64) * (max_sq - dist_sq) / max_sq) as u32
                };
                let idx = y * buf_w + x;
                pixels[idx] = (alpha << 24) | color_rgb;
            }
        }
        Self {
            width: buf_w as u32,
            height: buf_h as u32,
            blur_radius,
            color,
            pixels,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty() || self.width == 0 || self.height == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_radius_mask_is_disabled() {
        let mask = RoundedCornerMask::new(0);
        assert!(mask.is_disabled());
        assert_eq!(mask.sample(0, 0), 255);
    }

    #[test]
    fn radius_8_mask_has_clear_corner_pixel() {
        let mask = RoundedCornerMask::new(8);
        // The pixel furthest from the circle's centre should be fully
        // transparent.
        assert_eq!(mask.sample(0, 0), 0);
        // The pixel closest to the circle's centre should be fully
        // opaque.
        assert_eq!(mask.sample(7, 7), 255);
    }

    #[test]
    fn apply_zero_radius_is_noop() {
        let mask = RoundedCornerMask::new(0);
        let mut pixels = vec![0xFFu8; 4 * 16 * 16];
        let writes = mask.apply(
            &mut pixels,
            Rect {
                x: 0,
                y: 0,
                w: 16,
                h: 16,
            },
            0x12345678,
        );
        assert_eq!(writes, 0);
        // All pixels untouched.
        assert!(pixels.iter().all(|b| *b == 0xFF));
    }

    #[test]
    fn apply_radius_8_clears_corner_pixels() {
        let mask = RoundedCornerMask::new(8);
        let mut pixels = vec![0xFFu8; 4 * 32 * 32];
        let _ = mask.apply(
            &mut pixels,
            Rect {
                x: 0,
                y: 0,
                w: 32,
                h: 32,
            },
            0x00_00_00_00,
        );
        // Top-left corner pixel must have been cleared.
        let tl = &pixels[0..4];
        assert!(tl.iter().all(|b| *b == 0));
        // A pixel far from any corner stays intact.
        let mid_off = (16 * 32 + 16) * 4;
        let mid = &pixels[mid_off..mid_off + 4];
        assert!(mid.iter().all(|b| *b == 0xFF));
    }

    #[test]
    fn drop_shadow_zero_blur_is_empty() {
        let s = DropShadow::compute(100, 100, 0, 0xFF00_0000);
        assert!(s.is_empty());
    }

    #[test]
    fn drop_shadow_grows_by_blur_padding() {
        let s = DropShadow::compute(40, 30, 8, 0x80_00_00_00);
        assert_eq!(s.width, 40 + 16);
        assert_eq!(s.height, 30 + 16);
        assert!(!s.is_empty());
        // Pixel near the inner rect must be more opaque than one at
        // the buffer edge.
        let inner = s.pixels[(8 + 1) * (s.width as usize) + (8 + 1)];
        let outer = s.pixels[0];
        assert!(inner >> 24 > outer >> 24, "shadow falls off");
    }
}
