//! Minimal cooperative single-threaded async executor for m3OS userspace.
//!
//! Supports dual `std` (host testing) and `no_std + alloc` (kernel target) modes.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod executor;
pub mod io;
pub mod reactor;
pub mod task;
