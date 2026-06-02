//! Userspace Wi-Fi control protocol (Phase 81, Task C.2).
//!
//! Defines the label constants, wire-format types, and encode/decode logic
//! for the userspace↔userspace Wi-Fi control IPC channel.  These labels are
//! NOT kernel `driver_ipc::net` labels; they live entirely in userspace.

use alloc::vec::Vec;

// ── Label constants ───────────────────────────────────────────────────────────

/// Scan request: trigger a Wi-Fi scan.
pub const WIFI_SCAN_REQ: u16 = 0x5601;
/// Scan result: one BSS entry from a completed scan.
pub const WIFI_SCAN_RESULT: u16 = 0x5602;
/// Connect request: associate with an SSID.
pub const WIFI_CONNECT_REQ: u16 = 0x5603;
/// Status update: current connection state.
pub const WIFI_STATUS: u16 = 0x5604;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Error codes for the Wi-Fi control protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiControlError {
    /// The station is not associated (operation requires an active connection).
    NotAssociated,
    /// The request was malformed or contained invalid parameters.
    BadRequest,
}

// ── ScanResult ────────────────────────────────────────────────────────────────

/// A single BSS entry returned in a `WIFI_SCAN_RESULT` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    /// AP BSSID (6 bytes).
    pub bssid: [u8; 6],
    /// SSID bytes (0..=32 bytes).
    pub ssid: Vec<u8>,
    /// Received signal strength indicator (dBm, signed).
    pub rssi: i8,
    /// 802.11 channel number.
    pub channel: u8,
}

impl ScanResult {
    /// Encode to wire bytes.
    ///
    /// Format: `bssid[6] | rssi[1] | channel[1] | ssid_len[1] | ssid[n]`
    pub fn encode(&self) -> Vec<u8> {
        let ssid_len = self.ssid.len().min(255) as u8;
        let mut out = Vec::with_capacity(6 + 1 + 1 + 1 + ssid_len as usize);
        out.extend_from_slice(&self.bssid);
        out.push(self.rssi as u8);
        out.push(self.channel);
        out.push(ssid_len);
        out.extend_from_slice(&self.ssid[..ssid_len as usize]);
        out
    }

    /// Decode from wire bytes. Returns `None` on truncation.
    pub fn decode(bytes: &[u8]) -> Option<ScanResult> {
        if bytes.len() < 9 {
            return None; // 6 + 1 + 1 + 1 minimum
        }
        let mut bssid = [0u8; 6];
        bssid.copy_from_slice(&bytes[0..6]);
        let rssi = bytes[6] as i8;
        let channel = bytes[7];
        let ssid_len = bytes[8] as usize;
        if bytes.len() < 9 + ssid_len {
            return None;
        }
        let ssid = bytes[9..9 + ssid_len].to_vec();
        Some(ScanResult {
            bssid,
            ssid,
            rssi,
            channel,
        })
    }
}

// ── WifiStatus ────────────────────────────────────────────────────────────────

/// Current Wi-Fi connection status, sent as a `WIFI_STATUS` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiStatus {
    /// Connected SSID bytes (empty if not associated).
    pub ssid: Vec<u8>,
    /// RSSI in dBm (signed).
    pub rssi: i8,
    /// Assigned IPv4 address (0.0.0.0 if not configured).
    pub ipv4: [u8; 4],
}

impl WifiStatus {
    /// Build the status the driver's `wifi.control` responder returns for the
    /// current supplicant state — the host-testable half of `m3ctl wifi status`.
    ///
    /// When `associated` is false the SSID is empty (which `m3ctl wifi status`
    /// renders as "not associated") and `rssi`/`ipv4` are zero. When associated,
    /// the connected SSID is reported; the live `rssi` and DHCP-assigned `ipv4`
    /// are read from the radio + lease on hardware (E.4) and are passed through
    /// here (0/`0.0.0.0` until the live values are available).
    pub fn for_connection(associated: bool, ssid: &[u8], rssi: i8, ipv4: [u8; 4]) -> WifiStatus {
        if associated {
            WifiStatus {
                ssid: ssid.to_vec(),
                rssi,
                ipv4,
            }
        } else {
            WifiStatus {
                ssid: Vec::new(),
                rssi: 0,
                ipv4: [0; 4],
            }
        }
    }

