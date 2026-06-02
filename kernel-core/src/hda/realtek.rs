//! Realtek HDA codec — host-testable pure logic — Phase 80c (Track E).
//!
//! Covers the Realtek-specific initialization sequences that the generic HDA
//! driver issues after pin-graph enumeration.  All functions are pure and
//! `no_std`-safe; no hardware access or syscalls occur here.
//!
//! ## Background
//!
//! Realtek AC'97/HDA codecs (ALC892, ALC1220, ALC887, ALC1150, …) power the
//! external amplifier off by default.  A "basic" HDA driver that only issues
//! `SET_PIN_WIDGET_CONTROL` + `SET_AMP_GAIN_MUTE` will produce silence on real
//! hardware because:
//!
//! 1. The EAPD (External Amplifier Power Down) pin is asserted (amp is off).
//! 2. Several boards additionally gate the amp through a GPIO line on the AFG.
//!
//! The canonical bring-up sequence for a Realtek output pin is therefore:
//!
//! ```text
//! [1] SET_EAPD_BTLENABLE(pin_nid, 0x02)  — de-assert EAPD → amp ON
//! [2] SET_GPIO_DIRECTION(afg, gpio_mask)  — GPIO output
//! [3] SET_GPIO_MASK(afg, gpio_mask)       — enable GPIO
//! [4] SET_GPIO_DATA(afg, gpio_mask)       — drive GPIO high → amp ON
//! ```
//!
//! Vendor-COEF writes (verb nibbles 0x5 / 0x4) are board-specific tuning
//! knobs and are **not** applied by default; [`coef_write_verbs`] is provided
//! for callers that have a board-specific COEF table.
//!
//! ## Pin-sense / jack detection
//!
//! The driver reads `GET_PIN_SENSE` (`0xF09`) on each output pin to obtain the
//! jack-presence bit (bit 31 of the response).  The [`realtek_output_select`]
//! function takes the decoded `jack_present` booleans and applies the Realtek
//! priority policy: prefer the internal speaker, then a plugged headphone out,
//! then line-out.
//!
//! ## References
//!
//! - Intel HDA spec §7.3 (verb encodings)
//! - Linux `sound/pci/hda/patch_realtek.c` (`alc_setup_gpio`, `alc_eapd_ctrl`)
//! - Redox `ihdad/src/realtek.rs`

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;

use super::verb::{encode_verb4, encode_verb12};
use super::widget::{
    DEFAULT_DEVICE_HP_OUT, DEFAULT_DEVICE_LINE_OUT, DEFAULT_DEVICE_SPEAKER, PinDefault,
    is_output_device,
};

// ---------------------------------------------------------------------------
// Vendor identification
// ---------------------------------------------------------------------------

/// Realtek Semiconductor PCI / HDA vendor ID (`0x10EC`).
///
/// Returned in the upper 16 bits of the `GET_PARAMETER(PARAM_VENDOR_ID)`
/// response (i.e. `response >> 16`).
pub const VENDOR_REALTEK: u16 = 0x10EC;

/// Returns `true` when `vendor` matches the Realtek vendor ID.
///
/// # Example
/// ```
/// # use kernel_core::hda::realtek::is_realtek;
/// assert!(is_realtek(0x10EC));
/// assert!(!is_realtek(0x8086));
/// ```
#[inline]
pub fn is_realtek(vendor: u16) -> bool {
    vendor == VENDOR_REALTEK
}

// ---------------------------------------------------------------------------
// EAPD bit constant
// ---------------------------------------------------------------------------

/// Payload for `SET_EAPD_BTLENABLE` that de-asserts the External Amplifier
/// Power Down signal (bit 1).
///
/// Per HDA spec §7.3.3.14: bit0 = balanced I/O enable; bit1 = EAPD enable.
/// Writing `0x02` turns the external amp ON.
pub const EAPD_ENABLE: u8 = 0x02;

// ---------------------------------------------------------------------------
// GPIO default mask
// ---------------------------------------------------------------------------

/// Conservative default GPIO mask: GPIO0 only.
///
/// Used by [`realtek_amp_enable_verbs`] as the GPIO mask when no board-specific
/// override is available.  Most desktop Realtek boards use GPIO0 or GPIO1;
/// this value is safe for single-GPIO configurations.
pub const GPIO_DEFAULT_MASK: u8 = 0x01;

