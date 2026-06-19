//! USB Audio Class (UAC 1.0) pure logic — Phase 92c Track E.
//!
//! Hardware-free helpers the ring-3 `usb-audio` driver uses to bring up a USB
//! speaker/headset and stream PCM out over its isochronous OUT endpoint:
//!
//! * [`find_isoch_out_stream`] walks a parsed configuration tree and locates the
//!   AudioStreaming interface alt-setting that carries an isochronous OUT
//!   endpoint, returning the interface/alt-setting to activate and the
//!   endpoint's xHCI Device Context Index + packet size.
//! * [`set_interface_setup`] builds the standard `SET_INTERFACE` SETUP packet
//!   that activates that alt-setting (alt 0 is the zero-bandwidth idle setting;
//!   the isoch endpoint only exists on alt ≥ 1).
//! * [`set_sample_rate_setup`] / [`sample_rate_bytes`] build the UAC 1.0
//!   class-specific endpoint `SET_CUR(SAMPLING_FREQ_CONTROL)` request that pins
//!   the sample rate.
//!
//! No MMIO/DMA — host-testable via
//! `cargo test -p kernel-core --target x86_64-unknown-linux-gnu usb::uac`.

extern crate alloc;

use crate::usb::descriptor::{
    CLASS_AUDIO, ParsedConfig, SUBCLASS_AUDIO_STREAMING, TRANSFER_TYPE_ISOCH,
};
use crate::usb::xhci::trb::dci;

/// UAC 1.0 class-specific request: `SET_CUR` (set current setting).
pub const UAC_SET_CUR: u8 = 0x01;
/// UAC 1.0 endpoint control selector: Sampling Frequency Control.
pub const UAC_SAMPLING_FREQ_CONTROL: u8 = 0x01;

/// The isochronous-OUT streaming endpoint discovered on a UAC device, plus the
/// interface/alt-setting that must be selected to activate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UacStreamInfo {
    /// `bInterfaceNumber` of the AudioStreaming interface (the `SET_INTERFACE`
    /// `wIndex`).
    pub interface_num: u8,
    /// `bAlternateSetting` carrying the isochronous endpoint (the
    /// `SET_INTERFACE` `wValue`). Alt 0 is the zero-bandwidth idle setting.
    pub alt_setting: u8,
    /// `bEndpointAddress` of the isochronous OUT endpoint.
    pub ep_address: u8,
    /// xHCI Device Context Index of the isochronous OUT endpoint.
    pub ep_dci: u8,
    /// `wMaxPacketSize` of the isochronous OUT endpoint.
    pub mps: u16,
}

/// Find the first AudioStreaming interface alt-setting that exposes an
/// isochronous **OUT** endpoint (the UAC PCM-out path), or `None` if the device
/// exposes no such stream. The xHCI enumerator has already configured every
/// endpoint context from the same parsed tree, so the returned `ep_dci` names a
/// ring that already exists — the driver only has to `SET_INTERFACE` the
/// alt-setting and start submitting isochronous TRBs.
pub fn find_isoch_out_stream(cfg: &ParsedConfig) -> Option<UacStreamInfo> {
    for iface in &cfg.interfaces {
        let i = &iface.interface;
        if i.b_interface_class != CLASS_AUDIO || i.b_interface_sub_class != SUBCLASS_AUDIO_STREAMING
        {
            continue;
        }
        for ep in &iface.endpoints {
            if ep.transfer_type() == TRANSFER_TYPE_ISOCH && !ep.is_in() {
                return Some(UacStreamInfo {
                    interface_num: i.b_interface_number,
                    alt_setting: i.b_alternate_setting,
                    ep_address: ep.b_endpoint_address,
                    ep_dci: dci(ep.endpoint_number(), false),
                    mps: ep.w_max_packet_size,
                });
            }
        }
    }
    None
}

/// Build the standard `SET_INTERFACE` SETUP packet (USB 2.0 §9.4.10) that
/// selects alt-setting `alt` on interface `iface`. `bmRequestType = 0x01`
/// (host→device, standard, interface recipient), `bRequest = 0x0B`, no data
/// stage (`wLength = 0`).
pub const fn set_interface_setup(iface: u8, alt: u8) -> [u8; 8] {
    [
        0x01,  // bmRequestType: OUT | Standard | Interface
        0x0B,  // bRequest: SET_INTERFACE
        alt,   // wValue lo = alternate setting
        0x00,  // wValue hi
        iface, // wIndex lo = interface number
        0x00,  // wIndex hi
        0x00,  // wLength lo
        0x00,  // wLength hi
    ]
}

/// The UAC 1.0 3-byte little-endian sample-rate value carried in the
/// `SET_CUR(SAMPLING_FREQ_CONTROL)` data stage (24-bit Hz).
pub const fn sample_rate_bytes(hz: u32) -> [u8; 3] {
    [hz as u8, (hz >> 8) as u8, (hz >> 16) as u8]
}

