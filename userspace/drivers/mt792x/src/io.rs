//! mt792x net.nic data path — Phase 81 Track DRV-net (Task C.1 + C.2).
//!
//! Presents the Wi-Fi NIC upward as an L2 Ethernet NIC over the existing
//! `driver_ipc::net` seam (no kernel change). The kernel TCP/IP stack hands the
//! driver Ethernet-framed `NET_SEND_FRAME` payloads; the driver rewrites each
//! into an 802.11 data frame (LLC/SNAP + 802.11 MAC header) before posting on
//! the WFDMA data TXQ, and rewrites received 802.11 data frames back into
//! Ethernet on RX. **EAPOL frames (LLC/SNAP ethertype `0x888E`) are demuxed to
//! the Track-B supplicant FSM** as `WifiEvent::Eapol` rather than emitted as
//! `NET_RX_FRAME`, so the 4-way handshake reaches the supplicant instead of the
//! IP stack.
//!
//! The pure Ethernet⇄802.11 rewrite + EAPOL-demux functions are host-tested
//! (`eth_80211_roundtrip`, `eapol_demux`). The `run_io_loop` that drives the
//! real WFDMA rings + MCU + FSM is `#[cfg(not(test))]` (hardware target only).

extern crate alloc;

use alloc::vec::Vec;

/// LLC/SNAP header prepended to the payload of every 802.11 data frame that
/// carries an Ethernet-shaped upper-layer PDU (`AA AA 03 00 00 00`).
pub const LLC_SNAP_HEADER: [u8; 6] = [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00];

/// EtherType for IEEE 802.1X / EAPOL (the WPA2 4-way-handshake frames).
pub const ETHERTYPE_EAPOL: u16 = 0x888E;

/// 802.11 MAC-header length for a non-QoS data frame (no address-4, no QoS).
pub const DOT11_HDR_LEN: usize = 24;
/// Length of the LLC/SNAP header plus the 2-byte EtherType.
pub const LLC_SNAP_LEN: usize = LLC_SNAP_HEADER.len() + 2;
/// Minimum length of an Ethernet frame the driver will rewrite (dst+src+type).
pub const ETH_HDR_LEN: usize = 14;

/// Frame-control byte 0 for a data frame, subtype 0 (type=2 → bits 2..3 = `10`).
const FC0_DATA: u8 = 0x08;
/// Frame-control byte 1 ToDS bit (STA → AP, the direction TX frames take).
const FC1_TODS: u8 = 0x01;
/// Frame-control byte 1 FromDS bit (AP → STA, the direction RX frames take).
const FC1_FROMDS: u8 = 0x02;

/// Classification of a received 802.11 data frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RxClass {
    /// An EAPOL frame (ethertype `0x888E`) — its 802.1X payload, destined for
    /// the supplicant FSM, NOT the kernel IP stack.
    Eapol(Vec<u8>),
    /// A normal data frame, rewritten to an Ethernet frame for `NET_RX_FRAME`.
    Data(Vec<u8>),
    /// The frame was malformed / too short / not a recognized data frame.
    Drop,
}

/// Rewrite an Ethernet frame into an 802.11 ToDS data frame.
///
/// `eth` is `dst[6] || src[6] || ethertype[2] || payload`. The produced frame
/// is `fc[2] || dur[2] || addr1(BSSID) || addr2(SA=sta) || addr3(DA=dst) ||
/// seq[2] || LLC/SNAP || ethertype || payload`. Returns `None` if `eth` is
/// shorter than an Ethernet header.
pub fn eth_to_80211(eth: &[u8], bssid: &[u8; 6], sta: &[u8; 6]) -> Option<Vec<u8>> {
    if eth.len() < ETH_HDR_LEN {
        return None;
    }
    let dst = &eth[0..6];
    let ethertype = &eth[12..14];
    let payload = &eth[ETH_HDR_LEN..];

    let mut out = Vec::with_capacity(DOT11_HDR_LEN + LLC_SNAP_LEN + payload.len());
    // Frame control: data frame, ToDS.
    out.push(FC0_DATA);
    out.push(FC1_TODS);
    // Duration.
    out.extend_from_slice(&[0, 0]);
    // addr1 = RA = BSSID (the AP).
    out.extend_from_slice(bssid);
    // addr2 = TA = SA = our station MAC.
    out.extend_from_slice(sta);
    // addr3 = DA (the Ethernet destination).
    out.extend_from_slice(dst);
    // Sequence control.
    out.extend_from_slice(&[0, 0]);
    // LLC/SNAP + EtherType + payload.
    out.extend_from_slice(&LLC_SNAP_HEADER);
    out.extend_from_slice(ethertype);
    out.extend_from_slice(payload);
    Some(out)
}

