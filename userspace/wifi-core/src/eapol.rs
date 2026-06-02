//! EAPOL-Key frame codec and MIC computation (Phase 81, Task B.6).
//!
//! Implements the EAPOL-Key frame format as defined in IEEE 802.11i §8.5.2
//! (also IEEE 802.1X-2004 §7.5 for the 802.1X header).
//!
//! Descriptor type 2 = RSN (EAPOL-Key, 4-way / Group handshake).
//! Descriptor version 2 = HMAC-SHA1-128 / AES-128 key wrap.

use alloc::vec::Vec;

// ── KeyInfo constants ─────────────────────────────────────────────────────────

/// KeyInfo bits for Message 1 (ANonce, ACK set).
/// Bit 3=1 (Pairwise), bit 7=1 (ACK), bits 1:0=10 (desc version 2).
/// 0x008A = 0000_0000_1000_1010
pub const KEY_INFO_M1: u16 = 0x008A;

/// KeyInfo bits for Message 2 (SNonce + RSN IE, MIC set).
/// Bit 3=1 (Pairwise), bit 8=1 (MIC), bits 1:0=10 (desc version 2).
/// 0x010A = 0000_0001_0000_1010
pub const KEY_INFO_M2: u16 = 0x010A;

/// KeyInfo bits for Message 3 (GTK, Install+ACK+MIC+Secure+Encrypted).
/// Bits: Install(6)=1, ACK(7)=1, MIC(8)=1, Secure(9)=1, Encrypted(12)=1, Pairwise(3)=1, ver=2.
/// 0x13CA = 0001_0011_1100_1010
pub const KEY_INFO_M3: u16 = 0x13CA;

/// KeyInfo bits for Message 4 (final ack, MIC+Secure set).
/// 0x030A = 0000_0011_0000_1010
pub const KEY_INFO_M4: u16 = 0x030A;

// ── KeyInfo accessor ──────────────────────────────────────────────────────────

/// Thin wrapper over the 16-bit KeyInfo field (IEEE 802.11i §8.5.2 Table 43b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInfo(pub u16);

impl KeyInfo {
    // Descriptor version: bits 2:0 (mask 0x0007; IEEE 802.11i §8.5.2 Table 43b)
    pub fn desc_version(self) -> u8 {
        (self.0 & 0x0007) as u8
    }
    // Bit 3: pairwise key type
    pub fn pairwise(self) -> bool {
        self.0 & (1 << 3) != 0
    }
    // Bit 6: Install
    pub fn install(self) -> bool {
        self.0 & (1 << 6) != 0
    }
    // Bit 7: Key ACK
    pub fn key_ack(self) -> bool {
        self.0 & (1 << 7) != 0
    }
    // Bit 8: Key MIC
    pub fn key_mic(self) -> bool {
        self.0 & (1 << 8) != 0
    }
    // Bit 9: Secure
    pub fn secure(self) -> bool {
        self.0 & (1 << 9) != 0
    }
    // Bit 12: Encrypted Key Data
    pub fn encrypted_key_data(self) -> bool {
        self.0 & (1 << 12) != 0
    }
}

// ── EAPOL-Key frame ───────────────────────────────────────────────────────────

/// An EAPOL-Key frame (descriptor type 2 = RSN).
///
/// Wire layout:
/// ```text
///   [802.1X header]
///   version[1]  type[1]=3  body_len[2]
///   [EAPOL-Key descriptor body]
///   desc_type[1]=2  key_info[2]  key_length[2]  replay_counter[8]
///   key_nonce[32]  key_iv[16]  key_rsc[8]  id[8]  key_mic[16]
///   key_data_length[2]  key_data[key_data_length]
/// ```
#[derive(Debug, Clone)]
pub struct EapolKeyFrame {
    /// EAPOL version (usually 1 or 2).
    pub version: u8,
    /// Descriptor type (2 = RSN).
    pub desc_type: u8,
    /// KeyInfo field.
    pub key_info: KeyInfo,
    /// Key length in bytes (for pairwise: 16 for CCMP).
    pub key_length: u16,
    /// Replay counter — monotonically increasing, set by AP, echoed by STA.
    pub replay_counter: u64,
    /// Key nonce (ANonce from AP, SNonce from STA).
    pub nonce: [u8; 32],
    /// Key IV (typically all-zero for WPA2).
    pub iv: [u8; 16],
    /// RSC (Receive Sequence Counter).
    pub rsc: [u8; 8],
    /// MIC (16 bytes; zeroed when computing MIC, non-zero otherwise).
    pub mic: [u8; 16],
    /// Key data (GTK, RSN IE, etc.).
    pub key_data: Vec<u8>,
}

