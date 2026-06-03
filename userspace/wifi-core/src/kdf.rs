//! WPA2 PTK derivation and GTK unwrap (Phase 81, Task B.6).
//!
//! Implements:
//! - `derive_ptk`: PRF-512 over HMAC-SHA-1 per IEEE 802.11i §8.5.1.2.
//! - `unwrap_gtk`: AES Key-Unwrap (RFC 3394) of the wrapped GTK from M3 key-data.

use alloc::vec::Vec;
use crypto_lib::CryptoError;

// ── PTK ───────────────────────────────────────────────────────────────────────

/// WPA2 Pairwise Transient Key, split into its three sub-keys.
///
/// PRF-512 produces 64 bytes; we take the first 48:
///   bytes  0..16 → KCK (Key Confirmation Key — used to MIC EAPOL frames)
///   bytes 16..32 → KEK (Key Encryption Key — used to encrypt GTK in M3)
///   bytes 32..48 → TK  (Temporal Key — used for unicast data encryption)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ptk {
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    pub tk: [u8; 16],
}

/// Derive the PTK using PRF-512 (HMAC-SHA-1 based) per IEEE 802.11i §8.5.1.2.
///
/// The PRF expands 80 bytes (4 × 20-byte HMAC-SHA-1 rounds):
/// ```text
/// For i = 0..3:
///   R[i] = HMAC-SHA1(PMK, "Pairwise key expansion" || 0x00 || B || i)
/// B = min(AA,SPA) || max(AA,SPA) || min(ANonce,SNonce) || max(ANonce,SNonce)
/// ```
/// `kck = R[0..16]`, `kek = R[16..32]`, `tk = R[32..48]`.
///
/// The byte-wise lexicographic min/max ordering ensures the PRF input is the
/// same regardless of which side (AP or STA) computes the PTK.
pub fn derive_ptk(
    pmk: &[u8; 32],
    aa: &[u8; 6],
    spa: &[u8; 6],
    anonce: &[u8; 32],
    snonce: &[u8; 32],
) -> Ptk {
    use crypto_lib::hash::hmac_sha1;

    let label: &[u8] = b"Pairwise key expansion";

    // Build B: min/max over MAC addresses then nonces.
    let mut b = [0u8; 6 + 6 + 32 + 32]; // 76 bytes
    let (min_mac, max_mac) = if aa <= spa { (aa, spa) } else { (spa, aa) };
    let (min_nonce, max_nonce) = if anonce <= snonce {
        (anonce, snonce)
    } else {
        (snonce, anonce)
    };
    b[0..6].copy_from_slice(min_mac);
    b[6..12].copy_from_slice(max_mac);
    b[12..44].copy_from_slice(min_nonce);
    b[44..76].copy_from_slice(max_nonce);

    // PRF-512: compute 80 bytes (4 rounds of 20 bytes), truncate to 64.
    let mut r = [0u8; 80];
    for i in 0u8..4 {
        // Concatenate: label || 0x00 || B || counter_byte
        let mut input = Vec::with_capacity(label.len() + 1 + 76 + 1);
        input.extend_from_slice(label);
        input.push(0x00);
        input.extend_from_slice(&b);
        input.push(i);
        let h = hmac_sha1(pmk, &input);
        r[i as usize * 20..(i as usize + 1) * 20].copy_from_slice(&h);
    }

    let mut kck = [0u8; 16];
    let mut kek = [0u8; 16];
    let mut tk = [0u8; 16];
    kck.copy_from_slice(&r[0..16]);
    kek.copy_from_slice(&r[16..32]);
    tk.copy_from_slice(&r[32..48]);
    Ptk { kck, kek, tk }
}

// ── GTK ───────────────────────────────────────────────────────────────────────

/// Group Temporal Key unwrapped from M3 key-data.
#[derive(Debug, Clone)]
pub struct Gtk {
    /// GTK bytes (16, 32, or 32 bytes depending on cipher).
    pub bytes: Vec<u8>,
    /// GTK index (0–3) extracted from the GTK KDE header.
    pub key_idx: u8,
}

/// KDE element id (`0xDD`, the vendor-specific element type).
const KDE_TYPE: u8 = 0xDD;
/// IEEE 802.11 KDE OUI (`00:0F:AC`).
const KDE_OUI: [u8; 3] = [0x00, 0x0F, 0xAC];
/// KDE data-type selector for the GTK KDE.
const KDE_DATA_TYPE_GTK: u8 = 0x01;
/// Bytes preceding the GTK in the KDE body that the `len` field counts:
/// OUI(3) + data_type(1) + KeyInfo(1) + reserved(1).
const KDE_GTK_PREAMBLE: usize = 6;

