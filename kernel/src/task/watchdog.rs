//! Stuck-task watchdog — G.1 (Phase 57a).
//!
//! This module re-exports [`watchdog_scan`] which is implemented in
//! `scheduler.rs` (where `SCHEDULER` is accessible as `pub(super)`).
//!
//! # Why implementation lives in scheduler.rs
//!
//! `SCHEDULER` is `pub(super)`, visible to the `task` module but not to child
//! modules of `task`. Moving the scan into `scheduler.rs` avoids adding a
//! wider visibility path for the lock just for the watchdog.
//!
//! # Integration point
//!
//! `watchdog_scan` is gated by [`WATCHDOG_COUNTER`] (in scheduler.rs), an
//! atomically-incremented counter checked at each dispatch iteration — the
//! same pattern as `BALANCE_COUNTER` for load balancing. Every
//! `WATCHDOG_SCAN_INTERVAL_TICKS` ticks a full O(n) scan runs on BSP only.
//!
//! # Core selection
//!
//! Only core 0 (BSP) calls `watchdog_scan` — this matches the existing
//! convention for BSP-only background work (`drain_dead`,
//! `drain_pending_waiters`, `maybe_load_balance`).

pub use super::scheduler::watchdog_scan;
