//! HDA widget-graph logic — host-testable pure logic (Phase 80b, Track C.1).
//!
//! HDA codecs expose a directed graph of NID (Node ID) *widgets*.  This
//! module provides:
//!
//! - [`WidgetType`] extracted from `AUDIO_WIDGET_CAPABILITIES` bits[23:20].
//! - [`PinDefault`] decoded from `GET_CONFIG_DEFAULT` (HDA spec §7.3.3.31),
//!   plus helpers for classifying pins as output devices / jacks / fixed.
//! - [`find_path_to_dac`]: BFS over a synthetic widget graph to find an
//!   output path from a pin to an `AudioOutput` (DAC) node.
//! - [`CodecSummary`] / [`select_codec`]: pick the best codec for analog
//!   audio output from the set of enumerated codecs.
//!
//! All logic is `no_std`-compatible.  `alloc::vec::Vec` is used for
//! connection lists and path results; callers must initialise the heap
//! before calling path/codec APIs.

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// WidgetType
// ---------------------------------------------------------------------------

/// Widget type extracted from bits[23:20] of `AUDIO_WIDGET_CAPABILITIES`
/// (`GET_PARAMETER 0x09`), per HDA spec §7.3.6.6 table 82.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetType {
    /// Type 0x0 — Audio Output (DAC).
    AudioOutput,
    /// Type 0x1 — Audio Input (ADC).
    AudioInput,
    /// Type 0x2 — Audio Mixer.
    Mixer,
    /// Type 0x3 — Audio Selector (mux).
    Selector,
    /// Type 0x4 — Pin Complex (physical jack / speaker / etc.).
    PinComplex,
    /// Type 0x5 — Power Widget.
    PowerWidget,
    /// Type 0x6 — Volume Knob.
    VolumeKnob,
    /// Type 0x7 — Beep Generator.
    BeepGenerator,
    /// Type 0xF — Vendor Defined.
    VendorDefined,
    /// Any other value (reserved / unknown).
    Other(u8),
}

/// Decode the widget type from the `AUDIO_WIDGET_CAPABILITIES` parameter
/// (bits[23:20], per HDA spec §7.3.6.6 table 82).
pub fn widget_type(audio_widget_caps: u32) -> WidgetType {
    let ty = ((audio_widget_caps >> 20) & 0xF) as u8;
    match ty {
        0x0 => WidgetType::AudioOutput,
        0x1 => WidgetType::AudioInput,
        0x2 => WidgetType::Mixer,
        0x3 => WidgetType::Selector,
        0x4 => WidgetType::PinComplex,
        0x5 => WidgetType::PowerWidget,
        0x6 => WidgetType::VolumeKnob,
        0x7 => WidgetType::BeepGenerator,
        0xF => WidgetType::VendorDefined,
        other => WidgetType::Other(other),
    }
}

// ---------------------------------------------------------------------------
// PinDefault — GET_CONFIG_DEFAULT layout (HDA spec §7.3.3.31)
// ---------------------------------------------------------------------------

// Default-device codes (bits[23:20] of GET_CONFIG_DEFAULT).
/// Line Out (rear green jack, typical desktop).
pub const DEFAULT_DEVICE_LINE_OUT: u8 = 0x0;
/// Internal Speaker (fixed, laptop LCD lid speaker).
pub const DEFAULT_DEVICE_SPEAKER: u8 = 0x1;
/// Headphone Out (front panel or combo jack).
pub const DEFAULT_DEVICE_HP_OUT: u8 = 0x2;
/// CD Audio input.
pub const DEFAULT_DEVICE_CD: u8 = 0x3;
/// SPDIF Out.
pub const DEFAULT_DEVICE_SPDIF_OUT: u8 = 0x4;
/// Digital Other Out.
pub const DEFAULT_DEVICE_DIGITAL_OTHER_OUT: u8 = 0x5;
/// Modem Line Side.
pub const DEFAULT_DEVICE_MODEM_LINE: u8 = 0x6;
/// Modem Handset Side.
pub const DEFAULT_DEVICE_MODEM_HANDSET: u8 = 0x7;
/// Line In.
pub const DEFAULT_DEVICE_LINE_IN: u8 = 0x8;
/// AUX input.
pub const DEFAULT_DEVICE_AUX: u8 = 0x9;
/// Microphone In.
pub const DEFAULT_DEVICE_MIC_IN: u8 = 0xA;
/// Telephony.
pub const DEFAULT_DEVICE_TELEPHONY: u8 = 0xB;
/// SPDIF In.
pub const DEFAULT_DEVICE_SPDIF_IN: u8 = 0xC;
/// Digital Other In.
pub const DEFAULT_DEVICE_DIGITAL_OTHER_IN: u8 = 0xD;
/// Other (reserved/vendor).
pub const DEFAULT_DEVICE_OTHER: u8 = 0xF;

