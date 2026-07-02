//! The immediate-mode `Ui` context + widgets (Phase 105 Track A.4/A.6).
//!
//! Each frame the app builds a [`Ui`] against a [`Painter`], the folded
//! [`InputState`], a persistent [`Focus`] tracker, and a [`Theme`], then
//! declares widgets. A widget carves its [`Rect`] from a vertical cursor
//! (or an explicit rect via the `*_at` methods / [`Ui::split_row`]),
//! computes its interaction from the pointer + keyboard, draws itself,
//! and returns a [`Response`]. Because everything is generic over
//! `Painter`, the whole widget layer is host-tested with
//! [`crate::paint::RecordingPainter`].

use alloc::string::String;
use alloc::vec::Vec;

use crate::geom::Rect;
use crate::input::{Focus, InputState, KeyCode, MouseButton};
use crate::layout::{Dir, Item, LayoutSpec, solve};
use crate::paint::Painter;
use crate::text_edit::TextBuffer;
use crate::theme::{Theme, Visual};

/// The result of declaring a widget this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Response {
    pub rect: Rect,
    /// Pointer is over the widget.
    pub hovered: bool,
    /// Activated this frame (pointer click inside, or Enter/Space while
    /// focused).
    pub clicked: bool,
    /// Widget holds keyboard focus.
    pub focused: bool,
    /// The widget's value changed this frame (text_field / checkbox /
    /// slider).
    pub changed: bool,
}

/// Immediate-mode UI context for one frame.
pub struct Ui<'a, P: Painter> {
    painter: &'a mut P,
    input: &'a InputState,
    focus: &'a mut Focus,
    theme: &'a Theme,
    /// The container region widgets are placed within.
    bounds: Rect,
    /// Current y offset (column cursor) within `bounds`.
    cursor_y: i32,
    /// Optional clipboard fetch for text-field paste (render layer wires
    /// it to the compositor; tests pass `None`).
    clipboard: Option<&'a dyn Fn() -> Option<String>>,
}

