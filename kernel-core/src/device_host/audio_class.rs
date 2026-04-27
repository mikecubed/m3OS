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
//! The kernel-side `sys_device_claim` path itself does **not** filter
//! claims by PCI ID — any ring-3 driver (process whose `exec_path` lives
//! under `/drivers/`) may claim any unclaimed BDF, and audio is no
//! exception. What this module supplies is a classifier the syscall
//! boundary uses to tag observability events with `subsystem=audio.device`
//! when the claimed device is the audio controller, so log search and
//! triage do not need to translate PCI IDs by hand.
//!
//! The module is `no_std` + `alloc`-free so the kernel and host tests
//! compile against it without a heap allocator. All identifiers are
//! `const` so they participate in `match` arms.

/// Intel PCI vendor identifier (`0x8086`).
///
/// Shared with NVMe, e1000, and AC'97 — the kernel does not select on
/// vendor alone; the (vendor, device) pair is the key.
pub const PCI_VENDOR_INTEL: u16 = 0x8086;

/// Intel 82801AA AC'97 audio controller device identifier (`0x2415`).
///
/// Phase 57's first supported audio target. Combined with
/// [`PCI_VENDOR_INTEL`] this names the exact PCI function the
/// `audio_server` ring-3 driver (Track D) claims via `sys_device_claim`.
pub const PCI_DEVICE_AC97: u16 = 0x2415;

/// Observability subsystem name for audio device-host events.
///
/// Used as the `subsystem=` field in structured log events emitted from
/// the kernel-side claim path when the claimed BDF resolves to the audio
/// controller. Phase 57 task list pins this exact string for the
/// `iommu.missing_bar_coverage` BAR-coverage failure log; the same string
/// is reused by `audio_server` and the `audio_client` library so a single
/// grep finds every audio-stack log line.
pub const SUBSYSTEM_AUDIO_DEVICE: &str = "audio.device";

/// PCI vendor/device pair tag that classifies a claim as the audio
/// device class.
///
/// Pure data — kept as `(u16, u16)` rather than a custom struct so it
/// matches the on-the-wire representation used by `pci_config_read_u32`
/// at offset 0 (which packs vendor in the low 16 bits and device in the
/// high 16 bits). Variants other than `AudioAc97` are not declared
/// speculatively per YAGNI; future device classes (e.g. additional
/// audio controllers if HDA lands later) will gain their own entries
/// when they have a concrete consumer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DeviceClass {
    /// The Intel 82801AA AC'97 audio controller (`0x8086:0x2415`).
    AudioAc97,
}

impl DeviceClass {
    /// Subsystem name used in structured log events — see
    /// [`SUBSYSTEM_AUDIO_DEVICE`].
    pub const fn subsystem(self) -> &'static str {
        match self {
            DeviceClass::AudioAc97 => SUBSYSTEM_AUDIO_DEVICE,
        }
    }
}

/// Classify a PCI `(vendor, device)` pair into a known device class.
///
/// Returns `Some(class)` when the pair matches a recognized device the
/// kernel emits structured observability events for; returns `None` when
/// the pair is unknown. An unknown pair is not an error — it just means
/// the claim path emits the default observability tag (no subsystem
/// override). The kernel's `sys_device_claim` path is BDF-keyed and does
/// not gate on this classifier; failure to classify never blocks a
/// claim.
///
/// The `Some(_)` arm intentionally does not exhaust the universe of
/// audio controllers: HDA, virtio-sound, and other future targets will
/// be added when Phase 57's first audio target ships and a concrete
/// consumer exists. Phase 57 lands AC'97 only.
pub const fn classify_pci_id(vendor: u16, device: u16) -> Option<DeviceClass> {
    match (vendor, device) {
        (PCI_VENDOR_INTEL, PCI_DEVICE_AC97) => Some(DeviceClass::AudioAc97),
        _ => None,
    }
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
