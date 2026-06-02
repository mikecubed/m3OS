//! `/etc/wpa.conf` parser (Phase 81, Task D.1).
//!
//! Parses a minimal wpa_supplicant-style network block:
//!   ssid=<name>
//!   psk=<passphrase>        (8..=63 ASCII characters)
//!   freq=2.4|5              (optional, default 2.4 GHz)
//!
//! The PMK is derived at parse time via PBKDF2-HMAC-SHA1 (IEEE 802.11i §H.4)
//! and the plaintext passphrase is NOT stored.

use alloc::vec::Vec;

// ── Band ─────────────────────────────────────────────────────────────────────

/// Radio frequency band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// 2.4 GHz (802.11b/g/n/ax).
    Ghz24,
    /// 5 GHz (802.11a/n/ac/ax).
    Ghz5,
}

// ── ConfigError ──────────────────────────────────────────────────────────────

/// Error returned by [`parse_wpa_conf`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// `ssid=` line is missing.
    MissingSsid,
    /// `psk=` line is missing.
    MissingPsk,
    /// PSK passphrase is shorter than 8 characters (IEEE 802.11 minimum).
    ShortPsk,
    /// PSK passphrase is longer than 63 characters (IEEE 802.11 maximum).
    LongPsk,
    /// The configuration text is syntactically malformed.
    Malformed,
}

// ── WpaConfig ────────────────────────────────────────────────────────────────

/// Parsed WPA2 network configuration.
///
/// Stores the derived PMK, not the raw passphrase.
#[derive(Debug)]
pub struct WpaConfig {
    ssid: Vec<u8>,
    pmk: [u8; 32],
    freq: Band,
}

impl WpaConfig {
    /// SSID bytes.
    pub fn ssid(&self) -> &[u8] {
        &self.ssid
    }

    /// Derived PMK (32 bytes, PBKDF2-HMAC-SHA1).
    pub fn pmk(&self) -> &[u8; 32] {
        &self.pmk
    }

    /// Frequency band.
    pub fn freq(&self) -> Band {
        self.freq
    }
}

// ── Parser ───────────────────────────────────────────────────────────────────

/// Parse a `/etc/wpa.conf` text and return a [`WpaConfig`].
///
/// Recognised keys: `ssid`, `psk`, `freq` (optional, `2.4` or `5`).
/// All other keys are silently ignored.
///
/// The passphrase is validated (8..=63 characters), converted to the PMK via
/// `PBKDF2-HMAC-SHA1(passphrase, ssid, 4096, 32)`, and the plaintext buffer is
/// then volatile-zeroed before returning so only the derived PMK is retained.
pub fn parse_wpa_conf(text: &str) -> Result<WpaConfig, ConfigError> {
    let mut ssid_opt: Option<Vec<u8>> = None;
    let mut psk_opt: Option<Vec<u8>> = None;
    let mut freq = Band::Ghz24;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split on first '=' only.
        let mut parts = line.splitn(2, '=');
        let key = match parts.next() {
            Some(k) => k.trim(),
            None => return Err(ConfigError::Malformed),
        };
        let value = match parts.next() {
            Some(v) => v.trim(),
            None => return Err(ConfigError::Malformed),
        };

        match key {
            "ssid" => {
                ssid_opt = Some(value.as_bytes().to_vec());
            }
            "psk" => {
                psk_opt = Some(value.as_bytes().to_vec());
            }
            "freq" => {
                freq = match value {
                    "2.4" => Band::Ghz24,
                    "5" => Band::Ghz5,
                    _ => return Err(ConfigError::Malformed),
                };
            }
            _ => {} // ignore unknown keys
        }
    }

    let ssid = ssid_opt.ok_or(ConfigError::MissingSsid)?;
    let mut psk_bytes = psk_opt.ok_or(ConfigError::MissingPsk)?;

    // Validate passphrase length per IEEE 802.11 §H.4 (8..=63 octets).
    if psk_bytes.len() < 8 {
        zero_secret(&mut psk_bytes);
        return Err(ConfigError::ShortPsk);
    }
    if psk_bytes.len() > 63 {
        zero_secret(&mut psk_bytes);
        return Err(ConfigError::LongPsk);
    }

    // Derive PMK at parse time; keep only the PMK.
    let pmk = crypto_lib::hash::wpa_pmk(&psk_bytes, &ssid);

    // Volatile-zero the plaintext passphrase's HEAP CONTENTS (not just the Vec
    // header) so the secret does not linger after the PMK is derived. Mirrors
    // the `write_volatile` pattern in `crypto_lib::random`.
    zero_secret(&mut psk_bytes);
    drop(psk_bytes);

    Ok(WpaConfig { ssid, pmk, freq })
}

/// Volatile-zero a heap byte buffer so the optimizer cannot elide the wipe.
fn zero_secret(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid, uniquely-borrowed, aligned `u8`.
        unsafe { core::ptr::write_volatile(b, 0u8) };
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid config with all fields — ssid, psk, and freq=5.
    #[test]
    fn parse_valid() {
        let text = "ssid=Home\npsk=secret123\nfreq=5\n";
        let cfg = parse_wpa_conf(text).expect("valid config must parse");
        assert_eq!(cfg.ssid(), b"Home");
        assert_eq!(cfg.freq(), Band::Ghz5);
        // PMK must equal wpa_pmk("secret123", "Home")
        let expected_pmk = crypto_lib::hash::wpa_pmk(b"secret123", b"Home");
        assert_eq!(cfg.pmk(), &expected_pmk, "PMK must equal wpa_pmk result");
    }

    /// Missing `psk=` line must return `MissingPsk`.
    #[test]
    fn rejects_missing_psk() {
        let text = "ssid=TestNet\n";
        let err = parse_wpa_conf(text).expect_err("missing psk must fail");
        assert_eq!(err, ConfigError::MissingPsk);
    }

    /// A 7-character passphrase is too short (minimum is 8).
    #[test]
    fn rejects_short_psk() {
        let text = "ssid=Net\npsk=1234567\n"; // 7 chars
        let err = parse_wpa_conf(text).expect_err("short psk must fail");
        assert_eq!(err, ConfigError::ShortPsk);
    }

    /// A 64-character passphrase is too long (maximum is 63).
    #[test]
    fn rejects_long_psk() {
        // 64 chars
        let long_psk = "a".repeat(64);
        let text = alloc::format!("ssid=Net\npsk={long_psk}\n");
        let err = parse_wpa_conf(&text).expect_err("long psk must fail");
        assert_eq!(err, ConfigError::LongPsk);
    }

    /// `WpaConfig` must NOT expose a passphrase getter — only PMK and SSID.
    /// This is a structural test: the type simply has no such method.
    /// (Compiler enforces this; confirmed by code review.)
    #[test]
    fn no_passphrase_getter() {
        // The only way to retrieve the passphrase would be a method call.
        // This test asserts that the only bytes-returning methods are ssid() and pmk(),
        // which expose the SSID and the derived PMK — never the raw passphrase.
        let cfg = parse_wpa_conf("ssid=Net\npsk=passw0rd\n").unwrap();
        // ssid and pmk are the only getters.
        let _ssid: &[u8] = cfg.ssid();
        let _pmk: &[u8; 32] = cfg.pmk();
        // cfg.passphrase() would not compile — there is no such method.
    }
}
