//! Phase 57 Track G.4 — screen state machine + ANSI-parser consumer.
//!
//! Filled in by G.4. The G.1 scaffold only declares the error and
//! command types so [`crate::TermError`] can name [`ScreenError`].

/// Errors observable on the screen public surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScreenError {
    /// Render command requested an `(row, col)` outside the cell grid.
    OutOfBounds,
}
