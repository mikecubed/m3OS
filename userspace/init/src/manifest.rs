//! Phase 68 Track D.1 — service-manifest types and parser.
//!
//! The pure-logic body lives in
//! [`kernel_core::init::manifest`] so it is host-testable. This thin
//! wrapper re-exports the surface init's main loop needs and lets a
//! reviewer find the manifest API at the path the task doc names.

#[allow(unused_imports)]
pub use kernel_core::init::manifest::{
    DEFAULT_MAX_RESTART, DEFAULT_STOP_TIMEOUT_SECS, OnRestartAction, ParseWarning, RestartPolicy,
    ServiceManifest, ServiceType, detect_cycles, parse_manifest,
};
