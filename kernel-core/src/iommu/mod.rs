//! IOMMU pure-logic foundation — Phase 55a.
//!
//! Host-testable types and algorithms shared between Intel VT-d and AMD-Vi
//! vendor implementations that live under `kernel/src/iommu/`. This module
//! contains no hardware register access, no MMIO, and no kernel-only
//! dependencies — it is unit-testable via `cargo test -p kernel-core`.
//!
//! Submodules:
//! - [`tables`] — ACPI DMAR (Intel) and IVRS (AMD) structure types and decoders.
//! - [`contract`] — the `IommuUnit` trait that both vendor implementations
//!   satisfy, along with `DmaDomain`, `IommuError`, and capability data.
//! - [`iova`] — IOVA space allocator pure logic.
//! - [`regions`] — reserved-region set algebra shared by VT-d RMRR and
//!   AMD-Vi unity-map handling.

pub mod contract;
pub mod iova;
pub mod regions;
pub mod tables;