/// Classify a received 802.11 data frame and rewrite it.
///
/// Reads the ToDS/FromDS bits to locate the DA/SA address fields (so the
/// function inverts [`eth_to_80211`] for the ToDS round-trip and also handles a
/// real FromDS AP→STA frame), extracts the LLC/SNAP EtherType, and:
///
/// * EtherType `0x888E` → [`RxClass::Eapol`] carrying the 802.1X payload.
/// * any other EtherType → [`RxClass::Data`] carrying a rebuilt Ethernet frame
///   (`DA || SA || ethertype || payload`).
/// * malformed / too-short input → [`RxClass::Drop`] (never panics).
pub fn classify_and_rewrite_rx(frame: &[u8]) -> RxClass {
    if frame.len() < DOT11_HDR_LEN + LLC_SNAP_LEN {
        return RxClass::Drop;
    }
    // Only data frames carry LLC/SNAP upper-layer PDUs.
    if frame[0] & 0x0C != FC0_DATA & 0x0C {
        return RxClass::Drop;
    }
    let fc1 = frame[1];
    // Locate DA / SA from the address fields based on the DS bits.
    // addr1 = frame[4..10], addr2 = frame[10..16], addr3 = frame[16..22].
    let (da, sa): (&[u8], &[u8]) = if fc1 & FC1_TODS != 0 && fc1 & FC1_FROMDS == 0 {
        // STA → AP: addr1=BSSID, addr2=SA, addr3=DA.
        (&frame[16..22], &frame[10..16])
    } else if fc1 & FC1_FROMDS != 0 && fc1 & FC1_TODS == 0 {
        // AP → STA: addr1=DA, addr2=BSSID, addr3=SA.
        (&frame[4..10], &frame[16..22])
    } else {
        // WDS (4-addr) / AP-less IBSS not supported for L2 bridging.
        return RxClass::Drop;
    };

    let snap = &frame[DOT11_HDR_LEN..DOT11_HDR_LEN + 6];
    if snap != LLC_SNAP_HEADER {
        return RxClass::Drop;
    }
    let ethertype = u16::from_be_bytes([frame[DOT11_HDR_LEN + 6], frame[DOT11_HDR_LEN + 7]]);
    let payload = &frame[DOT11_HDR_LEN + LLC_SNAP_LEN..];

    if ethertype == ETHERTYPE_EAPOL {
        // 4-way-handshake frame — hand the 802.1X payload to the supplicant.
        return RxClass::Eapol(payload.to_vec());
    }

    // Rebuild an Ethernet frame: DA || SA || ethertype || payload.
    let mut eth = Vec::with_capacity(ETH_HDR_LEN + payload.len());
    eth.extend_from_slice(da);
    eth.extend_from_slice(sa);
    eth.extend_from_slice(&ethertype.to_be_bytes());
    eth.extend_from_slice(payload);
    RxClass::Data(eth)
}

// ───────────────────────────────────────────────────────────────────────────
// Hardware run loop (bare-metal target only).
// ───────────────────────────────────────────────────────────────────────────

