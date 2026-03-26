//! Per-process capability table — re-exported from kernel-core.
#![allow(dead_code)]

pub use kernel_core::ipc::capability::{CapError, CapHandle, Capability, CapabilityTable};
