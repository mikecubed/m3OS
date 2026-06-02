//! HDA codec enumeration + output-path configuration — Phase 80b Track C.1/C.2.
//!
//! Generic, codec-*agnostic* widget-graph traversal (the Redox `ihdad` /
//! Linux `hda_generic.c` approach, zero quirk tables): root → AFG → widgets,
//! reading widget caps / connection lists / pin defaults, then selecting an
//! output path from a usable pin back to a DAC and configuring every widget on
//! it (power D0, amp unmute, pin out-enable + EAPD, converter stream-id +
//! format). A muted intermediate amp or an un-powered widget anywhere on the
//! path leaves the output silent even with DAC + pin correct.

use crate::controller::HdaController;
use alloc::vec::Vec;
use kernel_core::hda::{self, realtek, widget};

/// The configured output path the stream engine drives.
pub struct OutputPath {
    pub codec: u8,
    pub dac_nid: u8,
    pub pin_nid: u8,
}

/// One enumerated codec's widget graph + the chosen output pin.
struct CodecGraph {
    addr: u8,
    /// Codec vendor id (`GET_PARAMETER VENDOR_ID >> 16`) — drives Realtek-
    /// specific amp-enable + selection (Track E).
    vendor: u16,
    /// Audio Function Group NID (for AFG-level verbs: GPIO-EAPD, COEF).
    afg: u8,
    nodes: Vec<widget::WidgetNode>,
    /// The generically-preferred output pin (Speaker > HP > Line-Out).
    output_pin: Option<(u8, widget::PinDefault)>,
    /// Every usable output pin (for Realtek jack-presence selection).
    output_pins: Vec<(u8, widget::PinDefault)>,
}

/// Decode a `SUBORDINATE_NODE_COUNT` response into `(start_nid, count)`.
fn node_range(val: u32) -> (u8, u8) {
    (((val >> 16) & 0xFF) as u8, (val & 0xFF) as u8)
}

/// Read a widget's connection list, decoding both short- and long-form layouts
/// **and range entries** (HDA spec §7.1.2) via the host-tested
/// [`widget::expand_connection_list`]. Each `GET_CONNECTION_LIST` response packs
/// 4 entries (short form) or 2 (long form); the expander turns range markers
/// into the runs of NIDs they denote, so real Realtek/IDT/Conexant codecs that
/// use range encoding select a valid pin→DAC path instead of chasing phantom
/// NIDs.
fn read_connections(ctrl: &mut HdaController, codec: u8, nid: u8) -> Vec<u8> {
    let len_resp = ctrl
        .get_parameter(codec, nid, hda::PARAM_CONNECTION_LIST_LENGTH)
        .unwrap_or(0);
    let long_form = (len_resp & 0x80) != 0;
    let len = (len_resp & 0x7F) as usize;
    if len == 0 {
        return Vec::new();
    }
    let per_dword = if long_form { 2 } else { 4 };
    let mut responses = Vec::new();
    let mut i = 0;
    while i < len {
        let r = ctrl
            .command(codec, nid, hda::VERB_GET_CONNECTION_LIST, i as u8)
            .unwrap_or(0);
        responses.push(r);
        i += per_dword;
    }
    widget::expand_connection_list(&responses, len, long_form)
}

