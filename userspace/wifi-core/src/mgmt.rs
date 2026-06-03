//! 802.11 management-frame builders and RSN IE codec (Phase 81, Task B.4).
//!
//! Covers:
//! - RSN Information Element encoding (CCMP/PSK, 22 bytes).
//! - Probe Request frame builder.
//! - Open System Authentication frame builder.
//! - Association Request frame builder.
//! - Probe Response parser (SSID, BSSID, DS-Param channel, RSN IE validation).
//!
//! All frame formats follow IEEE 802.11-2020.

use alloc::vec::Vec;

// ── OUI/suite constants ────────────────────────────────────────────────────────

/// OUI prefix for 802.11 cipher / AKM suites (00:0F:AC).
const OUI_80211: [u8; 3] = [0x00, 0x0F, 0xAC];

/// Suite type: CCMP-128 (pairwise / group cipher).
const SUITE_CCMP: u8 = 0x04;
/// Suite type: TKIP (used in RSN IE validation).
#[allow(dead_code)]
const SUITE_TKIP: u8 = 0x02;
/// Suite type: PSK AKM.
const SUITE_PSK: u8 = 0x02;

// ── RSN IE ────────────────────────────────────────────────────────────────────

/// RSN Information Element (IEEE 802.11-2020 §9.4.2.24).
///
/// `ccmp_psk()` builds the minimal RSN IE suitable for WPA2-Personal:
/// group cipher CCMP-128, one pairwise suite CCMP-128, one AKM PSK, no PMKID.
#[derive(Debug, Clone)]
pub struct RsnIe {
    /// Group cipher suite (4 bytes: OUI[3] + type[1]).
    pub group_cipher: [u8; 4],
    /// Pairwise cipher suites.
    pub pairwise_ciphers: Vec<[u8; 4]>,
    /// AKM suites.
    pub akm_suites: Vec<[u8; 4]>,
    /// RSN Capabilities (2 bytes, little-endian).
    pub rsn_caps: u16,
}

impl RsnIe {
    /// Construct the canonical CCMP + PSK RSN IE.
    ///
    /// Produces exactly 22 bytes when encoded:
    /// `30 14 01 00 00 0F AC 04 01 00 00 0F AC 04 01 00 00 0F AC 02 00 00`
    pub fn ccmp_psk() -> Self {
        let ccmp_suite = make_suite(OUI_80211, SUITE_CCMP);
        let psk_suite = make_suite(OUI_80211, SUITE_PSK);
        Self {
            group_cipher: ccmp_suite,
            pairwise_ciphers: alloc::vec![ccmp_suite],
            akm_suites: alloc::vec![psk_suite],
            rsn_caps: 0x0000,
        }
    }

    /// Encode this RSN IE as a TLV byte vector.
    ///
    /// Layout (§9.4.2.24):
    ///   element id (1) | length (1) | version[2] | group[4] |
    ///   pairwise-count[2] | pairwise[4*n] | akm-count[2] | akm[4*m] | caps[2]
    pub fn encode(&self) -> Vec<u8> {
        let pw_count = self.pairwise_ciphers.len() as u16;
        let akm_count = self.akm_suites.len() as u16;
        // body length: version(2) + group(4) + pw_count(2) + pw(4*n)
        //              + akm_count(2) + akm(4*m) + caps(2)
        let body_len = 2 + 4 + 2 + 4 * pw_count as usize + 2 + 4 * akm_count as usize + 2;

        let mut out = Vec::with_capacity(2 + body_len);
        out.push(0x30); // element id = RSN
        out.push(body_len as u8);
        // Version 1 (LE)
        out.extend_from_slice(&1u16.to_le_bytes());
        // Group cipher
        out.extend_from_slice(&self.group_cipher);
        // Pairwise
        out.extend_from_slice(&pw_count.to_le_bytes());
        for s in &self.pairwise_ciphers {
            out.extend_from_slice(s);
        }
        // AKM
        out.extend_from_slice(&akm_count.to_le_bytes());
        for s in &self.akm_suites {
            out.extend_from_slice(s);
        }
        // Capabilities
        out.extend_from_slice(&self.rsn_caps.to_le_bytes());
        out
    }
}

fn make_suite(oui: [u8; 3], suite_type: u8) -> [u8; 4] {
    [oui[0], oui[1], oui[2], suite_type]
}

// ── Frame-control constants ───────────────────────────────────────────────────

/// Frame Control: Management / Probe Request (subtype 0x04), version 0.
/// FC = 0x0040 (subtype = 0100 in bits 7:4, type = 00 in bits 9:8).
const FC_PROBE_REQ: [u8; 2] = [0x40, 0x00];
/// Frame Control: Management / Authentication (subtype 0x0B).
const FC_AUTH: [u8; 2] = [0xB0, 0x00];
/// Frame Control: Management / Association Request (subtype 0x00).
const FC_ASSOC_REQ: [u8; 2] = [0x00, 0x00];