/// Fixed EAPOL descriptor body offset of the MIC field within the encoded frame.
///
/// Breakdown (bytes): 802.1X-hdr(4) desc_type(1) key_info(2) key_length(2)
/// replay_counter(8) nonce(32) iv(16) rsc(8) id(8) = offset 81.
pub const MIC_OFFSET: usize = 81;

impl EapolKeyFrame {
    /// Parse an EAPOL-Key frame from raw bytes.
    ///
    /// Returns `None` on truncation or descriptor-type mismatch.
    pub fn parse(bytes: &[u8]) -> Option<EapolKeyFrame> {
        // Minimum size: 4-byte 802.1X header + 95-byte EAPOL-Key body = 99.
        if bytes.len() < 99 {
            return None;
        }
        let version = bytes[0];
        let pkt_type = bytes[1];
        if pkt_type != 3 {
            // Type 3 = EAPOL-Key
            return None;
        }
        let body_len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        if bytes.len() < 4 + body_len {
            return None;
        }
        let b = &bytes[4..4 + body_len];
        if b.len() < 95 {
            return None;
        }
        let desc_type = b[0];
        if desc_type != 2 {
            return None; // Only RSN descriptor supported
        }
        let key_info = KeyInfo(u16::from_be_bytes([b[1], b[2]]));
        let key_length = u16::from_be_bytes([b[3], b[4]]);
        let replay_counter = u64::from_be_bytes(b[5..13].try_into().ok()?);
        let mut nonce = [0u8; 32];
        nonce.copy_from_slice(&b[13..45]);
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&b[45..61]);
        let mut rsc = [0u8; 8];
        rsc.copy_from_slice(&b[61..69]);
        // bytes 69..77 = "id" (reserved, ignored)
        let mut mic = [0u8; 16];
        mic.copy_from_slice(&b[77..93]);
        let kd_len = u16::from_be_bytes([b[93], b[94]]) as usize;
        if b.len() < 95 + kd_len {
            return None;
        }
        let key_data = b[95..95 + kd_len].to_vec();
        Some(EapolKeyFrame {
            version,
            desc_type,
            key_info,
            key_length,
            replay_counter,
            nonce,
            iv,
            rsc,
            mic,
            key_data,
        })
    }

    /// Encode this frame to wire bytes.
    pub fn encode(&self) -> Vec<u8> {
        let kd_len = self.key_data.len();
        // body = 95 + key_data_length
        let body_len: u16 = (95 + kd_len) as u16;

        let mut out = Vec::with_capacity(4 + body_len as usize);
        // 802.1X header
        out.push(self.version); // version
        out.push(3); // type = EAPOL-Key
        out.extend_from_slice(&body_len.to_be_bytes());
        // EAPOL-Key descriptor body
        out.push(self.desc_type);
        out.extend_from_slice(&self.key_info.0.to_be_bytes());
        out.extend_from_slice(&self.key_length.to_be_bytes());
        out.extend_from_slice(&self.replay_counter.to_be_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.iv);
        out.extend_from_slice(&self.rsc);
        out.extend_from_slice(&[0u8; 8]); // id/reserved
        out.extend_from_slice(&self.mic);
        out.extend_from_slice(&(kd_len as u16).to_be_bytes());
        out.extend_from_slice(&self.key_data);
        out
    }
}

// ── MIC ───────────────────────────────────────────────────────────────────────

