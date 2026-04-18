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
//! - [`device_map`] — pure-logic `(segment, bus, device, function) →
//!   unit_index` map used to route a PCI device to its owning IOMMU unit.
//! - [`acpi_integration`] — converters turning decoded `DmarTables` /
//!   `IvrsTables` into the shapes kernel-side code consumes (unit
//!   descriptors, reserved-region sets).

pub mod acpi_integration;
pub mod amdvi_page_table;
pub mod amdvi_regs;
pub mod contract;
pub mod device_map;
pub mod identity;
pub mod iova;
pub mod regions;
pub mod tables;
pub mod vtd_page_table;
pub mod vtd_regs;
