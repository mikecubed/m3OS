//! Pragmatic AML interpreter subset (Phase 101 Track A).
//!
//! Implements the AML surface sufficient for *device enumeration* — not a
//! full ACPICA-class VM: the opcode stream + `PkgLength` + `NameString`
//! encodings ([`decode`]), the value/object model ([`object`]), and a
//! control-method evaluator ([`interp`]) covering `Store`, `If`/`Else`,
//! `While`, `Return`, `Local0..7`/`Arg0..6`, the integer/logical/buffer/
//! package operators, and method invocation. Namespace-creating opcodes
//! (`Scope`, `Device`, `Method`, `Name`, `OperationRegion`, `Field`, …)
//! populate the [`crate::acpi::namespace::Namespace`] arena.
//!
//! Behavior is referenced against the ACPI 6.5 specification and checked
//! against what ACPICA/uACPI do where the spec is loose; no code is
//! copied. Everything outside the enumeration subset returns
//! [`AmlError::UnsupportedOpcode`] rather than guessing.
//!
//! # Safety requirements (untrusted firmware bytecode)
//!
//! - Recursion is bounded ([`interp::MAX_DEPTH`]).
//! - Total executed operations are bounded ([`interp::MAX_OPS`]) so a
//!   `While` loop (or nest of them) cannot spin forever.
//! - Malformed or truncated AML returns [`AmlError`]; no input may panic.

pub mod decode;
pub mod interp;
pub mod object;
pub mod wire;

pub use object::{AmlError, AmlValue, MockRegionSpace, RegionSpace};
