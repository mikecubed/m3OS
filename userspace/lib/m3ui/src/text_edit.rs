//! Single-line text-edit buffer (Phase 105 Track A.4 support).
//!
//! The cursor + editing state machine behind `text_field`, kept as pure
//! logic so insertion, deletion, and cursor motion are host-tested
//! independent of the framebuffer. Operates on `char` positions (not
//! bytes), so multi-byte UTF-8 never splits mid-character.

use alloc::string::String;
use alloc::vec::Vec;

use crate::input::{InputState, KeyCode};

/// The result of applying a frame of input to a [`TextBuffer`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EditOutcome {
    /// The buffer text changed this frame (insert/delete/paste/cut).
    pub text_changed: bool,
    /// `Some(text)` when the user pressed Ctrl+C (copy) or Ctrl+X (cut)
    /// — the caller should publish `text` to the clipboard. For a cut,
    /// `text_changed` is also set (the content was cleared).
    pub copy: Option<String>,
}

/// A single-line editable string with a cursor.
#[derive(Debug, Clone, Default)]
pub struct TextBuffer {
    /// The text as a `char` vector (edits are O(n) but fields are short).
    chars: Vec<char>,
    /// Cursor position as a char index in `0..=chars.len()`.
    cursor: usize,
}

impl TextBuffer {
    pub fn new() -> TextBuffer {
        TextBuffer::default()
    }

