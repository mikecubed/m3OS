use alloc::collections::VecDeque;
use alloc::vec::Vec;

use super::ipv4;
use crate::types::Ipv4Addr;

/// TCP flag bits.
pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;

/// Parsed TCP header.
#[derive(Debug, Clone, Copy)]
pub struct TcpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub data_offset: u8,
    pub flags: u8,
    pub window: u16,
    pub checksum: u16,
    pub urgent: u16,
}

/// Maximum TCP segment size that fits in an IPv4 packet.
pub const MAX_TCP_SEGMENT: usize = 65515;

/// Compute TCP checksum with pseudo-header.
///
/// # Panics
///
/// Panics if `tcp_data.len()` exceeds `u16::MAX` (65535 bytes).
pub fn tcp_checksum(src_ip: Ipv4Addr, dst_ip: Ipv4Addr, tcp_data: &[u8]) -> u16 {
    assert!(
        tcp_data.len() <= u16::MAX as usize,
        "tcp_checksum: tcp_data too long ({} bytes, max {})",
        tcp_data.len(),
        u16::MAX
    );
    let tcp_len = tcp_data.len() as u16;
    let mut pseudo = Vec::with_capacity(12 + tcp_data.len());
    pseudo.extend_from_slice(&src_ip);
    pseudo.extend_from_slice(&dst_ip);
    pseudo.push(0); // reserved
    pseudo.push(6); // protocol TCP
    pseudo.extend_from_slice(&tcp_len.to_be_bytes());
    pseudo.extend_from_slice(tcp_data);
    ipv4::checksum(&pseudo)
}

/// Parse a TCP segment.
pub fn parse(data: &[u8]) -> Option<(TcpHeader, &[u8])> {
    if data.len() < 20 {
        return None;
    }

    let data_offset = data[12] >> 4;
    if data_offset < 5 {
        return None;
    }
    let header_len = (data_offset as usize) * 4;
    if data.len() < header_len {
        return None;
    }

    let header = TcpHeader {
        src_port: u16::from_be_bytes([data[0], data[1]]),
        dst_port: u16::from_be_bytes([data[2], data[3]]),
        seq: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        ack: u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
        data_offset,
        flags: data[13],
        window: u16::from_be_bytes([data[14], data[15]]),
        checksum: u16::from_be_bytes([data[16], data[17]]),
        urgent: u16::from_be_bytes([data[18], data[19]]),
    };

    Some((header, &data[header_len..]))
}

/// Parameters for building a TCP segment.
pub struct TcpBuildParams {
    pub src_ip: Ipv4Addr,
    pub dst_ip: Ipv4Addr,
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
}

