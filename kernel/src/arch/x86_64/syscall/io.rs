//! I/O multiplexing syscall handlers (poll, select, epoll, pipe).
//!
//! Handler functions live in the parent module (`mod.rs`).  This file
//! existed for the chained `Option`-returning dispatcher which was removed
//! in favour of the flat dispatch table for QEMU TCG performance.
