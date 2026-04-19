//! # Ownership: Keep
//! High-level kernel syscall helpers that sit between the raw arch-level
//! dispatcher (`arch::x86_64::syscall`) and the subsystem modules.
//!
//! Phase 55b Track B.1 introduces [`device_host`] — the ring-3 driver-host
//! syscall family (`sys_device_claim` today; `sys_device_mmio_map`,
//! `sys_device_dma_alloc`, `sys_device_irq_subscribe` in later B-tracks).
//! The submodule hosts the capability-gated wrapper that the arch dispatcher
//! routes `SYS_DEVICE_CLAIM` (0x1120) into.
//!
//! Older subsystem syscalls (fs, io, ipc, mm, net, process, signal, time,
//! misc) continue to live under `arch::x86_64::syscall`. New work hangs off
//! this module instead so the arch tree stops accreting per-subsystem code.

pub mod device_host;
