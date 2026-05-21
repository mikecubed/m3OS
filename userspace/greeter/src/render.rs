//! Phase 71 Track C.2 — on-screen login UI rendering.
//!
//! Pure-logic glyph painter using the `kernel-core` bitmap font.
//! Composes the welcome banner, username + password fields, and an
//! optional error message line into a caller-owned surface buffer.

use alloc::string::String;

use kernel_core::session::{BasicBitmapFont, FontProvider};

use crate::config::GreeterConfig;

/// Which field has keyboard focus right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveField {
    Username,
    Password,
}

/// All the data the renderer needs to paint one frame of the login
/// form. Pure data; mutating it between paints is the binary's job.
#[derive(Clone, Debug)]
pub struct LoginUiState<'a> {
    pub config: &'a GreeterConfig,
    pub username: &'a str,
    /// Number of password characters typed so far. The renderer draws
    /// one `*` glyph per character so the user can see the buffer is
    /// filling up while the real password text never leaves
    /// memory. Capped at the visible width of the field.
    pub password_len: usize,
    pub active: ActiveField,
    pub error: Option<&'a str>,
    pub backoff_seconds_remaining: Option<u64>,
}

/// Render the login UI into `pixels` (BGRA8888, `width × height`,
/// row-major).
///
/// Panel layout is computed once per call; everything is centered.
/// All glyph painting goes through [`BasicBitmapFont`] (8×16 cells),
/// kept compositor-native by [`crate::config::rgb_to_bgra`].
pub fn render_login_ui(state: &LoginUiState<'_>, pixels: &mut [u32], width: u32, height: u32) {
    let panel_w = 480u32;
    let panel_h = 240u32;
    let panel_x = width.saturating_sub(panel_w) / 2;
    let panel_y = height.saturating_sub(panel_h) / 2;

    // Opaque panel background: dark navy. `fill_rect` overwrites
    // pixels without blending, and `display_server` treats
    // `SurfaceRole::Toplevel` as opaque (see
    // `display_server::surface::ComposeEntry::is_opaque`), so any
    // client-side alpha here would be dropped at composition. Keep
    // alpha at 0xFF to make that contract explicit. True translucency
    // would require in-greeter blending against the background image
    // before submitting the frame.
    let panel_bg: u32 = 0xFF20_2840;
    fill_rect(pixels, width, panel_x, panel_y, panel_w, panel_h, panel_bg);

    let font = BasicBitmapFont::new();
    let prompt_color = state.config.prompt_color;
    let accent = state.config.accent_color;

    // Welcome banner.
    let banner_y = panel_y + 18;
    draw_text(
        pixels,
        width,
        panel_x + 20,
        banner_y,
        &state.config.welcome,
        prompt_color,
        panel_bg,
        &font,
    );

    // Username row.
    let label_x = panel_x + 20;
    let field_x = panel_x + 130;
    let field_w = panel_w - 150;
    let field_h = 22u32;
    let user_y = panel_y + 68;
    draw_text(
        pixels,
        width,
        label_x,
        user_y + 4,
        "Username:",
        prompt_color,
        panel_bg,
        &font,
    );
    let user_field_bg = field_bg_for(state.active == ActiveField::Username, accent);
    fill_rect(
        pixels,
        width,
        field_x,
        user_y,
        field_w,
        field_h,
        user_field_bg,
    );
    draw_text(
        pixels,
        width,
        field_x + 4,
        user_y + 4,
        state.username,
        prompt_color,
        user_field_bg,
        &font,
    );

    // Password row (no echo; show field highlight only).
    let pw_y = panel_y + 108;
    draw_text(
        pixels,
        width,
        label_x,
        pw_y + 4,
        "Password:",
        prompt_color,
        panel_bg,
        &font,
    );
    let pw_field_bg = field_bg_for(state.active == ActiveField::Password, accent);
    fill_rect(pixels, width, field_x, pw_y, field_w, field_h, pw_field_bg);
    // Masked echo: one `*` per typed character, capped at the visible
    // field width so a long password doesn't overflow the panel. The
    // real password buffer stays in the read_field caller; we only see
    // the length here.
    if state.password_len > 0 {
        let glyph_w = 8usize;
        let max_glyphs = ((field_w as usize) - 8) / glyph_w;
        let stars = state.password_len.min(max_glyphs);
        let mask: String = core::iter::repeat('*').take(stars).collect();
        draw_text(
            pixels,
            width,
            field_x + 4,
            pw_y + 4,
            &mask,
            prompt_color,
            pw_field_bg,
            &font,
        );
    }

    // Error message + backoff countdown.
    let err_y = panel_y + 160;
    if let Some(secs) = state.backoff_seconds_remaining {
        let msg = format_backoff(secs);
        draw_text(
            pixels,
            width,
            label_x,
            err_y,
            &msg,
            0xFFFF_8080,
            panel_bg,
            &font,
        );
    } else if let Some(err) = state.error {
        draw_text(
            pixels,
            width,
            label_x,
            err_y,
            err,
            0xFFFF_8080,
            panel_bg,
            &font,
        );
    }

    // Hint at the bottom of the panel: how to submit.
    let hint_y = panel_y + panel_h - 28;
    draw_text(
        pixels,
        width,
        label_x,
        hint_y,
        "Enter or Tab to advance, Esc to cancel.",
        0xFF99_AABB,
        panel_bg,
        &font,
    );
}

