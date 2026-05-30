//! USB host-stack pure logic.
//!
//! Everything in this subtree is hardware-free: register/TRB/context bit
//! layouts and the small state machines (producer/consumer cycle bits, port
//! status RW1C masking) that the xHCI driver bring-up depends on. No MMIO, no
//! DMA, no kernel dependencies beyond `core`/`alloc`. Host-testable via
//! `cargo test -p kernel-core --target x86_64-unknown-linux-gnu usb::`.

pub mod descriptor;
pub mod enumerate;
pub mod xhci;
