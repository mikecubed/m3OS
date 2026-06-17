//! Phase 96 Stage-2 — `ure` RX/TX path + `RemoteNic` service loop.
//!
//! The RTL8156 moves Ethernet frames over USB **bulk** endpoints, each frame
//! wrapped in a Realtek descriptor (re-expressed from OpenBSD ure(4)
//! `ure_decap`/`ure_encap_txpkt`, BSD-2-Clause). The RX and TX descriptors are
//! **different sizes** — a trap that silently corrupts RX if conflated:
//!
//! - **RX (bulk-IN):** one bulk-IN buffer carries one-or-more frames, each
//!   prefixed by the **24-byte** `ure_rxpkt` descriptor (`URE_RXPKT_HDR_SIZE`)
//!   whose word-0 low 15 bits (`URE_RXPKT_LEN_MASK`) are the packet length
//!   *including* the 4-byte Ethernet CRC; words 2..5 are csum/reserved. The
//!   Ethernet frame begins after all 24 bytes. Frames are `URE_RX_BUF_ALIGN`
//!   (8-byte) aligned within the buffer.
//! - **TX (bulk-OUT):** prepend the **8-byte** `ure_txpkt` descriptor
//!   (`URE_TXPKT_HDR_SIZE`) — word0 = `len | TX_FS | TX_LS`, word1 = 0 (no
//!   checksum offload). `len` excludes the CRC (hardware appends it).
//!
//! This driver has **no hardware IRQ** (it is behind xHCI), so it cannot block
//! on a bound notification like a PCIe NIC; it uses the polled
//! `NetServer::try_handle_next` + `PollBulkIn` pattern.

use alloc::vec::Vec;

use driver_runtime::ipc::EndpointCap;
use driver_runtime::ipc::net::{NetReply, NetServer};
use kernel_core::driver_ipc::net::{MAX_FRAME_BYTES, NetDriverError, NetLinkEvent};
use syscall_lib::STDOUT_FILENO;
use usb_core::protocol::{UsbReply, UsbRequest};

use crate::regs::{
    URE_MCU_TYPE_PLA, URE_PHYSTATUS_10MBPS, URE_PHYSTATUS_100MBPS, URE_PHYSTATUS_1000MBPS,
    URE_PHYSTATUS_2500MBPS, URE_PHYSTATUS_LINK, URE_PLA_PHYSTATUS, URE_RX_BUF_ALIGN,
    URE_RXPKT_HDR_SIZE, URE_RXPKT_LEN_MASK, URE_TXPKT_HDR_SIZE, URE_TXPKT_TX_FS, URE_TXPKT_TX_LS,
};

/// Well-known service name a NIC driver registers its TX command endpoint as.
const SERVICE_NAME: &str = "net.nic";
/// Kernel-owned ingress endpoint a NIC publishes RX frames + link events to.
const INGRESS_SERVICE_NAME: &str = "net.nic.ingress";

/// Bulk-IN arm length: one max frame + descriptor + margin, kept under
/// `USB_MSG_MAX` (4096) so the captured buffer rides back inline.
const BULK_RX_LEN: u16 = 2048;

/// Ethernet frame-check-sequence length stripped from each received frame.
const ETHER_CRC_LEN: usize = 4;

/// Loop iterations between `PLA_PHYSTATUS` link polls (~1 s at the 1 ms idle
/// pacing below) — the dongle has no link-change interrupt.
const LINK_POLL_INTERVAL: u32 = 1000;

/// Max TX requests drained from the net stack per loop iteration (bounds the
/// inner drain so RX/link polling is not starved under heavy egress).
const TX_DRAIN_BUDGET: u32 = 16;

/// Max bulk-IN buffers pulled from the xHCI driver per loop iteration. The
/// driver keeps several Normal TRBs outstanding per IN endpoint
/// (`RX_QUEUE_DEPTH`), so a TX-busy gap can leave a burst of completed RX
/// buffers queued in its report FIFO; draining several per iteration keeps that
/// bounded FIFO from overflowing (and silently dropping frames) under load. The
/// loop stops early on the first empty poll, so an idle link still costs one
/// poll per iteration.
const RX_DRAIN_BUDGET: u32 = 8;