/// Broadcast address used for probe requests.
const BROADCAST: [u8; 6] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
/// Placeholder source / BSSID (all zeros — caller replaces as needed).
const ZERO_ADDR: [u8; 6] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

/// Duration field value (zero; set by hardware or caller).
const DURATION: [u8; 2] = [0x00, 0x00];
/// Sequence control (zero; set by hardware or caller).
const SEQ_CTRL: [u8; 2] = [0x00, 0x00];

// ── Information Element helpers ───────────────────────────────────────────────

fn append_ie(out: &mut Vec<u8>, id: u8, data: &[u8]) {
    out.push(id);
    out.push(data.len() as u8);
    out.extend_from_slice(data);
}

// ── Probe Request ─────────────────────────────────────────────────────────────

/// Build an 802.11 Probe Request frame.
///
/// Layout: 802.11 mgmt header (24 bytes) + SSID IE (id 0) + Supported-Rates IE (id 1).
///
/// The source and BSSID addresses are left as zero; the transmitting driver
/// fills them in before radio transmission.
pub fn build_probe_request(ssid: &[u8], rates: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    // --- 802.11 management header ---
    out.extend_from_slice(&FC_PROBE_REQ); // Frame Control
    out.extend_from_slice(&DURATION); // Duration
    out.extend_from_slice(&BROADCAST); // DA = broadcast
    out.extend_from_slice(&ZERO_ADDR); // SA (caller fills)
    out.extend_from_slice(&BROADCAST); // BSSID = broadcast for probe
    out.extend_from_slice(&SEQ_CTRL); // Sequence Control
    // --- IEs ---
    append_ie(&mut out, 0, ssid); // SSID
    append_ie(&mut out, 1, rates); // Supported Rates
    out
}

// ── Open System Authentication ────────────────────────────────────────────────

/// Build an 802.11 Open System Authentication frame.
///
/// Auth Algorithm = 0 (Open), Auth Seq = `seq`, Status = 0.
pub fn build_auth_open(seq: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(30);
    out.extend_from_slice(&FC_AUTH);
    out.extend_from_slice(&DURATION);
    out.extend_from_slice(&ZERO_ADDR); // DA (AP address — caller fills)
    out.extend_from_slice(&ZERO_ADDR); // SA (STA address — caller fills)
    out.extend_from_slice(&ZERO_ADDR); // BSSID (caller fills)
    out.extend_from_slice(&SEQ_CTRL);
    // Auth body
    out.extend_from_slice(&0u16.to_le_bytes()); // Auth Algorithm = 0 (Open)
    out.extend_from_slice(&seq.to_le_bytes()); // Auth Transaction Seq
    out.extend_from_slice(&0u16.to_le_bytes()); // Status code = 0 (Success)
    out
}

// ── Association Request ───────────────────────────────────────────────────────

/// Build an 802.11 Association Request frame.
///
/// Carries SSID IE, RSN IE (verbatim bytes), and Supported Rates IE.
pub fn build_assoc_request(ssid: &[u8], rsn_ie: &[u8], rates: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(&FC_ASSOC_REQ);
    out.extend_from_slice(&DURATION);
    out.extend_from_slice(&ZERO_ADDR); // DA (AP)
    out.extend_from_slice(&ZERO_ADDR); // SA (STA)
    out.extend_from_slice(&ZERO_ADDR); // BSSID
    out.extend_from_slice(&SEQ_CTRL);
    // Capability info + listen interval (fixed fields)
    out.extend_from_slice(&0x0431u16.to_le_bytes()); // ESS + Privacy + Short Preamble + Short Slot
    out.extend_from_slice(&10u16.to_le_bytes()); // Listen Interval = 10
    // IEs
    append_ie(&mut out, 0, ssid); // SSID
    append_ie(&mut out, 1, rates); // Supported Rates
    // RSN IE verbatim (already a complete TLV: id + len + body).
    out.extend_from_slice(rsn_ie);
    out
}

// ── BSS info parsed from Probe Response ──────────────────────────────────────

/// Summary of a BSS extracted from a Probe Response frame.
#[derive(Debug, Clone)]
pub struct BssInfo {
    /// SSID bytes (may be empty for hidden networks).
    pub ssid: Vec<u8>,
    /// AP BSSID (6 bytes).
    pub bssid: [u8; 6],
    /// Channel from DS Parameter Set IE (id 3). 0 if absent.
    pub channel: u8,
    /// `true` only if the AP advertises CCMP pairwise + PSK AKM in its RSN IE.
    pub rsn: bool,
}