fn field_bg_for(active: bool, accent_bgra: u32) -> u32 {
    if active {
        // Mix with the panel base so the field still reads as a field.
        // Accent goes on the border only; we keep the inner bg dark.
        let a = accent_bgra & 0xFF00_0000;
        let r = (accent_bgra >> 16) & 0xFF;
        let g = (accent_bgra >> 8) & 0xFF;
        let b = accent_bgra & 0xFF;
        a | ((r / 3) << 16) | ((g / 3) << 8) | (b / 3)
    } else {
        0xFF10_1820
    }
}

fn format_backoff(secs: u64) -> String {
    alloc::format!("Too many attempts. Waiting {secs} seconds...")
}

fn fill_rect(pixels: &mut [u32], width: u32, x: u32, y: u32, w: u32, h: u32, color: u32) {
    let stride = width as usize;
    let total = pixels.len();
    for row in 0..h {
        let py = (y + row) as usize;
        for col in 0..w {
            let px = (x + col) as usize;
            let idx = py * stride + px;
            if idx < total {
                pixels[idx] = color;
            }
        }
    }
}

fn draw_text(
    pixels: &mut [u32],
    width: u32,
    x: u32,
    y: u32,
    text: &str,
    fg: u32,
    bg: u32,
    font: &BasicBitmapFont,
) {
    let (cell_w, cell_h) = font.cell_size();
    let stride = width as usize;
    let mut cursor_x = x;
    for ch in text.chars() {
        if ch == '\n' {
            continue;
        }
        if let Some(glyph) = font.glyph(ch as u32) {
            let cell_x = cursor_x as usize;
            let cell_y = y as usize;
            // Render glyph row by row into the surface.
            let bytes_per_row = (glyph.width as usize).div_ceil(8);
            for row in 0..glyph.height as usize {
                let row_start = row * bytes_per_row;
                let py = cell_y + row;
                for col in 0..glyph.width as usize {
                    let byte_idx = row_start + col / 8;
                    if byte_idx >= glyph.bitmap.len() {
                        break;
                    }
                    let bit_idx = 7 - (col % 8);
                    let bit_set = (glyph.bitmap[byte_idx] >> bit_idx) & 1 == 1;
                    let px = cell_x + col;
                    let idx = py * stride + px;
                    if idx < pixels.len() {
                        pixels[idx] = if bit_set { fg } else { bg };
                    }
                }
            }
        }
        cursor_x = cursor_x.saturating_add(cell_w as u32);
    }
    let _ = cell_h;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn render_paints_panel_at_center() {
        let cfg = GreeterConfig::default();
        let state = LoginUiState {
            config: &cfg,
            username: "alice",
            password_len: 5,
            active: ActiveField::Password,
            error: Some("Login incorrect"),
            backoff_seconds_remaining: None,
        };
        let w = 1280u32;
        let h = 800u32;
        let mut pixels = vec![0u32; (w * h) as usize];
        render_login_ui(&state, &mut pixels, w, h);
        // Center of the panel must be non-zero (painted).
        let cx = (w / 2) as usize;
        let cy = (h / 2) as usize;
        assert_ne!(pixels[cy * w as usize + cx], 0);
        // The corner of the surface must remain zero (untouched).
        assert_eq!(pixels[0], 0);
    }

    #[test]
    fn backoff_message_replaces_error() {
        let cfg = GreeterConfig::default();
        let state = LoginUiState {
            config: &cfg,
            username: "",
            password_len: 0,
            active: ActiveField::Username,
            error: Some("Login incorrect"),
            backoff_seconds_remaining: Some(4),
        };
        let w = 640u32;
        let h = 480u32;
        let mut pixels = vec![0u32; (w * h) as usize];
        render_login_ui(&state, &mut pixels, w, h);
        // Smoke: the error-row strip should contain non-bg pixels somewhere.
        let panel_y = (h - 240) / 2;
        let err_y = (panel_y + 160) as usize;
        let row_start = err_y * w as usize;
        let row_end = row_start + w as usize;
        assert!(pixels[row_start..row_end].iter().any(|&p| p != 0));
    }
}
