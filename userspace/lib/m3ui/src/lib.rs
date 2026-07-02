//! `m3ui` — a minimal immediate-mode GUI toolkit for m3OS (Phase 105
//! Track A).
//!
//! The compositor stack (`display_server` + `desktop_client`) gives a
//! client a raw BGRA8888 shared surface and a focus-aware input stream,
//! but no *toolkit*: every GUI app (`greeter`, `bar`, `launcher`) hand-
//! codes button hit-testing and text-field editing pixel by pixel. `m3ui`
//! is the missing widget / layout / event-routing layer, shaped after
//! egui / microui: each frame the app re-declares its UI against a
//! [`Ui`], widgets carve their [`geom::Rect`] from a layout pass, and
//! interaction collapses to "did the pointer/keyboard land on this
//! frame's widget rect."
//!
//! # Structure
//!
//! - [`geom`] — points, rects, colors (the shared vocabulary).
//! - [`layout`] — the pure-logic constraint solver (Row/Column, fixed +
//!   flex sizing, padding, clip stack). The falsifiable core, host-tested.
//! - [`input`] — per-frame input folding + focus traversal.
//! - [`text_edit`] — the text-field cursor/edit state machine.
//! - [`theme`] — the color/metric palette every widget reads.
//! - [`paint`] — the [`paint::Painter`] drawing seam + a recording mock,
//!   so widgets are host-testable without a framebuffer.
//! - [`ui`] — the [`Ui`] context + widgets (label, button, text_field,
//!   checkbox, selectable, slider, separator), generic over `Painter`.
//! - [`render`] (feature `render`) — a concrete `SurfacePainter` over
//!   `desktop_client` + the event-decode helpers.
//!
//! The default build (no features) is the entire pure-logic toolkit —
//! layout, input, widgets, everything except the concrete surface — and
//! is exercised by `cargo test -p m3ui`. The `render` feature adds the
//! framebuffer/font/IPC bindings a real Toplevel app links.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod geom;
pub mod input;
pub mod layout;
pub mod paint;
pub mod text_edit;
pub mod theme;
pub mod ui;

#[cfg(feature = "render")]
pub mod render;

pub use geom::{Color, Point, Rect};
pub use input::{Focus, InputState, KeyCode, KeyPress, Mods, MouseButton};
pub use layout::{ClipStack, Dir, Item, LayoutSpec, Padding, Size, solve};
pub use paint::Painter;
pub use text_edit::{EditOutcome, TextBuffer};
pub use theme::{Theme, Visual};
pub use ui::{Response, Ui};

#[cfg(feature = "render")]
pub use render::{SurfacePainter, apply_pointer, decode_key, decode_mods};
