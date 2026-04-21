//! Driver IPC protocol schemas.
//!
//! Phase 55b splits the driver-process <-> kernel IPC surface into two
//! per-subsystem schemas declared in this module. Both the kernel-side
//! facade (`RemoteBlockDevice`, `RemoteNic`) and the userspace driver
//! processes (`userspace/drivers/nvme`, `userspace/drivers/e1000`) consume
//! the schemas from here, so divergence is a compile error rather than a
//! runtime corruption bug.

pub mod blk_dispatch;
pub mod block;
pub mod net;

pub use blk_dispatch::{BlockDispatchState, GrantIdTracker, RemoteDeviceError, WaitOutcome};
