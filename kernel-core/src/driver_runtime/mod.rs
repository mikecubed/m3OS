//! `driver_runtime` shared abstractions.
//!
//! The abstract contracts for the ring-3 `driver_runtime` wrappers live
//! here so the same contract suite can run against a mock syscall backend
//! (host tests) and the real syscall ABI shape (QEMU integration).
//!
//! # Authoritative behavioral spec
//!
//! Every implementation of the contracts in [`contract`] must pass the
//! test suite at:
//!
//! ```text
//! kernel-core/tests/driver_runtime_contract.rs
//! ```
//!
//! The suite is parameterized over the pure-logic `MockBackend` reference
//! implementation in `kernel-core/tests/fixtures/driver_runtime_mock.rs`;
//! Track C.2 re-runs it against the real syscall backend. The filename is
//! reproduced here (and in the module docs on `contract.rs`) so grep for
//! "driver_runtime_contract.rs" from any implementation site lands on the
//! same file.

pub mod contract;
