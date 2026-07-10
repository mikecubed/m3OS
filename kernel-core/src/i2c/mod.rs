//! Phase 102 — I2C substrate for the built-in I2C-HID touchpad, pure logic.
//!
//! QEMU models neither the Intel LPSS DesignWare I2C controller nor an
//! I2C-HID device, so the entire substrate below the (bus-agnostic) HID
//! report-descriptor layer is hardware-free, host-tested logic here; the ring-3
//! `i2c-hid` daemon supplies the actual MMIO and drives these state machines.
//! The live datapath is Dell-validated per `docs/appendix/bare-metal-validation.md`.
//!
//! - [`designware`] — the DesignWare I2C **master**: register/bit layout, the
//!   `DW_IC_DATA_CMD` transfer planner, and `TX_ABRT` decode (Track A).
//! - [`hid_over_i2c`] — the **HID-over-I2C v1.0** transport: the HID-descriptor
//!   parse and the RESET/SET_POWER/GET_REPORT command frames + input-report
//!   length-prefix parse (Track B).
//!
//! The multitouch report *decode* (Track C) reuses the USB HID report machinery
//! and lives in [`crate::usb::hid_report::decode_touchpad_report`] — the report
//! descriptor language is identical over I2C and USB.
//!
//! Host-testable via `cargo test -p kernel-core --target x86_64-unknown-linux-gnu i2c::`.

pub mod designware;
pub mod hid_over_i2c;
