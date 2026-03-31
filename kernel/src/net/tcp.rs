//! TCP state machine — pure types re-exported from kernel-core,
//! connection state machine and global state remain in kernel.

use alloc::collections::VecDeque;
use spin::Mutex;

use super::arp::Ipv4Addr;
use super::ipv4::{self, Ipv4Header};

pub use kernel_core::net::tcp::{TCP_ACK, TCP_FIN, TCP_PSH, TCP_RST, TCP_SYN, TcpHeader};
use kernel_core::net::tcp::{TcpBuildParams, build, parse};

// ===========================================================================
// TCP State
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

/// Default TCP window size.
const DEFAULT_WINDOW: u16 = 8192;

/// A single TCP connection.
pub struct TcpConnection {
    pub state: TcpState,
    pub local_ip: Ipv4Addr,
    pub local_port: u16,
    pub remote_ip: Ipv4Addr,
    pub remote_port: u16,
    pub snd_una: u32,
    pub snd_nxt: u32,
    pub snd_wnd: u16,
    pub rcv_nxt: u32,
    pub rcv_wnd: u16,
    pub recv_buf: VecDeque<u8>,
    #[allow(dead_code)]
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

    fn send_segment(&self, flags: u8, payload: &[u8]) {
        let p = self.build_params(flags);
        let seg = build(&p, payload);
        ipv4::send(self.remote_ip, ipv4::PROTO_TCP, &seg);
    }

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

    fn tcp_send(&mut self, data: &[u8]) {
        if self.state != TcpState::Established {
            return;
        }
        self.send_segment(TCP_ACK | TCP_PSH, data);
        self.snd_nxt = self.snd_nxt.wrapping_add(data.len() as u32);
    }

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

