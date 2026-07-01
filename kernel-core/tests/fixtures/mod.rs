//! Shared test fixtures for `kernel-core` integration tests.
//!
//! Phase 55a Track A.1 — hosts the `MockUnit` reference implementation of
//! the `IommuUnit` trait. Additional fixtures for later tracks (page-table
//! encoders, IOVA allocator stress helpers) can live in sibling modules
//! and be re-exported from here as they land.
//!
//! Phase 55b Track A.4 — hosts the `MockBackend` reference implementation
//! of the `driver_runtime` contract traits.

pub mod aml_builder;
pub mod driver_runtime_mock;
pub mod mock_unit;
