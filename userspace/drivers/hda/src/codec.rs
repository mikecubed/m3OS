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
use kernel_core::hda::{self, widget};

/// The configured output path the stream engine drives.
pub struct OutputPath {
    pub codec: u8,
    pub dac_nid: u8,
    pub pin_nid: u8,
}

/// One enumerated codec's widget graph + the chosen output pin.
struct CodecGraph {
    addr: u8,
    nodes: Vec<widget::WidgetNode>,
    output_pin: Option<(u8, widget::PinDefault)>,
}

/// Decode a `SUBORDINATE_NODE_COUNT` response into `(start_nid, count)`.
fn node_range(val: u32) -> (u8, u8) {
    (((val >> 16) & 0xFF) as u8, (val & 0xFF) as u8)
}

/// Read a widget's short-form connection list (sufficient for QEMU + the
/// common analog codecs; long-form entries are truncated to the first byte).
fn read_connections(ctrl: &mut HdaController, codec: u8, nid: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let len_resp = ctrl
        .get_parameter(codec, nid, hda::PARAM_CONNECTION_LIST_LENGTH)
        .unwrap_or(0);
    let long_form = (len_resp & 0x80) != 0;
    let len = (len_resp & 0x7F) as usize;
    if len == 0 {
        return out;
    }
    if long_form {
        // Long form: 2 entries per GET_CONNECTION_LIST response (16-bit each).
        let mut i = 0;
        while i < len {
            let r = ctrl
                .command(codec, nid, hda::VERB_GET_CONNECTION_LIST, i as u8)
                .unwrap_or(0);
            out.push((r & 0xFFFF) as u8);
            if i + 1 < len {
                out.push(((r >> 16) & 0xFFFF) as u8);
            }
            i += 2;
        }
    } else {
        // Short form: 4 entries per response (8-bit each).
        let mut i = 0;
        while i < len {
            let r = ctrl
                .command(codec, nid, hda::VERB_GET_CONNECTION_LIST, i as u8)
                .unwrap_or(0);
            for b in 0..4 {
                if i + b < len {
                    out.push(((r >> (b * 8)) & 0xFF) as u8);
                }
            }
            i += 4;
        }
    }
    out
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
        nodes,
        output_pin,
    })
}

/// SET_AMP_GAIN_MUTE payload: unmute + set gain on both channels.
/// `set_output` chooses the output amp (vs. input amp). `index` selects which
/// input amp on a mixer/selector.
fn amp_unmute_payload(set_output: bool, index: u8, gain: u8) -> u16 {
    let mut p: u16 = (1 << 13) | (1 << 12); // set-left | set-right
    if set_output {
        p |= 1 << 15; // set-output amp
    } else {
        p |= 1 << 14; // set-input amp
    }
    p |= ((index & 0xF) as u16) << 8;
    // bit7 = mute (cleared); bits[6:0] = gain.
    p |= (gain & 0x7F) as u16;
    p
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

    let (pin_nid, pin_def) = graph.output_pin.ok_or("no analog output pin")?;
    let path = widget::find_path_to_dac(&graph.nodes, pin_nid).ok_or("no pin→DAC path")?;
    let dac_nid = *path.last().ok_or("empty path")?;
    let codec = graph.addr;

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

    Ok(OutputPath {
        codec,
        dac_nid,
        pin_nid,
    })
}