/// Enumerate a codec: find its Audio Function Group, walk every widget, build
/// the [`widget::WidgetNode`] graph, and pick the best output pin (Speaker >
/// HP-Out > Line-Out, skipping unconnected pins).
fn enumerate_codec(ctrl: &mut HdaController, codec: u8) -> Option<CodecGraph> {
    // Root node (NID 0) → function groups.
    let root = ctrl.get_parameter(codec, 0, hda::PARAM_SUBORDINATE_NODE_COUNT)?;
    let (fg_start, fg_count) = node_range(root);

    // Find the Audio Function Group.
    let mut afg: Option<u8> = None;
    for nid in fg_start..fg_start.saturating_add(fg_count) {
        let ty = ctrl.get_parameter(codec, nid, hda::PARAM_FUNCTION_GROUP_TYPE)?;
        if (ty & 0xFF) as u8 == hda::FN_GROUP_AUDIO {
            afg = Some(nid);
            break;
        }
    }
    let afg = afg?;

    // Codec vendor id (`VENDOR_ID >> 16`) — gates the Realtek amp-enable path.
    let vendor = (ctrl
        .get_parameter(codec, 0, hda::PARAM_VENDOR_ID)
        .unwrap_or(0)
        >> 16) as u16;

    // Power the AFG up before touching its widgets.
    let _ = ctrl.command(
        codec,
        afg,
        hda::VERB_SET_POWER_STATE,
        hda::POWER_STATE_D0 as u8,
    );

    // Walk the AFG's widgets.
    let afg_nodes = ctrl.get_parameter(codec, afg, hda::PARAM_SUBORDINATE_NODE_COUNT)?;
    let (w_start, w_count) = node_range(afg_nodes);

    let mut nodes = Vec::new();
    // Output-pin candidates, ranked by default-device preference.
    let mut best_pin: Option<(u8, widget::PinDefault, u8)> = None; // (nid, def, rank)
    let mut output_pins: Vec<(u8, widget::PinDefault)> = Vec::new();

    for nid in w_start..w_start.saturating_add(w_count) {
        let caps = ctrl
            .get_parameter(codec, nid, hda::PARAM_AUDIO_WIDGET_CAPS)
            .unwrap_or(0);
        let kind = widget::widget_type(caps);
        let connections = read_connections(ctrl, codec, nid);

        if matches!(kind, widget::WidgetType::PinComplex) {
            let cfg = ctrl
                .command(codec, nid, hda::VERB_GET_CONFIG_DEFAULT, 0)
                .unwrap_or(0);
            let def = widget::decode_pin_default(cfg);
            if widget::is_output_device(&def) && def.port_connectivity != widget::PORT_CONN_NONE {
                output_pins.push((nid, def));
                let rank = match def.default_device {
                    widget::DEFAULT_DEVICE_SPEAKER => 3,
                    widget::DEFAULT_DEVICE_HP_OUT => 2,
                    widget::DEFAULT_DEVICE_LINE_OUT => 1,
                    _ => 0,
                };
                if best_pin.as_ref().map(|(_, _, r)| rank > *r).unwrap_or(true) {
                    best_pin = Some((nid, def, rank));
                }
            }
        }

        nodes.push(widget::WidgetNode {
            nid,
            kind,
            connections,
        });
    }

    let output_pin = best_pin.map(|(nid, def, _)| (nid, def));
    Some(CodecGraph {
        addr: codec,
        vendor,
        afg,
        nodes,
        output_pin,
        output_pins,
    })
}

/// SET_AMP_GAIN_MUTE payload: unmute + set gain on both channels. Delegates to
/// the host-tested encoder (`kernel_core::hda::realtek::amp_gain_mute_payload`,
/// Track E.3) so the bit layout lives + is tested in exactly one place.
fn amp_unmute_payload(set_output: bool, index: u8, gain: u8) -> u16 {
    realtek::amp_gain_mute_payload(set_output, true, true, index, false, gain)
}

