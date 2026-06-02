#![no_std]
//! Phase 81 — userspace Wi-Fi management plane + WPA2-PSK supplicant.
//!
//! This crate houses the soft-MAC 802.11 management plane (scan/auth/assoc),
//! the association FSM, the EAPOL-Key 4-way-handshake codec, the WPA2 key
//! derivation (orchestrating `crypto-lib` primitives), the `/etc/wpa.conf`
//! parser, and the userspace Wi-Fi control protocol. It is `#![no_std] + alloc`
//! and host-tested. The `mt792x` ring-3 driver links it; the kernel does not.

extern crate alloc;

pub mod config;
pub mod control;
pub mod eapol;
pub mod fsm;
pub mod kdf;
pub mod mgmt;