/// Build the UAC 1.0 class-specific endpoint `SET_CUR(SAMPLING_FREQ_CONTROL)`
/// SETUP packet that pins the sampling frequency on the isochronous endpoint
/// `ep_address`. `bmRequestType = 0x22` (host→device, class, endpoint
/// recipient), `bRequest = SET_CUR`, `wValue = SAMPLING_FREQ_CONTROL << 8`,
/// `wIndex = ep_address`, `wLength = 3` (a 3-byte rate follows in the data
/// stage — see [`sample_rate_bytes`]).
pub const fn set_sample_rate_setup(ep_address: u8) -> [u8; 8] {
    [
        0x22,                      // bmRequestType: OUT | Class | Endpoint
        UAC_SET_CUR,               // bRequest: SET_CUR
        0x00,                      // wValue lo (control unit = 0)
        UAC_SAMPLING_FREQ_CONTROL, // wValue hi = SAMPLING_FREQ_CONTROL
        ep_address,                // wIndex lo = endpoint address
        0x00,                      // wIndex hi
        0x03,                      // wLength lo = 3
        0x00,                      // wLength hi
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb::descriptor::parse_config_tree;
    use crate::usb::xhci::trb::dci as compute_dci;

    /// A QEMU-style UAC 1.0 speaker configuration blob:
    ///   Config(9)
    ///   + AudioControl interface (9, class 0x01 / subclass 0x01, 0 endpoints)
    ///   + AudioStreaming interface alt 0 (9, class 0x01 / subclass 0x02, 0 eps)
    ///   + AudioStreaming interface alt 1 (9, class 0x01 / subclass 0x02, 1 ep)
    ///   + isoch OUT endpoint (9, audio endpoint descriptor)
    /// wTotalLength = 9 + 9 + 9 + 9 + 9 = 45 = 0x2D
    const UAC_SPEAKER_CONFIG: &[u8] = &[
        // Configuration descriptor
        0x09, 0x02, 0x2D, 0x00, 0x02, 0x01, 0x00, 0x80, 0x32,
        // AudioControl interface 0, alt 0, class 0x01 subclass 0x01, 0 endpoints
        0x09, 0x04, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00,
        // AudioStreaming interface 1, alt 0, class 0x01 subclass 0x02, 0 endpoints
        0x09, 0x04, 0x01, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00,
        // AudioStreaming interface 1, alt 1, class 0x01 subclass 0x02, 1 endpoint
        0x09, 0x04, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00,
        // Isochronous OUT endpoint, 9-byte audio endpoint descriptor:
        // bEndpointAddress=0x01 (OUT, ep1), bmAttributes=0x09 (isoch/adaptive),
        // wMaxPacketSize=192 (0x00C0), bInterval=1, bRefresh=0, bSynchAddress=0
        0x09, 0x05, 0x01, 0x09, 0xC0, 0x00, 0x01, 0x00, 0x00,
    ];

    #[test]
    fn finds_isoch_out_stream_on_alt1() {
        let cfg = parse_config_tree(UAC_SPEAKER_CONFIG).expect("parse");
        let info = find_isoch_out_stream(&cfg).expect("isoch OUT stream found");
        assert_eq!(info.interface_num, 1);
        assert_eq!(info.alt_setting, 1);
        assert_eq!(info.ep_address, 0x01);
        // EP1 OUT → DCI = 2*1 + 0 = 2.
        assert_eq!(info.ep_dci, compute_dci(1, false));
        assert_eq!(info.mps, 192);
    }

    #[test]
    fn no_stream_when_no_isoch_out() {
        // Only the AudioControl + alt-0 (no endpoint) interfaces — no isoch OUT.
        const NO_STREAM: &[u8] = &[
            0x09, 0x02, 0x1B, 0x00, 0x02, 0x01, 0x00, 0x80, 0x32, 0x09, 0x04, 0x00, 0x00, 0x00,
            0x01, 0x01, 0x00, 0x00, 0x09, 0x04, 0x01, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00,
        ];
        let cfg = parse_config_tree(NO_STREAM).expect("parse");
        assert_eq!(find_isoch_out_stream(&cfg), None);
    }

    #[test]
    fn set_interface_setup_encodes_alt() {
        let s = set_interface_setup(1, 1);
        assert_eq!(s, [0x01, 0x0B, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // wLength must be 0 (no data stage).
        assert_eq!(u16::from_le_bytes([s[6], s[7]]), 0);
    }

    #[test]
    fn set_sample_rate_setup_encodes_freq_control() {
        let s = set_sample_rate_setup(0x01);
        assert_eq!(s[0], 0x22, "class | endpoint OUT");
        assert_eq!(s[1], UAC_SET_CUR);
        assert_eq!(s[3], UAC_SAMPLING_FREQ_CONTROL);
        assert_eq!(s[4], 0x01, "wIndex lo = endpoint address");
        assert_eq!(u16::from_le_bytes([s[6], s[7]]), 3, "wLength = 3");
        // 48000 Hz little-endian 24-bit = 0x80 0xBB 0x00.
        assert_eq!(sample_rate_bytes(48_000), [0x80, 0xBB, 0x00]);
    }
}