// Port-connectivity codes (bits[31:30] of GET_CONFIG_DEFAULT).
/// Jack: a physical jack present on the chassis.
pub const PORT_CONN_JACK: u8 = 0x0;
/// Not present / no physical connection.
pub const PORT_CONN_NONE: u8 = 0x1;
/// Fixed-function / internal (soldered speaker, laptop display mic, etc.).
pub const PORT_CONN_FIXED: u8 = 0x2;
/// Both jack and internal fixed connection (combo).
pub const PORT_CONN_JACK_AND_FIXED: u8 = 0x3;

/// Decoded fields from the `GET_CONFIG_DEFAULT` verb response
/// (HDA spec §7.3.3.31).
///
/// Bit layout:
/// ```text
/// [31:30] Port Connectivity
/// [29:24] Location
/// [23:20] Default Device
/// [19:16] Connection Type  (not decoded here — not needed for routing)
/// [15:12] Color
/// [11: 8] Misc             (not decoded here)
/// [ 7: 4] Default Association
/// [ 3: 0] Sequence
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinDefault {
    /// `DEFAULT_DEVICE_*` constant describing the device role.
    pub default_device: u8,
    /// `PORT_CONN_*` constant: jack, fixed, or absent.
    pub port_connectivity: u8,
    /// Location code (bits[29:24]).
    pub location: u8,
    /// Color code (bits[15:12]).
    pub color: u8,
    /// Sequence within the association group (bits[3:0]).
    pub sequence: u8,
    /// Association group tag (bits[7:4]); 0 = unassociated.
    pub association: u8,
}

/// Decode `GET_CONFIG_DEFAULT` into structured [`PinDefault`] fields.
pub fn decode_pin_default(config_default: u32) -> PinDefault {
    PinDefault {
        sequence: (config_default & 0x0F) as u8,
        association: ((config_default >> 4) & 0x0F) as u8,
        // bits[11:8] = misc (skipped)
        color: ((config_default >> 12) & 0x0F) as u8,
        // bits[19:16] = connection type (skipped)
        default_device: ((config_default >> 20) & 0x0F) as u8,
        location: ((config_default >> 24) & 0x3F) as u8,
        port_connectivity: ((config_default >> 30) & 0x03) as u8,
    }
}

/// Returns `true` when the pin's default device is a conventional analog
/// output: `LineOut`, `Speaker`, or `HPOut`.
pub fn is_output_device(d: &PinDefault) -> bool {
    matches!(
        d.default_device,
        DEFAULT_DEVICE_LINE_OUT | DEFAULT_DEVICE_SPEAKER | DEFAULT_DEVICE_HP_OUT
    )
}

/// Returns `true` when the pin is connected via a physical jack
/// (`PORT_CONN_JACK`).
pub fn is_jack(d: &PinDefault) -> bool {
    d.port_connectivity == PORT_CONN_JACK
}

/// Returns `true` when the pin is a fixed / internal connection (soldered
/// speaker, laptop LCD-hinge mic, etc.), i.e. `PORT_CONN_FIXED`.
pub fn is_fixed(d: &PinDefault) -> bool {
    d.port_connectivity == PORT_CONN_FIXED
}

// ---------------------------------------------------------------------------
// Widget-graph path search
// ---------------------------------------------------------------------------

