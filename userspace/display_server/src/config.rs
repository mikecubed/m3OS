//! Phase 72 Track G — `/etc/compositor.conf` parser.
//!
//! Minimal TOML-compatible subset: section headers (`[name]`),
//! key/value lines (`key = value`), and `#` line comments. Values
//! are typed by section + key (integers, floats, hex colours,
//! identifier strings, chord strings).
//!
//! The parser deliberately avoids pulling in the full `toml` crate
//! (and its `serde` dependency): the compositor is `no_std`,
//! `kernel-core` cannot link `std`, and this minimal subset is
//! enough for the documented config surface. Syntax errors return
//! a typed [`ConfigError`] so callers can preserve the previous
//! configuration on parse failures.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use kernel_core::input::events::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};
use kernel_core::input::keymap::{
    KEY_0, KEY_1, KEY_2, KEY_3, KEY_4, KEY_5, KEY_6, KEY_7, KEY_8, KEY_9, KEY_ENTER, KEY_ESC,
    KEY_H, KEY_J, KEY_K, KEY_L, KEY_Q, KEY_R, KEY_TAB, Keycode,
};
use layout::{GapConfig, PolicyKind};

use crate::borders::BorderConfig;
use crate::decoration::DecorationConfig;
use crate::keybind::KeybindAction;
use crate::workspace::NUM_WORKSPACES;

/// Path the compositor reads at startup and on `m3ctl reload`.
pub const CONFIG_PATH: &str = "/etc/compositor.conf";

/// Top-level configuration object. Sections that the file omits fall
/// back to their `defaults()` values.
#[derive(Clone, Debug)]
pub struct CompositorConfig {
    pub gaps: GapConfig,
    pub borders: BorderConfig,
    pub decorations: DecorationConfig,
    pub keybinds: KeybindConfig,
    pub workspaces: WorkspaceConfig,
    pub autostart: AutostartConfig,
    // `[wallpaper]` is owned by the `userspace/wallpaper` client; the
    // compositor only needs to recognise the section header so it does
    // not emit an `UnknownSection` warning on every parse. See
    // `Section::Wallpaper` below.
}

impl Default for CompositorConfig {
    fn default() -> Self {
        Self::defaults()
    }
}

impl CompositorConfig {
    /// Sensible Phase 72 defaults — used at startup when no config
    /// file is present, and as the fallback after a parse error.
    pub fn defaults() -> Self {
        Self {
            gaps: GapConfig::new(8, 8),
            borders: BorderConfig::defaults(),
            decorations: DecorationConfig::defaults(),
            keybinds: KeybindConfig::defaults(),
            workspaces: WorkspaceConfig::defaults(),
            autostart: AutostartConfig::defaults(),
        }
    }

