#![no_std]
//! Phase 81 — userspace Wi-Fi management plane + WPA2-PSK supplicant.
//!
//! This crate houses the soft-MAC 802.11 management plane (scan/auth/assoc),
//! the association FSM, the EAPOL-Key 4-way-handshake codec, the WPA2 key
//! derivation (orchestrating `crypto-lib` primitives), the `/etc/wpa.conf`
//! parser, and the userspace Wi-Fi control protocol. It is `#![no_std] + alloc`
//! and host-tested. The `mt792x` ring-3 driver links it; the kernel does not.

extern crate alloc;

// Track B / C / D modules are added here as they land:
//   pub mod mgmt;     // B.4 — 802.11 mgmt-frame builders + RSN IE
//   pub mod fsm;      // B.5 — association FSM
//   pub mod eapol;    // B.6 — EAPOL-Key frame codec
//   pub mod kdf;      // B.6 — PTK derivation
//   pub mod control;  // C.2 — userspace Wi-Fi control protocol
//   pub mod config;   // D.1 — /etc/wpa.conf parser