    fn handle_segment(&mut self, header: &TcpHeader, payload: &[u8]) {
        if header.flags & TCP_RST != 0 {
            log::info!("[tcp] RST received — connection closed");
            self.state = TcpState::Closed;
            return;
        }

        let has_syn = header.flags & TCP_SYN != 0;
        let has_ack = header.flags & TCP_ACK != 0;
        let has_fin = header.flags & TCP_FIN != 0;

        match self.state {
            TcpState::SynSent if has_syn && has_ack => {
                self.rcv_nxt = header.seq.wrapping_add(1);
                self.snd_una = header.ack;
                self.snd_wnd = header.window;
                self.send_segment(TCP_ACK, &[]);
                self.state = TcpState::Established;
                log::info!("[tcp] connection established (active)");
            }
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
            TcpState::SynReceived if has_ack => {
                self.snd_una = header.ack;
                self.snd_wnd = header.window;
                self.state = TcpState::Established;
                log::info!("[tcp] connection established (passive)");
            }
            TcpState::Established => {
                if has_ack {
                    self.snd_una = header.ack;
                    self.snd_wnd = header.window;
                }
                if !payload.is_empty() && header.seq == self.rcv_nxt {
                    self.recv_buf.extend(payload);
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(payload.len() as u32);
                    self.send_segment(TCP_ACK, &[]);
                }
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

const MAX_TCP_CONNECTIONS: usize = 8;

struct TcpConnections {
    conns: [Option<TcpConnection>; MAX_TCP_CONNECTIONS],
}

impl TcpConnections {
    const fn new() -> Self {
        Self {
            conns: [None, None, None, None, None, None, None, None],
        }
    }
}

static TCP_CONNS: Mutex<TcpConnections> = Mutex::new(TcpConnections::new());

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

pub fn connect(conn_idx: usize, remote_ip: Ipv4Addr, remote_port: u16) {
    let mut conns = TCP_CONNS.lock();
    if let Some(slot) = conns.conns.get_mut(conn_idx)
        && let Some(conn) = slot.as_mut()
    {
        conn.connect(remote_ip, remote_port);
    }
}

pub fn listen(conn_idx: usize) {
    let mut conns = TCP_CONNS.lock();
    if let Some(slot) = conns.conns.get_mut(conn_idx)
        && let Some(conn) = slot.as_mut()
    {
        conn.listen();
    }
}

pub fn send(conn_idx: usize, data: &[u8]) {
    let mut conns = TCP_CONNS.lock();
    if let Some(slot) = conns.conns.get_mut(conn_idx)
        && let Some(conn) = slot.as_mut()
    {
        conn.tcp_send(data);
    }
}

pub fn recv(conn_idx: usize, buf: &mut [u8]) -> usize {
    let mut conns = TCP_CONNS.lock();
    let conn = match conns.conns.get_mut(conn_idx).and_then(|s| s.as_mut()) {
        Some(c) => c,
        None => return 0,
    };
    let n = buf.len().min(conn.recv_buf.len());
    for byte in buf.iter_mut().take(n) {
        *byte = conn.recv_buf.pop_front().unwrap();
    }
    n
}

pub fn state(conn_idx: usize) -> TcpState {
    let conns = TCP_CONNS.lock();
    conns
        .conns
        .get(conn_idx)
        .and_then(|s| s.as_ref())
        .map(|c| c.state)
        .unwrap_or(TcpState::Closed)
}

pub fn close(conn_idx: usize) {
    let mut conns = TCP_CONNS.lock();
    if let Some(slot) = conns.conns.get_mut(conn_idx)
        && let Some(conn) = slot.as_mut()
    {
        conn.close();
    }
}

pub fn destroy(conn_idx: usize) {
    let mut conns = TCP_CONNS.lock();
    if let Some(slot) = conns.conns.get_mut(conn_idx) {
        *slot = None;
    }
}

/// Read the peer (remote) IP, remote port, and local port for a connection.
pub fn peer_info(conn_idx: usize) -> Option<([u8; 4], u16, u16)> {
    let conns = TCP_CONNS.lock();
    conns
        .conns
        .get(conn_idx)?
        .as_ref()
        .map(|c| (c.remote_ip, c.remote_port, c.local_port))
}

/// Check if the TCP connection's recv buffer has data.
pub fn has_recv_data(conn_idx: usize) -> bool {
    let conns = TCP_CONNS.lock();
    conns
        .conns
        .get(conn_idx)
        .and_then(|s| s.as_ref())
        .map(|c| !c.recv_buf.is_empty())
        .unwrap_or(false)
}

pub fn handle_tcp(ip_header: &Ipv4Header, payload: &[u8]) {
    let (tcp_hdr, tcp_data) = match parse(payload) {
        Some(h) => h,
        None => return,
    };

    let mut conns = TCP_CONNS.lock();

    // First pass: prefer exact (established) match over listen match.
    // This prevents a listen socket on the same port from stealing
    // data segments destined for an established connection.
    let mut listen_idx: Option<usize> = None;
    for (i, conn) in conns.conns.iter_mut().enumerate() {
        let conn = match conn.as_mut() {
            Some(c) => c,
            None => continue,
        };
        let port_match = conn.local_port == tcp_hdr.dst_port;
        if !port_match {
            continue;
        }
        let full_match = conn.remote_ip == ip_header.src && conn.remote_port == tcp_hdr.src_port;
        if full_match {
            conn.handle_segment(&tcp_hdr, tcp_data);
            return;
        }
        if conn.state == TcpState::Listen && listen_idx.is_none() {
            listen_idx = Some(i);
        }
    }
    // No established match — fall back to listen socket (for SYN).
    if let Some(idx) = listen_idx
        && let Some(conn) = conns.conns[idx].as_mut()
    {
        conn.remote_ip = ip_header.src;
        conn.handle_segment(&tcp_hdr, tcp_data);
        return;
    }

    // No connection matched — send RST for non-RST segments.
    log::warn!(
        "[tcp] no match for port {} from {:?}:{} flags=0x{:x} — sending RST",
        tcp_hdr.dst_port,
        ip_header.src,
        tcp_hdr.src_port,
        tcp_hdr.flags
    );
    if tcp_hdr.flags & TCP_RST == 0 {
        let local_ip = super::config::our_ip();
        let has_ack = tcp_hdr.flags & TCP_ACK != 0;

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
