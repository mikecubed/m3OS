//! Phase 71 Track E.1 — `/etc/greeter.conf` parser.
//!
//! Flat key=value file with `#` comments and blank-line tolerance.
//! Unknown keys are reported through [`ConfigParseEvent::UnknownKey`]
//! so the binary can `log::warn!` them as structured observability
//! events. The events carry owned [`String`] payloads (key and value),
//! so each unknown/invalid line costs one or two allocations — that
//! tradeoff is intentional, since misconfiguration is the cold path.

use alloc::string::String;

/// Default background color (dark slate). BGRA8888.
pub const DEFAULT_BACKGROUND_COLOR: u32 = 0xFF18_2233;
/// Default prompt-text color (near-white).
pub const DEFAULT_PROMPT_COLOR_RGB: u32 = 0x00FF_FFFF;
/// Default accent color (cornflower blue).
pub const DEFAULT_ACCENT_COLOR_RGB: u32 = 0x0044_88CC;
/// Default welcome banner.
pub const DEFAULT_WELCOME: &str = "m3OS Login";
/// Default background image path candidates, tried in order.
pub const DEFAULT_BACKGROUND_PATHS: &[&str] =
    &["/etc/greeter/background.png", "/etc/greeter/background.bmp"];

/// In-memory greeter configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GreeterConfig {
    /// Optional override for background image path. When `None`, the
    /// binary tries [`DEFAULT_BACKGROUND_PATHS`] in order.
    pub background: Option<String>,
    /// BGRA8888 prompt-text color. Alpha is always 0xFF.
    pub prompt_color: u32,
    /// BGRA8888 accent color for the active field highlight.
    pub accent_color: u32,
    /// Welcome banner shown above the login form.
    pub welcome: String,
}

impl Default for GreeterConfig {
    fn default() -> Self {
        Self {
            background: None,
            prompt_color: rgb_to_bgra(DEFAULT_PROMPT_COLOR_RGB),
            accent_color: rgb_to_bgra(DEFAULT_ACCENT_COLOR_RGB),
            welcome: String::from(DEFAULT_WELCOME),
        }
    }
}

/// Observability event emitted by [`parse_config`] for keys the parser
/// does not recognise. The caller emits a structured log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigParseEvent {
    UnknownKey(String),
    InvalidColor { key: String, value: String },
}

/// Parse a `key=value` config text. Unknown keys are reported through
/// `events` and ignored. Returns the populated [`GreeterConfig`].
pub fn parse_config(text: &str, events: &mut alloc::vec::Vec<ConfigParseEvent>) -> GreeterConfig {
    let mut cfg = GreeterConfig::default();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(eq) = trimmed.find('=') else {
            continue;
        };
        let key = trimmed[..eq].trim();
        let value = trimmed[eq + 1..].trim();
        match key {
            "background" => cfg.background = Some(String::from(value)),
            "prompt-color" => match parse_hex_color(value) {
                Some(rgb) => cfg.prompt_color = rgb_to_bgra(rgb),
                None => events.push(ConfigParseEvent::InvalidColor {
                    key: String::from(key),
                    value: String::from(value),
                }),
            },
            "accent-color" => match parse_hex_color(value) {
                Some(rgb) => cfg.accent_color = rgb_to_bgra(rgb),
                None => events.push(ConfigParseEvent::InvalidColor {
                    key: String::from(key),
                    value: String::from(value),
                }),
            },
            "welcome" => cfg.welcome = String::from(value),
            _ => events.push(ConfigParseEvent::UnknownKey(String::from(key))),
        }
    }
    cfg
}

/// Convert an `rrggbb` (24-bit RGB) integer into BGRA8888 with full
/// alpha so it can be written directly into a surface buffer.
pub const fn rgb_to_bgra(rgb: u32) -> u32 {
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

/// Parse a 6-character hex string (`rrggbb`, optionally prefixed
/// with `#` or `0x`) into a packed 24-bit RGB value.
pub fn parse_hex_color(s: &str) -> Option<u32> {
    let trimmed = s.trim();
    let body = trimmed
        .strip_prefix('#')
        .or_else(|| trimmed.strip_prefix("0x"))
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if body.len() != 6 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(body, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn defaults_have_full_alpha() {
        let cfg = GreeterConfig::default();
        assert_eq!(cfg.prompt_color & 0xFF00_0000, 0xFF00_0000);
        assert_eq!(cfg.accent_color & 0xFF00_0000, 0xFF00_0000);
    }

    #[test]
    fn parse_all_four_keys() {
        let text = "# comment\n\nbackground=/etc/greeter/img.bmp\nprompt-color=ffffff\naccent-color=#4488cc\nwelcome=Hello\n";
        let mut events = Vec::new();
        let cfg = parse_config(text, &mut events);
        assert!(events.is_empty());
        assert_eq!(cfg.background.as_deref(), Some("/etc/greeter/img.bmp"));
        assert_eq!(cfg.prompt_color, rgb_to_bgra(0xFFFFFF));
        assert_eq!(cfg.accent_color, rgb_to_bgra(0x4488CC));
        assert_eq!(cfg.welcome, "Hello");
    }

    #[test]
    fn unknown_key_reported() {
        let text = "wallpaper=/foo\n";
        let mut events = Vec::new();
        let cfg = parse_config(text, &mut events);
        assert_eq!(cfg, GreeterConfig::default());
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], ConfigParseEvent::UnknownKey(k) if k == "wallpaper"));
    }

    #[test]
    fn invalid_color_falls_back_to_default() {
        let text = "prompt-color=notahex\n";
        let mut events = Vec::new();
        let cfg = parse_config(text, &mut events);
        assert_eq!(cfg.prompt_color, GreeterConfig::default().prompt_color);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn rgb_to_bgra_packs_full_alpha() {
        assert_eq!(rgb_to_bgra(0x00FF_00), 0xFF00_FF00);
    }
}
