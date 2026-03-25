//! TCP state machine for a single connection (P16-T040 through P16-T052).

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

use super::arp::Ipv4Addr;
use super::ipv4::{self, Ipv4Header};

// ===========================================================================
// TCP Header (P16-T040)
// ===========================================================================

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
    pub data_offset: u8, // in 32-bit words
    pub flags: u8,
    pub window: u16,
    pub checksum: u16,
    pub urgent: u16,
}

// ===========================================================================
// TCP checksum (P16-T041)
// ===========================================================================

/// Compute TCP checksum with pseudo-header.
fn tcp_checksum(src_ip: Ipv4Addr, dst_ip: Ipv4Addr, tcp_data: &[u8]) -> u16 {
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

// ===========================================================================
// Parse / Build (P16-T042)
// ===========================================================================

/// Parse a TCP segment.
pub fn parse(data: &[u8]) -> Option<(TcpHeader, &[u8])> {
    if data.len() < 20 {
        return None;
    }

    let data_offset = data[12] >> 4;
    // Minimum data offset is 5 (20-byte header). Reject malformed segments.
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
struct TcpBuildParams {
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
}

/// Build a TCP segment with auto-computed checksum.
fn build(p: &TcpBuildParams, payload: &[u8]) -> Vec<u8> {
    let data_offset: u8 = 5; // 20 bytes, no options
    let total_len = 20 + payload.len();
    let mut pkt = Vec::with_capacity(total_len);

    pkt.extend_from_slice(&p.src_port.to_be_bytes());
    pkt.extend_from_slice(&p.dst_port.to_be_bytes());
    pkt.extend_from_slice(&p.seq.to_be_bytes());
    pkt.extend_from_slice(&p.ack.to_be_bytes());
    pkt.push(data_offset << 4); // data offset + reserved
    pkt.push(p.flags);
    pkt.extend_from_slice(&p.window.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    pkt.extend_from_slice(&0u16.to_be_bytes()); // urgent pointer
    pkt.extend_from_slice(payload);

    // Compute and fill checksum.
    let cksum = tcp_checksum(p.src_ip, p.dst_ip, &pkt);
    pkt[16] = (cksum >> 8) as u8;
    pkt[17] = cksum as u8;

    pkt
}

// ===========================================================================
// TCP State (P16-T043)
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    LastAck,
    TimeWait,
}

// ===========================================================================
// TCP Connection (P16-T044)
// ===========================================================================

/// Default TCP window size.
const DEFAULT_WINDOW: u16 = 8192;

/// A single TCP connection.
pub struct TcpConnection {
    pub state: TcpState,
    pub local_ip: Ipv4Addr,
    pub local_port: u16,
    pub remote_ip: Ipv4Addr,
    pub remote_port: u16,
    // Send sequence variables
    pub snd_una: u32,
    pub snd_nxt: u32,
    pub snd_wnd: u16,
    // Receive sequence variables
    pub rcv_nxt: u32,
    pub rcv_wnd: u16,
    // Buffers
    pub recv_buf: VecDeque<u8>,
    pub send_buf: VecDeque<u8>,
}

impl TcpConnection {
    fn new(local_ip: Ipv4Addr, local_port: u16) -> Self {
        Self {
            state: TcpState::Closed,
            local_ip,
            local_port,
            remote_ip: [0; 4],
            remote_port: 0,
            snd_una: 0,
            snd_nxt: 0,
            snd_wnd: DEFAULT_WINDOW,
            rcv_nxt: 0,
            rcv_wnd: DEFAULT_WINDOW,
            recv_buf: VecDeque::new(),
            send_buf: VecDeque::new(),
        }
    }

    /// Helper to create build params for this connection.
    fn build_params(&self, flags: u8) -> TcpBuildParams {
        TcpBuildParams {
            src_ip: self.local_ip,
            dst_ip: self.remote_ip,
            src_port: self.local_port,
            dst_port: self.remote_port,
            seq: self.snd_nxt,
            ack: self.rcv_nxt,
            flags,
            window: self.rcv_wnd,
        }
    }

    /// Send a segment with the given flags and payload, then send via IPv4.
    fn send_segment(&self, flags: u8, payload: &[u8]) {
        let p = self.build_params(flags);
        let seg = build(&p, payload);
        ipv4::send(self.remote_ip, ipv4::PROTO_TCP, &seg);
    }

    // P16-T045: Active open (client connect)
    fn connect(&mut self, remote_ip: Ipv4Addr, remote_port: u16) {
        self.remote_ip = remote_ip;
        self.remote_port = remote_port;
        self.snd_nxt = crate::arch::x86_64::interrupts::tick_count() as u32;
        self.snd_una = self.snd_nxt;

        let p = TcpBuildParams {
            src_ip: self.local_ip,
            dst_ip: self.remote_ip,
            src_port: self.local_port,
            dst_port: self.remote_port,
            seq: self.snd_nxt,
            ack: 0,
            flags: TCP_SYN,
            window: self.rcv_wnd,
        };
        let syn = build(&p, &[]);
        ipv4::send(self.remote_ip, ipv4::PROTO_TCP, &syn);
        self.snd_nxt = self.snd_nxt.wrapping_add(1);
        self.state = TcpState::SynSent;

        log::debug!(
            "[tcp] SYN sent to {}.{}.{}.{}:{}",
            remote_ip[0],
            remote_ip[1],
            remote_ip[2],
            remote_ip[3],
            remote_port
        );
    }

    fn listen(&mut self) {
        self.state = TcpState::Listen;
    }

    // P16-T047: Data send
    fn tcp_send(&mut self, data: &[u8]) {
        if self.state != TcpState::Established {
            return;
        }
        self.send_segment(TCP_ACK | TCP_PSH, data);
        self.snd_nxt = self.snd_nxt.wrapping_add(data.len() as u32);
    }

    // P16-T049 / P16-T050: Close
    fn close(&mut self) {
        match self.state {
            TcpState::Established | TcpState::CloseWait => {
                self.send_segment(TCP_FIN | TCP_ACK, &[]);
                self.snd_nxt = self.snd_nxt.wrapping_add(1);
                self.state = if self.state == TcpState::Established {
                    TcpState::FinWait1
                } else {
                    TcpState::LastAck
                };
            }
            _ => {}
        }
    }

    /// Process an incoming TCP segment.
    fn handle_segment(&mut self, header: &TcpHeader, payload: &[u8]) {
        // P16-T051: RST handling
        if header.flags & TCP_RST != 0 {
            log::info!("[tcp] RST received — connection closed");
            self.state = TcpState::Closed;
            return;
        }

        let has_syn = header.flags & TCP_SYN != 0;
        let has_ack = header.flags & TCP_ACK != 0;
        let has_fin = header.flags & TCP_FIN != 0;

        match self.state {
            // P16-T045: Expecting SYN-ACK
            TcpState::SynSent if has_syn && has_ack => {
                self.rcv_nxt = header.seq.wrapping_add(1);
                self.snd_una = header.ack;
                self.snd_wnd = header.window;
                self.send_segment(TCP_ACK, &[]);
                self.state = TcpState::Established;
                log::info!("[tcp] connection established (active)");
            }
            // P16-T046: Incoming SYN on listening socket
            TcpState::Listen if has_syn => {
                self.remote_port = header.src_port;
                self.rcv_nxt = header.seq.wrapping_add(1);
                self.snd_nxt = crate::arch::x86_64::interrupts::tick_count() as u32;
                self.snd_una = self.snd_nxt;
                self.send_segment(TCP_SYN | TCP_ACK, &[]);
                self.snd_nxt = self.snd_nxt.wrapping_add(1);
                self.state = TcpState::SynReceived;
                log::debug!("[tcp] SYN-ACK sent (passive open)");
            }
            // Handshake completion
            TcpState::SynReceived if has_ack => {
                self.snd_una = header.ack;
                self.snd_wnd = header.window;
                self.state = TcpState::Established;
                log::info!("[tcp] connection established (passive)");
            }
            TcpState::Established => {
                // P16-T052: Flow control
                if has_ack {
                    self.snd_una = header.ack;
                    self.snd_wnd = header.window;
                }
                // P16-T048: Data receive
                if !payload.is_empty() && header.seq == self.rcv_nxt {
                    self.recv_buf.extend(payload);
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(payload.len() as u32);
                    self.send_segment(TCP_ACK, &[]);
                }
                // P16-T050: Passive close — FIN received
                if has_fin {
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                    self.send_segment(TCP_ACK, &[]);
                    self.state = TcpState::CloseWait;
                    log::debug!("[tcp] FIN received → CloseWait");
                }
            }
            TcpState::FinWait1 if has_ack => {
                self.snd_una = header.ack;
                if has_fin {
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                    self.send_segment(TCP_ACK, &[]);
                    self.state = TcpState::TimeWait;
                } else {
                    self.state = TcpState::FinWait2;
                }
            }
            TcpState::FinWait2 if has_fin => {
                self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                self.send_segment(TCP_ACK, &[]);
                self.state = TcpState::TimeWait;
                log::debug!("[tcp] FIN received in FinWait2 → TimeWait");
            }
            TcpState::LastAck if has_ack => {
                self.state = TcpState::Closed;
                log::debug!("[tcp] ACK received in LastAck → Closed");
            }
            TcpState::TimeWait => {
                self.state = TcpState::Closed;
            }
            _ => {}
        }
    }
}

// ===========================================================================
// Global TCP state
// ===========================================================================

const MAX_TCP_CONNECTIONS: usize = 4;

struct TcpConnections {
    conns: [Option<TcpConnection>; MAX_TCP_CONNECTIONS],
}

impl TcpConnections {
    const fn new() -> Self {
        Self {
            conns: [None, None, None, None],
        }
    }
}

static TCP_CONNS: Mutex<TcpConnections> = Mutex::new(TcpConnections::new());

/// Create a new TCP connection in the Closed state and return its index.
pub fn create(local_port: u16) -> Option<usize> {
    let local_ip = super::config::our_ip();
    let mut conns = TCP_CONNS.lock();
    for (i, slot) in conns.conns.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(TcpConnection::new(local_ip, local_port));
            return Some(i);
        }
    }
    None
}

