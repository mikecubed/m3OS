//! Shared test fixtures for `kernel-core` integration tests.
//!
//! Phase 55a Track A.1 — hosts the `MockUnit` reference implementation of
//! the `IommuUnit` trait. Additional fixtures for later tracks (page-table
//! encoders, IOVA allocator stress helpers) can live in sibling modules
//! and be re-exported from here as they land.

pub mod mock_unit;
