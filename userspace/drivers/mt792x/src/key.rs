//! mt792x key-install seam — Phase 81 Track DRV-net (Task B.7 driver-side).
//!
//! Bridges the supplicant FSM's `InstallKey(KeyMaterial)` action to the WM MCU:
//! the host-derived 16-byte TK (and the GTK) are pushed into the chipset's WTBL
//! via a `STA_REC_UPDATE` MCU command carrying a `STA_REC_KEY` TLV. After this,
//! **all per-packet CCMP encrypt/decrypt + replay is done in hardware** — the
//! host hands the chip plaintext frames thereafter (no software AES-CCM).
//!
//! The TLV byte-packing is the host-tested `kernel_core::mt792x::mcu::
//! encode_sta_rec_key` (no crypto here). These functions are invoked ONLY from
//! the `WifiAction::InstallKey` / `WifiAction::PurgeKeys` arms in `io.rs`, which
//! the FSM reaches only after verifying EAPOL M3 (Track B.5
//! `m3_mic_fail_no_install`) — so a key never reaches the chip before the
//! handshake authenticates it.

extern crate alloc;

use kernel_core::mt792x::mcu::{CIPHER_CCMP, encode_sta_rec_key};
use wifi_core::fsm::KeyMaterial;

use crate::mcu::{McuError, McuRing};

/// MCU command id for a station-record update (carries STA_REC_* TLVs).
pub const STA_REC_UPDATE_CID: u8 = 0x25;

/// Key index for the pairwise (unicast) key — always 0 for CCMP pairwise.
const PAIRWISE_KEY_IDX: u8 = 0;

/// Install the pairwise Temporal Key (TK) into the chipset WTBL for `wcid`.
///
/// Builds a `STA_REC_KEY` TLV for the CCMP pairwise key and submits it on the
/// WM MCU queue. The 16-byte TK is the unicast CCMP key derived by the host
/// 4-way handshake (Track B.6).
pub fn install_pairwise_key(mcu: &mut McuRing, wcid: u16, tk: &[u8; 16]) -> Result<(), McuError> {
    let tlv = encode_sta_rec_key(wcid, CIPHER_CCMP, PAIRWISE_KEY_IDX, tk);
    mcu.submit_and_reap(STA_REC_UPDATE_CID, &tlv)
        .map(|_| ())
        .map_err(|_| McuError::Timeout)
}

/// Install the Group Temporal Key (GTK) into the chipset WTBL for `wcid`.
pub fn install_group_key(
    mcu: &mut McuRing,
    wcid: u16,
    gtk: &[u8],
    gtk_idx: u8,
) -> Result<(), McuError> {
    let tlv = encode_sta_rec_key(wcid, CIPHER_CCMP, gtk_idx, gtk);
    mcu.submit_and_reap(STA_REC_UPDATE_CID, &tlv)
        .map(|_| ())
        .map_err(|_| McuError::Timeout)
}

/// Install both the pairwise TK and the GTK produced by the 4-way handshake.
///
/// Called from the `WifiAction::InstallKey` arm only.
pub fn install_keys(mcu: &mut McuRing, wcid: u16, km: &KeyMaterial) -> Result<(), McuError> {
    install_pairwise_key(mcu, wcid, &km.tk)?;
    install_group_key(mcu, wcid, &km.gtk, km.gtk_idx)
}

/// Purge the session keys for `wcid` from the chipset (on deauth/disconnect),
/// so stale keys do not linger in the WTBL. Installs zero-length CCMP keys.
pub fn purge_keys(mcu: &mut McuRing, wcid: u16) -> Result<(), McuError> {
    let empty: [u8; 0] = [];
    let tlv = encode_sta_rec_key(wcid, CIPHER_CCMP, PAIRWISE_KEY_IDX, &empty);
    mcu.submit_and_reap(STA_REC_UPDATE_CID, &tlv)
        .map(|_| ())
        .map_err(|_| McuError::Timeout)
}