/// Parse a Probe Response frame and extract BSS information.
///
/// Returns `None` if the frame is too short to be a valid management frame.
/// Never panics on truncated or malformed input.
pub fn parse_probe_response(frame: &[u8]) -> Option<BssInfo> {
    // Minimum probe-response: 24-byte header + 12-byte fixed fields (timestamp[8]
    // + beacon interval[2] + capability[2]) = 36 bytes.
    if frame.len() < 36 {
        return None;
    }

    // Extract BSSID from header offset 16..22.
    let mut bssid = [0u8; 6];
    bssid.copy_from_slice(&frame[16..22]);

    // Skip management header (24 bytes) + fixed fields (12 bytes) = 36 bytes.
    let ie_start = 36;
    let ies = &frame[ie_start..];

    let mut ssid = Vec::new();
    let mut channel: u8 = 0;
    let mut rsn = false;

    let mut pos = 0;
    while pos + 2 <= ies.len() {
        let id = ies[pos];
        let len = ies[pos + 1] as usize;
        pos += 2;
        if pos + len > ies.len() {
            break; // truncated IE — stop parsing, use what we have
        }
        let body = &ies[pos..pos + len];
        match id {
            0 => {
                // SSID
                ssid = body.to_vec();
            }
            3 if !body.is_empty() => {
                // DS Parameter Set: channel
                channel = body[0];
            }
            48 => {
                // RSN IE
                rsn = parse_rsn_ie_accept_ccmp_psk(body);
            }
            _ => {}
        }
        pos += len;
    }

    Some(BssInfo {
        ssid,
        bssid,
        channel,
        rsn,
    })
}

