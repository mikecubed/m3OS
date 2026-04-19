//! Device-claim wrapper module (Phase 55b Track C.1 stub).
//!
//! Track C.1 lands only the module shell. The concrete `DeviceHandle`
//! wrapper, its `claim` / `release` implementation, and the Drop-releases
//! invariant are built in Track C.2 against the
//! [`DeviceHandleContract`](kernel_core::driver_runtime::contract::DeviceHandleContract)
//! trait re-exported below. Downstream drivers (`userspace/drivers/nvme/`,
//! `userspace/drivers/e1000/`) must consume this module — not
//! `syscall_lib` directly — when claiming a PCI device.

/// Re-export of the authoritative device-claim contract from
/// `kernel-core`. Track C.2 adds a concrete `DeviceHandle` type here
/// that implements this trait against the Phase 55b Track B
/// `sys_device_claim` syscall.
pub use kernel_core::driver_runtime::contract::DeviceHandleContract;

/// Re-export of the capability-key ABI type shared with kernel-side
/// syscall handlers.
pub use kernel_core::device_host::DeviceCapKey;