/// A single node in a synthetic (or parsed) HDA widget graph.
///
/// `connections` lists the NIDs that this node's connection-list entry
/// points to — i.e. the nodes "downstream" from this one when traversing
/// toward a DAC.
#[derive(Debug, Clone)]
pub struct WidgetNode {
    /// Node ID of this widget.
    pub nid: u8,
    /// Widget type.
    pub kind: WidgetType,
    /// Connection-list NIDs (HDA `GET_PARAMETER CONNECTION_LIST`).
    pub connections: Vec<u8>,
}

/// Perform a depth-first search from `start_pin_nid` through `nodes`,
/// following each node's `connections`, until an `AudioOutput` (DAC) is
/// found.  Returns the NID path from the starting pin to the DAC
/// (inclusive at both ends), or `None` if no path exists.
///
/// Cycles are avoided via a visited-set.  The first path found is returned
/// (DFS order — callers that need the shortest path should use a BFS
/// variant, but for codec init the first valid path is sufficient).
pub fn find_path_to_dac(nodes: &[WidgetNode], start_pin_nid: u8) -> Option<Vec<u8>> {
    // Build a NID → node index map for O(1) lookup.
    // (Codec graphs are small — typically ≤ 20 nodes — so a linear scan
    //  would also be acceptable, but an index keeps the BFS cleaner.)
    let lookup = |nid: u8| -> Option<&WidgetNode> { nodes.iter().find(|n| n.nid == nid) };

    // DFS stack: each entry is (nid, path-so-far).
    // We use an explicit stack rather than recursion to stay `no_std` safe
    // in deep graphs (no guarantee of a large kernel stack).
    let mut stack: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut visited: Vec<u8> = Vec::new();

    // Seed with the start pin.
    {
        let start = lookup(start_pin_nid)?;
        stack.push((start.nid, alloc::vec![start.nid]));
    }

    while let Some((nid, path)) = stack.pop() {
        if visited.contains(&nid) {
            continue;
        }
        visited.push(nid);

        let node = match lookup(nid) {
            Some(n) => n,
            None => continue,
        };

        if matches!(node.kind, WidgetType::AudioOutput) {
            return Some(path);
        }

        // Push connected nodes (reverse order so the first connection is
        // explored first — consistent left-to-right DFS).
        for &conn in node.connections.iter().rev() {
            if !visited.contains(&conn) {
                let mut next_path = path.clone();
                next_path.push(conn);
                stack.push((conn, next_path));
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Codec selection
// ---------------------------------------------------------------------------

/// Summary of a single enumerated HDA codec, sufficient for routing audio
/// output path selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecSummary {
    /// Codec address (0–14 in a single-link HDA controller).
    pub addr: u8,
    /// `true` if the codec has at least one analog output pin (`LineOut`,
    /// `Speaker`, or `HPOut`) — i.e. it is *not* purely HDMI/DP.
    pub has_analog_output_pin: bool,
}

/// Choose the best codec for analog audio output from `codecs`.
///
/// Policy (mirrors Linux `hda_codec.c` codec-selection heuristics):
///
/// 1. Prefer the first codec that has `has_analog_output_pin == true`.
/// 2. If no codec has an analog output pin (e.g. pure HDMI system), fall
///    back to the first codec in the list so a single-codec HDMI setup
///    still gets audio output.
/// 3. Returns `None` only when `codecs` is empty.
pub fn select_codec(codecs: &[CodecSummary]) -> Option<u8> {
    // Prefer first codec with an analog output pin.
    if let Some(c) = codecs.iter().find(|c| c.has_analog_output_pin) {
        return Some(c.addr);
    }
    // Fallback: first codec regardless of type.
    codecs.first().map(|c| c.addr)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- widget_type ----

    #[test]
    fn widget_type_all_known_values() {
        let cases: &[(u32, WidgetType)] = &[
            (0x0 << 20, WidgetType::AudioOutput),
            (0x1 << 20, WidgetType::AudioInput),
            (0x2 << 20, WidgetType::Mixer),
            (0x3 << 20, WidgetType::Selector),
            (0x4 << 20, WidgetType::PinComplex),
            (0x5 << 20, WidgetType::PowerWidget),
            (0x6 << 20, WidgetType::VolumeKnob),
            (0x7 << 20, WidgetType::BeepGenerator),
            (0xF << 20, WidgetType::VendorDefined),
        ];
        for &(caps, ref expected) in cases {
            assert_eq!(&widget_type(caps), expected, "caps={caps:#010x}");
        }
        // Other/reserved value
        assert_eq!(widget_type(0x8 << 20), WidgetType::Other(0x8));
    }

    #[test]
    fn widget_type_ignores_non_type_bits() {
        // Extra bits outside [23:20] must not affect the type decode.
        let caps = (0x4u32 << 20) | 0x000F_FFFF; // PinComplex with all low bits set
        assert_eq!(widget_type(caps), WidgetType::PinComplex);
    }

    // ---- decode_pin_default / classification ----

    /// Decode a GET_CONFIG_DEFAULT for a fixed internal speaker and a
    /// rear green line-out jack, then assert classification.
    #[test]
    fn pin_default_classify() {
        // Fixed internal speaker:
        //   port_connectivity = 0x2 (Fixed)      → bits[31:30] = 0b10
        //   location          = 0x00 (Internal)   → bits[29:24] = 0b000000
        //   default_device    = 0x1 (Speaker)     → bits[23:20] = 0b0001
        //   connection_type   = 0x1 (Analog)      → bits[19:16] = 0b0001
        //   color             = 0x0 (Unknown)     → bits[15:12] = 0b0000
        //   misc              = 0x0                → bits[11:8]  = 0b0000
        //   association       = 0x1               → bits[7:4]   = 0b0001
        //   sequence          = 0x0               → bits[3:0]   = 0b0000
        let speaker_cfg: u32 = (0x2u32 << 30)
            | (0x00 << 24)
            | (0x1 << 20)
            | (0x1 << 16)
            | (0x0 << 12)
            | (0x0 << 8)
            | (0x1 << 4)
            | 0x0;

        let spk = decode_pin_default(speaker_cfg);
        assert_eq!(spk.default_device, DEFAULT_DEVICE_SPEAKER);
        assert_eq!(spk.port_connectivity, PORT_CONN_FIXED);
        assert_eq!(spk.association, 1);
        assert_eq!(spk.sequence, 0);
        assert!(is_output_device(&spk), "Speaker is an output device");
        assert!(is_fixed(&spk), "Speaker is a fixed/internal connection");
        assert!(!is_jack(&spk), "Speaker is not a jack");

        // Rear green line-out jack:
        //   port_connectivity = 0x0 (Jack)        → bits[31:30] = 0b00
        //   location          = 0x01 (Rear)       → bits[29:24] = 0b000001
        //   default_device    = 0x0 (LineOut)     → bits[23:20] = 0b0000
        //   connection_type   = 0x1 (Analog)      → bits[19:16] = 0b0001
        //   color             = 0x2 (Green)       → bits[15:12] = 0b0010
        //   misc              = 0x0                → bits[11:8]  = 0b0000
        //   association       = 0x1               → bits[7:4]   = 0b0001
        //   sequence          = 0x0               → bits[3:0]   = 0b0000
        let lineout_cfg: u32 = (0x0u32 << 30)
            | (0x01 << 24)
            | (0x0 << 20)
            | (0x1 << 16)
            | (0x2 << 12)
            | (0x0 << 8)
            | (0x1 << 4)
            | 0x0;

        let lo = decode_pin_default(lineout_cfg);
        assert_eq!(lo.default_device, DEFAULT_DEVICE_LINE_OUT);
        assert_eq!(lo.port_connectivity, PORT_CONN_JACK);
        assert_eq!(lo.color, 0x2, "green color code = 0x2");
        assert!(is_output_device(&lo), "LineOut is an output device");
        assert!(is_jack(&lo), "LineOut is a jack");
        assert!(!is_fixed(&lo), "LineOut jack is not fixed");

        // Headphone out must also be accepted as an output device
        let hp = PinDefault {
            default_device: DEFAULT_DEVICE_HP_OUT,
            port_connectivity: PORT_CONN_JACK,
            location: 0,
            color: 0,
            sequence: 0,
            association: 0,
        };
        assert!(is_output_device(&hp));

        // Mic In must not be classified as output
        let mic = PinDefault {
            default_device: DEFAULT_DEVICE_MIC_IN,
            port_connectivity: PORT_CONN_JACK,
            location: 0,
            color: 0,
            sequence: 0,
            association: 0,
        };
        assert!(!is_output_device(&mic));
    }

    // ---- find_path_to_dac ----

    /// Synthetic graph: pin(0x14) → selector(0x0C) → dac(0x02).
    /// `find_path_to_dac` must return a path ending at nid 0x02.
    #[test]
    fn path_to_dac() {
        let nodes = alloc::vec![
            WidgetNode {
                nid: 0x14,
                kind: WidgetType::PinComplex,
                connections: alloc::vec![0x0C],
            },
            WidgetNode {
                nid: 0x0C,
                kind: WidgetType::Selector,
                connections: alloc::vec![0x02],
            },
            WidgetNode {
                nid: 0x02,
                kind: WidgetType::AudioOutput,
                connections: alloc::vec![],
            },
        ];

        let path =
            find_path_to_dac(&nodes, 0x14).expect("path must be found for pin→selector→dac graph");

        // Path must include the DAC nid.
        assert!(path.contains(&0x02), "path must end at the DAC (0x02)");
        // Path must start at the pin.
        assert_eq!(path[0], 0x14, "path must start at the pin nid");
        // Path must be: [0x14, 0x0C, 0x02]
        assert_eq!(path, alloc::vec![0x14u8, 0x0C, 0x02]);
    }

    /// Missing start node returns None.
    #[test]
    fn path_to_dac_missing_start() {
        let nodes: Vec<WidgetNode> = alloc::vec![];
        assert!(find_path_to_dac(&nodes, 0x14).is_none());
    }

    /// Graph with no DAC returns None.
    #[test]
    fn path_to_dac_no_dac() {
        let nodes = alloc::vec![WidgetNode {
            nid: 0x14,
            kind: WidgetType::PinComplex,
            connections: alloc::vec![],
        }];
        assert!(find_path_to_dac(&nodes, 0x14).is_none());
    }

    /// Graph with a cycle must not loop forever.
    #[test]
    fn path_to_dac_cycle_safety() {
        // pin(0x14) → sel(0x0C) → pin(0x14) [cycle], no DAC
        let nodes = alloc::vec![
            WidgetNode {
                nid: 0x14,
                kind: WidgetType::PinComplex,
                connections: alloc::vec![0x0C],
            },
            WidgetNode {
                nid: 0x0C,
                kind: WidgetType::Selector,
                connections: alloc::vec![0x14],
            },
        ];
        assert!(find_path_to_dac(&nodes, 0x14).is_none());
    }

    // ---- select_codec ----

    /// Given one HDMI-only codec and one analog codec, the analog codec must
    /// be selected.
    #[test]
    fn codec_selection_prefers_analog() {
        let codecs = alloc::vec![
            CodecSummary {
                addr: 0,
                has_analog_output_pin: false
            }, // HDMI-only
            CodecSummary {
                addr: 1,
                has_analog_output_pin: true
            }, // analog
        ];
        assert_eq!(select_codec(&codecs), Some(1));

        // Single analog codec
        let only_analog = alloc::vec![CodecSummary {
            addr: 2,
            has_analog_output_pin: true
        }];
        assert_eq!(select_codec(&only_analog), Some(2));

        // Single HDMI-only codec — fallback returns it
        let only_hdmi = alloc::vec![CodecSummary {
            addr: 3,
            has_analog_output_pin: false
        }];
        assert_eq!(select_codec(&only_hdmi), Some(3));

        // Empty list
        let empty: Vec<CodecSummary> = alloc::vec![];
        assert_eq!(select_codec(&empty), None);

        // Multiple analog codecs — first wins
        let multi = alloc::vec![
            CodecSummary {
                addr: 0,
                has_analog_output_pin: false
            },
            CodecSummary {
                addr: 1,
                has_analog_output_pin: true
            },
            CodecSummary {
                addr: 2,
                has_analog_output_pin: true
            },
        ];
        assert_eq!(select_codec(&multi), Some(1));
    }
}