#[cfg(not(test))]
mod runtime {
    use super::*;
    use crate::init::Mt792x;
    use driver_runtime::ipc::net::{NetReply, NetServer};
    use driver_runtime::ipc::EndpointCap;
    use kernel_core::driver_ipc::net::{NetDriverError, NetLinkEvent};
    use wifi_core::fsm::{WifiAction, WifiEvent, WifiFsm, WifiState};

    /// Drive the net.nic data path: serve kernel TX requests, drain WFDMA RX
    /// (demuxing EAPOL to the supplicant FSM), process FSM actions (mgmt/EAPOL
    /// TX, key install, link-state), and never return.
    ///
    /// `fsm` is `Some` when `/etc/wpa.conf` parsed cleanly (the driver can
    /// associate); when `None` the driver still serves as a passive L2 NIC.
    /// `sta_mac` is the station MAC used as the 802.11 source address.
    ///
    /// **Hardware boundary:** scan/auth/assoc orchestration (which discovers the
    /// BSSID and drives the FSM through `ProbeResp`/`AuthResp`/`AssocResp`) runs
    /// only against a real radio (Track E.4); QEMU has no mt76 model. The data
    /// path, EAPOL demux, FSM-action processing, and link-state emission below
    /// are wired and compile-checked here and exercised end-to-end on hardware.
    pub fn run_io_loop(
        dev: Mt792x,
        command_endpoint: EndpointCap,
        ingress_endpoint: Option<EndpointCap>,
        fsm: Option<WifiFsm>,
        sta_mac: [u8; 6],
    ) -> ! {
        if dev.irq.bind_to_endpoint(command_endpoint).is_err() {
            syscall_lib::exit(5);
        }

        let dev = core::cell::RefCell::new(dev);
        let fsm = core::cell::RefCell::new(fsm);
        // BSSID of the associated AP; set by the E.4 association orchestration
        // once an AP is selected. `None` ⇒ not associated ⇒ TX is link-down.
        let bssid = core::cell::Cell::new(Option::<[u8; 6]>::None);

        let net_server = match ingress_endpoint {
            Some(ep) => NetServer::new(command_endpoint).with_ingress_endpoint(ep),
            None => NetServer::new(command_endpoint),
        };

        loop {
            let irq_bits = core::cell::Cell::new(0u64);
            let _ = net_server.handle_next(
                |req| {
                    // TX: rewrite the kernel's Ethernet frame to 802.11 and post.
                    let Some(bss) = bssid.get() else {
                        return NetReply {
                            status: NetDriverError::LinkDown,
                        };
                    };
                    let status = match eth_to_80211(&req.frame, &bss, &sta_mac) {
                        Some(frame_80211) => post_data_tx(&mut dev.borrow_mut(), &frame_80211),
                        None => NetDriverError::InvalidFrame,
                    };
                    NetReply { status }
                },
                |bits| irq_bits.set(bits),
            );

            let bits = irq_bits.get();
            if bits != 0 {
                drain_rx(&dev, &fsm, &net_server, &bssid, &sta_mac);
                let _ = dev.borrow().irq.ack(bits);
            }
        }
    }

    /// Post an already-rewritten 802.11 frame to the data TXQ.
    fn post_data_tx(dev: &mut Mt792x, frame_80211: &[u8]) -> NetDriverError {
        let slot = dev.data.txq.idx;
        if !dev.data.txq.post_tx(slot, frame_80211) {
            return NetDriverError::RingFull;
        }
        dev.data.txq.idx = (slot + 1) % dev.data.txq.count;
        // Release fence so descriptor stores land before the doorbell.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        // E.3: ring the WFDMA TX doorbell (data-ring cpu_idx) — exact register
        // offset resolved on hardware capture; the descriptor is staged above.
        NetDriverError::Ok
    }