/// Consecutive empty RX polls before the stall watchdog re-asserts the chip's
/// RX datapath (≈1.5 s at the loop's ~1 ms idle pacing). Long enough not to
/// fight a momentarily quiet link, short enough to recover a wedged RX quickly.
const RX_STALL_KICK_ITERS: u32 = 1500;

/// Round `n` up to a multiple of `align` (a power of two).
#[inline]
fn roundup(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

/// Read `PLA_PHYSTATUS` and return `(link_up, speed_mbps)`.
fn link_state(usb_ep: u32, slot_id: u8) -> (bool, u32) {
    let phys = crate::ure_read_2(usb_ep, slot_id, URE_PLA_PHYSTATUS, URE_MCU_TYPE_PLA).unwrap_or(0);
    let up = phys & URE_PHYSTATUS_LINK != 0;
    let speed = if phys & URE_PHYSTATUS_2500MBPS != 0 {
        2500
    } else if phys & URE_PHYSTATUS_1000MBPS != 0 {
        1000
    } else if phys & URE_PHYSTATUS_100MBPS != 0 {
        100
    } else if phys & URE_PHYSTATUS_10MBPS != 0 {
        10
    } else {
        0
    };
    (up, speed)
}

/// Prepend the 8-byte Realtek V1 TX descriptor to `frame`. word0 carries the
/// length plus the first-segment/last-segment flags (single-buffer frame);
/// word1 (VLAN/offload) is zero.
fn build_tx_frame(frame: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(URE_TXPKT_HDR_SIZE + frame.len());
    let word0 = (frame.len() as u32) | URE_TXPKT_TX_FS | URE_TXPKT_TX_LS;
    out.extend_from_slice(&word0.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(frame);
    out
}

/// Number of received frames whose length + EtherType are sanity-logged at
/// startup (Track B.5 acceptance) — proves the bulk-IN path moves real frames
/// without flooding the serial log under sustained traffic.
const RX_SANITY_LOG_COUNT: u32 = 8;

/// Sanity-log one received frame's length + EtherType (`ure: rx len=… etype=…`).
/// Bounded by `RX_SANITY_LOG_COUNT` via the caller's counter.
fn log_rx_frame(frame: &[u8]) {
    syscall_lib::write_str(STDOUT_FILENO, "ure: rx len=0x");
    crate::write_u8_hex((frame.len() >> 8) as u8);
    crate::write_u8_hex((frame.len() & 0xff) as u8);
    // EtherType is bytes [12..14] (after dst+src MAC), big-endian.
    if frame.len() >= 14 {
        syscall_lib::write_str(STDOUT_FILENO, " etype=0x");
        crate::write_u8_hex(frame[12]);
        crate::write_u8_hex(frame[13]);
    }
    syscall_lib::write_str(STDOUT_FILENO, "\n");
}

/// Split a raw bulk-IN buffer into individual Ethernet frames (stripping each
/// 24-byte Realtek RX descriptor and the trailing 4-byte CRC) and publish each
/// up to the kernel net stack. The first `RX_SANITY_LOG_COUNT` frames are
/// length/EtherType-logged (`rx_logged` is the running count). Defensive: stops
/// at the first malformed or truncated descriptor rather than reading past the
/// buffer.
fn parse_and_publish_rx(buf: &[u8], net_server: &NetServer, rx_logged: &mut u32) {
    let mut off = 0usize;
    // A bulk buffer holds at most `BULK_RX_LEN / (hdr + min frame)` records;
    // bound the loop defensively regardless.
    while off + URE_RXPKT_HDR_SIZE <= buf.len() {
        let word0 = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
        let pktlen = (word0 & URE_RXPKT_LEN_MASK) as usize;
        // A frame must carry at least its CRC; a zero/short length marks the
        // end of valid records in this buffer.
        if pktlen <= ETHER_CRC_LEN {
            break;
        }
        let frame_start = off + URE_RXPKT_HDR_SIZE;
        if frame_start + pktlen > buf.len() {
            break; // truncated tail — no complete frame here
        }
        let frame = &buf[frame_start..frame_start + pktlen - ETHER_CRC_LEN];
        if !frame.is_empty() && frame.len() <= MAX_FRAME_BYTES as usize {
            if *rx_logged < RX_SANITY_LOG_COUNT {
                log_rx_frame(frame);
                *rx_logged += 1;
            }
            let _ = net_server.publish_rx_frame(frame);
        }
        // Next descriptor: header + frame, the frame rounded up to the RX align.
        off = frame_start + roundup(pktlen, URE_RX_BUF_ALIGN as usize);
    }
}

/// Register as a `RemoteNic` and serve bulk RX/TX until the process exits.
/// Diverges — the daemon never returns.
pub fn run_io_loop(usb_ep: u32, slot_id: u8, bulk_in_dci: u8, bulk_out_dci: u8, mac: [u8; 6]) -> ! {
    // 1. Create + register the TX command endpoint as `net.nic`.
    let cmd_ep = syscall_lib::create_endpoint();
    if cmd_ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "ure: net endpoint create failed — idling\n");
        crate::idle_loop();
    }
    let cmd_ep = cmd_ep as u32;
    if syscall_lib::ipc_register_service(cmd_ep, SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "ure: register 'net.nic' failed — idling\n");
        crate::idle_loop();
    }

    // 2. Look up the kernel ingress endpoint for RX + link-state publishing.
    let ingress = syscall_lib::ipc_lookup_service(INGRESS_SERVICE_NAME);
    let net_server = {
        let s = NetServer::new(EndpointCap::new(cmd_ep));
        if ingress == u64::MAX {
            s
        } else {
            s.with_ingress_endpoint(EndpointCap::new(ingress as u32))
        }
    };

    // 3. Publish the initial link state (mandatory — the net stack needs the
    //    MAC to program its filter, even on link-down).
    let (mut prev_up, speed) = link_state(usb_ep, slot_id);
    let _ = net_server.publish_link_state(NetLinkEvent {
        up: prev_up,
        mac,
        speed_mbps: if prev_up { speed } else { 0 },
    });
    syscall_lib::write_str(STDOUT_FILENO, "URE_STAGE2:NIC-UP\n");

    // Announce the resolved bulk endpoints (diagnostic).
    syscall_lib::write_str(STDOUT_FILENO, "ure: io-loop bulk_in_dci=0x");
    crate::write_u8_hex(bulk_in_dci);
    syscall_lib::write_str(STDOUT_FILENO, " bulk_out_dci=0x");
    crate::write_u8_hex(bulk_out_dci);
    syscall_lib::write_str(STDOUT_FILENO, "\n");

    // 4. Polled service loop (no IRQ available behind xHCI).
    let mut link_tick = 0u32;
    let mut rx_logged = 0u32;
    let mut rx_polls: u64 = 0;
    let mut rx_data_polls: u64 = 0;
    let mut rx_ipc_fail: u64 = 0;
    // TX counters: completed (ok) vs failed/timed-out bulk-OUT submits. These
    // are the wedge discriminator — if RX freezes (`rxd` stops climbing) while
    // `txo` keeps climbing, the dongle's RX engine stalled with TX healthy (a
    // chip-side RX wedge); if `txf` climbs instead, the bulk-OUT path is
    // stalling (TX wedge). A bounded log of the first few TX failures too.
    let mut tx_ok: u64 = 0;
    let mut tx_fail: u64 = 0;
    let mut tx_fail_logged: u32 = 0;
    // RX-stall watchdog: consecutive polls returning no frame, and a bounded
    // log counter for the recovery kicks.
    let mut rx_idle: u32 = 0;
    let mut kick_logged: u32 = 0;
    // Heartbeat cadence counter (one tick per loop iteration ≈ 1 ms), so the
    // stat line fires at a steady ~3 s regardless of how many RX polls ran.
    let mut hb_tick: u32 = 0;
    loop {
        // TX: drain pending net-stack send requests (bounded, non-blocking).
        let mut drained = 0u32;
        while drained < TX_DRAIN_BUDGET {
            let handled = net_server
                .try_handle_next(
                    |req| {
                        let status = send_frame(usb_ep, slot_id, bulk_out_dci, &req.frame);
                        // Tally TX outcomes for the wedge discriminator; log only
                        // the first few failures so a stalling bulk-OUT is visible
                        // without flooding serial.
                        if matches!(status, NetDriverError::Ok) {
                            tx_ok = tx_ok.wrapping_add(1);
                        } else {
                            tx_fail = tx_fail.wrapping_add(1);
                            if tx_fail_logged < 8 {
                                tx_fail_logged += 1;
                                syscall_lib::write_str(STDOUT_FILENO, "ure: tx FAIL len=0x");
                                crate::write_u8_hex((req.frame.len() >> 8) as u8);
                                crate::write_u8_hex(req.frame.len() as u8);
                                syscall_lib::write_str(STDOUT_FILENO, "\n");
                            }
                        }
                        NetReply { status }
                    },
                    |_bits| {},
                )
                .unwrap_or(false);
            if !handled {
                break;
            }
            drained += 1;
        }

        // RX: drain the bulk-IN endpoint. The xHCI driver keeps several Normal
        // TRBs outstanding per IN endpoint, so a TX-busy gap can leave several
        // completed RX buffers queued; pull up to `RX_DRAIN_BUDGET` of them per
        // iteration so the driver-side report FIFO does not overflow (and drop
        // frames) under burst. Stop early on the first empty/failed poll — the
        // FIFO is drained, nothing more to pull this iteration.
        let mut rx_got = false;
        for _ in 0..RX_DRAIN_BUDGET {
            rx_polls = rx_polls.wrapping_add(1);
            match crate::usb_call(
                usb_ep,
                &UsbRequest::PollBulkIn {
                    slot_id,
                    dci: bulk_in_dci,
                    len: BULK_RX_LEN,
                },
            ) {
                Some(UsbReply::BulkData {
                    data,
                    completion_code: 1,
                }) if !data.is_empty() => {
                    rx_data_polls = rx_data_polls.wrapping_add(1);
                    rx_got = true;
                    parse_and_publish_rx(&data, &net_server, &mut rx_logged);
                }
                Some(UsbReply::BulkData { .. }) => break, // empty — FIFO drained
                _ => {
                    rx_ipc_fail = rx_ipc_fail.wrapping_add(1);
                    break;
                }
            }
        }
        // The stall watchdog tracks consecutive *iterations* with no RX: reset
        // on any frame this iteration, else count toward a kick.
        if rx_got {
            rx_idle = 0;
        } else {
            rx_idle = rx_idle.saturating_add(1);
        }

        // RX-stall watchdog: under TX load the RTL8156 stops feeding the bulk-IN
        // endpoint with no USB error, wedging all inbound traffic (the armed TRB
        // never completes, so the xHCI layer cannot detect or reset it). If no
        // frame has arrived for ~RX_STALL_KICK_ITERS polls, re-assert the chip's
        // RX datapath. A no-op on a genuinely idle link, so this also fires when
        // there is simply no traffic — harmless, and it recovers the stall.
        if rx_idle >= RX_STALL_KICK_ITERS {
            rx_idle = 0;
            crate::ure_kick_rx(usb_ep, slot_id);
            if kick_logged < 8 {
                kick_logged += 1;
                syscall_lib::write_str(STDOUT_FILENO, "ure: RX idle — kicked RX datapath\n");
            }
        }
        // DIAGNOSTIC: periodic RX+TX stats (decimal, full-width — the old hex
        // path truncated each counter to a `u8` and wrapped at 256, making a
        // busy `data` look frozen). Watch this line for the wedge signature:
        //   rxp climbs, rxd FROZEN, txo climbs  → chip-side RX wedge (TX healthy)
        //   rxp climbs, rxd climbs, txf climbs   → bulk-OUT (TX) wedge
        //   everything frozen                    → io-loop blocked (TX timeout)
        // Heartbeat ONLY on idle iterations: `hb_tick` advances ~once/ms while
        // the link is quiet (≈one line / 3 s), but is frozen under load so a
        // busy link cannot flood the uncached bare-metal framebuffer console
        // with stat lines (each line costs ~hundreds of ms there, which would
        // throttle the very traffic it is meant to report). Under sustained load
        // the system is obviously alive; the line resumes when traffic quiets.
        if drained == 0 && !rx_got {
            hb_tick = hb_tick.wrapping_add(1);
        }
        if drained == 0 && !rx_got && hb_tick.is_multiple_of(3000) {
            syscall_lib::write_str(STDOUT_FILENO, "ure: hb rxp=");
            syscall_lib::write_u64(STDOUT_FILENO, rx_polls);
            syscall_lib::write_str(STDOUT_FILENO, " rxd=");
            syscall_lib::write_u64(STDOUT_FILENO, rx_data_polls);
            syscall_lib::write_str(STDOUT_FILENO, " rxf=");
            syscall_lib::write_u64(STDOUT_FILENO, rx_ipc_fail);
            syscall_lib::write_str(STDOUT_FILENO, " txo=");
            syscall_lib::write_u64(STDOUT_FILENO, tx_ok);
            syscall_lib::write_str(STDOUT_FILENO, " txf=");
            syscall_lib::write_u64(STDOUT_FILENO, tx_fail);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
        }

        // Link: poll occasionally (no link-change interrupt) and report edges.
        link_tick += 1;
        if link_tick >= LINK_POLL_INTERVAL {
            link_tick = 0;
            let (up, speed) = link_state(usb_ep, slot_id);
            if up != prev_up {
                let _ = net_server.publish_link_state(NetLinkEvent {
                    up,
                    mac,
                    speed_mbps: if up { speed } else { 0 },
                });
                prev_up = up;
            }
        }

        // Idle pacing — sleep ~1 ms ONLY when this iteration was fully idle (no
        // TX drained and no RX received). Under load the synchronous USB calls
        // (`SubmitBulkOut`/`PollBulkIn`, each a round-trip to the xHCI driver)
        // already yield the core, so an unconditional 1 ms sleep just adds
        // latency to interactive traffic (slow SSH key echo) and throttles TX
        // throughput (the "freezes when typing fast" backpressure). When idle,
        // keep the 1 ms pacing so the loop doesn't peg a core polling a quiet
        // link.
        if drained == 0 && !rx_got {
            let _ = syscall_lib::nanosleep_for(0, 1_000_000);
        }
    }
}

/// Hand one outbound frame to the hardware: prepend the TX descriptor and
/// submit it on the bulk-OUT endpoint. Maps the USB result onto a
/// `NetDriverError` for the net stack.
fn send_frame(usb_ep: u32, slot_id: u8, bulk_out_dci: u8, frame: &[u8]) -> NetDriverError {
    if frame.is_empty() || frame.len() > MAX_FRAME_BYTES as usize {
        return NetDriverError::InvalidFrame;
    }
    let tx = build_tx_frame(frame);
    match crate::usb_call(
        usb_ep,
        &UsbRequest::SubmitBulkOut {
            slot_id,
            dci: bulk_out_dci,
            data: tx,
        },
    ) {
        Some(UsbReply::TransferComplete {
            completion_code: 1, ..
        }) => NetDriverError::Ok,
        _ => NetDriverError::RingFull,
    }
}