/// Build a TCP segment with auto-computed checksum.
pub fn build(p: &TcpBuildParams, payload: &[u8]) -> Vec<u8> {
    let max_payload = MAX_TCP_SEGMENT - 20;
    let payload = if payload.len() > max_payload {
        &payload[..max_payload]
    } else {
        payload
    };
    let data_offset: u8 = 5;
    let total_len = 20 + payload.len();
    let mut pkt = Vec::with_capacity(total_len);

    pkt.extend_from_slice(&p.src_port.to_be_bytes());
    pkt.extend_from_slice(&p.dst_port.to_be_bytes());
    pkt.extend_from_slice(&p.seq.to_be_bytes());
    pkt.extend_from_slice(&p.ack.to_be_bytes());
    pkt.push(data_offset << 4);
    pkt.push(p.flags);
    pkt.extend_from_slice(&p.window.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    pkt.extend_from_slice(&0u16.to_be_bytes()); // urgent pointer
    pkt.extend_from_slice(payload);

    let cksum = tcp_checksum(p.src_ip, p.dst_ip, &pkt);
    pkt[16] = (cksum >> 8) as u8;
    pkt[17] = cksum as u8;

    pkt
}

// ===========================================================================
// Phase 77 Track D.2 — RFC 6298 retransmission timeout estimator
// ===========================================================================

/// Minimum RTO (RFC 6298 §2.4: "SHOULD be rounded up to 1 second").
pub const RTO_MIN_MS: u32 = 1_000;
/// Maximum RTO (RFC 6298 §5.7 permits a 60 s cap).
pub const RTO_MAX_MS: u32 = 60_000;
/// Initial RTO before any measurement (RFC 6298 §2.1).
pub const RTO_INITIAL_MS: u32 = 1_000;
/// Clock granularity G used in the variance term (ms-resolution ticks → 1).
const RTO_CLOCK_GRANULARITY_MS: u32 = 1;
/// K factor from RFC 6298 (RTO = SRTT + max(G, K·RTTVAR)).
const RTO_K: u32 = 4;

/// Per-connection round-trip-time estimator implementing the RFC 6298 SRTT /
/// RTTVAR smoothing with the standard 1 s minimum / 60 s maximum RTO clamps and
/// exponential backoff on timeout. All values are in milliseconds; integer
/// math mirrors the RFC's 1/8 (SRTT) and 1/4 (RTTVAR) gains exactly.
#[derive(Debug, Clone, Copy)]
pub struct RttEstimator {
    srtt_ms: u32,
    rttvar_ms: u32,
    rto_ms: u32,
    have_measurement: bool,
}

impl Default for RttEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl RttEstimator {
    pub const fn new() -> Self {
        Self {
            srtt_ms: 0,
            rttvar_ms: 0,
            rto_ms: RTO_INITIAL_MS,
            have_measurement: false,
        }
    }

    /// Current retransmission timeout in milliseconds.
    pub fn rto_ms(&self) -> u32 {
        self.rto_ms
    }

    /// Feed a fresh RTT sample `r_ms` (must NOT be a retransmitted segment —
    /// Karn's algorithm: the caller skips measurements on retransmits).
    /// Recomputes SRTT, RTTVAR, and RTO per RFC 6298 §2.2/§2.3.
    pub fn on_measurement(&mut self, r_ms: u32) {
        if !self.have_measurement {
            // First measurement (§2.2): SRTT = R, RTTVAR = R/2.
            self.srtt_ms = r_ms;
            self.rttvar_ms = r_ms / 2;
            self.have_measurement = true;
        } else {
            // Subsequent (§2.3): RTTVAR = 3/4·RTTVAR + 1/4·|SRTT-R|,
            //                    SRTT  = 7/8·SRTT  + 1/8·R.
            let delta = self.srtt_ms.abs_diff(r_ms);
            self.rttvar_ms = (3 * self.rttvar_ms + delta) / 4;
            self.srtt_ms = (7 * self.srtt_ms + r_ms) / 8;
        }
        let var_term = (RTO_K * self.rttvar_ms).max(RTO_CLOCK_GRANULARITY_MS);
        self.rto_ms = self
            .srtt_ms
            .saturating_add(var_term)
            .clamp(RTO_MIN_MS, RTO_MAX_MS);
    }

    /// RFC 6298 §5.5: on RTO expiry, double the timeout (exponential backoff),
    /// capped at the maximum.
    pub fn on_timeout(&mut self) {
        self.rto_ms = self.rto_ms.saturating_mul(2).min(RTO_MAX_MS);
    }
}

// ===========================================================================
// Phase 77 Track D.2 (follow-up) — multi-segment retransmit queue
// ===========================================================================

/// Maximum retransmissions of a single segment before the peer is presumed
/// dead and the connection is reset (RFC 1122 R2 equivalent). With the RFC 6298
/// exponential backoff (1s, 2s, 4s, …, 60s) this is well over a minute of total
/// retry time before giving up.
pub const MAX_RETRANSMITS: u32 = 8;

/// One unacknowledged outbound segment retained for possible retransmission.
///
/// m3OS tracks **every** in-flight segment (not just the oldest), so a dropped
/// segment anywhere in a multi-segment window is recovered — the prior
/// single-slot design left every segment after the first unprotected.
#[derive(Debug, Clone)]
pub struct RetransmitSeg {
    /// Sequence number just past this segment: an inbound ACK whose value is
    /// `>= end_seq` (wrapping-aware) fully acknowledges it. For a pure SYN/FIN
    /// this is `start_seq + 1` (the control flag's phantom sequence byte).
    pub end_seq: u32,
    /// The segment's TCP flags, replayed verbatim on retransmit.
    pub flags: u8,
    /// The payload bytes, retained for replay (empty for SYN/FIN).
    pub payload: Vec<u8>,
    /// Tick (ms) at which this segment was FIRST transmitted (RTT sample base).
    first_sent_tick: u64,
    /// Retransmission count. Karn's algorithm: an RTT sample is taken only when
    /// this is 0 (the segment was never retransmitted, so the ACK is
    /// unambiguous).
    retx_count: u32,
}

impl RetransmitSeg {
    /// Sequence length this segment consumes: 1 for a pure SYN/FIN phantom
    /// byte (empty payload), else the payload length.
    fn seq_len(&self) -> u32 {
        if self.payload.is_empty() && self.flags & (TCP_SYN | TCP_FIN) != 0 {
            1
        } else {
            self.payload.len() as u32
        }
    }

    /// The segment's original (first) sequence number.
    pub fn start_seq(&self) -> u32 {
        self.end_seq.wrapping_sub(self.seq_len())
    }
}

/// What `RetransmitQueue::service_rto` decided when the RTO timer fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtoAction {
    /// Timer not yet expired (or nothing outstanding) — nothing to do.
    Idle,
    /// The oldest segment exhausted `MAX_RETRANSMITS`; the peer is presumed
    /// dead. The caller must reset the connection. The queue is now empty.
    Reset,
    /// Replay this segment with its original sequence number.
    Retransmit {
        seq: u32,
        flags: u8,
        payload: Vec<u8>,
    },
}

