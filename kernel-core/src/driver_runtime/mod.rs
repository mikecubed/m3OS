//! `driver_runtime` shared abstractions.
//!
//! The abstract contracts for the ring-3 `driver_runtime` wrappers live
//! here so the same contract suite can run against a mock syscall backend
//! (host tests) and the real syscall ABI shape (QEMU integration). Track
//! A.4 populates this module.

pub mod contract;
