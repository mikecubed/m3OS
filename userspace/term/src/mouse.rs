//! Phase 69 Track E — `PointerEvent`-to-PTY mouse reporting encoder.
//!
//! `MouseReporter` owns the X10 / button-event / motion / SGR mouse
//! state set by the DEC private mode codes `?9` / `?1000` / `?1002`
//! / `?1003` / `?1006`. Tracking-mode (which events get reported)
//! and encoding-mode (the wire format) are stored separately, the
//! way xterm models them: `?1006l` only reverts the encoding back
//! to the legacy form and `?1000l` only disables tracking. The
//! reporter does not move pixels — it converts pointer events into
//! the byte sequences ncurses-class TUI apps expect on stdin.
//!
//! `encode(event, cols, rows)` returns `Some(bytes)` when tracking
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
/// Phase 112 Track A.4 — what the main loop should do with one pointer
/// event, as decided by [`MouseReporter::classify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerAction {
    /// Forward these bytes to the PTY — the application is tracking the
    /// mouse and this was a button edge.
    Report(MouseBytes),
    /// Move `term`'s own scrollback viewport by this many rows; positive
    /// is toward older history.
    ScrollView(isize),
    /// Nothing to do (motion-only sample, or a wheel notch while the app
    /// holds the mouse).
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Phase 69 Track E — tracking-mode state (which pointer-event
/// transitions get reported). xterm models this as orthogonal to
/// encoding-mode: `?9` / `?1000` / `?1002` / `?1003` flip tracking,
/// `?1006` flips encoding. We keep the two pieces of state separate
/// so `?1006l` only changes the wire format and `?1000l` only
/// disables tracking — earlier revisions stored a single `Mode` and
/// `?1006l` therefore disabled the whole mouse stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrackingMode {
    /// No reporting; `encode` returns `None` regardless of encoding.
    #[default]
    Disabled,
    /// `?9` — X10 mouse mode. Press only.
    X10,
    /// `?1000` — normal tracking. Press + release.
    Normal,
    /// `?1002` — button-event tracking (motion while a button is
    /// held). Phase 69 treats motion as deferred, so this currently
    /// behaves like [`TrackingMode::Normal`]: press + release are
    /// reported, intra-button motion is dropped. Kept as its own
    /// variant so the deferred motion work has a name to switch on.
    ButtonMotion,
    /// `?1003` — any-event tracking. Same caveat as
    /// [`TrackingMode::ButtonMotion`].
    AnyEvent,
}

/// Phase 69 Track E — encoding-mode state (the wire format used for
/// each reported transition). Independent of [`TrackingMode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EncodingMode {
    /// xterm legacy form: `\x1b[M Cb Cx Cy` with the `+32` offset.
    /// Press events emit the button index; release events emit `3`
    /// (Normal / ButtonMotion / AnyEvent), or are dropped entirely
    /// (X10).
    #[default]
    Legacy,
    /// `?1006` SGR-encoded form: `\x1b[<Pb;Px;Py M` (press) /
    /// `\x1b[<Pb;Px;Py m` (release). Decimal coordinates, no offset.
    Sgr,
}

/// Legacy single-axis mode kept for callers that drive the reporter
/// with a single `enable(Mode)` call (tests and the `tui-smoke`
/// binary). Each variant maps to a concrete `(tracking, encoding)`
/// pair via [`MouseReporter::enable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// No reporting.
    #[default]
    Disabled,
    /// X10 tracking + Legacy encoding.
    X10,
    /// Normal tracking + Legacy encoding.
    ButtonEvent,
    /// Normal tracking + SGR encoding (the typical xterm-style combo).
    Sgr,
}

/// Phase 69 Track E — pointer-event-to-PTY-bytes encoder.
///
/// `MouseReporter::new()` starts with both tracking and encoding
/// disabled / default. The Phase 56 `Pointer` event variant is the
/// only input. `Screen::feed`'s `DecPrivateMode` arms route mode
/// changes through [`set_tracking`] / [`set_encoding`] (or, for the
/// single-axis legacy callers, [`enable`] / [`disable`]).
pub struct MouseReporter {
    tracking: TrackingMode,
    encoding: EncodingMode,
}

impl Default for MouseReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseReporter {
    pub const fn new() -> Self {
        Self {
            tracking: TrackingMode::Disabled,
            encoding: EncodingMode::Legacy,
        }
    }

    /// Current tracking mode (which events get reported).
    pub fn tracking(&self) -> TrackingMode {
        self.tracking
    }

    /// Current encoding mode (the wire format for reports).
    pub fn encoding(&self) -> EncodingMode {
        self.encoding
    }