    /// Encode to wire bytes.
    ///
    /// Format: `rssi[1] | ipv4[4] | ssid_len[1] | ssid[n]`
    pub fn encode(&self) -> Vec<u8> {
        let ssid_len = self.ssid.len().min(255) as u8;
        let mut out = Vec::with_capacity(1 + 4 + 1 + ssid_len as usize);
        out.push(self.rssi as u8);
        out.extend_from_slice(&self.ipv4);
        out.push(ssid_len);
        out.extend_from_slice(&self.ssid[..ssid_len as usize]);
        out
    }

    /// Decode from wire bytes. Returns `None` on truncation.
    pub fn decode(bytes: &[u8]) -> Option<WifiStatus> {
        if bytes.len() < 6 {
            return None;
        }
        let rssi = bytes[0] as i8;
        let mut ipv4 = [0u8; 4];
        ipv4.copy_from_slice(&bytes[1..5]);
        let ssid_len = bytes[5] as usize;
        if bytes.len() < 6 + ssid_len {
            return None;
        }
        let ssid = bytes[6..6 + ssid_len].to_vec();
        Some(WifiStatus { ssid, rssi, ipv4 })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// ScanResult and WifiStatus must round-trip through encode/decode identically.
    #[test]
    fn roundtrip() {
        // ScanResult round-trip
        let sr = ScanResult {
            bssid: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            ssid: b"HomeNetwork".to_vec(),
            rssi: -65i8,
            channel: 11,
        };
        let encoded = sr.encode();
        let decoded = ScanResult::decode(&encoded).expect("ScanResult decode must succeed");
        assert_eq!(decoded, sr, "ScanResult must round-trip byte-for-byte");

        // WifiStatus round-trip
        let ws = WifiStatus {
            ssid: b"HomeNetwork".to_vec(),
            rssi: -72i8,
            ipv4: [192, 168, 1, 42],
        };
        let encoded_ws = ws.encode();
        let decoded_ws = WifiStatus::decode(&encoded_ws).expect("WifiStatus decode must succeed");
        assert_eq!(decoded_ws, ws, "WifiStatus must round-trip byte-for-byte");
    }

    /// The responder helper reports the SSID only when associated, and an
    /// empty-SSID status otherwise (which m3ctl renders as "not associated").
    #[test]
    fn status_for_connection() {
        let assoc = WifiStatus::for_connection(true, b"HomeNet", -55, [10, 0, 0, 7]);
        assert_eq!(assoc.ssid, b"HomeNet");
        assert_eq!(assoc.rssi, -55);
        assert_eq!(assoc.ipv4, [10, 0, 0, 7]);
        // Round-trips on the wire.
        assert_eq!(WifiStatus::decode(&assoc.encode()).unwrap(), assoc);

        let down = WifiStatus::for_connection(false, b"HomeNet", -55, [10, 0, 0, 7]);
        assert!(down.ssid.is_empty(), "not-associated status has empty SSID");
        assert_eq!(down.ipv4, [0; 4]);
    }

    /// Label constants must be distinct u16 values.
    #[test]
    fn labels_distinct() {
        let labels = [
            WIFI_SCAN_REQ,
            WIFI_SCAN_RESULT,
            WIFI_CONNECT_REQ,
            WIFI_STATUS,
        ];
        for i in 0..labels.len() {
            for j in 0..labels.len() {
                if i != j {
                    assert_ne!(labels[i], labels[j], "control labels must be distinct");
                }
            }
        }
    }
}
