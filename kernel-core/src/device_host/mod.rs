// Phase 55b — ring-3 driver host pure-logic types.
//
// This module hosts the ABI types shared by the kernel-side syscall handlers
// (Track B) and the userspace `driver_runtime` (Track C). All types live here
// once — per the Phase 55b task list's DRY discipline — and nothing later in
// the tree is permitted to redeclare them.
//
// The module is `no_std` + `alloc`-only so it compiles for the kernel and is
// still testable on the host via `cargo test -p kernel-core`.

pub mod dma_logic;
pub mod registry_logic;
pub mod syscalls;
pub mod types;

pub use dma_logic::{
    DMA_MIN_ALIGN, DmaAllocEntry, DmaAllocId, DmaAllocationRegistryCore, DmaRegistryError,
    validate_size_align,
};
pub use registry_logic::{DeviceHostRegistryCore, RegistryError, RegistryPid};
pub use types::{
    DRIVER_RESTART_TIMEOUT_MS, DeviceCapKey, DeviceHostError, DmaHandle,
    MMIO_WINDOW_DESCRIPTOR_SIZE, MmioCacheMode, MmioWindowDescriptor,
};
