//! Phase 68 Track D.2 — supervisor pure-logic re-exports.
//!
//! The dependency-ordered start planner and the `on-restart=`
//! action dispatcher live in
//! [`kernel_core::init::supervisor`] so they are host-testable.
//! This wrapper exists so a reviewer can grep
//! `userspace/init/src/supervisor.rs` and land in the right place.

#[allow(unused_imports)]
pub use kernel_core::init::supervisor::{
    BudgetExhaustionOutcome, ServiceState, StartPlan, handle_budget_exhaustion,
    start_services_ordered,
};