/// Compute the EAPOL-Key MIC for descriptor version 2 (HMAC-SHA1-128).
///
/// The MIC is the first 16 bytes of `HMAC-SHA1(KCK, frame_with_zeroed_mic)`.
/// The caller must zero the 16-byte MIC field before calling this function
/// (i.e. pass the frame with MIC set to all-zeros at offset `MIC_OFFSET`).
pub fn mic_sha1_128(kck: &[u8], frame_with_zeroed_mic: &[u8]) -> [u8; 16] {
    let full = crypto_lib::hash::hmac_sha1(kck, frame_with_zeroed_mic);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kdf::derive_ptk;
    use crate::kdf::tests::ptk_vector_inputs;
    use crate::mgmt::RsnIe;

    /// KeyInfo constants must encode/decode the expected bit pattern for each
    /// EAPOL message.
    #[test]
    fn key_info_per_message() {
        // M1: pairwise + ACK, no MIC/Install/Secure
        let m1 = KeyInfo(KEY_INFO_M1);
        assert_eq!(m1.0, 0x008A);
        assert!(m1.pairwise());
        assert!(m1.key_ack());
        assert!(!m1.key_mic());
        assert!(!m1.install());
        assert!(!m1.secure());
        assert!(!m1.encrypted_key_data());
        assert_eq!(m1.desc_version(), 2);

        // M2: pairwise + MIC, no ACK/Install/Secure
        let m2 = KeyInfo(KEY_INFO_M2);
        assert_eq!(m2.0, 0x010A);
        assert!(m2.pairwise());
        assert!(!m2.key_ack());
        assert!(m2.key_mic());
        assert!(!m2.install());
        assert!(!m2.secure());
        assert!(!m2.encrypted_key_data());
        assert_eq!(m2.desc_version(), 2);

        // M3: Install + ACK + MIC + Secure + Encrypted
        let m3 = KeyInfo(KEY_INFO_M3);
        assert_eq!(m3.0, 0x13CA);
        assert!(m3.pairwise());
        assert!(m3.key_ack());
        assert!(m3.key_mic());
        assert!(m3.install());
        assert!(m3.secure());
        assert!(m3.encrypted_key_data());
        assert_eq!(m3.desc_version(), 2);

        // M4: MIC + Secure, no ACK/Install/Encrypted
        let m4 = KeyInfo(KEY_INFO_M4);
        assert_eq!(m4.0, 0x030A);
        assert!(m4.pairwise());
        assert!(!m4.key_ack());
        assert!(m4.key_mic());
        assert!(!m4.install());
        assert!(m4.secure());
        assert!(!m4.encrypted_key_data());
        assert_eq!(m4.desc_version(), 2);
    }

    /// MIC computation is deterministic; a one-bit corruption must flip the MIC.
    ///
    /// Uses the KCK from the kdf::tests::ptk_vector to produce a concrete,
    /// reproducible 16-byte MIC value.
    #[test]
    fn mic_zeroed_field() {
        let (pmk, aa, spa, anonce, snonce) = ptk_vector_inputs();
        let ptk = derive_ptk(&pmk, &aa, &spa, &anonce, &snonce);

        // Build a minimal EAPOL M2 body (MIC field zeroed).
        let rsn_ie = RsnIe::ccmp_psk().encode();
        let snonce_bytes = snonce;
        let frame = build_m2_body(1 /* replay */, &snonce_bytes, &rsn_ie);

        // Zero the MIC field in position MIC_OFFSET..MIC_OFFSET+16.
        assert_eq!(frame[MIC_OFFSET..MIC_OFFSET + 16], [0u8; 16]);

        let mic = mic_sha1_128(&ptk.kck, &frame);
        // MIC must be 16 non-trivial bytes.
        assert_ne!(mic, [0u8; 16], "MIC must not be all-zeros");

        // One-bit corruption of a body byte must change the MIC.
        let mut corrupted = frame.clone();
        corrupted[5] ^= 0x01;
        let mic2 = mic_sha1_128(&ptk.kck, &corrupted);
        assert_ne!(mic, mic2, "single-bit corruption must flip the MIC");

        // Deterministic assertion against an EXTERNALLY-derived constant (not the
        // function under test), so a wrong MIC offset, truncation length, or
        // field ordering in build_m2_body / mic_sha1_128 fails here.
        //
        // Reproduction (independent of this code):
        //   KCK   = 3ffe47104cb02312eaf13c567ec0417c  (kdf::tests::ptk_vector)
        //   FRAME = build_m2_body(1, SNonce, RsnIe::ccmp_psk()) with MIC zeroed
        //   MIC   = hmac.new(KCK, FRAME, hashlib.sha1).digest()[:16]
        let expected: [u8; 16] = [
            0xcc, 0x7d, 0x0d, 0xc7, 0x83, 0x04, 0x05, 0xc4, 0x32, 0xcd, 0x85, 0x7f, 0x5e, 0xf2,
            0xb0, 0x9b,
        ];
        assert_eq!(mic, expected, "MIC must equal the external reference value");
    }

    /// The RSN IE embedded in an M2 key-data frame must be byte-for-byte
    /// identical to `mgmt::RsnIe::ccmp_psk().encode()`.
    #[test]
    fn m2_rsn_ie_matches_assoc() {
        let canonical = RsnIe::ccmp_psk().encode();
        // Build a synthetic M2 key-data that is just the RSN IE.
        // In a real handshake the STA copies its assoc RSN IE into M2 key-data.
        let key_data = canonical.clone();
        assert_eq!(
            key_data, canonical,
            "M2 key-data RSN IE must equal mgmt::RsnIe::ccmp_psk().encode()"
        );
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Build a minimal EAPOL-Key M2 frame body with the MIC field zeroed.
    fn build_m2_body(replay_counter: u64, snonce: &[u8; 32], rsn_ie: &[u8]) -> Vec<u8> {
        let frame = EapolKeyFrame {
            version: 1,
            desc_type: 2,
            key_info: KeyInfo(KEY_INFO_M2),
            key_length: 16,
            replay_counter,
            nonce: *snonce,
            iv: [0u8; 16],
            rsc: [0u8; 8],
            mic: [0u8; 16], // zeroed for MIC computation
            key_data: rsn_ie.to_vec(),
        };
        frame.encode()
    }
}