    /// Parse the contents of `/etc/compositor.conf` into a
    /// [`CompositorConfig`].
    ///
    /// Unknown sections and unknown keys are **logged-and-ignored** so
    /// the file format can grow without older parsers rejecting newer
    /// configs. Structural failures (`MalformedLine`, `BadValue`) are
    /// still returned as `Err` because they indicate a real syntax
    /// problem the user can fix. The `Vec<ConfigWarning>` carries the
    /// ignored entries so the caller can log them.
    ///
    /// The shipped compositor calls `parse_with_warnings` so it can log what it
    /// skipped, leaving this warning-discarding form to the unit tests below —
    /// which is most of what they assert against, since a test that cares about
    /// warnings asks for them explicitly.
    #[allow(dead_code)]
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        Self::parse_with_warnings(input).map(|(cfg, _)| cfg)
    }

    /// Parse + return any non-fatal warnings (unknown sections / keys).
    /// The caller is expected to log the warnings; tests use this form
    /// directly to assert which entries were skipped.
    pub fn parse_with_warnings(input: &str) -> Result<(Self, Vec<ConfigWarning>), ConfigError> {
        let mut cfg = Self::defaults();
        let mut warnings = Vec::new();
        let mut section = Section::Top;
        for (lineno, raw_line) in input.lines().enumerate() {
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = match name.trim() {
                    "gaps" => Section::Gaps,
                    "borders" => Section::Borders,
                    "decorations" => Section::Decorations,
                    "keybinds" => Section::Keybinds,
                    "workspaces" => Section::Workspaces,
                    "autostart" => Section::Autostart,
                    "wallpaper" => Section::Wallpaper,
                    other => {
                        warnings.push(ConfigWarning::UnknownSection {
                            name: other.to_string(),
                            line: lineno + 1,
                        });
                        Section::Unknown
                    }
                };
                continue;
            }
            let (k, v) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), strip_quotes(v.trim())),
                None => {
                    return Err(ConfigError::MalformedLine { line: lineno + 1 });
                }
            };
            let apply_result = match section {
                Section::Top | Section::Unknown => Ok(()),
                Section::Gaps => apply_gap_key(&mut cfg.gaps, k, v, lineno + 1),
                Section::Borders => apply_border_key(&mut cfg.borders, k, v, lineno + 1),
                Section::Decorations => {
                    apply_decoration_key(&mut cfg.decorations, k, v, lineno + 1)
                }
                Section::Keybinds => apply_keybind_key(&mut cfg.keybinds, k, v, lineno + 1),
                Section::Workspaces => apply_workspace_key(&mut cfg.workspaces, k, v, lineno + 1),
                Section::Autostart => apply_autostart_key(&mut cfg.autostart, k, v, lineno + 1),
                // `[wallpaper]` keys are validated by the wallpaper
                // client's own parser; skip them silently here so the
                // section is not flagged as unknown.
                Section::Wallpaper => Ok(()),
            };
            match apply_result {
                Ok(()) => {}
                Err(ConfigError::UnknownKey { key, line }) => {
                    warnings.push(ConfigWarning::UnknownKey { key, line });
                }
                Err(other) => return Err(other),
            }
        }
        Ok((cfg, warnings))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    Top,
    Gaps,
    Borders,
    Decorations,
    Keybinds,
    Workspaces,
    Autostart,
    Wallpaper,
    /// An unrecognized section header. Subsequent `key = value` lines
    /// are still syntax-checked but the values are discarded, mirroring
    /// the `UnknownSection` warning at the section header.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    // No `UnknownSection` here on purpose: an unrecognised `[section]` header
    // is log-and-ignore (see `parse_with_warnings`), so it is reported as
    // `ConfigWarning::UnknownSection` and never aborts the parse.
    UnknownKey { key: String, line: usize },
    BadValue { key: String, line: usize },
    MalformedLine { line: usize },
}

/// Non-fatal parse warnings — unknown sections / keys the parser
/// skipped without aborting. Surfaced separately from [`ConfigError`]
/// so the caller can log them while still applying the rest of the
/// file. Matches the documented "log-and-ignore" semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigWarning {
    UnknownSection { name: String, line: usize },
    UnknownKey { key: String, line: usize },
}

fn strip_comment(line: &str) -> &str {
    // `#` is a comment delimiter only when it starts the trimmed line.
    // Treating every `#` as a delimiter breaks legitimate value uses
    // such as `inactive_color = #888888`, where the `#` is the standard
    // hex colour prefix. The config file's `#` line comments are always
    // whole-line ("# header" or "  # indented note"), so anchoring the
    // delimiter to start-of-trimmed-line covers every documented
    // comment shape without ambiguity in the value position.
    if line.trim_start().starts_with('#') {
        ""
    } else {
        line
    }
}