/// Unwrap the GTK from M3 key-data per IEEE 802.11i.
///
/// The EAPOL-Key M3 Key Data field is AES-Key-Wrapped (RFC 3394) under the KEK
/// (the "Encrypted Key Data" KeyInfo bit is set). This:
///
/// 1. AES-Key-Unwraps the **entire** `m3_keydata` under `kek`;
/// 2. parses the plaintext as a **GTK KDE**:
///    `[0xDD][len][00 0F AC][0x01][KeyInfo][reserved][GTK…]`,
///    rejecting a malformed type / OUI / data-type / length;
/// 3. extracts the GTK key index (KeyInfo bits 0–1) and the GTK bytes
///    (`len - 6` bytes, i.e. 16 for CCMP).
///
/// Returns `Err(CryptoError::InvalidLength)` when the buffer is too short or the
/// KDE length is inconsistent, or `Err(CryptoError::AuthenticationFailed)` when
/// the AES-KW integrity check fails or the KDE header is not a GTK KDE.
pub fn unwrap_gtk(kek: &[u8; 16], m3_keydata: &[u8]) -> Result<Gtk, CryptoError> {
    // Unwrap the whole encrypted Key Data field (AES-KW yields n×8 plaintext).
    let plaintext = crypto_lib::symmetric::aes_key_unwrap(kek, m3_keydata)?;

    // Parse the GTK KDE from the front of the plaintext.
    if plaintext.len() < 8 {
        return Err(CryptoError::InvalidLength);
    }
    if plaintext[0] != KDE_TYPE || plaintext[2..5] != KDE_OUI || plaintext[5] != KDE_DATA_TYPE_GTK {
        return Err(CryptoError::AuthenticationFailed);
    }
    // `len` counts OUI(3)+data_type(1)+KeyInfo(1)+reserved(1)+GTK(n).
    let kde_len = plaintext[1] as usize;
    if kde_len < KDE_GTK_PREAMBLE || 2 + kde_len > plaintext.len() {
        return Err(CryptoError::InvalidLength);
    }
    let key_info = plaintext[6];
    let key_idx = key_info & 0x03;
    let gtk_len = kde_len - KDE_GTK_PREAMBLE;
    let gtk = plaintext[8..8 + gtk_len].to_vec();
    Ok(Gtk {
        bytes: gtk,
        key_idx,
    })
}