/// Returns `true` only if the RSN IE body advertises CCMP-128 as at least one
/// pairwise cipher AND PSK (AKM type 2) as at least one AKM suite.
///
/// WPA1 IEs (element id 0xDD with Microsoft OUI) are handled by the caller
/// returning `rsn = false` — this function is only called for element id 48.
fn parse_rsn_ie_accept_ccmp_psk(body: &[u8]) -> bool {
    // Minimum RSN body: version(2) + group(4) + pairwise-count(2) = 8 bytes.
    if body.len() < 8 {
        return false;
    }
    // Version must be 1.
    let version = u16::from_le_bytes([body[0], body[1]]);
    if version != 1 {
        return false;
    }
    // Skip group cipher (offset 2..6).
    let mut pos = 6usize;

    // Pairwise cipher count.
    if pos + 2 > body.len() {
        return false;
    }
    let pw_count = u16::from_le_bytes([body[pos], body[pos + 1]]) as usize;
    pos += 2;
    if pos + pw_count * 4 > body.len() {
        return false;
    }
    // Check at least one pairwise suite is CCMP (00:0F:AC:04).
    let mut has_ccmp_pairwise = false;
    for i in 0..pw_count {
        let s = &body[pos + i * 4..pos + i * 4 + 4];
        if s[0..3] == OUI_80211 && s[3] == SUITE_CCMP {
            has_ccmp_pairwise = true;
        }
    }
    pos += pw_count * 4;
    if !has_ccmp_pairwise {
        return false;
    }

    // AKM count.
    if pos + 2 > body.len() {
        return false;
    }
    let akm_count = u16::from_le_bytes([body[pos], body[pos + 1]]) as usize;
    pos += 2;
    if pos + akm_count * 4 > body.len() {
        return false;
    }
    // Check at least one AKM is PSK (00:0F:AC:02).
    let mut has_psk_akm = false;
    for i in 0..akm_count {
        let s = &body[pos + i * 4..pos + i * 4 + 4];
        if s[0..3] == OUI_80211 && s[3] == SUITE_PSK {
            has_psk_akm = true;
        }
    }
    has_psk_akm
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// RSN IE for CCMP + PSK must encode to exactly 22 canonical bytes:
    /// 30 14 01 00 00 0F AC 04 01 00 00 0F AC 04 01 00 00 0F AC 02 00 00
    #[test]
    fn rsn_ie_ccmp_psk() {
        let encoded = RsnIe::ccmp_psk().encode();
        let expected: [u8; 22] = [
            0x30, 0x14, // element id=0x30, len=0x14
            0x01, 0x00, // version 1 LE
            0x00, 0x0F, 0xAC, 0x04, // group: CCMP
            0x01, 0x00, // pairwise count = 1
            0x00, 0x0F, 0xAC, 0x04, // pairwise[0]: CCMP
            0x01, 0x00, // AKM count = 1
            0x00, 0x0F, 0xAC, 0x02, // AKM[0]: PSK
            0x00, 0x00, // RSN caps
        ];
        assert_eq!(
            encoded.as_slice(),
            &expected,
            "RSN IE must match canonical 22-byte encoding"
        );
    }

    /// Build a synthetic Probe Response that carries CCMP + PSK RSN IE;
    /// parse_probe_response must accept it and set rsn=true.
    #[test]
    fn probe_response_rsn_accept() {
        let frame = build_synthetic_probe_response(
            b"TestNet",
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            6,
            true,  // CCMP + PSK
            false, // not TKIP-only
        );
        let info = parse_probe_response(&frame).expect("should parse");
        assert_eq!(info.ssid, b"TestNet");
        assert_eq!(info.bssid, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(info.channel, 6);
        assert!(info.rsn, "CCMP+PSK RSN IE must be accepted");
    }

    /// A probe response that only advertises TKIP pairwise (no CCMP) must
    /// result in rsn=false.
    #[test]
    fn rejects_tkip_only() {
        let frame = build_synthetic_probe_response(
            b"TKIPNet",
            [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            11,
            false, // CCMP
            true,  // TKIP only
        );
        let info = parse_probe_response(&frame).expect("should parse");
        assert!(!info.rsn, "TKIP-only RSN IE must be rejected");
    }

    /// Open System Authentication must have algorithm=0, seq=1, status=0.
    #[test]
    fn auth_open_open_system() {
        let frame = build_auth_open(1);
        // Auth fixed fields start after the 24-byte management header.
        assert!(frame.len() >= 30, "auth frame too short");
        let body = &frame[24..];
        let alg = u16::from_le_bytes([body[0], body[1]]);
        let seq = u16::from_le_bytes([body[2], body[3]]);
        let status = u16::from_le_bytes([body[4], body[5]]);
        assert_eq!(alg, 0, "Open System auth algorithm must be 0");
        assert_eq!(seq, 1, "auth seq must equal the supplied seq");
        assert_eq!(status, 0, "status must be 0 (success)");
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Construct a minimal valid Probe Response frame for testing.
    ///
    /// Frame layout:
    ///   FC[2] Duration[2] DA[6] SA[6] BSSID[6] SeqCtrl[2]  ← 24 bytes header
    ///   Timestamp[8] BeaconInterval[2] Capability[2]         ← 12 bytes fixed
    ///   SSID_IE  DS_Param_IE  RSN_IE
    fn build_synthetic_probe_response(
        ssid: &[u8],
        bssid: [u8; 6],
        channel: u8,
        ccmp_psk: bool,
        tkip_only: bool,
    ) -> Vec<u8> {
        let mut frame = Vec::new();
        // Management header
        frame.extend_from_slice(&[0x50, 0x00]); // FC: Probe Response
        frame.extend_from_slice(&[0x00, 0x00]); // Duration
        frame.extend_from_slice(&[0xFF; 6]); // DA
        frame.extend_from_slice(&bssid); // SA = BSSID for simplicity
        frame.extend_from_slice(&bssid); // BSSID
        frame.extend_from_slice(&[0x00, 0x00]); // SeqCtrl
        // Fixed fields
        frame.extend_from_slice(&[0u8; 8]); // Timestamp
        frame.extend_from_slice(&[0x64, 0x00]); // Beacon interval = 100 TU
        frame.extend_from_slice(&[0x11, 0x00]); // Capability
        // SSID IE
        append_ie(&mut frame, 0, ssid);
        // DS Parameter Set IE
        append_ie(&mut frame, 3, &[channel]);
        // RSN IE
        if ccmp_psk {
            // CCMP pairwise + PSK AKM
            let rsn_body: &[u8] = &[
                0x01, 0x00, // version 1
                0x00, 0x0F, 0xAC, 0x04, // group: CCMP
                0x01, 0x00, // pairwise count 1
                0x00, 0x0F, 0xAC, 0x04, // pairwise: CCMP
                0x01, 0x00, // AKM count 1
                0x00, 0x0F, 0xAC, 0x02, // AKM: PSK
                0x00, 0x00, // caps
            ];
            append_ie(&mut frame, 48, rsn_body);
        }
        if tkip_only {
            // TKIP pairwise (no CCMP) + PSK AKM
            let rsn_body: &[u8] = &[
                0x01, 0x00, 0x00, 0x0F, 0xAC, 0x02, // group: TKIP
                0x01, 0x00, 0x00, 0x0F, 0xAC, 0x02, // pairwise: TKIP
                0x01, 0x00, 0x00, 0x0F, 0xAC, 0x02, // AKM: PSK
                0x00, 0x00,
            ];
            append_ie(&mut frame, 48, rsn_body);
        }
        frame
    }
}
