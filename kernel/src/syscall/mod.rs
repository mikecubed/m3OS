//! # Ownership: Keep
//! High-level kernel syscall helpers that sit between the raw arch-level
//! dispatcher (`arch::x86_64::syscall`) and the subsystem modules.
//!
//! Phase 55b Tracks B.1–B.4 introduce [`device_host`] — the ring-3 driver-host
//! syscall family covering `sys_device_claim`, `sys_device_mmio_map`,
//! `sys_device_dma_alloc`, `sys_device_dma_handle_info`, and
//! `sys_device_irq_subscribe`. The submodule hosts the capability-gated
//! wrappers that the arch dispatcher routes each of these syscalls into;
//! the reserved numbers are pinned in
//! `kernel_core::device_host::syscalls` (`0x1120..=0x1124`).
//!
//! Older subsystem syscalls (fs, io, ipc, mm, net, process, signal, time,
//! misc) continue to live under `arch::x86_64::syscall`. New work hangs off
//! this module instead so the arch tree stops accreting per-subsystem code.

pub mod device_host;
pub mod net;
