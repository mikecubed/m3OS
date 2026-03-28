//! Network stack — Phase 16.
//!
//! Layers: virtio-net driver → Ethernet → ARP → IPv4 → ICMP / UDP / TCP.

pub mod arp;
pub mod config;
#[allow(dead_code)]
pub mod dispatch;
pub mod ethernet;
pub mod icmp;
pub mod ipv4;
pub mod tcp;
pub mod udp;
pub mod virtio_net;

// ===========================================================================
// Socket table — Phase 23
// ===========================================================================

use spin::Mutex;

/// Maximum number of open sockets system-wide.
pub const MAX_SOCKETS: usize = 32;

/// Socket handle — index into the global socket table.
pub type SocketHandle = u32;

/// Socket kind: stream (TCP) or datagram (UDP/ICMP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketKind {
    Stream,
    Dgram,
}

/// Socket protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketProtocol {
    Tcp,
    Udp,
    Icmp,
}

/// Socket lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    Unbound,
    Bound,
    Connected,
    Listening,
    Closed,
}

/// Socket options set by setsockopt.
#[derive(Debug, Clone, Copy)]
pub struct SocketOptions {
    pub reuse_addr: bool,
    pub keep_alive: bool,
    pub tcp_nodelay: bool,
    pub recv_buf_size: u32,
    pub send_buf_size: u32,
}

impl SocketOptions {
    const fn default() -> Self {
        Self {
            reuse_addr: false,
            keep_alive: false,
            tcp_nodelay: false,
            recv_buf_size: 8192,
            send_buf_size: 8192,
        }
    }
}

/// A single socket entry in the global table.
pub struct SocketEntry {
    pub kind: SocketKind,
    pub protocol: SocketProtocol,
    pub local_addr: [u8; 4],
    pub local_port: u16,
    pub remote_addr: [u8; 4],
    pub remote_port: u16,
    pub state: SocketState,
    /// Index into TCP connection table (for Stream sockets).
    pub tcp_slot: Option<usize>,
    /// UDP port binding exists (for Dgram/UDP sockets).
    pub udp_bound: bool,
    pub options: SocketOptions,
    /// True if shutdown(SHUT_RD) was called.
    pub shut_rd: bool,
    /// True if shutdown(SHUT_WR) was called.
    pub shut_wr: bool,
}

struct SocketTable {
    entries: [Option<SocketEntry>; MAX_SOCKETS],
}

impl SocketTable {
    const fn new() -> Self {
        // const initializer: array of None
        const NONE: Option<SocketEntry> = None;
        Self {
            entries: [NONE; MAX_SOCKETS],
        }
    }
}

static SOCKET_TABLE: Mutex<SocketTable> = Mutex::new(SocketTable::new());

/// Allocate a new socket entry. Returns the handle (index) or None if full.
pub fn alloc_socket(kind: SocketKind, protocol: SocketProtocol) -> Option<SocketHandle> {
    let mut table = SOCKET_TABLE.lock();
    for (i, slot) in table.entries.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(SocketEntry {
                kind,
                protocol,
                local_addr: [0; 4],
                local_port: 0,
                remote_addr: [0; 4],
                remote_port: 0,
                state: SocketState::Unbound,
                tcp_slot: None,
                udp_bound: false,
                options: SocketOptions::default(),
                shut_rd: false,
                shut_wr: false,
            });
            return Some(i as SocketHandle);
        }
    }
    None
}

/// Free a socket entry, cleaning up TCP/UDP resources.
pub fn free_socket(handle: SocketHandle) {
    let mut table = SOCKET_TABLE.lock();
    if let Some(entry) = table
        .entries
        .get_mut(handle as usize)
        .and_then(|s| s.as_mut())
    {
        // Clean up TCP connection slot
        if let Some(tcp_idx) = entry.tcp_slot {
            tcp::close(tcp_idx);
            tcp::destroy(tcp_idx);
        }
        if entry.udp_bound {
            udp::unbind(entry.local_port);
        }
    }
    if let Some(slot) = table.entries.get_mut(handle as usize) {
        *slot = None;
    }
}

/// Access a socket entry immutably under the lock.
pub fn with_socket<F, R>(handle: SocketHandle, f: F) -> Option<R>
where
    F: FnOnce(&SocketEntry) -> R,
{
    let table = SOCKET_TABLE.lock();
    table.entries.get(handle as usize)?.as_ref().map(f)
}

/// Access a socket entry mutably under the lock.
pub fn with_socket_mut<F, R>(handle: SocketHandle, f: F) -> Option<R>
where
    F: FnOnce(&mut SocketEntry) -> R,
{
    let mut table = SOCKET_TABLE.lock();
    table.entries.get_mut(handle as usize)?.as_mut().map(f)
}
