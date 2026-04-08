//! Miscellaneous syscall handlers (ioctl, uname, arch_prctl, reboot, etc.).
//!
//! Handler functions live in the parent module (`mod.rs`).  This file
//! existed for the chained `Option`-returning dispatcher which was removed
//! in favour of the flat dispatch table for QEMU TCG performance.