// ---------------------------------------------------------------------------
// E.1 — Amp-enable verb sequences
// ---------------------------------------------------------------------------

/// Returns the single-verb sequence that de-asserts EAPD on `pin_nid`,
/// turning on the Realtek external amplifier for that pin.
///
/// Sequence: `[SET_EAPD_BTLENABLE(codec, pin_nid, EAPD_ENABLE)]`
///
/// This must be sent **before** `SET_AMP_GAIN_MUTE` on Realtek codecs that
/// default the external amp to off (ALC892, ALC1220, ALC887, ALC1150, …).
///
/// # Arguments
/// * `codec`   — HDA codec address (0–14).
/// * `pin_nid` — Pin Complex NID of the output pin to enable.
pub fn eapd_enable_verbs(codec: u8, pin_nid: u8) -> Vec<u32> {
    alloc::vec![encode_verb12(
        codec,
        pin_nid,
        super::VERB_SET_EAPD_BTLENABLE,
        EAPD_ENABLE,
    )]
}

/// Returns the three-verb GPIO sequence used on boards where EAPD is gated
/// through a GPIO line on the Audio Function Group (AFG) node.
///
/// Sequence:
/// ```text
/// [0] SET_GPIO_DIRECTION(codec, afg, gpio_mask)  — configure GPIO as output
/// [1] SET_GPIO_MASK(codec, afg, gpio_mask)        — unmask the GPIO pin
/// [2] SET_GPIO_DATA(codec, afg, gpio_mask)        — drive high → amp ON
/// ```
///
/// Reference: Linux `alc_setup_gpio()` in `patch_realtek.c`.
///
/// # Arguments
/// * `codec`    — HDA codec address.
/// * `afg`      — Audio Function Group NID (typically `0x01`).
/// * `gpio_mask`— Bitmask of GPIO lines to control (e.g. `0x01` for GPIO0).
pub fn gpio_eapd_verbs(codec: u8, afg: u8, gpio_mask: u8) -> Vec<u32> {
    alloc::vec![
        encode_verb12(codec, afg, super::VERB_SET_GPIO_DIRECTION, gpio_mask),
        encode_verb12(codec, afg, super::VERB_SET_GPIO_MASK, gpio_mask),
        encode_verb12(codec, afg, super::VERB_SET_GPIO_DATA, gpio_mask),
    ]
}

/// Returns the two-verb vendor-COEF write sequence.
///
/// Uses the **4-bit-verb encoding**:
/// - Nibble `0x5` = `SET_COEF_INDEX` (selects the coefficient register).
/// - Nibble `0x4` = `SET_PROC_COEF`  (writes the 16-bit coefficient value).
///
/// Note: the 12-bit opcode names `VERB_SET_COEF_INDEX` (0x500) and
/// `VERB_SET_PROC_COEF` (0x400) look like 12-bit verbs, but in practice
/// the HDA spec mandates the 4-bit-verb form for these opcodes because their
/// payloads are 16 bits wide.  The nibble values are therefore `0x5` and
/// `0x4` respectively.
///
/// # ⚠ Board-specific — do not call by default
///
/// COEF register layouts differ between Realtek models and board revisions.
/// Applying the wrong COEF sequence can mute, distort, or hardware-break
/// the codec.  This function is provided for callers that ship a verified,
/// model-specific COEF table.  [`realtek_amp_enable_verbs`] does **not**
/// call this function.
///
/// # Arguments
/// * `codec` — HDA codec address.
/// * `afg`   — AFG NID (coefficients are written on the AFG, not on a pin).
/// * `index` — Coefficient register index (0–255).
/// * `value` — 16-bit value to write into the selected coefficient register.
pub fn coef_write_verbs(codec: u8, afg: u8, index: u8, value: u16) -> Vec<u32> {
    alloc::vec![
        // Nibble 0x5 = SET_COEF_INDEX, payload = index (zero-extended to u16)
        encode_verb4(codec, afg, 0x5, index as u16),
        // Nibble 0x4 = SET_PROC_COEF, payload = 16-bit coefficient value
        encode_verb4(codec, afg, 0x4, value),
    ]
}

