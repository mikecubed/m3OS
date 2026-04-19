//! DMA-buffer wrapper module (Phase 55b Track C.1 stub).
//!
//! Track C.1 lands only the module shell. The concrete `DmaBuffer<T>`
//! wrapper, its `user_va` / `iova` / `len` accessors, and the
//! Drop-frees-IOVA invariant land in Track C.2 against the
//! [`DmaBufferContract`](kernel_core::driver_runtime::contract::DmaBufferContract)
//! and
//! [`DmaBufferHandle`](kernel_core::driver_runtime::contract::DmaBufferHandle)
//! traits re-exported below.

/// Re-export of the DMA allocation contract from `kernel-core`.
pub use kernel_core::driver_runtime::contract::{DmaBufferContract, DmaBufferHandle};

/// Re-export of the DMA handle ABI type shared with kernel-side
/// syscall handlers.
pub use kernel_core::device_host::DmaHandle;
