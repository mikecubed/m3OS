//! Phase 55b Track C.1 — RED stub. Module surface not yet declared.
//!
//! This file exists so the crate compiles as a workspace member before
//! the Green commit wires up the public module tree
//! (`device`, `mmio`, `dma`, `irq`, `ipc`). The failing test below
//! asserts the module surface is absent; it flips to an assertion over
//! the declared module surface in the Green commit.

#![no_std]

extern crate alloc;

#[cfg(test)]
mod tests {
    /// C.1 RED — the crate compiles but the public module surface is
    /// not yet declared. This test is expected to fail at runtime until
    /// the Green commit lands `pub mod device;`, `pub mod mmio;`,
    /// `pub mod dma;`, `pub mod irq;`, and `pub mod ipc;` plus a
    /// smoke import of a kernel-core contract type to prove wiring.
    #[test]
    fn driver_runtime_public_module_surface_declared() {
        // SAFETY: intentional failing assertion for the RED commit of
        // the Phase 55b C.1 TDD cycle. Replaced by a real module
        // smoke-import in the Green commit.
        assert!(
            false,
            "C.1 RED: driver_runtime public module surface not yet declared"
        );
    }
}