/// Returns the combined default amp-enable sequence for a Realtek codec.
///
/// Sequence (in order):
/// 1. `eapd_enable_verbs(codec, pin_nid)` — de-assert EAPD on the output pin.
/// 2. `gpio_eapd_verbs(codec, afg, GPIO_DEFAULT_MASK)` — drive GPIO0 high on the
///    AFG (the GPIO-gated EAPD path many ALC892/ALC1220 boards use for their
///    external amp). **Board-dependent, not universally safe:** GPIO0 is wired
///    per board and may instead drive a mute relay, an LED, or be an input, so
///    forcing it can mute/distort output on a board that does not gate EAPD
///    through it. Linux drives codec GPIOs only under a per-subsystem-id quirk;
///    m3OS has no quirk table yet, so this is applied to every Realtek codec as
///    a bring-up default and should move behind a subsystem-id gate once real
///    boards are characterized (see Track F).
///
/// COEF writes are intentionally **excluded**: they are model/board-specific
/// and must be applied separately via [`coef_write_verbs`] when a verified
/// COEF table is available.
///
/// # Arguments
/// * `codec`   — HDA codec address (0–14).
/// * `afg`     — AFG NID (typically `0x01`).
/// * `pin_nid` — Output pin NID to enable.
pub fn realtek_amp_enable_verbs(codec: u8, afg: u8, pin_nid: u8) -> Vec<u32> {
    let mut out = eapd_enable_verbs(codec, pin_nid);
    out.extend_from_slice(&gpio_eapd_verbs(codec, afg, GPIO_DEFAULT_MASK));
    out
}

// ---------------------------------------------------------------------------
// E.2 — Pin-default output selection (jack-presence aware)
// ---------------------------------------------------------------------------

/// Choose the best output pin NID from a list of enumerated output pins,
/// applying Realtek jack-detection priority policy.
///
/// Priority order (a bring-up heuristic in the spirit of Linux's `hda_generic`
/// auto-parser `alc_*` jack-presence handling — not a single named function):
///
/// 1. **Internal Speaker** (`DEFAULT_DEVICE_SPEAKER`) — always preferred when
///    present; internal speakers are not pluggable so `jack_present` is
///    irrelevant (always selected if found).
/// 2. **Headphone Out** (`DEFAULT_DEVICE_HP_OUT`) — selected only when
///    `jack_present` is `true`; if the jack is empty it is skipped.
/// 3. **Line Out** (`DEFAULT_DEVICE_LINE_OUT`) — fallback when no speaker and
///    no plugged HP is found.
///
/// Non-output pins (where [`is_output_device`] returns `false`) are ignored.
///
/// Returns the NID of the chosen pin, or `None` if no suitable pin exists.
///
/// # Note on `VERB_GET_PIN_SENSE` (0xF09)
///
/// The driver must issue `GET_PIN_SENSE` on each jack-capable pin and check
/// bit 31 of the response to populate the `jack_present` field before calling
/// this function.  The presence-detection logic itself is hardware-bound and
/// therefore not implemented here.
///
/// # Arguments
/// * `pins` — Slice of `(nid, PinDefault, jack_present)` tuples for all
///   enumerated pin complexes on the codec.
pub fn realtek_output_select(pins: &[(u8, PinDefault, bool /* jack_present */)]) -> Option<u8> {
    let mut speaker: Option<u8> = None;
    let mut hp_plugged: Option<u8> = None;
    let mut line_out: Option<u8> = None;

    for &(nid, ref pin, jack_present) in pins {
        if !is_output_device(pin) {
            continue;
        }
        match pin.default_device {
            // First speaker wins; internal speakers are always "present".
            DEFAULT_DEVICE_SPEAKER if speaker.is_none() => {
                speaker = Some(nid);
            }
            DEFAULT_DEVICE_HP_OUT if jack_present && hp_plugged.is_none() => {
                hp_plugged = Some(nid);
            }
            DEFAULT_DEVICE_LINE_OUT if line_out.is_none() => {
                line_out = Some(nid);
            }
            _ => {}
        }
    }

    // Priority: speaker > plugged HP > line-out.
    speaker.or(hp_plugged).or(line_out)
}

// ---------------------------------------------------------------------------
// E.3 — Volume / mute amp payload
// ---------------------------------------------------------------------------