    /// Derive the legacy [`Mode`] view from the current
    /// `(tracking, encoding)` pair. Surface area kept for callers
    /// that still think in terms of the pre-split single mode.
    pub fn mode(&self) -> Mode {
        match (self.tracking, self.encoding) {
            (TrackingMode::Disabled, _) => Mode::Disabled,
            (TrackingMode::X10, _) => Mode::X10,
            (_, EncodingMode::Sgr) => Mode::Sgr,
            (_, EncodingMode::Legacy) => Mode::ButtonEvent,
        }
    }

    /// Flip the tracking-mode independently of the encoding. The
    /// encoding stays whatever was last set, so the common xterm
    /// pattern `?1000h ?1006h` (enable button-event tracking, then
    /// enable SGR encoding) lands on `(Normal, Sgr)` and a later
    /// `?1006l` reverts to `(Normal, Legacy)` without disabling
    /// tracking.
    pub fn set_tracking(&mut self, tracking: TrackingMode) {
        self.tracking = tracking;
    }

    /// Flip the encoding-mode independently of the tracking.
    /// `?1006l` reverts to [`EncodingMode::Legacy`] but leaves
    /// tracking enabled if it was; this is the bug the Phase 69
    /// review surfaced.
    pub fn set_encoding(&mut self, encoding: EncodingMode) {
        self.encoding = encoding;
    }

    /// Legacy single-axis API kept for callers that still think in
    /// terms of one mode. Maps the requested [`Mode`] onto a
    /// concrete `(tracking, encoding)` pair and writes both fields.
    pub fn enable(&mut self, mode: Mode) {
        let (t, e) = match mode {
            Mode::Disabled => (TrackingMode::Disabled, EncodingMode::Legacy),
            Mode::X10 => (TrackingMode::X10, EncodingMode::Legacy),
            Mode::ButtonEvent => (TrackingMode::Normal, EncodingMode::Legacy),
            Mode::Sgr => (TrackingMode::Normal, EncodingMode::Sgr),
        };
        self.tracking = t;
        self.encoding = e;
    }

    /// Phase 112 Track A.4 — decide what one pointer event should do.
    ///
    /// This is the whole pointer policy in one host-testable place, so the
    /// binary's event loop stays a three-arm match. The order matters:
    ///
    /// 1. An application that has grabbed the mouse gets button events
    ///    reported to it, exactly as before Phase 112.
    /// 2. Otherwise a wheel event scrolls `term`'s own scrollback
    ///    viewport (`wheel_dy` positive = wheel-up = older history).
    /// 3. Anything else is ignored.
    ///
    /// `wheel_rows` is the rows-per-notch step the caller wants.
    pub fn classify(
        &self,
        event: &PointerEvent,
        cols: u16,
        rows: u16,
        wheel_rows: isize,
    ) -> PointerAction {
        if let Some(bytes) = self.encode(event, cols, rows) {
            return PointerAction::Report(bytes);
        }
        // The wheel drives the viewport only when the app has not grabbed
        // the mouse — the xterm convention. When it has, a wheel notch is
        // the app's to interpret (and `encode` already declined it, since
        // Phase 69 reports button edges only).
        if !self.tracking_enabled()
            && matches!(event.button, PointerButton::None)
            && event.wheel_dy != 0
        {
            return PointerAction::ScrollView(event.wheel_dy as isize * wheel_rows);
        }
        PointerAction::Ignore
    }

    /// Phase 112 Track A.4 — `true` when the application has grabbed the
    /// mouse (any tracking mode other than [`TrackingMode::Disabled`]).
    ///
    /// [`MouseReporter::encode`] returns `None` both when tracking is off
    /// *and* for a button-less event, so the main loop cannot use its
    /// return value alone to decide whether a wheel event is free to drive
    /// the scrollback viewport. This accessor disambiguates: the wheel
    /// scrolls history only when the app is *not* tracking, matching
    /// xterm.
    pub fn tracking_enabled(&self) -> bool {
        !matches!(self.tracking, TrackingMode::Disabled)
    }

    /// Disable reporting. Resets tracking to [`TrackingMode::Disabled`]
    /// and encoding to its default ([`EncodingMode::Legacy`]).
    pub fn disable(&mut self) {
        self.tracking = TrackingMode::Disabled;
        self.encoding = EncodingMode::Legacy;
    }

