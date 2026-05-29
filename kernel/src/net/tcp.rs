//! TCP state machine — pure types re-exported from kernel-core,
//! connection state machine and global state remain in kernel.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::task::scheduler::IrqSafeMutex;

use super::arp::Ipv4Addr;
use super::ipv4::{self, Ipv4Header};

use kernel_core::net::tcp::{
    MAX_RETRANSMITS, RetransmitQueue, RtoAction, RttEstimator, TcpBuildParams, build, parse,
};
pub use kernel_core::net::tcp::{TCP_ACK, TCP_FIN, TCP_PSH, TCP_RST, TCP_SYN, TcpHeader};

/// Rate-limited log for dropped out-of-order/duplicate TCP payload (m3OS has no
/// reassembly queue). Budgeted so a lossy link cannot flood the serial console;
/// the count confirms whether this path fires during the SSH-disconnect hang.
fn log_tcp_ooo_drop(seq: u32, rcv_nxt: u32, len: usize) {
    use core::sync::atomic::{AtomicU32, Ordering};
    static BUDGET: AtomicU32 = AtomicU32::new(64);
    if BUDGET
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |r| r.checked_sub(1))
        .is_ok()
    {
        log::warn!(
            "[tcp] no-reassembly: dropped out-of-order/dup payload seq={seq} rcv_nxt={rcv_nxt} len={len} (dup-ACK sent)"
        );
    }
}

/// Per-connection cap on outstanding (unacknowledged) bytes. Bounds the
/// retransmit queue's memory footprint; the effective send window is the
/// smaller of this and the peer's advertised window. At 64 connections this is
/// a 4 MiB worst case, but the peer window keeps it far lower in practice.
const MAX_INFLIGHT_BYTES: usize = 64 * 1024;

/// Outbound TCP segments queued during `handle_segment` or other state-machine
/// methods that run under `TCP_CONNS.lock()`. Sending must happen AFTER the
/// lock is released: `ipv4::send` calls into ARP, virtio-net / RemoteNic and
/// allocates, and any of those paths may reacquire locks in an order that
/// would deadlock if held alongside `TCP_CONNS`. The cap of 2 covers the
/// worst case (Established with both data-ACK and FIN-ACK in one packet).
#[derive(Default)]
struct PendingTx {
    items: [Option<(Ipv4Addr, Vec<u8>)>; 2],
}

impl PendingTx {
    fn push(&mut self, dst: Ipv4Addr, bytes: Vec<u8>) {
        for slot in &mut self.items {
            if slot.is_none() {
                *slot = Some((dst, bytes));
                return;
            }
        }
        log::warn!("[tcp] PendingTx overflow — outbound segment dropped");
    }

