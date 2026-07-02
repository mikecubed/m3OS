//! Theme: the colors and metrics every widget reads (Phase 105 Track A.5).
//!
//! Centralizing these keeps the toolkit visually consistent and lets an
//! app restyle by swapping one struct. Pure data — no rendering — so it
//! is available in the host-testable core.

use crate::geom::Color;

/// Widget interaction state, used to pick the right theme color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visual {
    Normal,
    Hovered,
    Active,
    Focused,
    Disabled,
}

/// Colors + metrics for the whole toolkit.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub window_bg: Color,
    pub panel_bg: Color,
    pub text: Color,
    pub text_disabled: Color,

    pub button_bg: Color,
    pub button_bg_hover: Color,
    pub button_bg_active: Color,
    pub button_border: Color,

    pub field_bg: Color,
    pub field_border: Color,
    pub cursor: Color,

    pub accent: Color,
    pub focus_ring: Color,
    pub selection_bg: Color,
    pub separator: Color,

    /// Default row height for buttons/fields/rows.
    pub row_height: i32,
    /// Inner padding inside a button/field (horizontal).
    pub pad_x: i32,
    /// Inner padding (vertical).
    pub pad_y: i32,
    /// Gap between stacked widgets.
    pub spacing: i32,
    /// Border thickness for chrome.
    pub border: i32,
}

impl Theme {
    /// The default dark theme (matches the compositor's palette family).
    pub const fn dark() -> Theme {
        Theme {
            window_bg: Color::rgb(0x1e, 0x1e, 0x2e),
            panel_bg: Color::rgb(0x28, 0x28, 0x3c),
            text: Color::rgb(0xe0, 0xe0, 0xf0),
            text_disabled: Color::rgb(0x70, 0x70, 0x80),

            button_bg: Color::rgb(0x3a, 0x3a, 0x52),
            button_bg_hover: Color::rgb(0x4a, 0x4a, 0x66),
            button_bg_active: Color::rgb(0x2a, 0x2a, 0x3c),
            button_border: Color::rgb(0x55, 0x55, 0x70),

            field_bg: Color::rgb(0x14, 0x14, 0x20),
            field_border: Color::rgb(0x44, 0x44, 0x5c),
            cursor: Color::rgb(0xe0, 0xe0, 0xf0),

            accent: Color::rgb(0x89, 0xb4, 0xfa),
            focus_ring: Color::rgb(0x89, 0xb4, 0xfa),
            selection_bg: Color::rgb(0x45, 0x47, 0x5a),
            separator: Color::rgb(0x3a, 0x3a, 0x4e),

            row_height: 24,
            pad_x: 8,
            pad_y: 4,
            spacing: 6,
            border: 1,
        }
    }

    /// The button background for a given visual state.
    pub fn button_color(&self, v: Visual) -> Color {
        match v {
            Visual::Hovered => self.button_bg_hover,
            Visual::Active => self.button_bg_active,
            _ => self.button_bg,
        }
    }

    /// The text color for a given visual state.
    pub fn text_color(&self, v: Visual) -> Color {
        match v {
            Visual::Disabled => self.text_disabled,
            _ => self.text,
        }
    }
}

impl Default for Theme {
    fn default() -> Theme {
        Theme::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_color_tracks_visual() {
        let t = Theme::dark();
        assert_eq!(t.button_color(Visual::Normal), t.button_bg);
        assert_eq!(t.button_color(Visual::Hovered), t.button_bg_hover);
        assert_eq!(t.button_color(Visual::Active), t.button_bg_active);
    }

    #[test]
    fn disabled_text_is_dimmed() {
        let t = Theme::dark();
        assert_eq!(t.text_color(Visual::Disabled), t.text_disabled);
        assert_eq!(t.text_color(Visual::Normal), t.text);
    }
}