    /// Encode `event` to a stack-bounded byte buffer suitable for
    /// writing to the PTY primary fd. Returns `None` when tracking
    /// is disabled or when the event is a motion-only sample (no
    /// button edge) — Phase 69 ships mouse tracking that responds
    /// to button press / release only. True motion-mouse tracking
    /// (`?1002` / `?1003`) is deferred.
    ///
    /// `cols` and `rows` are the cell-grid dimensions; coordinates
    /// are clamped into `1..=cols` / `1..=rows`.
    pub fn encode(&self, event: &PointerEvent, cols: u16, rows: u16) -> Option<MouseBytes> {
        if matches!(self.tracking, TrackingMode::Disabled) {
            return None;
        }
        let (button_index, is_release) = match event.button {
            PointerButton::Down(i) => (i, false),
            PointerButton::Up(i) => (i, true),
            PointerButton::None => return None,
        };
        // X10 ships press events only; release is dropped regardless
        // of the encoding-mode the caller picked.
        if matches!(self.tracking, TrackingMode::X10) && is_release {
            return None;
        }
        let (px, py) = compute_cell_position(event, cols, rows);
        let mut out = MouseBytes::new();
        match self.encoding {
            EncodingMode::Legacy => {
                // X10 form: \x1b[M  Cb Cx Cy   (1-based + 32 offset)
                let cb_value = if is_release { 3u8 } else { button_index.min(2) };
                out.push(0x1b);
                out.push(b'[');
                out.push(b'M');
                out.push(cb_value.wrapping_add(32));
                out.push((px.min(223) as u8).wrapping_add(32));
                out.push((py.min(223) as u8).wrapping_add(32));
            }
            EncodingMode::Sgr => {
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
    // MUST match `term::display::CELL_WIDTH` / `CELL_HEIGHT`.
    // The `display` module is gated behind the `os-binary` feature
    // so we duplicate the literals here; mismatching them would
    // misproject pointer pixels onto the cell grid. The reporter is
    // bounded by the cell grid regardless; an off-by-one cell at
    // the grid boundary is harmless.
    const GLYPH_W: u16 = 16;
    const GLYPH_H: u16 = 32;
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

    // Mirror the literals in `compute_cell_position`. Tests use
    // these so the cell-coord assertions stay readable when the
    // pixel inputs are derived from `cell × CELL_*`.
    const CELL_W_PX: i32 = 16;
    const CELL_H_PX: i32 = 32;

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
    /// `cell_col = (px_x / CELL_W_PX) + 1` — to land at col 11 the
    /// pixel x must be `10 · CELL_W_PX` (col 11 == 10 + 1 because
    /// the encoder reports 1-based cells).
    #[test]
    fn sgr_press_left_button() {
        let mut r = MouseReporter::new();
        r.enable(Mode::Sgr);
        let bytes = r
            .encode(&press(0, 10 * CELL_W_PX, 5 * CELL_H_PX), 80, 25)
            .expect("sgr enabled");
        assert_eq!(bytes.as_slice(), b"\x1b[<0;11;6M");
    }

    /// Phase 69 Track E.1 acceptance — release form uses lower-case `m`.
    #[test]
    fn sgr_release_left_button() {
        let mut r = MouseReporter::new();
        r.enable(Mode::Sgr);
        let bytes = r
            .encode(&release(0, 10 * CELL_W_PX, 5 * CELL_H_PX), 80, 25)
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
            .encode(&press(0, 10 * CELL_W_PX, 5 * CELL_H_PX), 80, 25)
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
        assert!(
            r.encode(&release(0, 10 * CELL_W_PX, 5 * CELL_H_PX), 80, 25)
                .is_none()
        );
    }

    /// Phase 69 Track E.1 acceptance — ButtonEvent mode emits release
    /// with button == 3 (the legacy +32 offset → b'#').
    #[test]
    fn button_event_release_uses_button_three() {
        let mut r = MouseReporter::new();
        r.enable(Mode::ButtonEvent);
        let bytes = r
            .encode(&release(0, 10 * CELL_W_PX, 5 * CELL_H_PX), 80, 25)
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

    /// Phase 69 review-resolution — disabling SGR encoding via
    /// `?1006l` must not turn off tracking. The common xterm idiom
    /// `?1000h ?1006h` then `?1006l` should leave normal tracking
    /// active and revert to legacy encoding.
    #[test]
    fn sgr_encoding_reset_keeps_tracking() {
        let mut r = MouseReporter::new();
        r.set_tracking(TrackingMode::Normal);
        r.set_encoding(EncodingMode::Sgr);
        // ?1006l flips encoding back to Legacy but keeps tracking.
        r.set_encoding(EncodingMode::Legacy);
        assert_eq!(r.tracking(), TrackingMode::Normal);
        assert_eq!(r.encoding(), EncodingMode::Legacy);
        let bytes = r
            .encode(&press(0, 10 * CELL_W_PX, 5 * CELL_H_PX), 80, 25)
            .expect("tracking still active");
        // Expect legacy wire form: ESC '[' 'M' Cb Cx Cy.
        assert_eq!(bytes.as_slice(), b"\x1b[M +&");
    }

    /// Phase 69 review-resolution — disabling tracking via `?1000l`
    /// stops all reports, even when SGR encoding is still set.
    #[test]
    fn tracking_disabled_stops_reports() {
        let mut r = MouseReporter::new();
        r.set_tracking(TrackingMode::Normal);
        r.set_encoding(EncodingMode::Sgr);
        r.set_tracking(TrackingMode::Disabled);
        assert!(
            r.encode(&press(0, 10 * CELL_W_PX, 5 * CELL_H_PX), 80, 25)
                .is_none()
        );
    }

    /// Phase 69 review-resolution — the legacy `enable(Mode::Sgr)`
    /// shim sets tracking to Normal (the xterm-canonical pairing)
    /// so a single-axis caller still gets the expected wire form.
    #[test]
    fn legacy_enable_sgr_pairs_with_normal_tracking() {
        let mut r = MouseReporter::new();
        r.enable(Mode::Sgr);
        assert_eq!(r.tracking(), TrackingMode::Normal);
        assert_eq!(r.encoding(), EncodingMode::Sgr);
        assert_eq!(r.mode(), Mode::Sgr);
    }

    // -----------------------------------------------------------------
    // Phase 112 Track A.4 — wheel → scrollback viewport
    // -----------------------------------------------------------------

    /// A wheel-notch event: no button edge, non-zero `wheel_dy`. This is
    /// the exact shape the `usb-hid` Report-protocol decoder injects
    /// (`PointerEvent { button: None, wheel_dy: p.wheel, .. }`).
    fn wheel(dy: i32) -> PointerEvent {
        PointerEvent {
            timestamp_ms: 0,
            dx: 0,
            dy: 0,
            abs_position: None,
            button: PointerButton::None,
            wheel_dx: 0,
            wheel_dy: dy,
            modifiers: ModifierState(0),
        }
    }

    /// A.4 acceptance: with mouse reporting **off**, a wheel notch maps to
    /// a viewport scroll of `wheel_dy * wheel_rows`, positive = older.
    #[test]
    fn wheel_scrolls_viewport_when_app_is_not_tracking() {
        let r = MouseReporter::new();
        assert!(!r.tracking_enabled(), "reporter starts disabled");

        assert_eq!(
            r.classify(&wheel(1), 80, 25, 3),
            PointerAction::ScrollView(3),
            "wheel-up scrolls toward older history"
        );
        assert_eq!(
            r.classify(&wheel(-1), 80, 25, 3),
            PointerAction::ScrollView(-3),
            "wheel-down scrolls back toward the live tail"
        );
        // The step is the caller's to choose.
        assert_eq!(
            r.classify(&wheel(2), 80, 25, 1),
            PointerAction::ScrollView(2)
        );
    }

    /// A.4 acceptance: once the application grabs the mouse, the wheel is
    /// the app's — the viewport must not move (the xterm convention).
    #[test]
    fn wheel_does_not_scroll_viewport_while_app_tracks() {
        let mut r = MouseReporter::new();
        r.set_tracking(TrackingMode::Normal);
        assert!(r.tracking_enabled());
        assert_eq!(
            r.classify(&wheel(1), 80, 25, 3),
            PointerAction::Ignore,
            "a tracking app owns the wheel; term's viewport stays put"
        );
    }

    /// A.4 acceptance: button edges are still reported unchanged — the
    /// classifier must not have stolen the pre-Phase-112 behaviour.
    #[test]
    fn button_edges_still_report_to_the_application() {
        let mut r = MouseReporter::new();
        r.set_tracking(TrackingMode::Normal);
        let ev = press(0, CELL_W_PX, CELL_H_PX);
        let expected = r.encode(&ev, 80, 25).expect("button edge encodes");
        assert_eq!(r.classify(&ev, 80, 25, 3), PointerAction::Report(expected));
    }

    /// A.4 acceptance: a PS/2-only lane never produces a wheel delta, so
    /// the classifier is inert there — `wheel_dy == 0` yields `Ignore`,
    /// not a zero-row scroll that would still dirty the viewport.
    #[test]
    fn zero_wheel_delta_is_ignored_not_a_zero_scroll() {
        let r = MouseReporter::new();
        assert_eq!(r.classify(&wheel(0), 80, 25, 3), PointerAction::Ignore);
    }
}
