// Phase 55b — ring-3 driver host pure-logic types.
//
// This module hosts the ABI types shared by the kernel-side syscall handlers
// (Track B) and the userspace `driver_runtime` (Track C). All types live here
// once — per the Phase 55b task list's DRY discipline — and nothing later in
// the tree is permitted to redeclare them.
//
// The module is `no_std` + `alloc`-only so it compiles for the kernel and is
// still testable on the host via `cargo test -p kernel-core`.

pub mod audio_class;
pub mod dma_logic;
pub mod irq_logic;
pub mod mmio_bounds;
pub mod registry_logic;
pub mod syscalls;
pub mod types;

pub use audio_class::{
    AC97_BAR_LAYOUT, BarLayout, DeviceClass, PCI_DEVICE_AC97, PCI_VENDOR_INTEL,
    SUBSYSTEM_AUDIO_DEVICE, classify_pci_id,
};
pub use dma_logic::{
    DMA_MIN_ALIGN, DmaAllocEntry, DmaAllocId, DmaAllocationRegistryCore, DmaRegistryError,
    validate_size_align,
};
pub use irq_logic::{
    IrqBinding, IrqBindingRegistryCore, IrqRegistryError, MAX_IRQ_SUBSCRIPTIONS_PER_PID,
};
pub use mmio_bounds::{
    MAX_MMIO_BAR_BYTES, MmioBoundsError, bar_page_count, build_mmio_window, cache_mode_for_bar,
    validate_mmio_bar_size,
};
pub use registry_logic::{DeviceHostRegistryCore, RegistryError, RegistryPid};
pub use types::{
    DRIVER_RESTART_TIMEOUT_MS, DeviceCapKey, DeviceHostError, DmaHandle,
    MMIO_WINDOW_DESCRIPTOR_SIZE, MmioCacheMode, MmioWindowDescriptor,
};
