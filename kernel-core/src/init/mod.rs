//! Phase 68 Track D — pure-logic primitives for the `userspace/init`
//! daemon. The supervisor and manifest parser previously lived inline
//! in `userspace/init/src/main.rs` as fixed-size `no_std` data
//! structures (`FixedStr<MAX_NAME>`, `[[usize; MAX_DEPS]; MAX_SERVICES]`).
//! That shape cannot represent comma-separated `depends=` lists
//! cleanly, and `init` is a `no_std` + `no_main` binary so its
//! submodules cannot run `cargo test`.
//!
//! This module hosts the Phase 68 reshape:
//!
//! * [`manifest::ServiceManifest`] — heap-backed manifest with
//!   `Vec<String>` for `depends` and a typed
//!   [`manifest::OnRestartAction`] field.
//! * [`manifest::parse_manifest`] / [`manifest::detect_cycles`].
//! * [`supervisor::start_services_ordered`] /
//!   [`supervisor::ServiceState`].
//!
//! The new code is host-testable in `kernel-core/tests/` and the
//! `userspace/init` binary re-exports the surface via thin
//! `userspace/init/src/manifest.rs` and
//! `userspace/init/src/supervisor.rs` wrappers.

pub mod manifest;
pub mod supervisor;
