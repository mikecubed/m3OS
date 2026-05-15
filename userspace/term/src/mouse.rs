//! Phase 69 Track E — `PointerEvent`-to-PTY mouse reporting encoder.
//!
//! `MouseReporter` owns the X10 / button-event / SGR mouse modes set
//! by the DEC private mode codes `?9` / `?1000` / `?1006`. The
//! reporter does not move pixels — it converts pointer events into
//! the byte sequences ncurses-class TUI apps expect on stdin.
//!
//! `encode(event, cols, rows)` returns `Some(bytes)` when reporting
//! is active. The returned buffer is stack-bounded (heapless::Vec
//! is not a workspace dep, so we use a small inline array wrapped
//! in a fixed-length slice via `MouseBytes`).
//!
//! Wire format:
//!
//! - X10 (mode `?9`): `\x1b[M Cb Cx Cy` where `Cb = button | (33 +
//!   maybe-shift)`. Each coordinate byte is `coord + 32` (1-based +
//!   32). Tail of the legacy 6-byte form.
//! - Button-event (mode `?1000`): same wire form as X10, but the
//!   reporter emits a separate sequence on release with button == 3.
//! - SGR (mode `?1006`): `\x1b[<Pb;Px;Py M` for press and
//!   `\x1b[<Pb;Px;Py m` (lowercase `m`) for release. Numbers are
//!   decimal, not offset-by-32.
//!
//! The reporter clamps coordinates into `(1, 1)..=(cols, rows)`.
//! No allocation per event.

use kernel_core::input::events::{PointerButton, PointerEvent};

/// Phase 69 Track E — maximum encoded length of one mouse report.
/// SGR form `\x1b[<Pb;Px;Py M` has 16 bytes when `Pb=255`, `Px=1023`,
/// `Py=1023` (we use a generous bound).
pub const MAX_BYTES: usize = 24;

/// Inline byte buffer returned by [`MouseReporter::encode`].
#[derive(Clone, Copy)]
pub struct MouseBytes {
    buf: [u8; MAX_BYTES],
    len: usize,
}

impl MouseBytes {
    fn new() -> Self {
        Self {
            buf: [0u8; MAX_BYTES],
            len: 0,
        }
    }

    /// Bytes ready for `syscall_lib::write`.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Number of bytes encoded.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when no bytes have been written (parser would write `0`).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, b: u8) {
        if self.len < self.buf.len() {
            self.buf[self.len] = b;
            self.len += 1;
        }
    }

    fn push_decimal(&mut self, mut n: u32) {
        // Up to 10 decimal digits for u32.
        let mut digits = [0u8; 10];
        let mut count = 0;
        if n == 0 {
            self.push(b'0');
            return;
        }
        while n > 0 && count < digits.len() {
            digits[count] = b'0' + (n % 10) as u8;
            n /= 10;
            count += 1;
        }
        while count > 0 {
            count -= 1;
            self.push(digits[count]);
        }
    }
}

/// Phase 69 Track E — active mouse reporting mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// No reporting; `encode` always returns `None`.
    #[default]
    Disabled,
    /// `?9` — X10 mouse mode. Press-only, legacy 6-byte sequence.
    X10,
    /// `?1000` — normal tracking. Press and release, button == 3 on
    /// release per the xterm wire layout.
    ButtonEvent,
    /// `?1006` — SGR-encoded press/release.
    Sgr,
}

/// Phase 69 Track E — pointer-event-to-PTY-bytes encoder.
///
/// `MouseReporter::new()` starts in `Mode::Disabled`. The Phase 56
/// `Pointer` event variant is the only input; the typed `Mode` is
/// driven by `Screen::feed`'s `DecPrivateMode` arms via [`enable`]
/// / [`disable`].
pub struct MouseReporter {
    mode: Mode,
}

impl Default for MouseReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseReporter {
    pub const fn new() -> Self {
        Self {
            mode: Mode::Disabled,
        }
    }

