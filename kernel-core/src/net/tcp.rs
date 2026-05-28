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
}