/// Per-connection queue of unacknowledged outbound segments plus the single
/// RFC 6298 retransmission timer that times the **oldest** of them.
///
/// RFC 6298 model:
/// * §5.1 — start the timer when data is sent and the timer is not running.
/// * §5.2 — turn the timer off when all outstanding data is acknowledged.
/// * §5.3 — restart the timer when an ACK acknowledges new data and data
///   remains outstanding.
/// * §5.4 — on expiry, retransmit the **earliest** unacknowledged segment.
/// * §5.5/§5.6 — back the RTO off (double it) and restart the timer.
#[derive(Debug, Clone)]
pub struct RetransmitQueue {
    segs: VecDeque<RetransmitSeg>,
    /// Absolute tick (ms) at which the RTO fires; `None` when the queue is
    /// empty (timer off).
    rto_deadline: Option<u64>,
}

impl Default for RetransmitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl RetransmitQueue {
    pub const fn new() -> Self {
        Self {
            segs: VecDeque::new(),
            rto_deadline: None,
        }
    }

    /// True when no segment is outstanding.
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// Number of segments currently outstanding.
    pub fn len(&self) -> usize {
        self.segs.len()
    }

    /// Current RTO deadline (absolute tick), or `None` when the timer is off.
    pub fn deadline(&self) -> Option<u64> {
        self.rto_deadline
    }

    /// Retain a freshly-sent segment for possible replay. Starts the RTO timer
    /// if it was not already running (RFC 6298 §5.1); pushing a later segment
    /// while an earlier one is still timed does **not** restart the timer.
    pub fn push(
        &mut self,
        end_seq: u32,
        flags: u8,
        payload: Vec<u8>,
        now: u64,
        rtt: &RttEstimator,
    ) {
        self.segs.push_back(RetransmitSeg {
            end_seq,
            flags,
            payload,
            first_sent_tick: now,
            retx_count: 0,
        });
        if self.rto_deadline.is_none() {
            self.rto_deadline = Some(now.saturating_add(rtt.rto_ms() as u64));
        }
    }

