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
pub mod unix;
pub mod virtio_net;

// ===========================================================================
// Socket table — Phase 23
// ===========================================================================

use spin::Mutex;

use crate::task::wait_queue::WaitQueue;

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
    /// Reference count — number of FDs (across all processes) pointing to this socket.
    /// Only freed when this drops to zero.
    pub refcount: u32,
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

/// Per-socket wait queues — woken on data arrival, connection, close, etc.
#[allow(clippy::declare_interior_mutable_const)]
pub static SOCKET_WAITQUEUES: [WaitQueue; MAX_SOCKETS] = {
    const WQ: WaitQueue = WaitQueue::new();
    [WQ; MAX_SOCKETS]
};

/// Wake all tasks waiting on the given socket.
pub fn wake_socket(handle: SocketHandle) {
    if (handle as usize) < MAX_SOCKETS {
        SOCKET_WAITQUEUES[handle as usize].wake_all();
    }
}

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
                refcount: 1,
            });
            return Some(i as SocketHandle);
        }
    }
    None
}

/// Decrement socket refcount; free the entry only when it reaches zero.
pub fn free_socket(handle: SocketHandle) {
    let mut table = SOCKET_TABLE.lock();
    let should_free = if let Some(entry) = table
        .entries
        .get_mut(handle as usize)
        .and_then(|s| s.as_mut())
    {
        entry.refcount = entry.refcount.saturating_sub(1);
        entry.refcount == 0
    } else {
        return;
    };
    if should_free {
        // Clean up TCP/UDP resources.
        if let Some(entry) = table
            .entries
            .get_mut(handle as usize)
            .and_then(|s| s.as_mut())
        {
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
    drop(table);
    // Wake any pollers on this socket (HUP / close notification).
    wake_socket(handle);
}

/// Increment socket refcount (called when FD table is cloned on fork).
pub fn add_socket_ref(handle: SocketHandle) {
    let mut table = SOCKET_TABLE.lock();
    if let Some(entry) = table
        .entries
        .get_mut(handle as usize)
        .and_then(|s| s.as_mut())
    {
        entry.refcount += 1;
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

/// Wake all sockets that reference a given TCP connection slot.
/// Called from the TCP handler after processing an incoming segment.
pub fn wake_sockets_for_tcp_slot(tcp_idx: usize) {
    let mut handles = [0u32; MAX_SOCKETS];
    let mut count = 0;
    {
        let table = SOCKET_TABLE.lock();
        for (i, slot) in table.entries.iter().enumerate() {
            if let Some(entry) = slot
                && entry.tcp_slot == Some(tcp_idx)
            {
                handles[count] = i as u32;
                count += 1;
            }
        }
    }
    for h in &handles[..count] {
        wake_socket(*h);
    }
}

/// Wake all sockets bound to a given UDP port.
/// Called from the UDP handler after receiving a datagram.
pub fn wake_sockets_for_udp_port(port: u16) {
    let mut handles = [0u32; MAX_SOCKETS];
    let mut count = 0;
    {
        let table = SOCKET_TABLE.lock();
        for (i, slot) in table.entries.iter().enumerate() {
            if let Some(entry) = slot
                && entry.protocol == SocketProtocol::Udp
                && entry.local_port == port
            {
                handles[count] = i as u32;
                count += 1;
            }
        }
    }
    for h in &handles[..count] {
        wake_socket(*h);
    }
}