/// Encode the 16-bit payload for `SET_AMP_GAIN_MUTE` (4-bit verb `0x3`).
///
/// Bit layout (HDA spec §7.3.3.7):
/// ```text
/// bit 15    — set-output (1 = output amp; 0 = input amp)
/// bit 14    — set-input  (1 = input amp;  0 = output amp) [opposite of bit 15]
/// bit 13    — set-left   (apply to left channel)
/// bit 12    — set-right  (apply to right channel)
/// bits[11:8]— index      (connection-list index for input amp; 0 for output)
/// bit  7    — mute       (1 = mute; 0 = unmute)
/// bits[6:0] — gain       (0x00–0x7F; codec-specific dB steps)
/// ```
///
/// Typically the driver issues two verbs per pin:
/// - One with `set_output=true, left=true, right=true, mute=false, gain=max`
///   to unmute and set output volume.
/// - One with `set_output=false` (input side) as needed.
///
/// # Arguments
/// * `set_output` — `true` → set bit 15 (output amp); `false` → set bit 14
///   (input amp).
/// * `left`  — Include left channel.
/// * `right` — Include right channel.
/// * `index` — Connection-list index (0 for output amps).
/// * `mute`  — `true` = mute; `false` = unmute.
/// * `gain`  — 7-bit gain value (`0x00`–`0x7F`).
pub fn amp_gain_mute_payload(
    set_output: bool,
    left: bool,
    right: bool,
    index: u8,
    mute: bool,
    gain: u8,
) -> u16 {
    let mut p: u16 = 0;
    if set_output {
        p |= 1 << 15; // output amp
    } else {
        p |= 1 << 14; // input amp
    }
    if left {
        p |= 1 << 13;
    }
    if right {
        p |= 1 << 12;
    }
    p |= ((index & 0x0F) as u16) << 8;
    if mute {
        p |= 1 << 7;
    }
    p |= (gain & 0x7F) as u16;
    p
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hda::verb::{encode_verb4, encode_verb12};
    use crate::hda::widget::PinDefault;
    use crate::hda::{
        VERB_SET_EAPD_BTLENABLE, VERB_SET_GPIO_DATA, VERB_SET_GPIO_DIRECTION, VERB_SET_GPIO_MASK,
    };

    // -----------------------------------------------------------------------
    // E.1 — amp-enable verb sequences
    // -----------------------------------------------------------------------

    /// `eapd_enable_verbs` must produce exactly one verb matching the
    /// canonical `SET_EAPD_BTLENABLE` encoding.
    #[test]
    fn eapd_verb_sequence() {
        let codec: u8 = 0;
        let pin: u8 = 0x14;

        let verbs = eapd_enable_verbs(codec, pin);
        assert_eq!(
            verbs.len(),
            1,
            "eapd_enable_verbs must return exactly 1 verb"
        );

        let expected = encode_verb12(codec, pin, VERB_SET_EAPD_BTLENABLE, EAPD_ENABLE);
        assert_eq!(
            verbs[0], expected,
            "eapd verb must be SET_EAPD_BTLENABLE(pin={pin:#04x}, payload={EAPD_ENABLE:#04x})"
        );

        // realtek_amp_enable_verbs must begin with the EAPD verb.
        let afg: u8 = 0x01;
        let full = realtek_amp_enable_verbs(codec, afg, pin);
        assert!(
            full.len() >= 1,
            "realtek_amp_enable_verbs must not be empty"
        );
        assert_eq!(
            full[0], expected,
            "realtek_amp_enable_verbs must start with the EAPD verb"
        );
        // Full sequence = 1 (EAPD) + 3 (GPIO) = 4 verbs.
        assert_eq!(
            full.len(),
            4,
            "realtek_amp_enable_verbs: expected 4 verbs (1 EAPD + 3 GPIO)"
        );
    }

    /// `gpio_eapd_verbs` must return exactly 3 verbs: DIRECTION, MASK, DATA,
    /// in that order, each carrying `gpio_mask` as the 8-bit payload.
    #[test]
    fn gpio_eapd_sequence() {
        let codec: u8 = 1;
        let afg: u8 = 0x01;
        let mask: u8 = 0x02;

        let verbs = gpio_eapd_verbs(codec, afg, mask);
        assert_eq!(
            verbs.len(),
            3,
            "gpio_eapd_verbs must return exactly 3 verbs"
        );

        let expected_direction = encode_verb12(codec, afg, VERB_SET_GPIO_DIRECTION, mask);
        let expected_mask = encode_verb12(codec, afg, VERB_SET_GPIO_MASK, mask);
        let expected_data = encode_verb12(codec, afg, VERB_SET_GPIO_DATA, mask);

        assert_eq!(
            verbs[0], expected_direction,
            "verb[0] must be SET_GPIO_DIRECTION"
        );
        assert_eq!(verbs[1], expected_mask, "verb[1] must be SET_GPIO_MASK");
        assert_eq!(verbs[2], expected_data, "verb[2] must be SET_GPIO_DATA");
    }

    // -----------------------------------------------------------------------
    // E.2 — output pin selection
    // -----------------------------------------------------------------------

    /// Helper to build a minimal `PinDefault` for testing.
    fn make_pin(default_device: u8, port_connectivity: u8) -> PinDefault {
        PinDefault {
            default_device,
            port_connectivity,
            location: 0,
            color: 0,
            sequence: 0,
            association: 1,
        }
    }

    use crate::hda::widget::{
        DEFAULT_DEVICE_HP_OUT, DEFAULT_DEVICE_LINE_OUT, DEFAULT_DEVICE_MIC_IN,
        DEFAULT_DEVICE_SPEAKER, PORT_CONN_FIXED, PORT_CONN_JACK,
    };

    /// When a speaker, unplugged HP, and line-out are all present, the speaker
    /// must be preferred.
    #[test]
    fn output_selection_speaker_wins() {
        let pins = alloc::vec![
            (
                0x14u8,
                make_pin(DEFAULT_DEVICE_SPEAKER, PORT_CONN_FIXED),
                false
            ),
            (
                0x15u8,
                make_pin(DEFAULT_DEVICE_HP_OUT, PORT_CONN_JACK),
                false
            ), // unplugged
            (
                0x16u8,
                make_pin(DEFAULT_DEVICE_LINE_OUT, PORT_CONN_JACK),
                false
            ),
        ];
        assert_eq!(
            realtek_output_select(&pins),
            Some(0x14),
            "speaker must be chosen over unplugged HP and line-out"
        );
    }

    /// When there is no speaker but an HP jack is plugged in, the HP is chosen.
    #[test]
    fn output_selection_hp_when_plugged() {
        let pins = alloc::vec![
            (
                0x15u8,
                make_pin(DEFAULT_DEVICE_HP_OUT, PORT_CONN_JACK),
                true
            ), // plugged
            (
                0x16u8,
                make_pin(DEFAULT_DEVICE_LINE_OUT, PORT_CONN_JACK),
                false
            ),
        ];
        assert_eq!(
            realtek_output_select(&pins),
            Some(0x15),
            "plugged HP must be chosen over line-out when there is no speaker"
        );
    }

    /// An unplugged HP must be skipped; line-out should be selected instead.
    #[test]
    fn output_selection_hp_unplugged_falls_through() {
        let pins = alloc::vec![
            (
                0x15u8,
                make_pin(DEFAULT_DEVICE_HP_OUT, PORT_CONN_JACK),
                false
            ), // unplugged
            (
                0x16u8,
                make_pin(DEFAULT_DEVICE_LINE_OUT, PORT_CONN_JACK),
                false
            ),
        ];
        assert_eq!(
            realtek_output_select(&pins),
            Some(0x16),
            "unplugged HP must be skipped; line-out must be chosen"
        );
    }

    /// Non-output pins (mic, etc.) must be ignored entirely.
    #[test]
    fn output_selection_ignores_non_output_pins() {
        let pins = alloc::vec![
            (
                0x18u8,
                make_pin(DEFAULT_DEVICE_MIC_IN, PORT_CONN_JACK),
                false
            ),
            (
                0x16u8,
                make_pin(DEFAULT_DEVICE_LINE_OUT, PORT_CONN_JACK),
                false
            ),
        ];
        assert_eq!(
            realtek_output_select(&pins),
            Some(0x16),
            "mic-in must be ignored; line-out selected"
        );
    }

    /// Empty pin list must return None.
    #[test]
    fn output_selection_empty() {
        let pins: alloc::vec::Vec<(u8, PinDefault, bool)> = alloc::vec![];
        assert_eq!(realtek_output_select(&pins), None);
    }

    // -----------------------------------------------------------------------
    // E.3 — amp gain/mute payload bit layout
    // -----------------------------------------------------------------------

    /// Verify the bit layout for a typical unmuted full-volume output-amp command
    /// (set_output, both channels, index=0, unmute, gain=0x7F).
    #[test]
    fn amp_gain_mute_payload_output_full_volume() {
        // set_output=true, left=true, right=true, index=0, mute=false, gain=0x7F
        let p = amp_gain_mute_payload(true, true, true, 0, false, 0x7F);

        assert_ne!(p & (1 << 15), 0, "bit15 (set-output) must be set");
        assert_eq!(p & (1 << 14), 0, "bit14 (set-input) must be clear");
        assert_ne!(p & (1 << 13), 0, "bit13 (set-left) must be set");
        assert_ne!(p & (1 << 12), 0, "bit12 (set-right) must be set");
        assert_eq!((p >> 8) & 0xF, 0, "index must be 0");
        assert_eq!(p & (1 << 7), 0, "bit7 (mute) must be clear");
        assert_eq!(p & 0x7F, 0x7F, "gain bits[6:0] must be 0x7F");

        // Canonical value: 0b1011_0000_0111_1111 = 0xB07F
        assert_eq!(p, 0xB07F, "full output payload must equal 0xB07F");
    }

    /// Verify the mute case: set_output, both channels, index=0, mute=true, gain=0.
    #[test]
    fn amp_gain_mute_payload_mute() {
        let p = amp_gain_mute_payload(true, true, true, 0, true, 0x00);

        assert_ne!(p & (1 << 15), 0, "bit15 (set-output) must be set");
        assert_ne!(p & (1 << 7), 0, "bit7 (mute) must be set");
        assert_eq!(p & 0x7F, 0, "gain must be 0");

        // 0b1011_0000_1000_0000 = 0xB080
        assert_eq!(p, 0xB080, "muted output payload must equal 0xB080");
    }

    /// Verify input-amp form: set_output=false sets bit14 not bit15.
    #[test]
    fn amp_gain_mute_payload_input_form() {
        let p = amp_gain_mute_payload(false, true, false, 0, false, 0x00);
        assert_eq!(p & (1 << 15), 0, "bit15 (set-output) must be clear");
        assert_ne!(p & (1 << 14), 0, "bit14 (set-input) must be set");
        assert_ne!(p & (1 << 13), 0, "bit13 (set-left) must be set");
        assert_eq!(p & (1 << 12), 0, "bit12 (set-right) must be clear");
    }

    // -----------------------------------------------------------------------
    // COEF — shape and optional-use test
    // -----------------------------------------------------------------------

    /// `coef_write_verbs` must return exactly 2 verbs, both using the 4-bit
    /// verb form (nibbles 0x5 and 0x4 in bits[19:16]).
    #[test]
    fn coef_is_present_but_optional() {
        let codec: u8 = 0;
        let afg: u8 = 0x01;
        let index: u8 = 0x07;
        let value: u16 = 0x1234;

        let verbs = coef_write_verbs(codec, afg, index, value);
        assert_eq!(
            verbs.len(),
            2,
            "coef_write_verbs must return exactly 2 verbs"
        );

        // Verb 0: SET_COEF_INDEX (nibble 0x5), payload = index
        let v0 = verbs[0];
        assert_eq!(
            (v0 >> 16) & 0xF,
            0x5,
            "first coef verb must use nibble 0x5 (SET_COEF_INDEX)"
        );
        assert_eq!(
            v0 & 0xFFFF,
            index as u32,
            "first coef verb payload must be the index"
        );

        // Verb 1: SET_PROC_COEF (nibble 0x4), payload = value
        let v1 = verbs[1];
        assert_eq!(
            (v1 >> 16) & 0xF,
            0x4,
            "second coef verb must use nibble 0x4 (SET_PROC_COEF)"
        );
        assert_eq!(
            v1 & 0xFFFF,
            value as u32,
            "second coef verb payload must be the coefficient value"
        );

        // Cross-check against encode_verb4 directly.
        assert_eq!(v0, encode_verb4(codec, afg, 0x5, index as u16));
        assert_eq!(v1, encode_verb4(codec, afg, 0x4, value));
    }

    // -----------------------------------------------------------------------
    // Vendor identification
    // -----------------------------------------------------------------------

    #[test]
    fn vendor_identification() {
        assert!(is_realtek(VENDOR_REALTEK));
        assert!(is_realtek(0x10EC));
        assert!(!is_realtek(0x8086), "Intel must not match Realtek");
        assert!(!is_realtek(0x1022), "AMD must not match Realtek");
        assert!(!is_realtek(0x0000));
    }
}
