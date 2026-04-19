//! MMIO-window wrapper module (Phase 55b Track C.1 stub).
//!
//! Track C.1 lands only the module shell. The concrete `Mmio<T>`
//! wrapper, bounds-checked read/write, and BAR-map integration land
//! in Track C.2 against the
//! [`MmioContract`](kernel_core::driver_runtime::contract::MmioContract)
//! trait re-exported below. Drivers must use this module — not raw
//! volatile pointer access — for every BAR register touch.

/// Re-export of the authoritative MMIO contract from `kernel-core`.
pub use kernel_core::driver_runtime::contract::MmioContract;

/// Re-export of the BAR-window descriptor shared with kernel-side
/// syscall handlers.
pub use kernel_core::device_host::{MmioCacheMode, MmioWindowDescriptor};
