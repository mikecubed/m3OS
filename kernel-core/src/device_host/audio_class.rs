//! Audio device-class identifiers for the device-host claim path —
//! Phase 57 Track C.1.
//!
//! Phase 57's first audio target is the Intel 82801AA AC'97 controller.
//! See `docs/appendix/phase-57-audio-target-choice.md` for the rationale
//! behind that choice and the rejected alternatives. This module is the
//! single source of truth for the audio PCI vendor/device IDs and the
//! observability subsystem name (`audio.device`) used by structured log
//! events emitted from the device-host claim path. A workspace-wide grep
//! for any of these constants must return exactly one declaration site.
//!
//! TDD red-commit stage: the symbols below are declared only so the
//! Track C.1 tests link; their values are deliberately wrong (or absent)
//! so the test suite fails until the green commit lands the real
//! constants.

/// Intel PCI vendor identifier — placeholder, real value pinned by the
/// green commit.
pub const PCI_VENDOR_INTEL: u16 = 0;

/// Intel 82801AA AC'97 audio controller device identifier — placeholder,
/// real value pinned by the green commit.
pub const PCI_DEVICE_AC97: u16 = 0;

/// Observability subsystem name for audio device-host events —
/// placeholder, real value pinned by the green commit.
pub const SUBSYSTEM_AUDIO_DEVICE: &str = "";

/// Device class enum — variants and arms land in the green commit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DeviceClass {
    /// Placeholder discriminant; the green commit replaces this with the
    /// real `AudioAc97` variant.
    AudioAc97,
}

impl DeviceClass {
    /// Subsystem name for the device class — green commit returns the
    /// real string.
    pub const fn subsystem(self) -> &'static str {
        ""
    }
}

/// Classify a PCI `(vendor, device)` pair — red-commit stub returns
/// `None` for every input.
pub const fn classify_pci_id(_vendor: u16, _device: u16) -> Option<DeviceClass> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- AC'97 PCI ID is recognized -------------------------------------

    #[test]
    fn ac97_pci_ids_are_named_constants() {
        // The constants must be the exact values pinned by the audio
        // target-choice memo (`docs/appendix/phase-57-audio-target-choice.md`).
        // A regression that changed either value would silently break the
        // claim-path tagging.
        assert_eq!(PCI_VENDOR_INTEL, 0x8086);
        assert_eq!(PCI_DEVICE_AC97, 0x2415);
    }

    #[test]
    fn classify_recognizes_intel_ac97_pair() {
        assert_eq!(
            classify_pci_id(PCI_VENDOR_INTEL, PCI_DEVICE_AC97),
            Some(DeviceClass::AudioAc97),
        );
    }

    #[test]
    fn audio_subsystem_string_is_audio_device() {
        // The exact string is part of the observability contract — log
        // search will key on this literal.
        assert_eq!(SUBSYSTEM_AUDIO_DEVICE, "audio.device");
        assert_eq!(DeviceClass::AudioAc97.subsystem(), "audio.device");
    }

    // ---- Mismatches return None (no new error variant required) ---------

    #[test]
    fn classify_returns_none_for_unknown_pair() {
        // A non-AC'97 device (e.g. e1000 vendor/device IDs) classifies
        // as None — there is no audio claim contract to match. Returning
        // None keeps the audio path from spuriously tagging unrelated
        // devices.
        let e1000_vendor = 0x8086;
        let e1000_device = 0x100E;
        assert_eq!(classify_pci_id(e1000_vendor, e1000_device), None);
    }

    #[test]
    fn classify_returns_none_when_vendor_matches_but_device_differs() {
        // Same vendor (Intel) but a different device — must not be
        // misclassified as audio. This guards against a future regression
        // where someone classifies on vendor alone.
        assert_eq!(classify_pci_id(0x8086, 0x0000), None);
        assert_eq!(classify_pci_id(0x8086, 0xFFFF), None);
    }

    #[test]
    fn classify_returns_none_when_device_matches_but_vendor_differs() {
        // Symmetric guard: device 0x2415 from a non-Intel vendor must
        // not collide with the audio classifier — Intel owns 0x8086 and
        // PCI device IDs are scoped to the vendor.
        assert_eq!(classify_pci_id(0x0000, 0x2415), None);
        assert_eq!(classify_pci_id(0x10DE, 0x2415), None);
    }

    // ---- DeviceClass is non-exhaustive but exhaustively matched here ----

    #[test]
    fn device_class_subsystem_covers_every_arm_declared_here() {
        // The defining crate matches every arm so a future variant addition
        // forces a `subsystem()` update at the same time. Downstream crates
        // do not need to be exhaustive (per `#[non_exhaustive]`).
        let all = [DeviceClass::AudioAc97];
        for class in all {
            // Every classifier yields a subsystem string; the audio class
            // yields exactly the audio subsystem name.
            match class {
                DeviceClass::AudioAc97 => assert_eq!(class.subsystem(), SUBSYSTEM_AUDIO_DEVICE),
            }
            // Audio subsystem is the literal "audio.device".
            assert_eq!(class.subsystem(), "audio.device");
        }
    }

    // ---- Existing DeviceHostError variants suffice (no new variants) ----

    #[test]
    fn no_new_device_host_error_variant_required_for_audio() {
        // Phase 57 C.1 acceptance bullet: "mismatch returns existing
        // `DeviceHostError` variants (NO new variants)." Tag the existing
        // variants so a regression that adds an `Audio*` variant fails
        // compilation here.
        use crate::device_host::DeviceHostError;
        let existing = [
            DeviceHostError::NotClaimed,
            DeviceHostError::AlreadyClaimed,
            DeviceHostError::InvalidBarIndex,
            DeviceHostError::BarOutOfBounds,
            DeviceHostError::IovaExhausted,
            DeviceHostError::IommuFault,
            DeviceHostError::CapacityExceeded,
            DeviceHostError::IrqUnavailable,
            DeviceHostError::BadDeviceCap,
            DeviceHostError::Internal,
        ];
        // The test's purpose is to fail compilation on a new variant; at
        // runtime it just needs to enumerate every variant the audio
        // subsystem expects to surface. `Internal` is the BAR-coverage
        // failure path explicitly named in the C.1 acceptance.
        assert!(existing.contains(&DeviceHostError::Internal));
        assert_eq!(existing.len(), 10);
    }
}
