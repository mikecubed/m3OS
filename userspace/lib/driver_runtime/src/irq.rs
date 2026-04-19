//! IRQ-notification wrapper module (Phase 55b Track C.1 stub).
//!
//! Track C.1 lands only the module shell. The concrete
//! `IrqNotification` wrapper, the wait-loop helper, and the
//! Drop-unbinds-notification invariant land in Tracks C.2 / C.3
//! against the
//! [`IrqNotificationContract`](kernel_core::driver_runtime::contract::IrqNotificationContract)
//! and
//! [`IrqNotificationHandle`](kernel_core::driver_runtime::contract::IrqNotificationHandle)
//! traits re-exported below.

/// Re-export of the IRQ subscription contract from `kernel-core`.
pub use kernel_core::driver_runtime::contract::{IrqNotificationContract, IrqNotificationHandle};
