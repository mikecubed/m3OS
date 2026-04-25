//! # Ownership: Transition
//! Network protocol policy layer — target ring-3 server per microkernel Stage 4.
//!
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
pub mod remote;
pub mod tcp;
pub mod udp;
pub mod unix;
pub mod virtio_net;

use core::sync::atomic::AtomicBool;

// ===========================================================================
// Unified NIC wake flag — used by the network task's park/unpark.
// ===========================================================================

/// Shared signal for `block_current_unless_woken` in the network task.
///
/// The virtio-net ISR (`virtio_net::virtio_net_irq_handler`) sets this in
/// addition to its driver-specific progress flag. The ring-3 e1000 driver
/// (`userspace/drivers/e1000`) delivers RX frames via `RemoteNic::inject_rx_frame`
/// which also sets this flag. The network task parks exclusively on this flag
/// so a wake from any driver reliably unblocks it.
pub static NIC_WOKEN: AtomicBool = AtomicBool::new(false);

/// Edge-triggered flag set by the `net.nic.ingress` pending-send hook to
/// tell `net_task` it has ingress messages to drain. Without this gate
/// `net_task` would call `recv_msg_nowait` (acquiring `ENDPOINTS.lock()`)
/// on every wake even with no ring-3 NIC publishing — the lock-acquire
/// hot-path interacts badly with PID 1's `sys_nanosleep` busy-yield in
/// `serverization-fallback` and amplifies the documented starvation.
pub static INGRESS_HAS_WORK: AtomicBool = AtomicBool::new(false);

// ===========================================================================
// Driver dispatch — Phase 55 E.4
// ===========================================================================

/// Transmit-side error type for driver-facing send calls.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// The link is currently down; the caller should drop or retry later.
    LinkDown,
    /// No network driver has initialized; nothing to transmit on.
    NotReady,
    /// Packet length is zero or exceeds the driver's per-slot buffer.
    TooLarge,
    /// The TX ring is full; the oldest descriptor is still owned by
    /// hardware.
    TxRingFull,
}

/// Send a raw Ethernet frame through whichever NIC driver is initialized.
///
/// Dispatch priority (Phase 55b E.5):
/// 1. `RemoteNic` — ring-3 e1000 driver via IPC, when registered.
///    (See `kernel/src/net/remote.rs` for the `RemoteNic` facade; the
///    device-specific e1000 code now lives entirely in
///    `userspace/drivers/e1000`.)
/// 2. VirtIO-net — fallback for QEMU's default `virtio-net-pci` configuration.
///
/// The upper stack (`arp.rs`, `ipv4.rs`) routes all transmits through this
/// entry point so switching between drivers at runtime requires no upper-layer
/// change. Errors are logged and swallowed to preserve the existing `fn(_) -> ()`
/// surface; callers that need the error may call the driver directly.
pub fn send_frame(frame: &[u8]) {
    // RemoteNic has highest priority when the ring-3 e1000 driver is registered.
    if remote::RemoteNic::is_registered() {
        match remote::RemoteNic::send_frame(frame) {
            Ok(()) => {}
            Err(e) => {
                log::debug!("[net] remote_nic send_frame failed: {:?}", e);
            }
        }
        return;
    }
    virtio_net::send_frame(frame);
}

/// Returns the MAC address of whichever NIC driver initialized first — used
/// by `arp` / `ipv4` to stamp outgoing frames.
///
/// Priority mirrors [`send_frame`]: RemoteNic (ring-3 e1000 via IPC) > VirtIO-net.
/// The in-kernel e1000 driver was removed in Phase 55b E.5; device-specific
/// e1000 code now lives entirely in `userspace/drivers/e1000`.
#[allow(dead_code)]
pub fn mac_address() -> Option<kernel_core::types::MacAddr> {
    if let Some(m) = remote::RemoteNic::mac_address() {
        return Some(m);
    }
    virtio_net::mac_address()
}

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
    /// True once the last ref has started close-time teardown and the handle
    /// must not be reused or resurrected.
    pub closing: bool,
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
                closing: false,
            });
            return Some(i as SocketHandle);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SocketFreeResult {
    pub protocol: Option<SocketProtocol>,
    pub released_last_ref: bool,
    pub needs_finalization: bool,
}

/// Decrement socket refcount; free the entry only when it reaches zero.
pub fn free_socket_with_result(handle: SocketHandle, hold_udp_last_ref: bool) -> SocketFreeResult {
    let mut result = SocketFreeResult::default();
    let mut table = SOCKET_TABLE.lock();
    let should_free = if let Some(entry) = table
        .entries
        .get_mut(handle as usize)
        .and_then(|s| s.as_mut())
    {
        result.protocol = Some(entry.protocol);
        entry.refcount = entry.refcount.saturating_sub(1);
        entry.refcount == 0
    } else {
        return result;
    };
    if should_free {
        result.released_last_ref = true;
        if hold_udp_last_ref
            && result.protocol == Some(SocketProtocol::Udp)
            && let Some(entry) = table
                .entries
                .get_mut(handle as usize)
                .and_then(|s| s.as_mut())
        {
            entry.closing = true;
            result.needs_finalization = true;
        } else {
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
    }
    drop(table);
    // Wake any pollers on this socket (HUP / close notification).
    wake_socket(handle);
    result
}

pub fn finalize_socket_close(handle: SocketHandle) {
    let mut table = SOCKET_TABLE.lock();
    if let Some(entry) = table
        .entries
        .get_mut(handle as usize)
        .and_then(|s| s.as_mut())
    {
        if entry.refcount != 0 || !entry.closing {
            return;
        }
        if let Some(tcp_idx) = entry.tcp_slot {
            tcp::close(tcp_idx);
            tcp::destroy(tcp_idx);
        }
        if entry.udp_bound {
            udp::unbind(entry.local_port);
        }
    } else {
        return;
    }
    if let Some(slot) = table.entries.get_mut(handle as usize) {
        *slot = None;
    }
    drop(table);
    wake_socket(handle);
}

/// Increment socket refcount (called when FD table is cloned on fork).
pub fn add_socket_ref(handle: SocketHandle) {
    let mut table = SOCKET_TABLE.lock();
    if let Some(entry) = table
        .entries
        .get_mut(handle as usize)
        .and_then(|s| s.as_mut())
        && !entry.closing
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