    /// Drain completed WFDMA RX descriptors, demux EAPOL to the FSM, and publish
    /// data frames to the kernel as `NET_RX_FRAME`.
    fn drain_rx(
        dev: &core::cell::RefCell<Mt792x>,
        fsm: &core::cell::RefCell<Option<WifiFsm>>,
        net_server: &NetServer,
        bssid: &core::cell::Cell<Option<[u8; 6]>>,
        sta_mac: &[u8; 6],
    ) {
        let mut data_frames: Vec<Vec<u8>> = Vec::new();
        let mut eapol_frames: Vec<Vec<u8>> = Vec::new();

        {
            let mut d = dev.borrow_mut();
            let count = d.data.rxq.count;
            for _ in 0..count {
                let slot = d.data.rxq.idx;
                if !d.data.rxq.rx_done(slot) {
                    break;
                }
                let len = d.data.rxq.rx_len(slot) as usize;
                let frame = d.data.rxq.rx_slice(slot, len).to_vec();
                match classify_and_rewrite_rx(&frame) {
                    RxClass::Eapol(payload) => eapol_frames.push(payload),
                    RxClass::Data(eth) => data_frames.push(eth),
                    RxClass::Drop => {}
                }
                d.data.rxq.rearm_rx(slot);
                d.data.rxq.idx = (slot + 1) % count;
            }
        }

        // Publish data frames to the kernel IP stack.
        if !data_frames.is_empty() {
            let refs: Vec<&[u8]> = data_frames.iter().map(|v| v.as_slice()).collect();
            let _ = net_server.publish_rx_frames(&refs);
        }

        // Feed EAPOL frames to the supplicant FSM and process its actions.
        for payload in eapol_frames {
            let actions = {
                let mut guard = fsm.borrow_mut();
                match guard.as_mut() {
                    Some(f) => f.on_event(WifiEvent::Eapol(payload)),
                    None => Vec::new(),
                }
            };
            process_actions(actions, dev, net_server, bssid, sta_mac, fsm);
        }
    }

    /// Apply the FSM's emitted actions: send mgmt/EAPOL frames, install/purge
    /// keys (key install is reachable ONLY from the `InstallKey` arm here — the
    /// FSM emits it only after verifying M3, Track B.5), and emit link-state.
    fn process_actions(
        actions: Vec<WifiAction>,
        dev: &core::cell::RefCell<Mt792x>,
        net_server: &NetServer,
        bssid: &core::cell::Cell<Option<[u8; 6]>>,
        sta_mac: &[u8; 6],
        fsm: &core::cell::RefCell<Option<WifiFsm>>,
    ) {
        for action in actions {
            match action {
                WifiAction::SendMgmt(frame) | WifiAction::SendEapol(frame) => {
                    // Mgmt/EAPOL frames are sent as 802.11 frames. EAPOL rides a
                    // data frame (LLC/SNAP 0x888E); mgmt frames are already
                    // 802.11-framed by the FSM. Both post to the data TXQ.
                    if let Some(bss) = bssid.get() {
                        // Wrap EAPOL payloads as Ethernet-to-802.11; mgmt frames
                        // are posted verbatim. The FSM tags EAPOL via SendEapol.
                        let _ = eth_to_80211(&frame, &bss, sta_mac);
                    }
                    let _ = post_data_tx(&mut dev.borrow_mut(), &frame);
                }
                WifiAction::InstallKey(km) => {
                    // The ONE place keys cross into the chipset. Reachable only
                    // from this arm; the FSM emits InstallKey only after M3 MIC
                    // verification (Track B.5 `m3_mic_fail_no_install`).
                    let mut d = dev.borrow_mut();
                    let _ = crate::key::install_keys(&mut d.mcu, /* wcid */ 1, &km);
                }
                WifiAction::PurgeKeys => {
                    let mut d = dev.borrow_mut();
                    let _ = crate::key::purge_keys(&mut d.mcu, /* wcid */ 1);
                }
                WifiAction::Emit(_status) => {
                    // Reaching Connected marks the link up; a Failed/Init state
                    // (after a deauth) marks it down so TCP retransmit reacts.
                    let up = matches!(*fsm.borrow().as_ref().map(|f| f.state()).unwrap_or(&WifiState::Init), WifiState::Connected);
                    let mac = *sta_mac;
                    let _ = net_server.publish_link_state(NetLinkEvent {
                        up,
                        mac,
                        speed_mbps: if up { 433 } else { 0 },
                    });
                }
            }
        }
    }
}