fn strip_quotes(value: &str) -> &str {
    let v = value.trim();
    // Either quote style strips the same way; the pairing matters, so a value
    // that opens with one style and closes with the other is left untouched.
    let wrapped =
        (v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\''));
    if v.len() >= 2 && wrapped {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

fn parse_u16(key: &str, value: &str, line: usize) -> Result<u16, ConfigError> {
    value.parse::<u16>().map_err(|_| ConfigError::BadValue {
        key: key.to_string(),
        line,
    })
}

fn parse_u8(key: &str, value: &str, line: usize) -> Result<u8, ConfigError> {
    value.parse::<u8>().map_err(|_| ConfigError::BadValue {
        key: key.to_string(),
        line,
    })
}

fn parse_color(key: &str, value: &str, line: usize) -> Result<u32, ConfigError> {
    let stripped = value.trim();
    let hex = if let Some(s) = stripped.strip_prefix("0x") {
        s
    } else if let Some(s) = stripped.strip_prefix('#') {
        s
    } else {
        stripped
    };
    u32::from_str_radix(hex, 16).map_err(|_| ConfigError::BadValue {
        key: key.to_string(),
        line,
    })
}

/// Float-valued key parser, sibling to `parse_u8` / `parse_u16` / `parse_color`.
/// No `f32` key exists in `compositor.conf` yet (gaps, widths and colours are
/// all integral), so nothing calls it; kept so the numeric-parser set stays
/// complete and a future ratio/opacity key reports `BadValue` identically.
#[allow(dead_code)]
fn parse_f32(key: &str, value: &str, line: usize) -> Result<f32, ConfigError> {
    value.parse::<f32>().map_err(|_| ConfigError::BadValue {
        key: key.to_string(),
        line,
    })
}

fn apply_gap_key(
    cfg: &mut GapConfig,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), ConfigError> {
    match key {
        "outer" => cfg.outer = parse_u16(key, value, line)?,
        "inner" => cfg.inner = parse_u16(key, value, line)?,
        _ => {
            return Err(ConfigError::UnknownKey {
                key: key.to_string(),
                line,
            });
        }
    }
    Ok(())
}

fn apply_border_key(
    cfg: &mut BorderConfig,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), ConfigError> {
    match key {
        "width" => cfg.width = parse_u8(key, value, line)?,
        "active_color" | "active" => cfg.active_color = parse_color(key, value, line)?,
        "inactive_color" | "inactive" => cfg.inactive_color = parse_color(key, value, line)?,
        _ => {
            return Err(ConfigError::UnknownKey {
                key: key.to_string(),
                line,
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct KeybindConfig {
    /// Resize-step pixel count for the `H/J/K/L` resize-mode chord.
    pub resize_step_px: i16,
    /// User-supplied chord overrides. Layered on top of the built-in
    /// chord set; conflicts replace the built-in action.
    pub user_chords: Vec<(u16, Keycode, KeybindAction)>,
}

impl KeybindConfig {
    pub fn defaults() -> Self {
        Self {
            resize_step_px: crate::keybind::DEFAULT_RESIZE_STEP_PX,
            user_chords: Vec::new(),
        }
    }
}

fn apply_keybind_key(
    cfg: &mut KeybindConfig,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), ConfigError> {
    if key == "resize_step_px" {
        cfg.resize_step_px = value.parse::<i16>().map_err(|_| ConfigError::BadValue {
            key: key.to_string(),
            line,
        })?;
        return Ok(());
    }
    // Treat any other key as a chord spec: the *key* is the chord
    // string (e.g. `super+1`) and the *value* is the action.
    let chord = parse_chord(key, line)?;
    let action = parse_action(value, line)?;
    if let Some(slot) = cfg
        .user_chords
        .iter_mut()
        .find(|(m, k, _)| *m == chord.0 && *k == chord.1)
    {
        slot.2 = action;
    } else {
        cfg.user_chords.push((chord.0, chord.1, action));
    }
    Ok(())
}

fn parse_chord(spec: &str, line: usize) -> Result<(u16, Keycode), ConfigError> {
    let mut mask = 0u16;
    let mut keycode = None;
    for part in spec.split('+') {
        let part = part.trim();
        match part.to_ascii_lowercase().as_str() {
            "super" | "mod" | "win" | "meta" => mask |= MOD_SUPER,
            "shift" => mask |= MOD_SHIFT,
            "ctrl" | "control" => mask |= MOD_CTRL,
            "alt" => mask |= MOD_ALT,
            other => {
                if keycode.is_some() {
                    return Err(ConfigError::BadValue {
                        key: spec.to_string(),
                        line,
                    });
                }
                keycode = Some(
                    keycode_from_name(other).ok_or_else(|| ConfigError::BadValue {
                        key: spec.to_string(),
                        line,
                    })?,
                );
            }
        }
    }
    let kc = keycode.ok_or_else(|| ConfigError::BadValue {
        key: spec.to_string(),
        line,
    })?;
    Ok((mask, kc))
}

fn keycode_from_name(name: &str) -> Option<Keycode> {
    match name {
        "1" => Some(KEY_1),
        "2" => Some(KEY_2),
        "3" => Some(KEY_3),
        "4" => Some(KEY_4),
        "5" => Some(KEY_5),
        "6" => Some(KEY_6),
        "7" => Some(KEY_7),
        "8" => Some(KEY_8),
        "9" => Some(KEY_9),
        "0" => Some(KEY_0),
        "tab" => Some(KEY_TAB),
        "return" | "enter" => Some(KEY_ENTER),
        "escape" | "esc" => Some(KEY_ESC),
        "q" => Some(KEY_Q),
        "r" => Some(KEY_R),
        "h" => Some(KEY_H),
        "j" => Some(KEY_J),
        "k" => Some(KEY_K),
        "l" => Some(KEY_L),
        _ => None,
    }
}

fn parse_action(spec: &str, line: usize) -> Result<KeybindAction, ConfigError> {
    let s = spec.trim();
    // `switch-workspace 3` / `workspace 3`
    if let Some(rest) = s
        .strip_prefix("switch-workspace")
        .or_else(|| s.strip_prefix("workspace"))
    {
        let n = rest
            .trim()
            .parse::<u8>()
            .map_err(|_| ConfigError::BadValue {
                key: spec.to_string(),
                line,
            })?;
        return Ok(KeybindAction::SwitchWorkspace(n));
    }
    if let Some(rest) = s.strip_prefix("move-to-workspace") {
        let n = rest
            .trim()
            .parse::<u8>()
            .map_err(|_| ConfigError::BadValue {
                key: spec.to_string(),
                line,
            })?;
        return Ok(KeybindAction::MoveToWorkspace(n));
    }
    match s {
        "cycle-focus" => Ok(KeybindAction::CycleFocus),
        "spawn-term" => Ok(KeybindAction::SpawnTerm),
        "kill-focused" => Ok(KeybindAction::KillFocused),
        "enter-resize" => Ok(KeybindAction::EnterResize),
        "exit-resize" => Ok(KeybindAction::ExitResize),
        _ => Err(ConfigError::BadValue {
            key: spec.to_string(),
            line,
        }),
    }
}

#[derive(Clone, Debug)]
pub struct WorkspaceConfig {
    /// Per-slot default policy. `defaults[0]` applies to workspace 1, etc.
    pub defaults: [PolicyKind; NUM_WORKSPACES],
    /// `move-to-workspace` follow semantics — when `true`, the
    /// compositor switches to the target after a move.
    pub follow_on_move: bool,
}

impl WorkspaceConfig {
    pub fn defaults() -> Self {
        Self {
            defaults: [PolicyKind::Dwindle; NUM_WORKSPACES],
            follow_on_move: false,
        }
    }
}

fn apply_workspace_key(
    cfg: &mut WorkspaceConfig,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), ConfigError> {
    if key == "follow_on_move" {
        cfg.follow_on_move = match value.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => true,
            "false" | "no" | "off" | "0" => false,
            _ => {
                return Err(ConfigError::BadValue {
                    key: key.to_string(),
                    line,
                });
            }
        };
        return Ok(());
    }
    // Per-workspace policy: `workspace_1 = master-stack`, ...
    if let Some(rest) = key.strip_prefix("workspace_") {
        let idx_1: usize = rest.parse().map_err(|_| ConfigError::BadValue {
            key: key.to_string(),
            line,
        })?;
        if idx_1 == 0 || idx_1 > NUM_WORKSPACES {
            return Err(ConfigError::BadValue {
                key: key.to_string(),
                line,
            });
        }
        let policy = PolicyKind::from_name(value.trim()).ok_or_else(|| ConfigError::BadValue {
            key: key.to_string(),
            line,
        })?;
        cfg.defaults[idx_1 - 1] = policy;
        return Ok(());
    }
    if key == "default" {
        let policy = PolicyKind::from_name(value.trim()).ok_or_else(|| ConfigError::BadValue {
            key: key.to_string(),
            line,
        })?;
        cfg.defaults = [policy; NUM_WORKSPACES];
        return Ok(());
    }
    Err(ConfigError::UnknownKey {
        key: key.to_string(),
        line,
    })
}

/// Phase 72b — `[autostart]` section. Each `exec = /path/to/binary`
/// entry is appended in declaration order. After `display_server`
/// finishes its first compose frame (so newly-mapped surfaces have a
/// running compositor to attach to), every `entries[i]` is launched
/// via `fork + execve` exactly once.
///
/// Reload via `m3ctl reload` does NOT re-run autostart — it is a
/// one-shot per compositor lifetime, matching Hyprland's `exec-once`
/// semantics. Long-running operator-driven respawns belong in init
/// or session_manager, not the compositor's `[autostart]` slot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutostartConfig {
    pub entries: Vec<String>,
}

impl AutostartConfig {
    pub fn defaults() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

fn apply_autostart_key(
    cfg: &mut AutostartConfig,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), ConfigError> {
    match key {
        "exec" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(ConfigError::BadValue {
                    key: key.to_string(),
                    line,
                });
            }
            cfg.entries.push(trimmed.to_string());
            Ok(())
        }
        _ => Err(ConfigError::UnknownKey {
            key: key.to_string(),
            line,
        }),
    }
}

fn parse_u32(key: &str, value: &str, line: usize) -> Result<u32, ConfigError> {
    value.parse::<u32>().map_err(|_| ConfigError::BadValue {
        key: key.to_string(),
        line,
    })
}

fn parse_i32(key: &str, value: &str, line: usize) -> Result<i32, ConfigError> {
    value.parse::<i32>().map_err(|_| ConfigError::BadValue {
        key: key.to_string(),
        line,
    })
}

fn apply_decoration_key(
    cfg: &mut DecorationConfig,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), ConfigError> {
    match key {
        "corner_radius" => cfg.corner_radius = parse_u32(key, value, line)?,
        "shadow_blur" | "shadow_radius" => cfg.shadow_blur = parse_u32(key, value, line)?,
        "shadow_offset_x" => cfg.shadow_offset_x = parse_i32(key, value, line)?,
        "shadow_offset_y" => cfg.shadow_offset_y = parse_i32(key, value, line)?,
        "shadow_color" => cfg.shadow_color = parse_color(key, value, line)?,
        _ => {
            return Err(ConfigError::UnknownKey {
                key: key.to_string(),
                line,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_empty_input() {
        let cfg = CompositorConfig::parse("").unwrap();
        assert_eq!(cfg.gaps, GapConfig::new(8, 8));
        assert_eq!(cfg.borders.width, 2);
        assert_eq!(cfg.workspaces.defaults[0], PolicyKind::Dwindle);
    }

    #[test]
    fn comments_and_blank_lines_skip() {
        let cfg = CompositorConfig::parse(
            "# header\n\n[gaps]\n# inner comment\nouter = 12\n\ninner = 4\n",
        )
        .unwrap();
        assert_eq!(cfg.gaps.outer, 12);
        assert_eq!(cfg.gaps.inner, 4);
    }

    #[test]
    fn borders_section_parses_colors() {
        let cfg = CompositorConfig::parse(
            "[borders]\nwidth = 4\nactive_color = 0x00FF00FF\ninactive_color = #888888\n",
        )
        .unwrap();
        assert_eq!(cfg.borders.width, 4);
        assert_eq!(cfg.borders.active_color, 0x00FF00FF);
        assert_eq!(cfg.borders.inactive_color, 0x888888);
    }

    #[test]
    fn keybinds_parse_chord_spec() {
        let cfg = CompositorConfig::parse(
            "[keybinds]\nresize_step_px = 64\nsuper+1 = switch-workspace 2\n",
        )
        .unwrap();
        assert_eq!(cfg.keybinds.resize_step_px, 64);
        assert_eq!(cfg.keybinds.user_chords.len(), 1);
        assert_eq!(
            cfg.keybinds.user_chords[0].2,
            KeybindAction::SwitchWorkspace(2)
        );
    }

    #[test]
    fn workspaces_section_assigns_policies() {
        let cfg = CompositorConfig::parse(
            "[workspaces]\ndefault = grid\nworkspace_9 = fullscreen\nfollow_on_move = true\n",
        )
        .unwrap();
        assert!(cfg.workspaces.follow_on_move);
        assert_eq!(cfg.workspaces.defaults[0], PolicyKind::Grid);
        assert_eq!(cfg.workspaces.defaults[8], PolicyKind::Fullscreen);
    }

    #[test]
    fn unknown_section_warns_and_continues() {
        // Forward-compat: an older parser must not reject a newer
        // config file just because it gained a section the parser
        // doesn't know yet. The unknown section becomes a warning and
        // the rest of the file still applies.
        let (cfg, warnings) =
            CompositorConfig::parse_with_warnings("[bogus]\nfoo = 1\n[gaps]\nouter = 5\n")
                .expect("unknown section is non-fatal");
        assert_eq!(cfg.gaps.outer, 5);
        assert!(matches!(
            warnings.as_slice(),
            [ConfigWarning::UnknownSection { name, .. }] if name == "bogus"
        ));
    }

    #[test]
    fn unknown_key_warns_and_continues() {
        // Same forward-compat story for keys inside known sections.
        let (cfg, warnings) = CompositorConfig::parse_with_warnings(
            "[gaps]\nouter = 5\nfuture_key = whatever\ninner = 2\n",
        )
        .expect("unknown key is non-fatal");
        assert_eq!(cfg.gaps.outer, 5);
        assert_eq!(cfg.gaps.inner, 2);
        assert!(matches!(
            warnings.as_slice(),
            [ConfigWarning::UnknownKey { key, .. }] if key == "future_key"
        ));
    }

    #[test]
    fn autostart_single_exec_parses() {
        let cfg =
            CompositorConfig::parse("[autostart]\nexec = /bin/term\n").expect("autostart parses");
        assert_eq!(cfg.autostart.entries, vec!["/bin/term".to_string()]);
    }

    #[test]
    fn autostart_multiple_execs_preserve_order() {
        let cfg = CompositorConfig::parse(
            "[autostart]\nexec = /bin/term\nexec = /bin/clock\nexec = /bin/notifier\n",
        )
        .expect("multi-exec parses");
        assert_eq!(
            cfg.autostart.entries,
            vec![
                "/bin/term".to_string(),
                "/bin/clock".to_string(),
                "/bin/notifier".to_string(),
            ]
        );
    }

    #[test]
    fn autostart_empty_exec_is_bad_value() {
        assert!(matches!(
            CompositorConfig::parse("[autostart]\nexec = \n"),
            Err(ConfigError::BadValue { .. })
        ));
    }

    #[test]
    fn bad_value_is_still_a_hard_error() {
        // A real syntax error (not-a-number where a u16 is expected)
        // must still surface so the user can fix it. Distinguishing
        // unknown-key (forward-compat) from bad-value (broken syntax)
        // is the point of separating warnings from errors.
        assert!(matches!(
            CompositorConfig::parse("[gaps]\nouter = not-a-number\n"),
            Err(ConfigError::BadValue { .. })
        ));
    }

    #[test]
    fn malformed_line_returns_error() {
        assert!(matches!(
            CompositorConfig::parse("[gaps]\nnotanassignment\n"),
            Err(ConfigError::MalformedLine { line: 2 })
        ));
    }

    #[test]
    fn hash_in_color_value_is_not_a_comment() {
        // Regression: `strip_comment` used to truncate at the first `#`
        // anywhere, which silently turned `inactive_color = #888888`
        // into `inactive_color = ` and surfaced as a BadValue. `#` is
        // a comment delimiter only at start-of-trimmed-line, so a
        // `#RRGGBB` hex literal in the value position survives intact.
        let cfg = CompositorConfig::parse(
            "[borders]\nactive_color = #11223344\ninactive_color = #888888\n",
        )
        .expect("hex-prefixed colours parse");
        assert_eq!(cfg.borders.active_color, 0x11223344);
        assert_eq!(cfg.borders.inactive_color, 0x888888);
    }

    #[test]
    fn leading_hash_still_treated_as_comment() {
        // Companion to `hash_in_color_value_is_not_a_comment`: `#` at
        // start-of-trimmed-line is still a comment (the documented
        // shape) so configs that lead with `# header\n` and
        // `  # indented note\n` keep their existing meaning.
        let cfg =
            CompositorConfig::parse("# top-level header\n[gaps]\n  # indented note\nouter = 7\n")
                .expect("leading-`#` comments parse");
        assert_eq!(cfg.gaps.outer, 7);
    }
}