    /// Process an inbound cumulative ACK: drop every fully-acknowledged segment
    /// from the front, take one Karn-safe RTT sample (from the oldest segment
    /// that was never retransmitted), and stop or restart the RTO timer
    /// (RFC 6298 §5.2/§5.3).
    pub fn on_ack(&mut self, ack: u32, now: u64, rtt: &mut RttEstimator) {
        let mut acked_any = false;
        let mut sample: Option<u32> = None;
        while let Some(front) = self.segs.front() {
            // Fully acked iff `ack >= front.end_seq` in wrapping sequence space.
            if ack.wrapping_sub(front.end_seq) < 0x8000_0000 {
                let seg = self.segs.pop_front().expect("front exists");
                acked_any = true;
                // Karn's algorithm: never sample a retransmitted segment.
                if seg.retx_count == 0 && sample.is_none() {
                    let rtt_ms =
                        now.saturating_sub(seg.first_sent_tick).min(u32::MAX as u64) as u32;
                    sample = Some(rtt_ms);
                }
            } else {
                break;
            }
        }
        if let Some(r) = sample {
            rtt.on_measurement(r);
        }
        if self.segs.is_empty() {
            // §5.2 — all outstanding data acknowledged: timer off.
            self.rto_deadline = None;
        } else if acked_any {
            // §5.3 — new data acked, data still outstanding: restart the timer.
            self.rto_deadline = Some(now.saturating_add(rtt.rto_ms() as u64));
        }
    }

    /// Service the RTO timer at time `now`. When it has expired, retransmit the
    /// **oldest** unacknowledged segment (RFC 6298 §5.4), back the RTO off
    /// (§5.5), and restart the timer (§5.6) — unless that segment has already
    /// been retransmitted `MAX_RETRANSMITS` times, in which case the connection
    /// must be reset.
    pub fn service_rto(&mut self, now: u64, rtt: &mut RttEstimator) -> RtoAction {
        let Some(deadline) = self.rto_deadline else {
            return RtoAction::Idle;
        };
        if now < deadline {
            return RtoAction::Idle;
        }
        let Some(front) = self.segs.front_mut() else {
            // Timer armed with an empty queue should not happen, but stay safe.
            self.rto_deadline = None;
            return RtoAction::Idle;
        };
        if front.retx_count >= MAX_RETRANSMITS {
            self.clear();
            return RtoAction::Reset;
        }
        front.retx_count = front.retx_count.saturating_add(1);
        let action = RtoAction::Retransmit {
            seq: front.start_seq(),
            flags: front.flags,
            payload: front.payload.clone(),
        };
        // Karn: back off the RTO; the suppressed RTT sample (retx_count > 0)
        // keeps the doubled RTO from being corrupted by an ambiguous ACK.
        rtt.on_timeout();
        self.rto_deadline = Some(now.saturating_add(rtt.rto_ms() as u64));
        action
    }