    /// A buffer seeded with `s`, cursor at the end.
    //
    // `should_implement_trait` wants `core::str::FromStr`, but that trait
    // is `Result`-returning and this conversion cannot fail — every call
    // site would have to unwrap an `Infallible` error for no gain. The
    // inherent name is kept because it reads correctly at the call site.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> TextBuffer {
        let chars: Vec<char> = s.chars().collect();
        let cursor = chars.len();
        TextBuffer { chars, cursor }
    }

    pub fn as_string(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the whole buffer, cursor to end (e.g. programmatic set).
    pub fn set(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.cursor = self.chars.len();
    }

    /// Insert `ch` at the cursor and advance it.
    pub fn insert_char(&mut self, ch: char) {
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    /// Insert a whole string at the cursor (a paste).
    pub fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.insert_char(ch);
        }
    }

    /// Delete the char before the cursor (Backspace).
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    /// Delete the char at the cursor (Delete).
    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Empty the buffer and reset the cursor.
    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// Apply every key press queued this frame. `clipboard` supplies the
    /// paste text for Ctrl+V (the render layer fetches it from the
    /// compositor); a `None` return is a no-op paste. Returns an
    /// [`EditOutcome`]: whether the text changed, and a copy/cut request
    /// the caller publishes to the clipboard.
    pub fn apply_input(
        &mut self,
        input: &InputState,
        mut clipboard: impl FnMut() -> Option<String>,
    ) -> EditOutcome {
        let mut outcome = EditOutcome::default();
        for key in input.keys() {
            // Ctrl chords: copy/cut/paste, and Home/End motion.
            if key.mods.ctrl {
                match key.ch.map(|c| c.to_ascii_lowercase()) {
                    // Copy the whole field (no selection model → content).
                    Some('c') => outcome.copy = Some(self.as_string()),
                    // Cut: copy then clear.
                    Some('x') => {
                        outcome.copy = Some(self.as_string());
                        if !self.chars.is_empty() {
                            self.clear();
                            outcome.text_changed = true;
                        }
                    }
                    // Paste the clipboard's first line at the cursor.
                    Some('v') => {
                        if let Some(text) = clipboard() {
                            let line = text.split(['\n', '\r']).next().unwrap_or("");
                            self.insert_str(line);
                            outcome.text_changed = true;
                        }
                    }
                    // Ctrl+A/E emacs-style bol/eol.
                    Some('a') => self.move_home(),
                    Some('e') => self.move_end(),
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Backspace => {
                    self.backspace();
                    outcome.text_changed = true;
                }
                KeyCode::Delete => {
                    self.delete();
                    outcome.text_changed = true;
                }
                KeyCode::Left => self.move_left(),
                KeyCode::Right => self.move_right(),
                KeyCode::Home => self.move_home(),
                KeyCode::End => self.move_end(),
                // Printable character (space included when it carries a ch).
                KeyCode::Char | KeyCode::Space => {
                    if let Some(ch) = key.ch
                        && !ch.is_control()
                    {
                        self.insert_char(ch);
                        outcome.text_changed = true;
                    }
                }
                // Tab/Enter/Escape are focus/submit concerns, not edits.
                _ => {}
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{KeyCode, KeyPress, Mods};

    /// Feed keys and return whether the text changed (most tests only
    /// care about that; the copy/cut tests call `apply_input` directly).
    fn feed(buf: &mut TextBuffer, keys: &[KeyPress]) -> bool {
        let mut input = InputState::new();
        for k in keys {
            input.push_key(k.clone());
        }
        buf.apply_input(&input, || None).text_changed
    }

    fn ch_key(c: char) -> KeyPress {
        KeyPress {
            code: KeyCode::Char,
            ch: Some(c),
            mods: Mods::default(),
        }
    }

    fn nav(code: KeyCode) -> KeyPress {
        KeyPress {
            code,
            ch: None,
            mods: Mods::default(),
        }
    }

    #[test]
    fn insert_and_backspace() {
        let mut b = TextBuffer::new();
        assert!(feed(&mut b, &[ch_key('h'), ch_key('i')]));
        assert_eq!(b.as_string(), "hi");
        assert_eq!(b.cursor(), 2);
        feed(&mut b, &[nav(KeyCode::Backspace)]);
        assert_eq!(b.as_string(), "h");
        assert_eq!(b.cursor(), 1);
    }

    #[test]
    fn cursor_motion_and_mid_insert() {
        let mut b = TextBuffer::from_str("ac");
        feed(&mut b, &[nav(KeyCode::Left)]); // between a and c
        assert_eq!(b.cursor(), 1);
        feed(&mut b, &[ch_key('b')]);
        assert_eq!(b.as_string(), "abc");
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn home_end_and_delete() {
        let mut b = TextBuffer::from_str("xyz");
        feed(&mut b, &[nav(KeyCode::Home)]);
        assert_eq!(b.cursor(), 0);
        feed(&mut b, &[nav(KeyCode::Delete)]);
        assert_eq!(b.as_string(), "yz");
        feed(&mut b, &[nav(KeyCode::End)]);
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut b = TextBuffer::from_str("q");
        feed(&mut b, &[nav(KeyCode::Home), nav(KeyCode::Backspace)]);
        assert_eq!(b.as_string(), "q");
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn multibyte_chars_do_not_split() {
        let mut b = TextBuffer::new();
        feed(&mut b, &[ch_key('é'), ch_key('中')]);
        assert_eq!(b.len(), 2);
        feed(&mut b, &[nav(KeyCode::Backspace)]);
        assert_eq!(b.as_string(), "é");
    }

    #[test]
    fn ctrl_v_pastes_first_line_only() {
        let mut b = TextBuffer::from_str("x");
        let mut input = InputState::new();
        input.push_key(KeyPress {
            code: KeyCode::Char,
            ch: Some('v'),
            mods: Mods {
                ctrl: true,
                ..Default::default()
            },
        });
        let out = b.apply_input(&input, || Some(String::from("paste\nsecond")));
        assert!(out.text_changed);
        assert!(out.copy.is_none());
        assert_eq!(b.as_string(), "xpaste");
    }

    fn ctrl_key(c: char) -> KeyPress {
        KeyPress {
            code: KeyCode::Char,
            ch: Some(c),
            mods: Mods {
                ctrl: true,
                ..Default::default()
            },
        }
    }

    #[test]
    fn ctrl_c_copies_content_without_changing_it() {
        let mut b = TextBuffer::from_str("hello");
        let mut input = InputState::new();
        input.push_key(ctrl_key('c'));
        let out = b.apply_input(&input, || None);
        assert_eq!(out.copy.as_deref(), Some("hello"));
        assert!(!out.text_changed);
        assert_eq!(b.as_string(), "hello");
    }

    #[test]
    fn ctrl_x_cuts_content() {
        let mut b = TextBuffer::from_str("cutme");
        let mut input = InputState::new();
        input.push_key(ctrl_key('x'));
        let out = b.apply_input(&input, || None);
        assert_eq!(out.copy.as_deref(), Some("cutme"));
        assert!(out.text_changed);
        assert!(b.is_empty());
    }

    #[test]
    fn control_chars_are_not_inserted() {
        let mut b = TextBuffer::new();
        feed(
            &mut b,
            &[KeyPress {
                code: KeyCode::Char,
                ch: Some('\t'),
                mods: Mods::default(),
            }],
        );
        assert!(b.is_empty());
    }
}