/// Enumerate the selected codec and configure an output path for the given
/// `SDnFMT` value + stream tag. Returns the chosen DAC/pin.
pub fn configure_output(
    ctrl: &mut HdaController,
    sdnfmt: u16,
    stream_tag: u8,
) -> Result<OutputPath, &'static str> {
    // Enumerate every codec; prefer the analog one.
    let codec_addrs = ctrl.codecs.clone();
    let mut graphs: Vec<CodecGraph> = Vec::new();
    let mut summaries: Vec<widget::CodecSummary> = Vec::new();
    for addr in codec_addrs {
        if let Some(g) = enumerate_codec(ctrl, addr) {
            summaries.push(widget::CodecSummary {
                addr,
                has_analog_output_pin: g.output_pin.is_some(),
            });
            graphs.push(g);
        }
    }
    let chosen = widget::select_codec(&summaries).ok_or("no codec enumerated")?;
    let graph = graphs
        .iter()
        .find(|g| g.addr == chosen)
        .ok_or("chosen codec missing")?;

    let codec = graph.addr;

    // Pin selection (Track E.2): for a Realtek codec, prefer the internal
    // speaker, then a headphone pin **only when its jack is present**
    // (`GET_PIN_SENSE` bit31), then line-out — via the host-tested
    // `realtek_output_select`. Other codecs use the generic Speaker > HP >
    // Line-Out ranking.
    let (pin_nid, pin_def) = if realtek::is_realtek(graph.vendor) {
        let mut pins = Vec::with_capacity(graph.output_pins.len());
        for &(nid, def) in &graph.output_pins {
            // Fixed/internal outputs (speaker) are always "present"; jack pins
            // (HP) are gated on presence detect.
            let jack_present = if def.default_device == widget::DEFAULT_DEVICE_HP_OUT {
                ctrl.command(codec, nid, hda::VERB_GET_PIN_SENSE, 0)
                    .map(|r| r & 0x8000_0000 != 0)
                    .unwrap_or(false)
            } else {
                true
            };
            pins.push((nid, def, jack_present));
        }
        match realtek::realtek_output_select(&pins) {
            Some(nid) => {
                let def = graph
                    .output_pins
                    .iter()
                    .find(|(n, _)| *n == nid)
                    .map(|(_, d)| *d)
                    .ok_or("realtek pin vanished")?;
                (nid, def)
            }
            None => graph.output_pin.ok_or("no analog output pin")?,
        }
    } else {
        graph.output_pin.ok_or("no analog output pin")?
    };
    let path = widget::find_path_to_dac(&graph.nodes, pin_nid).ok_or("no pin→DAC path")?;
    let dac_nid = *path.last().ok_or("empty path")?;

    // Configure every widget on the path: power D0, then amps unmuted.
    const GAIN: u8 = 0x7F;
    for &nid in &path {
        let _ = ctrl.command(
            codec,
            nid,
            hda::VERB_SET_POWER_STATE,
            hda::POWER_STATE_D0 as u8,
        );
        // Unmute the widget's output amp and its first input amp (mixers /
        // selectors gate the signal on their input amps).
        let _ = ctrl.command4(
            codec,
            nid,
            hda::VERB4_SET_AMP_GAIN_MUTE,
            amp_unmute_payload(true, 0, GAIN),
        );
        let _ = ctrl.command4(
            codec,
            nid,
            hda::VERB4_SET_AMP_GAIN_MUTE,
            amp_unmute_payload(false, 0, GAIN),
        );
    }

    // Converter (DAC): bind the stream tag + channel 0 and the format. The tag
    // MUST match the stream descriptor's `SDnCTL[23:20]` or the DAC ignores the
    // DMA stream (silent).
    // Payload = (stream_tag << 4) | channel(0).
    let _ = ctrl.command(
        codec,
        dac_nid,
        hda::VERB_SET_CHANNEL_STREAMID,
        stream_tag << 4,
    );
    let _ = ctrl.command4(codec, dac_nid, hda::VERB4_SET_STREAM_FORMAT, sdnfmt);

    // Pin complex: out-enable (+ HP-enable for headphone pins) and EAPD.
    let mut pinctl: u8 = 0x40; // OUT_EN (bit6)
    if pin_def.default_device == widget::DEFAULT_DEVICE_HP_OUT {
        pinctl |= 0x80; // HP_EN (bit7)
    }
    let _ = ctrl.command(codec, pin_nid, hda::VERB_SET_PIN_WIDGET_CONTROL, pinctl);
    let _ = ctrl.command(codec, pin_nid, hda::VERB_SET_EAPD_BTLENABLE, 0x02); // EAPD (bit1)

    // Track E.1: a real Realtek output sits behind an external amplifier that
    // defaults OFF — `SET_EAPD_BTLENABLE` alone (above) is silent on ALC892/
    // ALC1220 boards. Issue the full Realtek amp-enable sequence (EAPD verb +
    // GPIO-driven-EAPD fallback) from the host-tested verb builder. No-op on
    // QEMU's generic codec (vendor != Realtek), so `hda-smoke` is unaffected;
    // exercised on real hardware (Track F).
    if realtek::is_realtek(graph.vendor) {
        for dword in realtek::realtek_amp_enable_verbs(codec, graph.afg, pin_nid) {
            let _ = ctrl.raw_command(dword);
        }
    }

    Ok(OutputPath {
        codec,
        dac_nid,
        pin_nid,
    })
}