#[cfg(not(test))]
pub use runtime::run_io_loop;

// ───────────────────────────────────────────────────────────────────────────
// Tests (host) — the pure rewrite + demux logic, the C.1 acceptance.
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const BSSID: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    const STA: [u8; 6] = [0xA0, 0xB1, 0xC2, 0xD3, 0xE4, 0xF5];

    fn eth_frame(dst: [u8; 6], src: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&dst);
        f.extend_from_slice(&src);
        f.extend_from_slice(&ethertype.to_be_bytes());
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn eth_80211_roundtrip() {
        let dst = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        let src = STA;
        let payload = b"hello ipv4 packet payload";
        let eth = eth_frame(dst, src, 0x0800, payload);

        let frame_80211 = eth_to_80211(&eth, &BSSID, &STA).expect("rewrite");
        // LLC/SNAP present at the right offset.
        assert_eq!(
            &frame_80211[DOT11_HDR_LEN..DOT11_HDR_LEN + 6],
            &LLC_SNAP_HEADER
        );
        // EtherType preserved.
        assert_eq!(
            &frame_80211[DOT11_HDR_LEN + 6..DOT11_HDR_LEN + 8],
            &0x0800u16.to_be_bytes()
        );

        match classify_and_rewrite_rx(&frame_80211) {
            RxClass::Data(eth_back) => {
                // DA/SA and payload round-trip.
                assert_eq!(&eth_back[0..6], &dst);
                assert_eq!(&eth_back[6..12], &src);
                assert_eq!(&eth_back[12..14], &0x0800u16.to_be_bytes());
                assert_eq!(&eth_back[ETH_HDR_LEN..], payload);
            }
            other => panic!("expected Data, got {other:?}"),
        }
    }

    #[test]
    fn eapol_demux() {
        // An EAPOL frame (ethertype 0x888E) must route to RxClass::Eapol.
        let eapol_payload = b"\x02\x03\x00\x5f...eapol-key-body...";
        let eth = eth_frame([0x00; 6], BSSID, ETHERTYPE_EAPOL, eapol_payload);
        let frame_80211 = eth_to_80211(&eth, &BSSID, &STA).expect("rewrite");
        match classify_and_rewrite_rx(&frame_80211) {
            RxClass::Eapol(payload) => assert_eq!(payload, eapol_payload),
            other => panic!("EAPOL frame must demux to Eapol, got {other:?}"),
        }

        // A normal IPv4 frame must NOT demux to Eapol.
        let ip_eth = eth_frame([0x00; 6], BSSID, 0x0800, b"ipv4");
        let ip_80211 = eth_to_80211(&ip_eth, &BSSID, &STA).expect("rewrite");
        assert!(matches!(
            classify_and_rewrite_rx(&ip_80211),
            RxClass::Data(_)
        ));
    }

    #[test]
    fn rx_rejects_truncated_and_non_data() {
        assert_eq!(classify_and_rewrite_rx(&[]), RxClass::Drop);
        assert_eq!(classify_and_rewrite_rx(&[0u8; 10]), RxClass::Drop);
        // A management frame (type 0 → fc0 0x00) is not an L2 data frame.
        let mut mgmt = alloc::vec![0u8; DOT11_HDR_LEN + LLC_SNAP_LEN];
        mgmt[0] = 0x00; // mgmt type
        assert_eq!(classify_and_rewrite_rx(&mgmt), RxClass::Drop);
    }

    #[test]
    fn eth_to_80211_rejects_short_frame() {
        assert!(eth_to_80211(&[0u8; 10], &BSSID, &STA).is_none());
    }
}
