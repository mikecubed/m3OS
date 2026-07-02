//! Per-frame input state + focus traversal (Phase 105 Track A.3).
//!
//! The toolkit is immediate-mode: the app re-declares its UI every frame.
//! Between frames the compositor's `ServerMessage` stream is folded into
//! an [`InputState`] — the pointer position, per-frame click/scroll
//! edges, held modifiers, and a queue of key presses (with decoded text
//! characters and navigation intents). Widgets consult this state during
//! the frame; the [`Focus`] tracker moves keyboard focus between them.
//!
//! Pure logic: `InputState` is fed by simple setters the render layer
//! calls from decoded events, so the fold + focus + hit-test rules are
//! host-tested without a compositor.

use alloc::vec::Vec;

use crate::geom::Point;

/// Held modifier keys, as of the current frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// A pointer button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// A decoded key press for the current frame. Text-producing keys carry
/// their `ch`; navigation/editing keys carry a [`KeyCode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    pub code: KeyCode,
    /// The character this key produces, if any (already case/shift
    /// resolved by the decoder). `None` for pure navigation keys.
    pub ch: Option<char>,
    pub mods: Mods,
}

/// Editing/navigation key intents the text field and focus router act on.
/// Printable characters arrive via [`KeyPress::ch`]; these are the keys
/// with semantic meaning beyond inserting a glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char,
    Tab,
    Enter,
    Space,
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Escape,
    Other,
}

/// Accumulated input for one frame. Reset the per-frame edges at the top
/// of each frame ([`InputState::begin_frame`]); feed events in between.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    pub pointer: Point,
    /// Pointer buttons pressed *this frame* (rising edge).
    pressed: Vec<MouseButton>,
    /// Pointer buttons released *this frame* (falling edge).
    released: Vec<MouseButton>,
    /// Buttons currently held.
    held: Vec<MouseButton>,
    /// Vertical scroll accumulated this frame (+down).
    pub scroll_y: i32,
    pub mods: Mods,
    /// Key presses queued this frame, in order.
    keys: Vec<KeyPress>,
}

impl InputState {
    pub fn new() -> InputState {
        InputState::default()
    }

    /// Clear the per-frame edges (clicks, releases, scroll, keys) while
    /// preserving continuous state (pointer position, held buttons,
    /// mods). Call at the top of each frame before folding events.
    pub fn begin_frame(&mut self) {
        self.pressed.clear();
        self.released.clear();
        self.scroll_y = 0;
        self.keys.clear();
    }

    // -- event folding (called by the render layer from decoded events) --

    pub fn set_pointer(&mut self, p: Point) {
        self.pointer = p;
    }

    pub fn set_mods(&mut self, mods: Mods) {
        self.mods = mods;
    }

    pub fn press_button(&mut self, b: MouseButton) {
        self.pressed.push(b);
        if !self.held.contains(&b) {
            self.held.push(b);
        }
    }

    pub fn release_button(&mut self, b: MouseButton) {
        self.released.push(b);
        self.held.retain(|&h| h != b);
    }

    pub fn scroll(&mut self, dy: i32) {
        self.scroll_y += dy;
    }

    pub fn push_key(&mut self, key: KeyPress) {
        self.keys.push(key);
    }

    // -- per-frame queries (called by widgets) --

    /// Was `b` pressed this frame?
    pub fn clicked(&self, b: MouseButton) -> bool {
        self.pressed.contains(&b)
    }

    /// Is `b` currently held?
    pub fn is_held(&self, b: MouseButton) -> bool {
        self.held.contains(&b)
    }

    /// Was `b` released this frame?
    pub fn released_this_frame(&self, b: MouseButton) -> bool {
        self.released.contains(&b)
    }

    /// The key presses queued this frame, in order.
    pub fn keys(&self) -> &[KeyPress] {
        &self.keys
    }

    /// True if a focus-advance key (Tab) was pressed this frame; the
    /// `bool` is `true` for reverse (Shift+Tab).
    pub fn tab(&self) -> Option<bool> {
        self.keys
            .iter()
            .find(|k| k.code == KeyCode::Tab)
            .map(|k| k.mods.shift)
    }

    /// True if an activation key (Enter or Space) was pressed this frame.
    pub fn activate(&self) -> bool {
        self.keys
            .iter()
            .any(|k| matches!(k.code, KeyCode::Enter | KeyCode::Space))
    }
}

/// Focus tracker for keyboard traversal. Widgets register themselves in
/// declaration order each frame via [`Focus::next_id`]; the tracker holds
/// which id is focused and advances it on Tab.
#[derive(Debug, Clone, Default)]
pub struct Focus {
    focused: Option<u32>,
    /// Ids registered this frame, in declaration order.
    registered: Vec<u32>,
    next: u32,
}