/// Build a GTK KDE plaintext (`[0xDD][len][00 0F AC][01][KeyInfo][rsv][GTK]`)
/// for a given GTK and key index, ready to be AES-Key-Wrapped into M3 key-data.
/// Shared by the supplicant's own tests and the FSM-level handshake fixtures so
/// both exercise the real KDE format.
pub fn build_gtk_kde(gtk: &[u8], key_idx: u8) -> Vec<u8> {
    let mut kde = Vec::with_capacity(8 + gtk.len());
    kde.push(KDE_TYPE);
    kde.push((KDE_GTK_PREAMBLE + gtk.len()) as u8); // len field
    kde.extend_from_slice(&KDE_OUI);
    kde.push(KDE_DATA_TYPE_GTK);
    kde.push(key_idx & 0x03); // KeyInfo: KeyID in bits 0..1
    kde.push(0x00); // reserved
    kde.extend_from_slice(gtk);
    kde
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Stored PTK result for use by eapol tests (KCK is needed there).
    pub(crate) fn ptk_vector_inputs() -> (
        [u8; 32], // pmk
        [u8; 6],  // aa
        [u8; 6],  // spa
        [u8; 32], // anonce
        [u8; 32], // snonce
    ) {
        // PMK: IEEE 802.11i §H.4 vector — passphrase="password", SSID="IEEE",
        // PBKDF2-HMAC-SHA1, 4096 iterations, 32 bytes.
        // Source: IEEE 802.11i-2004 Annex H §H.4 (also reproduced in
        // crypto_lib::hash::tests::wpa_pmk_kat).
        let pmk: [u8; 32] = [
            0xf4, 0x2c, 0x6f, 0xc5, 0x2d, 0xf0, 0xeb, 0xef, 0x9e, 0xbb, 0x4b, 0x90, 0xb3, 0x8a,
            0x5f, 0x90, 0x2e, 0x83, 0xfe, 0x1b, 0x13, 0x5a, 0x70, 0xe2, 0x3a, 0xed, 0x76, 0x2e,
            0x97, 0x10, 0xa1, 0x2e,
        ];
        // AA (AP MAC) and SPA (STA MAC) chosen so that AA > SPA, exercising the
        // min/max ordering path (min_mac = SPA, max_mac = AA).
        let aa: [u8; 6] = [0xB0, 0xB3, 0x43, 0xF7, 0x39, 0x05];
        let spa: [u8; 6] = [0xA0, 0xB1, 0xC2, 0xD3, 0xE4, 0xF5];
        // ANonce > SNonce — exercises the nonce min/max ordering path.
        let anonce: [u8; 32] = [
            0xAA, 0xBB, 0xCC, 0xDD, 0xAA, 0xBB, 0xCC, 0xDD, 0xAA, 0xBB, 0xCC, 0xDD, 0xAA, 0xBB,
            0xCC, 0xDD, 0xAA, 0xBB, 0xCC, 0xDD, 0xAA, 0xBB, 0xCC, 0xDD, 0xAA, 0xBB, 0xCC, 0xDD,
            0xAA, 0xBB, 0xCC, 0xDD,
        ];
        let snonce: [u8; 32] = [
            0x11, 0x22, 0x33, 0x44, 0x11, 0x22, 0x33, 0x44, 0x11, 0x22, 0x33, 0x44, 0x11, 0x22,
            0x33, 0x44, 0x11, 0x22, 0x33, 0x44, 0x11, 0x22, 0x33, 0x44, 0x11, 0x22, 0x33, 0x44,
            0x11, 0x22, 0x33, 0x44,
        ];
        (pmk, aa, spa, anonce, snonce)
    }

    /// PTK derivation — reproduces a full KCK/KEK/TK vector.
    ///
    /// PMK source: IEEE 802.11i-2004 Annex H §H.4
    ///   (passphrase="password", SSID="IEEE", PBKDF2-HMAC-SHA1 4096 iters).
    /// PRF-512 computed by the reference Python implementation:
    ///   hmac_sha1(pmk, "Pairwise key expansion" || 0x00 || B || i) for i=0..3
    /// where B = min(SPA,AA) || max(SPA,AA) || min(SNonce,ANonce) || max(SNonce,ANonce).
    ///
    /// AA > SPA  →  min_mac = SPA = A0:B1:C2:D3:E4:F5
    /// ANonce > SNonce  →  min_nonce = SNonce
    #[test]
    fn ptk_vector() {
        let (pmk, aa, spa, anonce, snonce) = ptk_vector_inputs();

        let ptk = derive_ptk(&pmk, &aa, &spa, &anonce, &snonce);

        // Expected values computed by reference Python:
        //   import hmac, hashlib
        //   def h(k,d): return hmac.new(k,d,hashlib.sha1).digest()
        //   r = b''.join(h(pmk, b"Pairwise key expansion\x00" + B + bytes([i])) for i in range(4))
        let expected_kck: [u8; 16] = [
            0x3f, 0xfe, 0x47, 0x10, 0x4c, 0xb0, 0x23, 0x12, 0xea, 0xf1, 0x3c, 0x56, 0x7e, 0xc0,
            0x41, 0x7c,
        ];
        let expected_kek: [u8; 16] = [
            0xbf, 0xa6, 0x07, 0xe5, 0x19, 0x05, 0x90, 0x22, 0xbc, 0x39, 0xd1, 0x0d, 0xe4, 0x8a,
            0x20, 0x5c,
        ];
        let expected_tk: [u8; 16] = [
            0x9e, 0x4a, 0x69, 0xab, 0xb1, 0x0a, 0x78, 0x5d, 0x58, 0x06, 0x44, 0x7d, 0xec, 0x81,
            0x76, 0xeb,
        ];

        assert_eq!(ptk.kck, expected_kck, "KCK mismatch");
        assert_eq!(ptk.kek, expected_kek, "KEK mismatch");
        assert_eq!(ptk.tk, expected_tk, "TK mismatch");

        // Ordering is exercised: verify the min/max paths are non-trivial.
        assert!(
            aa > spa,
            "AA must be > SPA for this test to exercise the swap path"
        );
        assert!(
            anonce > snonce,
            "ANonce must be > SNonce for this test to exercise the swap path"
        );
    }

    /// GTK wrap/unwrap round-trip and tamper rejection.
    #[test]
    fn gtk_unwrap() {
        let kek: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let gtk_plaintext: [u8; 16] = [
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
            0x1e, 0x1f,
        ];
        let key_idx: u8 = 1;

        // Build standards-shaped M3 key-data: AES-Key-Wrap of a real GTK KDE.
        let kde = build_gtk_kde(&gtk_plaintext, key_idx);
        // KDE is 8 + 16 = 24 bytes (a multiple of 8) — no padding needed.
        assert_eq!(kde.len(), 24);
        let m3_keydata = crypto_lib::symmetric::aes_key_wrap(&kek, &kde).expect("GTK KDE wraps");

        // Successful unwrap recovers the GTK and the key index from the KDE.
        let gtk = unwrap_gtk(&kek, &m3_keydata).expect("unwrap should succeed");
        assert_eq!(gtk.key_idx, key_idx);
        assert_eq!(gtk.bytes.as_slice(), &gtk_plaintext);

        // Tamper with wrapped bytes → integrity check must fail.
        let mut tampered = m3_keydata.clone();
        tampered[5] ^= 0xFF;
        assert!(
            unwrap_gtk(&kek, &tampered).is_err(),
            "tampered key-data must fail AES-KW integrity check"
        );

        // A correctly-wrapped but non-GTK KDE (wrong data-type) must be rejected.
        let mut bad_kde = build_gtk_kde(&gtk_plaintext, key_idx);
        bad_kde[5] = 0x02; // data_type != GTK
        let bad = crypto_lib::symmetric::aes_key_wrap(&kek, &bad_kde).expect("bad KDE wraps");
        assert!(
            matches!(
                unwrap_gtk(&kek, &bad),
                Err(CryptoError::AuthenticationFailed)
            ),
            "non-GTK KDE must be rejected"
        );
    }
}
