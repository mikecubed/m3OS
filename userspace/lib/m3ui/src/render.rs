//! The `render` layer (Phase 105 Track A.5/A.6): a concrete [`Painter`]
//! over `desktop_client`'s shared surface, plus helpers that fold the
//! compositor's `KeyEvent`/`PointerEvent` stream into the toolkit's
//! [`InputState`]. Feature-gated (`render`) so the pure-logic core stays
//! host-buildable without the framebuffer/font/IPC stack.
//!
//! Text is drawn **transparently** (only foreground pixels are written)
//! using the bundled 8×16 `BasicBitmapFont`, so a label reads correctly
//! over any widget background — unlike `desktop_client::draw_text`, which
//! paints an opaque cell box behind every glyph.

use alloc::vec::Vec;

use kernel_core::input::events::{
    KeyEvent, KeyEventKind, MOD_ALT, MOD_CTRL, MOD_SHIFT, ModifierState, PointerButton,
    PointerEvent,
};
use kernel_core::input::keymap;
use kernel_core::session::font::BasicBitmapFont;

use crate::geom::{Color, Point, Rect};
use crate::input::{InputState, KeyCode, KeyPress, Mods, MouseButton};
use crate::paint::Painter;

/// Cell metrics of the bundled bitmap font.
const CELL_W: i32 = 8;
const CELL_H: i32 = 16;

/// Convert the toolkit's ARGB [`Color`] into the `desktop_client`
/// BGRA8888 `u32` the surface expects. In-tree clients build colors as
/// `0xAARRGGBB` and the compositor ignores alpha, so the packed value is
/// used directly (R in bits 16..24, matching every existing client).
fn to_fb(color: Color) -> u32 {
    color.0
}

/// A [`Painter`] that draws into a `desktop_client` `SharedSurface`
/// pixel buffer, with an integer text scale and a scissor stack.
pub struct SurfacePainter<'a> {
    pixels: &'a mut [u32],
    stride: i32,
    height: i32,
    scale: i32,
    clip: Vec<Rect>,
    font: BasicBitmapFont,
}

impl<'a> SurfacePainter<'a> {
    /// Wrap `pixels` (a `width*height` BGRA buffer). `scale` pixel-doubles
    /// text (1 for 8×16, 2 for HiDPI).
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32, scale: u32) -> SurfacePainter<'a> {
        let w = width as i32;
        let h = height as i32;
        SurfacePainter {
            pixels,
            stride: w,
            height: h,
            scale: (scale as i32).max(1),
            clip: alloc::vec![Rect::new(0, 0, w, h)],
            font: BasicBitmapFont::new(),
        }
    }

    fn clip_top(&self) -> Rect {
        *self.clip.last().unwrap()
    }

    /// Set one pixel if it lies within the current clip + surface.
    #[inline]
    fn put(&mut self, x: i32, y: i32, fb: u32) {
        let c = self.clip_top();
        if x < c.x || x >= c.right() || y < c.y || y >= c.bottom() {
            return;
        }
        if x < 0 || x >= self.stride || y < 0 || y >= self.height {
            return;
        }
        let idx = (y * self.stride + x) as usize;
        if idx < self.pixels.len() {
            self.pixels[idx] = fb;
        }
    }

    /// Fill a rect clipped to the current scissor, via direct pixel writes.
    fn fill_clipped(&mut self, rect: Rect, fb: u32) {
        let r = rect.intersect(&self.clip_top());
        if r.is_empty() {
            return;
        }
        let x0 = r.x.max(0);
        let y0 = r.y.max(0);
        let x1 = r.right().min(self.stride);
        let y1 = r.bottom().min(self.height);
        for y in y0..y1 {
            let base = (y * self.stride) as usize;
            for x in x0..x1 {
                let idx = base + x as usize;
                if idx < self.pixels.len() {
                    self.pixels[idx] = fb;
                }
            }
        }
    }

    /// Blit one glyph's foreground pixels (transparent background) at
    /// (`gx`,`gy`), scaled by `self.scale`.
    fn blit_glyph(&mut self, gx: i32, gy: i32, codepoint: u32, fb: u32) {
        let glyph = self.font.glyph_or_fallback(codepoint);
        let w = glyph.width as usize;
        let h = glyph.height as usize;
        let bytes_per_row = w.div_ceil(8);
        let scale = self.scale;
        for row in 0..h {
            let row_start = row * bytes_per_row;
            for col in 0..w {
                let byte_idx = row_start + col / 8;
                let bit_idx = 7 - (col % 8);
                if (glyph.bitmap[byte_idx] >> bit_idx) & 1 != 1 {
                    continue;
                }
                // Scaled block for this source pixel.
                for sy in 0..scale {
                    for sx in 0..scale {
                        self.put(
                            gx + col as i32 * scale + sx,
                            gy + row as i32 * scale + sy,
                            fb,
                        );
                    }
                }
            }
        }
    }
}

