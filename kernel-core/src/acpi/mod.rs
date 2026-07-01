//! ACPI namespace, AML interpreter, and `_CRS` resource decoding.
//!
//! Phase 101 Tracks A–C. This module extends the kernel's *static* ACPI
//! table parsing (`kernel/src/acpi/mod.rs` — fixed-layout structs like
//! MADT/FADT/MCFG, plus the DMAR/IVRS decoders in
//! [`crate::iommu::tables`]) with the *dynamic* layer: the DSDT/SSDT
//! definition blocks are AML bytecode that must be interpreted to
//! discover the devices a laptop bring-up needs (the I2C-HID touchpad,
//! battery, lid switch, thermal zones) and to answer "what bus / slave
//! address / IRQ / GPIO is device X on?".
//!
//! Following the `iommu/tables.rs` pattern, everything here is pure
//! logic: firmware-provided byte buffers in, typed structures out. No
//! MMIO, no kernel-only dependencies, no `unsafe`. The single seam where
//! AML touches hardware — `OperationRegion` reads/writes — is abstracted
//! behind the [`aml::RegionSpace`] trait: host tests use a mock backend,
//! and the production ring-3 `acpid` daemon (Track E) implements it over
//! the capability-gated `device_host` syscalls.
//!
//! Safety posture: the interpreter executes **untrusted firmware
//! bytecode**. Recursion depth and loop iteration are bounded, and every
//! malformed input path returns [`aml::AmlError`] — never a panic.
//!
//! - [`aml`] — the pragmatic AML interpreter subset (Track A)
//! - [`namespace`] — namespace build + `_HID`/`_CID`/`_STA` queries (Track B)
//! - [`resource`] — `_CRS` resource-descriptor decode (Track C)

pub mod aml;
pub mod namespace;
pub mod resource;