/// Start an active connection (client).
pub fn connect(conn_idx: usize, remote_ip: Ipv4Addr, remote_port: u16) {
    let mut conns = TCP_CONNS.lock();
    if let Some(conn) = conns.conns[conn_idx].as_mut() {
        conn.connect(remote_ip, remote_port);
    }
}

/// Put a connection into Listen state (server).
pub fn listen(conn_idx: usize) {
    let mut conns = TCP_CONNS.lock();
    if let Some(conn) = conns.conns[conn_idx].as_mut() {
        conn.listen();
    }
}

/// Send data on an established connection.
pub fn send(conn_idx: usize, data: &[u8]) {
    let mut conns = TCP_CONNS.lock();
    if let Some(conn) = conns.conns[conn_idx].as_mut() {
        conn.tcp_send(data);
    }
}

/// Read received data from a connection.
pub fn recv(conn_idx: usize, buf: &mut [u8]) -> usize {
    let mut conns = TCP_CONNS.lock();
    if let Some(conn) = conns.conns[conn_idx].as_mut() {
        let n = buf.len().min(conn.recv_buf.len());
        for byte in buf.iter_mut().take(n) {
            *byte = conn.recv_buf.pop_front().unwrap();
        }
        n
    } else {
        0
    }
}

/// Get the state of a connection.
pub fn state(conn_idx: usize) -> TcpState {
    let conns = TCP_CONNS.lock();
    conns.conns[conn_idx]
        .as_ref()
        .map(|c| c.state)
        .unwrap_or(TcpState::Closed)
}

