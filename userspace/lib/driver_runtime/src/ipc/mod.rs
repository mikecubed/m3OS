//! Driver-side IPC client helpers (Phase 55b Track C.1 stub).
//!
//! Track C.1 lands only the module shell. The block-client and
//! net-client helpers that wrap the Phase 55b A.2 / A.3 IPC protocol
//! schemas in ergonomic request/response pairs land in Track C.4.
//! The schema types themselves live once in
//! [`kernel_core::driver_ipc`] per the Phase 55b DRY discipline and
//! are re-exported here so kernel-side facades and driver processes
//! consume the identical types.

pub mod block;
pub mod net;