impl Focus {
    pub fn new() -> Focus {
        Focus::default()
    }

    /// Reset the per-frame id registration (call at frame top).
    pub fn begin_frame(&mut self) {
        self.registered.clear();
        self.next = 0;
    }

    /// Allocate the next focusable widget id (monotonic per frame) and
    /// record it in traversal order.
    pub fn next_id(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        self.registered.push(id);
        // Default focus to the first focusable widget.
        if self.focused.is_none() {
            self.focused = Some(id);
        }
        id
    }

    /// Is `id` currently focused?
    pub fn is_focused(&self, id: u32) -> bool {
        self.focused == Some(id)
    }

    /// Directly focus `id` (e.g. on a pointer click into a widget).
    pub fn set(&mut self, id: u32) {
        self.focused = Some(id);
    }

    /// Advance focus to the next (or previous) registered widget,
    /// wrapping around. No-op when nothing is registered.
    pub fn advance(&mut self, reverse: bool) {
        if self.registered.is_empty() {
            return;
        }
        let n = self.registered.len();
        let cur = self
            .focused
            .and_then(|f| self.registered.iter().position(|&r| r == f));
        let next_idx = match cur {
            Some(i) if reverse => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None => 0,
        };
        self.focused = Some(self.registered[next_idx]);
    }

    /// Apply a Tab press from `input` if present (advance focus).
    pub fn handle_tab(&mut self, input: &InputState) {
        if let Some(reverse) = input.tab() {
            self.advance(reverse);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, ch: Option<char>, mods: Mods) -> KeyPress {
        KeyPress { code, ch, mods }
    }

    #[test]
    fn begin_frame_clears_edges_keeps_continuous() {
        let mut s = InputState::new();
        s.set_pointer(Point::new(5, 6));
        s.press_button(MouseButton::Left);
        s.scroll(3);
        assert!(s.clicked(MouseButton::Left));
        assert!(s.is_held(MouseButton::Left));
        s.begin_frame();
        // Edges cleared, held + pointer preserved.
        assert!(!s.clicked(MouseButton::Left));
        assert!(s.is_held(MouseButton::Left));
        assert_eq!(s.pointer, Point::new(5, 6));
        assert_eq!(s.scroll_y, 0);
    }

    #[test]
    fn release_clears_held() {
        let mut s = InputState::new();
        s.press_button(MouseButton::Left);
        s.release_button(MouseButton::Left);
        assert!(s.released_this_frame(MouseButton::Left));
        assert!(!s.is_held(MouseButton::Left));
    }

    #[test]
    fn tab_and_activate_detected() {
        let mut s = InputState::new();
        s.push_key(key(
            KeyCode::Tab,
            None,
            Mods {
                shift: true,
                ..Default::default()
            },
        ));
        assert_eq!(s.tab(), Some(true));
        s.begin_frame();
        s.push_key(key(KeyCode::Enter, None, Mods::default()));
        assert!(s.activate());
        assert_eq!(s.tab(), None);
    }

    #[test]
    fn focus_defaults_to_first_and_advances_wrapping() {
        let mut f = Focus::new();
        f.begin_frame();
        let a = f.next_id();
        let b = f.next_id();
        let c = f.next_id();
        assert!(f.is_focused(a), "first widget gets default focus");
        f.advance(false);
        assert!(f.is_focused(b));
        f.advance(false);
        assert!(f.is_focused(c));
        f.advance(false);
        assert!(f.is_focused(a), "wraps to first");
        f.advance(true);
        assert!(f.is_focused(c), "reverse wraps to last");
    }

    #[test]
    fn focus_survives_across_frames() {
        let mut f = Focus::new();
        f.begin_frame();
        let _a = f.next_id();
        let b = f.next_id();
        f.advance(false); // focus b
        assert!(f.is_focused(b));
        // Next frame: re-register the same ids; focus should stick to b.
        f.begin_frame();
        let _a2 = f.next_id();
        let b2 = f.next_id();
        assert_eq!(b, b2);
        assert!(f.is_focused(b2));
    }

    #[test]
    fn set_focuses_directly() {
        let mut f = Focus::new();
        f.begin_frame();
        let _a = f.next_id();
        let b = f.next_id();
        f.set(b);
        assert!(f.is_focused(b));
    }

    #[test]
    fn handle_tab_advances_from_input() {
        let mut f = Focus::new();
        f.begin_frame();
        let a = f.next_id();
        let _b = f.next_id();
        let mut input = InputState::new();
        input.push_key(key(KeyCode::Tab, None, Mods::default()));
        f.handle_tab(&input);
        assert!(!f.is_focused(a));
    }
}