/// Close a connection.
pub fn close(conn_idx: usize) {
    let mut conns = TCP_CONNS.lock();
    if let Some(conn) = conns.conns[conn_idx].as_mut() {
        conn.close();
    }
}

/// Destroy a connection (free the slot).
pub fn destroy(conn_idx: usize) {
    let mut conns = TCP_CONNS.lock();
    conns.conns[conn_idx] = None;
}

// ===========================================================================
// Incoming packet handler
// ===========================================================================

/// Handle an incoming TCP segment from the IPv4 layer.
pub fn handle_tcp(ip_header: &Ipv4Header, payload: &[u8]) {
    let (tcp_hdr, tcp_data) = match parse(payload) {
        Some(h) => h,
        None => return,
    };

    let mut conns = TCP_CONNS.lock();

    // Find matching connection.
    for conn in conns.conns.iter_mut().flatten() {
        let port_match = conn.local_port == tcp_hdr.dst_port;
        let is_listen = conn.state == TcpState::Listen;
        let full_match =
            port_match && conn.remote_ip == ip_header.src && conn.remote_port == tcp_hdr.src_port;

        if full_match || (is_listen && port_match) {
            if is_listen {
                conn.remote_ip = ip_header.src;
            }
            conn.handle_segment(&tcp_hdr, tcp_data);
            return;
        }
    }

    // No matching connection — send RST per RFC 793 Section 3.4.
    if tcp_hdr.flags & TCP_RST == 0 {
        let local_ip = super::config::our_ip();
        let has_ack = tcp_hdr.flags & TCP_ACK != 0;

        // RFC 793: If the incoming segment has ACK, the RST takes its
        // sequence number from the ACK field. Otherwise, the RST has
        // sequence 0, the ACK flag is set, and the ACK field is set to
        // SEQ + segment_length (SYN and FIN each count as one).
        let seg_len = tcp_data.len() as u32
            + if tcp_hdr.flags & TCP_SYN != 0 { 1 } else { 0 }
            + if tcp_hdr.flags & TCP_FIN != 0 { 1 } else { 0 };

        let (rst_seq, rst_ack, rst_flags) = if has_ack {
            (tcp_hdr.ack, 0u32, TCP_RST)
        } else {
            (0u32, tcp_hdr.seq.wrapping_add(seg_len), TCP_RST | TCP_ACK)
        };

        let p = TcpBuildParams {
            src_ip: local_ip,
            dst_ip: ip_header.src,
            src_port: tcp_hdr.dst_port,
            dst_port: tcp_hdr.src_port,
            seq: rst_seq,
            ack: rst_ack,
            flags: rst_flags,
            window: 0,
        };
        let rst = build(&p, &[]);
        ipv4::send(ip_header.src, ipv4::PROTO_TCP, &rst);
    }
}