    fn flush(self) {
        for slot in self.items.into_iter().flatten() {
            ipv4::send(slot.0, ipv4::PROTO_TCP, &slot.1);
        }
    }
}

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
    /// Phase 77 Track D.2 — RFC 6298 RTT/RTO estimator for this connection.
    rtt: RttEstimator,
    /// All outstanding unacked segments (SYN, data, FIN), retained for
    /// retransmission. Holds the single RFC 6298 retransmit timer, which times
    /// the oldest unacked segment.
    retransmit: RetransmitQueue,
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
            rtt: RttEstimator::new(),
            retransmit: RetransmitQueue::new(),
        }
    }

    /// Retain a freshly-sent segment for possible retransmit. `end_seq` is the
    /// sequence number just past the segment (so an inbound ACK >= `end_seq`
    /// fully acknowledges it). Appends to the per-connection queue and arms the
    /// RFC 6298 timer if it was not already running.
    fn arm_retransmit(&mut self, end_seq: u32, flags: u8, payload: &[u8]) {
        let now = crate::arch::x86_64::interrupts::tick_count();
        self.retransmit
            .push(end_seq, flags, payload.to_vec(), now, &self.rtt);
    }

    /// An inbound ACK acknowledged sequence `ack`. Drop every fully-covered
    /// segment from the queue (cumulative ACK), take a Karn-safe RTT sample,
    /// and stop or restart the timer — all handled by the queue.
    fn on_ack(&mut self, ack: u32) {
        let now = crate::arch::x86_64::interrupts::tick_count();
        self.retransmit.on_ack(ack, now, &mut self.rtt);
    }

    /// Called from the periodic `tcp_tick`. If the RTO has fired, replay the
    /// OLDEST outstanding segment (with its original sequence number) into
    /// `out`, applying RFC 6298 exponential backoff. After `MAX_RETRANSMITS` of
    /// the oldest segment the peer is presumed dead and the connection is reset
    /// to `Closed` (observed by the recv loop as EOF and the send loop as a
    /// broken pipe).
    fn service_rto(&mut self, now: u64, out: &mut Vec<(Ipv4Addr, Vec<u8>)>) {
        // A Closed connection must never replay outstanding segments: it may
        // have stale queue entries left by an RST, on_link_down, or
        // TimeWait→Closed transition. Skip entirely rather than generating
        // spurious retransmits to a peer that has already torn down.
        if self.state == TcpState::Closed {
            return;
        }
        match self.retransmit.service_rto(now, &mut self.rtt) {
            RtoAction::Idle => {}
            RtoAction::Reset => {
                log::warn!(
                    "[tcp] connection reset after {} retransmits (state={:?}) remote={}.{}.{}.{}:{}",
                    MAX_RETRANSMITS,
                    self.state,
                    self.remote_ip[0],
                    self.remote_ip[1],
                    self.remote_ip[2],
                    self.remote_ip[3],
                    self.remote_port,
                );
                self.state = TcpState::Closed;
            }
            RtoAction::Retransmit {
                seq,
                flags,
                payload,
            } => {
                let p = TcpBuildParams {
                    src_ip: self.local_ip,
                    dst_ip: self.remote_ip,
                    src_port: self.local_port,
                    dst_port: self.remote_port,
                    seq,
                    ack: self.rcv_nxt,
                    flags,
                    window: self.rcv_wnd,
                };
                let bytes = build(&p, &payload);
                out.push((self.remote_ip, bytes));
                log::debug!(
                    "[tcp] retransmit seq={:#x} flags={:#x} rto={}ms outstanding={}",
                    seq,
                    flags,
                    self.rtt.rto_ms(),
                    self.retransmit.len(),
                );
            }
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

    /// Build an outbound TCP segment and queue it on `pending`. The caller
    /// must call `pending.flush()` after dropping `TCP_CONNS.lock()` —
    /// never send while the lock is held (see `PendingTx` docs).
    fn queue_segment(&self, pending: &mut PendingTx, flags: u8, payload: &[u8]) {
        let p = self.build_params(flags);
        let seg = build(&p, payload);
        pending.push(self.remote_ip, seg);
    }

    fn connect(&mut self, remote_ip: Ipv4Addr, remote_port: u16, pending: &mut PendingTx) {
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
        pending.push(self.remote_ip, syn);
        self.snd_nxt = self.snd_nxt.wrapping_add(1);
        // Phase 77 Track D.2 — retain the SYN for retransmit: a dropped SYN is
        // the most common cause of a hung active open on a lossy link.
        self.arm_retransmit(self.snd_nxt, TCP_SYN, &[]);
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

    /// Queue as much of `data` as the send window allows, retaining every
    /// segment for retransmit. Returns the number of bytes accepted (0 when the
    /// window is currently full — the caller blocks or reports `EAGAIN`).
    ///
    /// Flow control: outstanding (unacked) bytes are capped at the smaller of
    /// the peer's advertised window and `MAX_INFLIGHT_BYTES`. When nothing is
    /// outstanding at least one segment is always accepted, so a small or
    /// transiently-zero advertised window cannot deadlock the sender (the ACK
    /// that reopens the window can only arrive once we have sent something).
    fn tcp_send(&mut self, data: &[u8], pending: &mut PendingTx) -> usize {
        if self.state != TcpState::Established || data.is_empty() {
            return 0;
        }
        let in_flight = self.snd_nxt.wrapping_sub(self.snd_una) as usize;
        let window = (self.snd_wnd as usize).min(MAX_INFLIGHT_BYTES);
        let available = if in_flight == 0 {
            data.len()
        } else {
            window.saturating_sub(in_flight)
        };
        let n = data.len().min(available);
        if n == 0 {
            return 0;
        }
        let payload = &data[..n];
        self.queue_segment(pending, TCP_ACK | TCP_PSH, payload);
        self.snd_nxt = self.snd_nxt.wrapping_add(n as u32);
        // Every data segment is retained — a dropped segment anywhere in a
        // multi-segment window is recovered by `service_rto`.
        self.arm_retransmit(self.snd_nxt, TCP_ACK | TCP_PSH, payload);
        n
    }

    fn close(&mut self, pending: &mut PendingTx) {
        match self.state {
            TcpState::Established | TcpState::CloseWait => {
                self.queue_segment(pending, TCP_FIN | TCP_ACK, &[]);
                self.snd_nxt = self.snd_nxt.wrapping_add(1);
                // Retain the FIN for retransmit so a dropped FIN is replayed by
                // `service_rto`. The queue appends it behind any still-unacked
                // data (its `end_seq` is `snd_nxt` after the +1 phantom byte),
                // so — unlike the old single-slot design — closing a connection
                // with data in flight no longer leaves the FIN unprotected. On
                // a lossless path the covering ACK calls `on_ack` which drops it
                // immediately, so happy-path behaviour is unchanged.
                self.arm_retransmit(self.snd_nxt, TCP_FIN | TCP_ACK, &[]);
                self.state = if self.state == TcpState::Established {
                    TcpState::FinWait1
                } else {
                    TcpState::LastAck
                };
            }
            _ => {}
        }
    }

    fn handle_segment(&mut self, header: &TcpHeader, payload: &[u8], pending: &mut PendingTx) {
        if header.flags & TCP_RST != 0 {
            log::info!("[tcp] RST received — connection closed");
            self.retransmit.clear();
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
                self.on_ack(header.ack); // disarm the SYN retransmit
                self.queue_segment(pending, TCP_ACK, &[]);
                self.state = TcpState::Established;
                log::info!("[tcp] connection established (active)");
            }
            TcpState::Listen if has_syn => {
                self.remote_port = header.src_port;
                self.rcv_nxt = header.seq.wrapping_add(1);
                self.snd_nxt = crate::arch::x86_64::interrupts::tick_count() as u32;
                self.snd_una = self.snd_nxt;
                self.queue_segment(pending, TCP_SYN | TCP_ACK, &[]);
                self.snd_nxt = self.snd_nxt.wrapping_add(1);
                // Retain the SYN-ACK for retransmit (mirrors the active-open SYN
                // arming in `connect`). A lost SYN-ACK is recovered by service_rto;
                // the final handshake ACK (SynReceived if has_ack) calls on_ack
                // which disarms it automatically.
                self.arm_retransmit(self.snd_nxt, TCP_SYN | TCP_ACK, &[]);
                self.state = TcpState::SynReceived;
                log::debug!("[tcp] SYN-ACK sent (passive open)");
            }
            TcpState::SynReceived if has_syn && !has_ack => {
                // Duplicate SYN while in SynReceived means the client did
                // not receive (or did not like) our prior SYN-ACK.
                // Re-queue the SYN-ACK with the original ISN so the
                // client can retry the handshake. Without this, a lost
                // SYN-ACK produces a permanent wedge: the client keeps
                // retransmitting SYNs, each one hitting the `_ => {}`
                // default arm silently, with no server-side recovery.
                //
                // RFC 793 3.4: "If the state is SYN-RECEIVED, then any
                // arriving SYN which does not match the recorded
                // parameters is treated as an error" — but when it DOES
                // match (same peer, same port), RFC-compliant behaviour
                // is to re-send SYN-ACK (or RST; re-send is the
                // recovery path we want here).
                self.queue_segment(pending, TCP_SYN | TCP_ACK, &[]);
                log::info!(
                    "[tcp] SYN-ACK re-queued (duplicate SYN in SynReceived) local_port={} remote={}.{}.{}.{}:{}",
                    self.local_port,
                    self.remote_ip[0],
                    self.remote_ip[1],
                    self.remote_ip[2],
                    self.remote_ip[3],
                    self.remote_port,
                );
            }
            TcpState::SynReceived if has_ack => {
                self.snd_una = header.ack;
                self.snd_wnd = header.window;
                self.on_ack(header.ack);
                self.state = TcpState::Established;
                log::info!("[tcp] connection established (passive)");
            }
            TcpState::Established => {
                if has_ack {
                    self.snd_una = header.ack;
                    self.snd_wnd = header.window;
                    self.on_ack(header.ack); // disarm/measure the data retransmit
                }
                if !payload.is_empty() && header.seq == self.rcv_nxt {
                    self.recv_buf.extend(payload);
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(payload.len() as u32);
                    self.queue_segment(pending, TCP_ACK, &[]);
                } else if !payload.is_empty() {
                    // Out-of-order or duplicate data. m3OS TCP has no reassembly
                    // queue, so we cannot buffer a future segment — but we MUST
                    // still send a duplicate ACK for our current `rcv_nxt` so the
                    // peer learns what we expect and (fast-)retransmits the
                    // missing in-order segment (RFC 5681 §3.2). The previous code
                    // dropped this segment *silently with no ACK*, which stalls
                    // the peer's retransmit logic and can permanently wedge an
                    // interactive stream — the SSH-disconnect hang: the client's
                    // `exit\n` (or a later byte) arrives out-of-order once, is
                    // dropped un-ACKed, and is never re-driven, so the relay
                    // polls forever with no data.
                    self.queue_segment(pending, TCP_ACK, &[]);
                    log_tcp_ooo_drop(header.seq, self.rcv_nxt, payload.len());
                }
                if has_fin {
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                    self.queue_segment(pending, TCP_ACK, &[]);
                    self.state = TcpState::CloseWait;
                    log::debug!("[tcp] FIN received → CloseWait");
                }
            }
            TcpState::FinWait1 if has_ack => {
                self.snd_una = header.ack;
                self.on_ack(header.ack);
                if has_fin {
                    self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                    self.queue_segment(pending, TCP_ACK, &[]);
                    self.state = TcpState::TimeWait;
                } else {
                    self.state = TcpState::FinWait2;
                }
            }
            TcpState::FinWait2 if has_fin => {
                self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                self.queue_segment(pending, TCP_ACK, &[]);
                self.state = TcpState::TimeWait;
                log::debug!("[tcp] FIN received in FinWait2 → TimeWait");
            }
            TcpState::LastAck if has_ack => {
                // Disarm the FIN retransmit before moving to Closed so the
                // CloseWait→LastAck FIN retransmit slot is not left live after
                // the connection teardown completes.
                self.on_ack(header.ack);
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

/// Phase 77 Track D.2 — raised from 8 to 64 to support a realistic concurrent
/// workload (the old fixed `[None; 8]` capped connections far below 1.0 needs).
const MAX_TCP_CONNECTIONS: usize = 64;

struct TcpConnections {
    conns: [Option<TcpConnection>; MAX_TCP_CONNECTIONS],
}

impl TcpConnections {
    const fn new() -> Self {
        // Inline-const array init: `TcpConnection` is not `Copy` (it owns
        // `VecDeque`s and an `RttEstimator`), so the slot list can no longer be
        // a hand-written `[None; N]`. `[const { None }; N]` constructs each slot
        // independently at compile time and scales to any `MAX_TCP_CONNECTIONS`.
        Self {
            conns: [const { None }; MAX_TCP_CONNECTIONS],
        }
    }
}

// Phase 57b G.2.a — IrqSafeMutex inherits Track F.1's preempt-discipline.
// TCP_CONNS is acquired only from task context; the NIC ISR feeds RX through
// wake-queues, never reaching this lock from inside an ISR.  Pure type
// change: callsites compile unchanged through auto-deref.
static TCP_CONNS: IrqSafeMutex<TcpConnections> = IrqSafeMutex::new(TcpConnections::new());

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
    let mut pending = PendingTx::default();
    {
        let mut conns = TCP_CONNS.lock();
        if let Some(slot) = conns.conns.get_mut(conn_idx)
            && let Some(conn) = slot.as_mut()
        {
            conn.connect(remote_ip, remote_port, &mut pending);
        }
    }
    pending.flush();
}

pub fn listen(conn_idx: usize) {
    let mut conns = TCP_CONNS.lock();
    if let Some(slot) = conns.conns.get_mut(conn_idx)
        && let Some(conn) = slot.as_mut()
    {
        conn.listen();
    }
}

/// Queue outbound data on `conn_idx`, honoring the connection's send window.
/// Returns the number of bytes accepted (0 when the window is full or the
/// connection is not `Established`); the syscall layer blocks or reports
/// `EAGAIN` on a 0 return.
pub fn send(conn_idx: usize, data: &[u8]) -> usize {
    let mut pending = PendingTx::default();
    let mut accepted = 0;
    {
        let mut conns = TCP_CONNS.lock();
        if let Some(slot) = conns.conns.get_mut(conn_idx)
            && let Some(conn) = slot.as_mut()
        {
            accepted = conn.tcp_send(data, &mut pending);
        }
    }
    pending.flush();
    accepted
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
    let mut pending = PendingTx::default();
    {
        let mut conns = TCP_CONNS.lock();
        if let Some(slot) = conns.conns.get_mut(conn_idx)
            && let Some(conn) = slot.as_mut()
        {
            conn.close(&mut pending);
        }
    }
    pending.flush();
}

pub fn destroy(conn_idx: usize) {
    let mut conns = TCP_CONNS.lock();
    if let Some(slot) = conns.conns.get_mut(conn_idx) {
        *slot = None;
    }
}

/// Phase 77 Track D.2 — periodic retransmission-timer tick.
///
/// Driven from the net task on a periodic deadline wake (≈200 ms). Scans every
/// active connection: any segment whose RFC 6298 RTO has expired is replayed
/// with its original sequence number and the RTO is doubled (exponential
/// backoff); a connection that exceeds `MAX_RETRANSMITS` is reset. Outbound
/// segments are built under `TCP_CONNS.lock()` into a local buffer and
/// transmitted only after the lock is released — the same never-send-under-lock
/// discipline `PendingTx` enforces, but unbounded so a tick that must
/// retransmit on many connections at once does not drop segments.
pub fn tcp_tick() {
    let now = crate::arch::x86_64::interrupts::tick_count();
    let mut out: Vec<(Ipv4Addr, Vec<u8>)> = Vec::new();
    {
        let mut conns = TCP_CONNS.lock();
        for slot in conns.conns.iter_mut().flatten() {
            slot.service_rto(now, &mut out);
        }
    }
    for (dst, bytes) in out {
        ipv4::send(dst, ipv4::PROTO_TCP, &bytes);
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

/// Force-close active TCP connections on link-down.
// Called by RemoteNic::apply_link_event; allow dead_code until E.5 wires it.
#[allow(dead_code)]
///
/// Phase 16 / Phase 55b E.4: called by `RemoteNic::apply_link_event` when the
/// PHY goes down. Transitions every `Established` / `SynSent` / `SynReceived`
/// / `FinWait1` / `FinWait2` connection to `Closed` so the upper layers
/// (VFS, application) observe an error rather than a silent hang. Retransmit
/// timer state lives on the per-connection slot and is released when the
/// connection slot is dropped; this function does not touch it directly.
/// The one-line call site in `remote.rs` is:
///
/// ```rust,ignore
/// super::tcp::on_link_down();
/// ```
pub fn on_link_down() {
    let mut conns = TCP_CONNS.lock();
    for slot in conns.conns.iter_mut().flatten() {
        match slot.state {
            TcpState::Established
            | TcpState::SynSent
            | TcpState::SynReceived
            | TcpState::FinWait1
            | TcpState::FinWait2 => {
                log::info!(
                    "[tcp] on_link_down: resetting connection (state={:?})",
                    slot.state,
                );
                slot.state = TcpState::Closed;
            }
            _ => {}
        }
    }
}

pub fn handle_tcp(ip_header: &Ipv4Header, payload: &[u8]) {
    let (tcp_hdr, tcp_data) = match parse(payload) {
        Some(h) => h,
        None => return,
    };

    // Outputs computed under the lock; actual sends + socket wakes happen
    // after `drop(conns)` so the ARP / NIC / allocator paths never run
    // with `TCP_CONNS` held.
    let mut pending = PendingTx::default();
    let mut wake_slot: Option<usize> = None;
    let mut send_rst = false;

    {
        let mut conns = TCP_CONNS.lock();

        // First pass: prefer exact (established) match over listen match.
        // This prevents a listen socket on the same port from stealing
        // data segments destined for an established connection.
        let mut listen_idx: Option<usize> = None;
        let mut matched = false;
        for (i, conn) in conns.conns.iter_mut().enumerate() {
            let conn = match conn.as_mut() {
                Some(c) => c,
                None => continue,
            };
            let port_match = conn.local_port == tcp_hdr.dst_port;
            if !port_match {
                continue;
            }
            let full_match =
                conn.remote_ip == ip_header.src && conn.remote_port == tcp_hdr.src_port;
            if full_match {
                conn.handle_segment(&tcp_hdr, tcp_data, &mut pending);
                wake_slot = Some(i);
                matched = true;
                break;
            }
            if conn.state == TcpState::Listen && listen_idx.is_none() {
                listen_idx = Some(i);
            }
        }
        if !matched {
            // No established match — fall back to listen socket (for SYN).
            if let Some(idx) = listen_idx
                && let Some(conn) = conns.conns[idx].as_mut()
            {
                conn.remote_ip = ip_header.src;
                conn.handle_segment(&tcp_hdr, tcp_data, &mut pending);
                wake_slot = Some(idx);
                matched = true;
            }
        }
        if !matched && tcp_hdr.flags & TCP_RST == 0 {
            send_rst = true;
        }
    }

    if send_rst {
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
        pending.push(ip_header.src, rst);
    }

    // Flush outbound segments with the lock released so ARP / NIC / heap
    // paths cannot deadlock against `TCP_CONNS`.
    pending.flush();

    if let Some(idx) = wake_slot {
        super::wake_sockets_for_tcp_slot(idx);
    }
}