impl<'a, P: Painter> Ui<'a, P> {
    /// Start a frame laid out as a top-to-bottom column inside `bounds`.
    /// Paints the window background first.
    pub fn new(
        painter: &'a mut P,
        input: &'a InputState,
        focus: &'a mut Focus,
        theme: &'a Theme,
        bounds: Rect,
    ) -> Ui<'a, P> {
        painter.fill_rect(bounds, theme.window_bg);
        Ui {
            painter,
            input,
            focus,
            theme,
            bounds,
            cursor_y: bounds.y + theme.pad_y,
            clipboard: None,
        }
    }

    /// Provide a clipboard source for text-field paste (Ctrl+V).
    pub fn with_clipboard(mut self, cb: &'a dyn Fn() -> Option<String>) -> Ui<'a, P> {
        self.clipboard = Some(cb);
        self
    }

    pub fn theme(&self) -> &Theme {
        self.theme
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Direct painter access for app-drawn chrome (title bars, custom
    /// pixels) outside the widget set.
    pub fn painter(&mut self) -> &mut P {
        self.painter
    }

    /// Carve a full-width row `height` px tall from the column cursor and
    /// advance past it (plus theme spacing). The row is inset by the
    /// theme's horizontal padding.
    pub fn allocate_row(&mut self, height: i32) -> Rect {
        let x = self.bounds.x + self.theme.pad_x;
        let w = (self.bounds.w - 2 * self.theme.pad_x).max(0);
        let r = Rect::new(x, self.cursor_y, w, height);
        self.cursor_y += height + self.theme.spacing;
        r
    }

    /// Split a freshly-allocated row of `height` into cells per `items`
    /// (using the pure layout solver), for horizontal widget groups.
    pub fn split_row(&mut self, height: i32, items: &[Item]) -> Vec<Rect> {
        let row = self.allocate_row(height);
        let spec = LayoutSpec::new(Dir::Row, row).spacing(self.theme.spacing);
        solve(&spec, items)
    }

    // -- interaction helper shared by focusable widgets -------------------

    /// Register a focusable widget occupying `rect`, returning
    /// `(id, hovered, activated)`. A pointer click inside grabs focus.
    fn interact(&mut self, rect: Rect) -> (u32, bool, bool) {
        let id = self.focus.next_id();
        let hovered = rect.contains(self.input.pointer);
        let pointer_click = hovered && self.input.clicked(MouseButton::Left);
        if pointer_click {
            self.focus.set(id);
        }
        let key_activate = self.focus.is_focused(id) && self.input.activate();
        (id, hovered, pointer_click || key_activate)
    }

    fn visual(&self, id: u32, hovered: bool) -> Visual {
        if self.input.is_held(MouseButton::Left) && hovered {
            Visual::Active
        } else if hovered {
            Visual::Hovered
        } else if self.focus.is_focused(id) {
            Visual::Focused
        } else {
            Visual::Normal
        }
    }

    /// Draw `text` vertically centered within `rect`, left-aligned after
    /// `pad_x`, clipped to `rect`.
    fn draw_label_in(&mut self, rect: Rect, text: &str, color: crate::geom::Color) {
        let th = self.painter.text_height();
        let ty = rect.y + (rect.h - th) / 2;
        let tx = rect.x + self.theme.pad_x;
        self.painter.clip_push(rect);
        self.painter.text(tx, ty, text, color);
        self.painter.clip_pop();
    }

    // -- widgets (cursor sugar) -------------------------------------------

    /// A non-interactive text label on its own row.
    pub fn label(&mut self, text: &str) -> Response {
        let rect = self.allocate_row(self.theme.row_height);
        self.label_at(rect, text)
    }

    /// A clickable button on its own row.
    pub fn button(&mut self, text: &str) -> Response {
        let rect = self.allocate_row(self.theme.row_height);
        self.button_at(rect, text)
    }

    /// A labelled checkbox bound to `value`.
    pub fn checkbox(&mut self, label: &str, value: &mut bool) -> Response {
        let rect = self.allocate_row(self.theme.row_height);
        self.checkbox_at(rect, label, value)
    }

    /// An editable single-line text field bound to `buf`.
    pub fn text_field(&mut self, buf: &mut TextBuffer) -> Response {
        let rect = self.allocate_row(self.theme.row_height);
        self.text_field_at(rect, buf)
    }

    /// A selectable list row (`selected` highlights it).
    pub fn selectable(&mut self, text: &str, selected: bool) -> Response {
        let rect = self.allocate_row(self.theme.row_height);
        self.selectable_at(rect, text, selected)
    }

    /// A horizontal slider bound to `value` in `[min, max]`.
    pub fn slider(&mut self, value: &mut i32, min: i32, max: i32) -> Response {
        let rect = self.allocate_row(self.theme.row_height);
        self.slider_at(rect, value, min, max)
    }

    /// A thin horizontal separator line.
    pub fn separator(&mut self) {
        let rect = self.allocate_row(self.theme.spacing.max(2));
        let y = rect.y + rect.h / 2;
        let line = Rect::new(rect.x, y, rect.w, self.theme.border.max(1));
        self.painter.fill_rect(line, self.theme.separator);
    }

    // -- widgets (explicit rect) ------------------------------------------

    pub fn label_at(&mut self, rect: Rect, text: &str) -> Response {
        let color = self.theme.text;
        self.draw_label_in(rect, text, color);
        Response {
            rect,
            ..Default::default()
        }
    }

    pub fn button_at(&mut self, rect: Rect, text: &str) -> Response {
        let (id, hovered, clicked) = self.interact(rect);
        let visual = self.visual(id, hovered);
        let bg = self.theme.button_color(visual);
        self.painter.fill_rect(rect, bg);
        // Focus ring / border.
        let border_color = if self.focus.is_focused(id) {
            self.theme.focus_ring
        } else {
            self.theme.button_border
        };
        self.painter
            .stroke_rect(rect, border_color, self.theme.border.max(1));
        // Centered label.
        let tw = self.painter.text_width(text);
        let th = self.painter.text_height();
        let tx = rect.x + (rect.w - tw) / 2;
        let ty = rect.y + (rect.h - th) / 2;
        self.painter.clip_push(rect);
        self.painter.text(tx, ty, text, self.theme.text);
        self.painter.clip_pop();
        Response {
            rect,
            hovered,
            clicked,
            focused: self.focus.is_focused(id),
            changed: false,
        }
    }

    pub fn checkbox_at(&mut self, rect: Rect, label: &str, value: &mut bool) -> Response {
        let (id, hovered, clicked) = self.interact(rect);
        if clicked {
            *value = !*value;
        }
        // Box on the left, label after it.
        let box_sz = (rect.h - 2 * self.theme.pad_y).max(8);
        let box_rect = Rect::new(rect.x, rect.y + (rect.h - box_sz) / 2, box_sz, box_sz);
        self.painter.fill_rect(box_rect, self.theme.field_bg);
        let border = if self.focus.is_focused(id) {
            self.theme.focus_ring
        } else {
            self.theme.field_border
        };
        self.painter
            .stroke_rect(box_rect, border, self.theme.border.max(1));
        if *value {
            self.painter.fill_rect(box_rect.inset(3), self.theme.accent);
        }
        let th = self.painter.text_height();
        let ty = rect.y + (rect.h - th) / 2;
        let tx = box_rect.right() + self.theme.pad_x;
        self.painter.text(tx, ty, label, self.theme.text);
        Response {
            rect,
            hovered,
            clicked,
            focused: self.focus.is_focused(id),
            changed: clicked,
        }
    }

    pub fn text_field_at(&mut self, rect: Rect, buf: &mut TextBuffer) -> Response {
        let (id, hovered, _clicked) = self.interact(rect);
        let focused = self.focus.is_focused(id);
        let mut changed = false;
        if focused {
            let cb = self.clipboard;
            changed = buf.apply_input(self.input, || cb.and_then(|f| f()));
        }
        // Field chrome.
        self.painter.fill_rect(rect, self.theme.field_bg);
        let border = if focused {
            self.theme.focus_ring
        } else {
            self.theme.field_border
        };
        self.painter
            .stroke_rect(rect, border, self.theme.border.max(1));
        // Text + cursor.
        let text = buf.as_string();
        let th = self.painter.text_height();
        let ty = rect.y + (rect.h - th) / 2;
        let tx = rect.x + self.theme.pad_x;
        self.painter.clip_push(rect);
        self.painter.text(tx, ty, &text, self.theme.text);
        if focused {
            // Cursor x = text width up to the cursor char index.
            let prefix: String = text.chars().take(buf.cursor()).collect();
            let cx = tx + self.painter.text_width(&prefix);
            let cursor_rect = Rect::new(cx, ty, self.theme.border.max(1), th);
            self.painter.fill_rect(cursor_rect, self.theme.cursor);
        }
        self.painter.clip_pop();
        Response {
            rect,
            hovered,
            clicked: false,
            focused,
            changed,
        }
    }

    pub fn selectable_at(&mut self, rect: Rect, text: &str, selected: bool) -> Response {
        let (id, hovered, clicked) = self.interact(rect);
        let bg = if selected {
            self.theme.selection_bg
        } else if hovered {
            self.theme.button_bg_hover
        } else {
            self.theme.panel_bg
        };
        self.painter.fill_rect(rect, bg);
        if self.focus.is_focused(id) {
            self.painter
                .stroke_rect(rect, self.theme.focus_ring, self.theme.border.max(1));
        }
        let color = self.theme.text;
        self.draw_label_in(rect, text, color);
        Response {
            rect,
            hovered,
            clicked,
            focused: self.focus.is_focused(id),
            changed: false,
        }
    }

    pub fn slider_at(&mut self, rect: Rect, value: &mut i32, min: i32, max: i32) -> Response {
        let (id, hovered, _clicked) = self.interact(rect);
        let focused = self.focus.is_focused(id);
        let span = (max - min).max(1);
        let mut changed = false;

        // Pointer drag sets the value from the x position within the track.
        if hovered && self.input.is_held(MouseButton::Left) && rect.w > 0 {
            let frac = (self.input.pointer.x - rect.x).clamp(0, rect.w) as i64;
            let nv = min + ((frac * span as i64) / rect.w as i64) as i32;
            if nv != *value {
                *value = nv;
                changed = true;
            }
        }
        // Keyboard: left/right nudges when focused.
        if focused {
            for key in self.input.keys() {
                let step = match key.code {
                    KeyCode::Left => -1,
                    KeyCode::Right => 1,
                    _ => 0,
                };
                if step != 0 {
                    let nv = (*value + step).clamp(min, max);
                    if nv != *value {
                        *value = nv;
                        changed = true;
                    }
                }
            }
        }
        let v = (*value).clamp(min, max);

        // Track + filled portion + knob.
        let track_h = 4.max(rect.h / 4);
        let track = Rect::new(rect.x, rect.y + (rect.h - track_h) / 2, rect.w, track_h);
        self.painter.fill_rect(track, self.theme.field_bg);
        let filled_w = (((v - min) as i64) * rect.w as i64 / span as i64) as i32;
        let filled = Rect::new(track.x, track.y, filled_w, track_h);
        self.painter.fill_rect(filled, self.theme.accent);
        let knob_x = rect.x + filled_w - rect.h / 4;
        let knob = Rect::new(knob_x, rect.y, rect.h / 2, rect.h);
        let knob_color = if focused {
            self.theme.focus_ring
        } else {
            self.theme.button_border
        };
        self.painter.fill_rect(knob, knob_color);
        Response {
            rect,
            hovered,
            clicked: false,
            focused,
            changed,
        }
    }

    /// Advance focus for next frame based on this frame's Tab presses.
    /// Call once after declaring all widgets.
    pub fn end(self) {
        self.focus.handle_tab(self.input);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use crate::input::{KeyPress, Mods};
    use crate::paint::{DrawOp, RecordingPainter};

    struct Harness {
        painter: RecordingPainter,
        input: InputState,
        focus: Focus,
        theme: Theme,
        bounds: Rect,
    }

    impl Harness {
        fn new() -> Harness {
            Harness {
                painter: RecordingPainter::new(),
                input: InputState::new(),
                focus: Focus::new(),
                theme: Theme::dark(),
                bounds: Rect::new(0, 0, 200, 300),
            }
        }

        fn begin(&mut self) {
            self.input.begin_frame();
            self.focus.begin_frame();
            self.painter.ops.clear();
        }
    }

    #[test]
    fn button_click_by_pointer() {
        let mut h = Harness::new();
        // First declare once to learn the button's rect.
        h.begin();
        let rect = {
            let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
            ui.button("OK").rect
        };
        // Next frame: pointer inside + left click → clicked.
        h.begin();
        h.input.set_pointer(Point::new(rect.x + 5, rect.y + 5));
        h.input.press_button(MouseButton::Left);
        let clicked = {
            let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
            ui.button("OK").clicked
        };
        assert!(clicked);
        assert!(h.painter.drew_text("OK"));
    }

    #[test]
    fn button_not_clicked_when_pointer_outside() {
        let mut h = Harness::new();
        h.begin();
        h.input.set_pointer(Point::new(500, 500));
        h.input.press_button(MouseButton::Left);
        let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
        assert!(!ui.button("OK").clicked);
    }

    #[test]
    fn keyboard_activates_focused_button() {
        let mut h = Harness::new();
        h.begin();
        // The first focusable widget gets default focus; Enter activates.
        h.input.push_key(KeyPress {
            code: KeyCode::Enter,
            ch: None,
            mods: Mods::default(),
        });
        let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
        assert!(
            ui.button("Go").clicked,
            "Enter on the focused button clicks it"
        );
    }

    #[test]
    fn checkbox_toggles_on_click() {
        let mut h = Harness::new();
        let mut value = false;
        // Learn rect.
        h.begin();
        let rect = {
            let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
            ui.checkbox("on", &mut value).rect
        };
        assert!(!value);
        h.begin();
        h.input.set_pointer(Point::new(rect.x + 3, rect.y + 3));
        h.input.press_button(MouseButton::Left);
        {
            let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
            let r = ui.checkbox("on", &mut value);
            assert!(r.changed);
        }
        assert!(value, "checkbox flipped true");
    }

    #[test]
    fn text_field_types_into_focused() {
        let mut h = Harness::new();
        let mut buf = TextBuffer::new();
        h.begin();
        // Field is the only (thus focused) widget; type "hi".
        h.input.push_key(KeyPress {
            code: KeyCode::Char,
            ch: Some('h'),
            mods: Mods::default(),
        });
        h.input.push_key(KeyPress {
            code: KeyCode::Char,
            ch: Some('i'),
            mods: Mods::default(),
        });
        {
            let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
            let r = ui.text_field(&mut buf);
            assert!(r.changed);
            assert!(r.focused);
        }
        assert_eq!(buf.as_string(), "hi");
    }

    #[test]
    fn slider_keyboard_nudges_focused() {
        let mut h = Harness::new();
        let mut v = 50;
        h.begin();
        h.input.push_key(KeyPress {
            code: KeyCode::Right,
            ch: None,
            mods: Mods::default(),
        });
        {
            let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
            let r = ui.slider(&mut v, 0, 100);
            assert!(r.changed);
        }
        assert_eq!(v, 51);
    }

    #[test]
    fn tab_advances_focus_between_frames() {
        let mut h = Harness::new();
        // Frame 1: two buttons; press Tab; end() advances focus.
        h.begin();
        h.input.push_key(KeyPress {
            code: KeyCode::Tab,
            ch: None,
            mods: Mods::default(),
        });
        let (r0, r1) = {
            let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
            let a = ui.button("A");
            let b = ui.button("B");
            let pair = (a.focused, b.focused);
            ui.end();
            pair
        };
        assert!(r0, "first button focused by default");
        assert!(!r1);
        // Frame 2: focus should have advanced to the second button.
        h.begin();
        let (f0, f1) = {
            let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
            let a = ui.button("A");
            let b = ui.button("B");
            (a.focused, b.focused)
        };
        assert!(!f0);
        assert!(f1, "Tab moved focus to the second button");
    }

    #[test]
    fn split_row_uses_solver() {
        let mut h = Harness::new();
        h.begin();
        let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
        let cells = ui.split_row(24, &[Item::flex(1), Item::flex(1)]);
        assert_eq!(cells.len(), 2);
        // Two equal cells side by side within the padded row.
        assert_eq!(cells[0].y, cells[1].y);
        assert!(cells[1].x > cells[0].x);
    }

    #[test]
    fn window_background_painted_first() {
        let mut h = Harness::new();
        h.begin();
        let mut ui = Ui::new(&mut h.painter, &h.input, &mut h.focus, &h.theme, h.bounds);
        ui.label("hi");
        assert!(matches!(
            h.painter.ops.first(),
            Some(DrawOp::FillRect { color, .. }) if *color == h.theme.window_bg
        ));
    }
}
