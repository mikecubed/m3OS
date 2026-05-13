//! Phase 57 Track F.1 — pure-logic session-step state model.
//!
//! `session_manager` (F.2) drives a fixed graphical-session boot sequence
//! (`display_server` → `kbd_server` → `mouse_server` → `audio_server` →
//! `term`) per the Phase 57 A.4 memo. The lifecycle (booting → running →
//! recovering → text-fallback) is a state machine. Locking it in pure
//! logic before wiring service calls catches ordering bugs (e.g.
//! starting `term` before `display_server` is ready) before any process
//! is spawned, and matches the F.1 acceptance criterion: "failing tests
//! commit first; `SessionState` transitions are total".
//!
//! The module is `no_std`-friendly and allocation-free in steady state.
//! `StartupSequence` borrows the caller's step slice — `kernel-core`
//! never owns the supervisor handle. `SessionStep` impls (the
//! `session_manager` daemon's per-service start/stop adapters) live in
//! `userspace/session_manager` (F.2 onward); this crate carries only the
//! abstract trait + the sequencer.
//!
//! See `docs/appendix/phase-57-session-entry.md` for the contract this
//! module enforces and `docs/roadmap/tasks/57-audio-and-local-session-tasks.md`
//! Track F.1 for the acceptance list.

pub mod font;
pub mod font_data;
pub mod recover;
pub mod startup;

pub use font::{BasicBitmapFont, FontError, FontProvider, Glyph};
pub use recover::{Recovery, RecoveryAction};
pub use startup::{SessionError, SessionState, SessionStep, StartupSequence};

/// Default per-step retry cap. Per the Phase 57 A.4 memo, every step in
/// the graphical session start sequence retries up to 3 attempts before
/// the whole session escalates to `text-fallback`. The constant is
/// reused by `session_manager` (F.2/F.4) so the cap is named once.
pub const MAX_RETRIES_PER_STEP: u32 = 3;

/// Minimum delay between retry attempts. The pure-logic state machine in
/// this module does not implement the wait itself — the supervisor
/// driver (F.4 `recover.rs`) consults this constant when sleeping
/// between retries. Named here so the resource-bound is a single fact
/// in the codebase rather than a sprinkled magic number.
pub const RETRY_BACKOFF_MS: u64 = 200;

/// Phase 64: steady-state full-restart budget. After this many full
/// `restart()` attempts on the same service (each attempt is a
/// `stop()` + `start()` pair), the supervisor escalates the service to
/// `ServiceState::Failed`. Named here alongside [`MAX_RETRIES_PER_STEP`]
/// so both Phase 64 budget constants live at one stable path and are
/// host-testable directly from `kernel-core` unit tests.
///
/// `MAX_RETRIES_PER_STEP` bounds the per-attempt step failures (one
/// stop call can fail up to N times within one restart); this constant
/// bounds the number of full restart attempts that have ever happened
/// for a given service since boot. Both gates fire independently — a
/// service that flaps three full restarts hits this gate; a service
/// whose individual `stop()` keeps failing within one restart hits
/// `MAX_RETRIES_PER_STEP`.
pub const MAX_RESTART_COUNT: u32 = 3;
