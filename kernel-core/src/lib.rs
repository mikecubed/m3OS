#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod address_space;
pub mod buddy;
pub mod cred;
pub mod cross_cpu_free;
pub mod fb;
pub mod fs;
pub mod input;
pub mod ipc;
pub mod log_ring;
pub mod magazine;
pub mod mm;
pub mod net;
pub mod pipe;
pub mod prng;
pub mod pty;
pub mod service;
pub mod size_class;
pub mod slab;
pub mod time;
pub mod trace_ring;
pub mod tty;
pub mod types;
pub mod user_range;