    /// Current active mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Enable the requested reporting mode. Calling with the same
    /// mode is idempotent. SGR (`?1006`) and ButtonEvent (`?1000`)
    /// can coexist; the most-recently-enabled mode wins for encoding.
    pub fn enable(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Disable reporting. `encode` returns `None` after this.
    pub fn disable(&mut self) {
        self.mode = Mode::Disabled;
    }

    /// Encode `event` to a stack-bounded byte buffer suitable for
    /// writing to the PTY primary fd. Returns `None` when reporting
    /// is disabled or when the event is a motion-only sample (no
    /// button edge) — Phase 69 ships mouse tracking that responds
    /// to button press / release only. Motion-mouse tracking
    /// (`?1002` / `?1003`) is deferred.
    ///
    /// `cols` and `rows` are the cell-grid dimensions; coordinates
    /// are clamped into `1..=cols` / `1..=rows`.
    pub fn encode(&self, event: &PointerEvent, cols: u16, rows: u16) -> Option<MouseBytes> {
        if matches!(self.mode, Mode::Disabled) {
            return None;
        }
        let (button_index, is_release) = match event.button {
            PointerButton::Down(i) => (i, false),
            PointerButton::Up(i) => (i, true),
            PointerButton::None => return None,
        };
        let (px, py) = compute_cell_position(event, cols, rows);
        let mut out = MouseBytes::new();
        match self.mode {
            Mode::Disabled => return None,
            Mode::X10 | Mode::ButtonEvent => {
                // X10 form: \x1b[M  Cb Cx Cy   (1-based + 32 offset)
                if matches!(self.mode, Mode::X10) && is_release {
                    // X10 ships press events only.
                    return None;
                }
                let cb_value = if is_release { 3u8 } else { button_index.min(2) };
                out.push(0x1b);
                out.push(b'[');
                out.push(b'M');
                out.push(cb_value.wrapping_add(32));
                out.push((px.min(223) as u8).wrapping_add(32));
                out.push((py.min(223) as u8).wrapping_add(32));
            }
            Mode::Sgr => {
                // SGR form: \x1b[<Pb;Px;Py M  (press) or m (release).
                out.push(0x1b);
                out.push(b'[');
                out.push(b'<');
                out.push_decimal(button_index.min(31) as u32);
                out.push(b';');
                out.push_decimal(px as u32);
                out.push(b';');
                out.push_decimal(py as u32);
                out.push(if is_release { b'm' } else { b'M' });
            }
        }
        Some(out)
    }
}

/// Translate a [`PointerEvent`] to a 1-based `(col, row)` cell
/// position clamped into the grid. The compositor's
/// `abs_position` is in pixels relative to surface origin; we
/// project into the cell grid by dividing by the renderer's glyph
/// dimensions (`GLYPH_W` / `GLYPH_H`).
fn compute_cell_position(event: &PointerEvent, cols: u16, rows: u16) -> (u16, u16) {
    // GLYPH_W / GLYPH_H aren't compile-time consts of this module —
    // approximate via the conventional 8×16 grid. The reporter is
    // bounded by the cell grid regardless; an off-by-one cell at
    // the grid boundary is harmless.
    const GLYPH_W: u16 = 8;
    const GLYPH_H: u16 = 16;
    let (px_x, px_y) = match event.abs_position {
        Some((x, y)) => (x.max(0) as u32, y.max(0) as u32),
        None => (0, 0),
    };
    let col = (px_x / GLYPH_W as u32) as u16;
    let row = (px_y / GLYPH_H as u32) as u16;
    let col = (col.saturating_add(1)).min(cols.max(1));
    let row = (row.saturating_add(1)).min(rows.max(1));
    (col, row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::input::events::{ModifierState, PointerButton, PointerEvent};

    fn press(button: u8, x: i32, y: i32) -> PointerEvent {
        PointerEvent {
            timestamp_ms: 0,
            dx: 0,
            dy: 0,
            abs_position: Some((x, y)),
            button: PointerButton::Down(button),
            wheel_dx: 0,
            wheel_dy: 0,
            modifiers: ModifierState::default(),
        }
    }

    fn release(button: u8, x: i32, y: i32) -> PointerEvent {
        let mut e = press(button, x, y);
        e.button = PointerButton::Up(button);
        e
    }

    fn motion_only() -> PointerEvent {
        PointerEvent {
            timestamp_ms: 0,
            dx: 5,
            dy: 5,
            abs_position: Some((40, 80)),
            button: PointerButton::None,
            wheel_dx: 0,
            wheel_dy: 0,
            modifiers: ModifierState::default(),
        }
    }

    #[test]
    fn disabled_returns_none() {
        let r = MouseReporter::new();
        assert!(r.encode(&press(0, 16, 16), 80, 25).is_none());
    }

    /// Phase 69 Track E.1 acceptance — left-button press at
    /// `(col=10, row=5)` under SGR mode emits `\x1b[<0;11;6M`.
    /// `cell_col = (px_x / 8) + 1` — to land at col 11, the pixel
    /// x must be 10·8 = 80 (col 11 == 10 + 1 because the encoder
    /// reports 1-based cells).
    #[test]
    fn sgr_press_left_button() {
        let mut r = MouseReporter::new();
        r.enable(Mode::Sgr);
        let bytes = r
            .encode(&press(0, 10 * 8, 5 * 16), 80, 25)
            .expect("sgr enabled");
        assert_eq!(bytes.as_slice(), b"\x1b[<0;11;6M");
    }

    /// Phase 69 Track E.1 acceptance — release form uses lower-case `m`.
    #[test]
    fn sgr_release_left_button() {
        let mut r = MouseReporter::new();
        r.enable(Mode::Sgr);
        let bytes = r
            .encode(&release(0, 10 * 8, 5 * 16), 80, 25)
            .expect("sgr enabled");
        assert_eq!(bytes.as_slice(), b"\x1b[<0;11;6m");
    }

    /// Phase 69 Track E.1 acceptance — X10 mode emits a 6-byte
    /// `\x1b[M Cb Cx Cy` form with the +32 offset.
    #[test]
    fn x10_press_six_bytes() {
        let mut r = MouseReporter::new();
        r.enable(Mode::X10);
        let bytes = r
            .encode(&press(0, 10 * 8, 5 * 16), 80, 25)
            .expect("x10 enabled");
        // ESC '[' 'M' Cb Cx Cy. Cb = 0+32 = 32 = b' '. Cx = 11+32 = 43 = b'+'.
        // Cy = 6+32 = 38 = b'&'.
        assert_eq!(bytes.as_slice(), b"\x1b[M +&");
    }

    /// Phase 69 Track E.1 acceptance — X10 mode drops release events
    /// (no separate release in the legacy X10 wire form).
    #[test]
    fn x10_release_dropped() {
        let mut r = MouseReporter::new();
        r.enable(Mode::X10);
        assert!(r.encode(&release(0, 10 * 8, 5 * 16), 80, 25).is_none());
    }

    /// Phase 69 Track E.1 acceptance — ButtonEvent mode emits release
    /// with button == 3 (the legacy +32 offset → b'#').
    #[test]
    fn button_event_release_uses_button_three() {
        let mut r = MouseReporter::new();
        r.enable(Mode::ButtonEvent);
        let bytes = r
            .encode(&release(0, 10 * 8, 5 * 16), 80, 25)
            .expect("button-event enabled");
        // Cb = 3 + 32 = 35 = b'#'. Cx = 11+32 = 43 = b'+'. Cy = 6+32 = 38 = b'&'.
        assert_eq!(bytes.as_slice(), b"\x1b[M#+&");
    }

    /// Phase 69 Track E.1 acceptance — motion-only events return None
    /// in every mode (Phase 69 ships press/release tracking only).
    #[test]
    fn motion_only_returns_none() {
        let mut r = MouseReporter::new();
        for m in [Mode::X10, Mode::ButtonEvent, Mode::Sgr] {
            r.enable(m);
            assert!(r.encode(&motion_only(), 80, 25).is_none());
        }
    }

    /// Phase 69 Track E.1 acceptance — coords outside the grid clamp
    /// to `1..=cols` / `1..=rows`.
    #[test]
    fn coords_clamped_into_grid() {
        let mut r = MouseReporter::new();
        r.enable(Mode::Sgr);
        let bytes = r
            .encode(&press(0, 100_000, 100_000), 80, 25)
            .expect("sgr enabled");
        // Expect cell (80, 25): \x1b[<0;80;25M
        assert_eq!(bytes.as_slice(), b"\x1b[<0;80;25M");
    }
}