    /// Drop all outstanding segments and stop the timer (connection reset/close).
    pub fn clear(&mut self) {
        self.segs.clear();
        self.rto_deadline = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_params() -> TcpBuildParams {
        TcpBuildParams {
            src_ip: [10, 0, 0, 1],
            dst_ip: [10, 0, 0, 2],
            src_port: 12345,
            dst_port: 80,
            seq: 1000,
            ack: 2000,
            flags: TCP_ACK,
            window: 8192,
        }
    }

    #[test]
    fn parse_valid() {
        let p = sample_params();
        let seg = build(&p, b"GET / HTTP/1.0");
        let (header, payload) = parse(&seg).unwrap();
        assert_eq!(header.src_port, 12345);
        assert_eq!(header.dst_port, 80);
        assert_eq!(header.seq, 1000);
        assert_eq!(header.ack, 2000);
        assert_eq!(header.flags, TCP_ACK);
        assert_eq!(payload, b"GET / HTTP/1.0");
    }

    #[test]
    fn parse_too_short() {
        assert!(parse(&[0u8; 19]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn build_round_trip() {
        let p = sample_params();
        let payload = b"hello tcp";
        let seg = build(&p, payload);
        let (header, data) = parse(&seg).unwrap();
        assert_eq!(header.src_port, p.src_port);
        assert_eq!(header.dst_port, p.dst_port);
        assert_eq!(header.seq, p.seq);
        assert_eq!(header.ack, p.ack);
        assert_eq!(data, payload);
    }

    #[test]
    fn tcp_checksum_verification() {
        let p = sample_params();
        let seg = build(&p, b"data");
        // Verify checksum: recomputing should yield 0
        let verify = tcp_checksum(p.src_ip, p.dst_ip, &seg);
        assert_eq!(verify, 0);
    }

    #[test]
    fn flag_constants() {
        assert_eq!(TCP_FIN, 0x01);
        assert_eq!(TCP_SYN, 0x02);
        assert_eq!(TCP_RST, 0x04);
        assert_eq!(TCP_PSH, 0x08);
        assert_eq!(TCP_ACK, 0x10);
        // Flags are combinable
        let combined = TCP_SYN | TCP_ACK;
        assert_eq!(combined, 0x12);
    }

    // ---- Phase 77 Track D.2 — RFC 6298 RTO estimator -----------------------

    #[test]
    fn rto_initial_is_one_second() {
        let est = RttEstimator::new();
        assert_eq!(est.rto_ms(), RTO_INITIAL_MS);
        assert_eq!(est.rto_ms(), 1_000);
    }

    #[test]
    fn rto_lan_rtt_clamps_to_min() {
        // A fast LAN RTT (20 ms): SRTT=20, RTTVAR=10, RTO=20+max(1,40)=60 →
        // clamped up to the 1 s minimum (RFC 6298 §2.4).
        let mut est = RttEstimator::new();
        est.on_measurement(20);
        assert_eq!(est.rto_ms(), RTO_MIN_MS);
    }

    #[test]
    fn rto_large_rtt_uses_formula() {
        // First measurement R=2000: SRTT=2000, RTTVAR=1000,
        // RTO = 2000 + max(1, 4·1000) = 6000.
        let mut est = RttEstimator::new();
        est.on_measurement(2_000);
        assert_eq!(est.rto_ms(), 6_000);
    }

    #[test]
    fn rto_subsequent_measurement_smooths() {
        // First R=2000 → SRTT=2000, RTTVAR=1000.
        // Second R=2000 → |SRTT-R|=0; RTTVAR=(3·1000+0)/4=750;
        // SRTT=(7·2000+2000)/8=2000; RTO=2000+max(1,4·750)=5000.
        let mut est = RttEstimator::new();
        est.on_measurement(2_000);
        est.on_measurement(2_000);
        assert_eq!(est.rto_ms(), 5_000);
    }

    #[test]
    fn rto_backoff_doubles_and_caps() {
        let mut est = RttEstimator::new();
        assert_eq!(est.rto_ms(), 1_000);
        est.on_timeout();
        assert_eq!(est.rto_ms(), 2_000);
        est.on_timeout();
        assert_eq!(est.rto_ms(), 4_000);
        // Drive well past the 60 s cap.
        for _ in 0..10 {
            est.on_timeout();
        }
        assert_eq!(est.rto_ms(), RTO_MAX_MS);
    }

    #[test]
    fn rto_never_below_min_after_backoff_then_measure() {
        let mut est = RttEstimator::new();
        est.on_timeout(); // 2000
        est.on_timeout(); // 4000
        // A fresh fast measurement recomputes from SRTT/RTTVAR, re-clamped ≥1s.
        est.on_measurement(10);
        assert_eq!(est.rto_ms(), RTO_MIN_MS);
    }

    // ---- multi-segment retransmit queue ------------------------------------

    fn data_seg(end_seq: u32, len: usize) -> (u32, u8, Vec<u8>) {
        (end_seq, TCP_ACK | TCP_PSH, alloc::vec![0xAB; len])
    }

    #[test]
    fn rtq_empty_is_idle() {
        let mut q = RetransmitQueue::new();
        let mut rtt = RttEstimator::new();
        assert!(q.is_empty());
        assert_eq!(q.deadline(), None);
        assert_eq!(q.service_rto(10_000, &mut rtt), RtoAction::Idle);
    }

    #[test]
    fn rtq_push_arms_timer_once() {
        let mut q = RetransmitQueue::new();
        let rtt = RttEstimator::new(); // RTO = 1000
        let (e, f, p) = data_seg(1100, 100); // seq 1000..1100
        q.push(e, f, p, 500, &rtt);
        assert_eq!(q.len(), 1);
        assert_eq!(q.deadline(), Some(1500)); // 500 + 1000
        // A second push while the first is outstanding does NOT restart the timer.
        let (e2, f2, p2) = data_seg(1300, 200);
        q.push(e2, f2, p2, 700, &rtt);
        assert_eq!(q.len(), 2);
        assert_eq!(q.deadline(), Some(1500)); // unchanged — still timing seg 1.
    }

    #[test]
    fn rtq_cumulative_ack_drops_covered_segments() {
        let mut q = RetransmitQueue::new();
        let mut rtt = RttEstimator::new();
        q.push(1100, TCP_ACK | TCP_PSH, alloc::vec![0; 100], 0, &rtt); // 1000..1100
        q.push(1300, TCP_ACK | TCP_PSH, alloc::vec![0; 200], 0, &rtt); // 1100..1300
        q.push(1400, TCP_ACK | TCP_PSH, alloc::vec![0; 100], 0, &rtt); // 1300..1400
        // ACK 1300 covers the first two segments exactly.
        q.on_ack(1300, 50, &mut rtt);
        assert_eq!(q.len(), 1);
        // Data still outstanding → timer restarted from `now`.
        assert_eq!(q.deadline(), Some(50 + rtt.rto_ms() as u64));
        // ACK 1400 clears the rest → timer off.
        q.on_ack(1400, 60, &mut rtt);
        assert!(q.is_empty());
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn rtq_partial_ack_keeps_uncovered_segment() {
        let mut q = RetransmitQueue::new();
        let mut rtt = RttEstimator::new();
        q.push(1100, TCP_ACK | TCP_PSH, alloc::vec![0; 100], 0, &rtt);
        q.push(1300, TCP_ACK | TCP_PSH, alloc::vec![0; 200], 0, &rtt);
        // ACK 1200 falls in the middle of the second segment → does NOT free it
        // (TCP acks whole segments here; a partial ack leaves the segment).
        q.on_ack(1200, 10, &mut rtt);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn rtq_ack_takes_rtt_sample_from_oldest_clean_segment() {
        let mut q = RetransmitQueue::new();
        let mut rtt = RttEstimator::new();
        assert_eq!(rtt.rto_ms(), 1000);
        // Sent at t=0; acked at t=2000 → RTT sample 2000.
        q.push(1100, TCP_ACK | TCP_PSH, alloc::vec![0; 100], 0, &rtt);
        q.on_ack(1100, 2000, &mut rtt);
        // First measurement R=2000: SRTT=2000, RTTVAR=1000, RTO=2000+4*1000=6000.
        assert_eq!(rtt.rto_ms(), 6000);
    }

    #[test]
    fn rtq_service_rto_retransmits_oldest_with_original_seq() {
        let mut q = RetransmitQueue::new();
        let mut rtt = RttEstimator::new(); // RTO 1000
        q.push(1100, TCP_ACK | TCP_PSH, alloc::vec![1; 100], 0, &rtt); // seq 1000
        q.push(1300, TCP_ACK | TCP_PSH, alloc::vec![2; 200], 0, &rtt); // seq 1100
        // Before the deadline: idle.
        assert_eq!(q.service_rto(999, &mut rtt), RtoAction::Idle);
        // At the deadline: retransmit the OLDEST segment with its first seq.
        match q.service_rto(1000, &mut rtt) {
            RtoAction::Retransmit {
                seq,
                flags,
                payload,
            } => {
                assert_eq!(seq, 1000); // 1100 - 100
                assert_eq!(flags, TCP_ACK | TCP_PSH);
                assert_eq!(payload, alloc::vec![1; 100]);
            }
            other => panic!("expected retransmit, got {other:?}"),
        }
        // RTO backed off to 2000, timer restarted from now.
        assert_eq!(rtt.rto_ms(), 2000);
        assert_eq!(q.deadline(), Some(1000 + 2000));
        // Both segments are still queued (only the oldest was replayed).
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn rtq_syn_and_fin_seq_len_is_one() {
        let mut q = RetransmitQueue::new();
        let mut rtt = RttEstimator::new();
        // SYN occupying seq 5000 (end_seq 5001).
        q.push(5001, TCP_SYN, Vec::new(), 0, &rtt);
        match q.service_rto(1000, &mut rtt) {
            RtoAction::Retransmit { seq, flags, .. } => {
                assert_eq!(seq, 5000);
                assert_eq!(flags, TCP_SYN);
            }
            other => panic!("expected SYN retransmit, got {other:?}"),
        }
    }

    #[test]
    fn rtq_reset_after_max_retransmits() {
        let mut q = RetransmitQueue::new();
        let mut rtt = RttEstimator::new();
        q.push(1100, TCP_ACK | TCP_PSH, alloc::vec![0; 100], 0, &rtt);
        let mut now;
        // Each expiry retransmits and doubles the RTO; after MAX_RETRANSMITS
        // replays the next expiry resets the connection.
        for _ in 0..MAX_RETRANSMITS {
            now = q.deadline().expect("armed");
            assert!(matches!(
                q.service_rto(now, &mut rtt),
                RtoAction::Retransmit { .. }
            ));
        }
        now = q.deadline().expect("still armed");
        assert_eq!(q.service_rto(now, &mut rtt), RtoAction::Reset);
        assert!(q.is_empty());
        assert_eq!(q.deadline(), None);
    }

    #[test]
    fn rtq_karn_suppresses_sample_for_retransmitted_segment() {
        let mut q = RetransmitQueue::new();
        let mut rtt = RttEstimator::new();
        q.push(1100, TCP_ACK | TCP_PSH, alloc::vec![0; 100], 0, &rtt);
        // Force a retransmit (RTO fires at 1000).
        let _ = q.service_rto(1000, &mut rtt);
        let rto_after_backoff = rtt.rto_ms(); // 2000
        // Now the ACK arrives. Karn: no RTT sample taken (segment was retx'd),
        // so the estimator keeps the backed-off RTO untouched.
        q.on_ack(1100, 1100, &mut rtt);
        assert!(q.is_empty());
        assert_eq!(rtt.rto_ms(), rto_after_backoff);
    }

    #[test]
    fn rtq_wrapping_ack_across_u32_boundary() {
        let mut q = RetransmitQueue::new();
        let mut rtt = RttEstimator::new();
        // Segment straddling the u32 wrap: seq 0xFFFF_FFF0..0x0000_0040.
        let end_seq = 0x0000_0040u32;
        q.push(end_seq, TCP_ACK | TCP_PSH, alloc::vec![0; 80], 0, &rtt);
        // start_seq must wrap correctly to 0xFFFF_FFF0.
        match q.service_rto(1000, &mut rtt) {
            RtoAction::Retransmit { seq, .. } => assert_eq!(seq, 0xFFFF_FFF0),
            other => panic!("expected retransmit, got {other:?}"),
        }
        // An ACK just past the wrap clears it.
        q.on_ack(0x0000_0040, 1100, &mut rtt);
        assert!(q.is_empty());
    }
}