impl<'a> Painter for SurfacePainter<'a> {
    fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.fill_clipped(rect, to_fb(color));
    }

    fn stroke_rect(&mut self, rect: Rect, color: Color, thickness: i32) {
        let t = thickness.max(1);
        let fb = to_fb(color);
        // Four edge bands, each clipped.
        self.fill_clipped(Rect::new(rect.x, rect.y, rect.w, t), fb); // top
        self.fill_clipped(Rect::new(rect.x, rect.bottom() - t, rect.w, t), fb); // bottom
        self.fill_clipped(Rect::new(rect.x, rect.y, t, rect.h), fb); // left
        self.fill_clipped(Rect::new(rect.right() - t, rect.y, t, rect.h), fb); // right
    }

    fn text(&mut self, x: i32, y: i32, text: &str, color: Color) {
        let fb = to_fb(color);
        let mut cx = x;
        let advance = CELL_W * self.scale;
        for ch in text.chars() {
            self.blit_glyph(cx, y, ch as u32, fb);
            cx += advance;
        }
    }

    fn text_width(&self, text: &str) -> i32 {
        text.chars().count() as i32 * CELL_W * self.scale
    }

    fn text_height(&self) -> i32 {
        CELL_H * self.scale
    }

    fn clip_push(&mut self, rect: Rect) {
        let clipped = self.clip_top().intersect(&rect);
        self.clip.push(clipped);
    }

    fn clip_pop(&mut self) {
        if self.clip.len() > 1 {
            self.clip.pop();
        }
    }
}

/// Decode compositor modifier bits into the toolkit's [`Mods`].
pub fn decode_mods(m: ModifierState) -> Mods {
    Mods {
        ctrl: m.contains(MOD_CTRL),
        shift: m.contains(MOD_SHIFT),
        alt: m.contains(MOD_ALT),
    }
}

/// Decode a compositor [`KeyEvent`] into a toolkit [`KeyPress`], or
/// `None` for key-up / key-repeat and modifier-only presses. Maps the
/// hardware-neutral keycode to a [`KeyCode`]; printable keys carry the
/// keymap-resolved Unicode `symbol` as `ch`.
pub fn decode_key(ev: &KeyEvent) -> Option<KeyPress> {
    // Only act on presses + auto-repeat (so held arrows/backspace repeat).
    if ev.kind == KeyEventKind::Up {
        return None;
    }
    let mods = decode_mods(ev.modifiers);
    let kc = keymap::Keycode(ev.keycode);
    let code = if kc == keymap::KEY_ENTER {
        KeyCode::Enter
    } else if kc == keymap::KEY_TAB {
        KeyCode::Tab
    } else if kc == keymap::KEY_BACKSPACE {
        KeyCode::Backspace
    } else if kc == keymap::KEY_DELETE {
        KeyCode::Delete
    } else if kc == keymap::KEY_LEFT {
        KeyCode::Left
    } else if kc == keymap::KEY_RIGHT {
        KeyCode::Right
    } else if kc == keymap::KEY_HOME {
        KeyCode::Home
    } else if kc == keymap::KEY_END {
        KeyCode::End
    } else if kc == keymap::KEY_ESC {
        KeyCode::Escape
    } else if kc == keymap::KEY_SPACE {
        KeyCode::Space
    } else {
        KeyCode::Char
    };
    // Character: a printable Unicode scalar from the keymap symbol.
    let ch = char::from_u32(ev.symbol).filter(|c| !c.is_control() || code == KeyCode::Space);
    // A pure modifier key (no code, no printable char) is not a press.
    match code {
        KeyCode::Char if ch.is_none() => None,
        _ => Some(KeyPress {
            code,
            ch: if code == KeyCode::Char || code == KeyCode::Space {
                ch
            } else {
                None
            },
            mods,
        }),
    }
}

/// Fold a compositor [`PointerEvent`] into `input`: absolute position,
/// button edges, and wheel scroll.
pub fn apply_pointer(input: &mut InputState, ev: &PointerEvent) {
    if let Some((x, y)) = ev.abs_position {
        input.set_pointer(Point::new(x, y));
    }
    input.set_mods(decode_mods(ev.modifiers));
    match ev.button {
        PointerButton::Down(b) => input.press_button(map_button(b)),
        PointerButton::Up(b) => input.release_button(map_button(b)),
        PointerButton::None => {}
    }
    if ev.wheel_dy != 0 {
        input.scroll(ev.wheel_dy);
    }
}

fn map_button(index: u8) -> MouseButton {
    match index {
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}
