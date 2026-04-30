//! Unix domain sockets — Phase 39.
//!
//! Provides `AF_UNIX` stream and datagram sockets for local IPC.
//! Uses a separate table from `SOCKET_TABLE` since IPv4-centric fields
//! (IP addresses, ports, TCP slots) do not apply to path-based semantics.

extern crate alloc;

use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
    vec::Vec,
};

use crate::task::scheduler::IrqSafeMutex;
use crate::task::wait_queue::WaitQueue;

// ===========================================================================
// A.1 — UnixSocketType and UnixSocketState enums
// ===========================================================================

/// Unix socket type: stream (connection-oriented) or datagram (connectionless).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketType {
    Stream,
    Datagram,
}

/// Unix socket lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketState {
    Unbound,
    Bound,
    Listening,
    Connecting,
    Connected,
    #[allow(dead_code)]
    Closed,
}

// ===========================================================================
// A.2 — UnixSocket and UnixDatagram structs
// ===========================================================================

/// A single datagram message with sender information.
pub struct UnixDatagram {
    pub data: Vec<u8>,
    pub sender_path: Option<String>,
}

/// Maximum size of the stream receive buffer per socket.
pub const UNIX_STREAM_BUF_SIZE: usize = 8192;

/// Maximum number of queued datagrams per socket.
pub const UNIX_DGRAM_QUEUE_MAX: usize = 32;

/// Per-socket kernel object for Unix domain sockets.
pub struct UnixSocket {
    pub socket_type: UnixSocketType,
    pub state: UnixSocketState,
    /// Filesystem path this socket is bound to (if any).
    pub path: Option<String>,
    /// Handle index of the peer socket (for connected stream or connected datagram).
    pub peer: Option<usize>,
    /// Stream receive buffer (byte-oriented ring buffer).
    pub recv_buf: VecDeque<u8>,
    /// Datagram receive queue (message-oriented).
    pub dgram_queue: VecDeque<UnixDatagram>,
    /// Pending connection backlog (handle indices of connecting sockets).
    pub backlog: VecDeque<usize>,
    /// Maximum backlog size for listening sockets.
    pub backlog_limit: usize,
    /// True if shutdown(SHUT_RD) was called.
    pub shut_rd: bool,
    /// True if shutdown(SHUT_WR) was called.
    pub shut_wr: bool,
    /// Reference count — number of FDs pointing to this socket.
    pub refcount: u32,
}

impl UnixSocket {
    /// Create a new Unix socket of the given type with default state.
    pub fn new(socket_type: UnixSocketType) -> Self {
        Self {
            socket_type,
            state: UnixSocketState::Unbound,
            path: None,
            peer: None,
            recv_buf: VecDeque::new(),
            dgram_queue: VecDeque::new(),
            backlog: VecDeque::new(),
            backlog_limit: 0,
            shut_rd: false,
            shut_wr: false,
            refcount: 1,
        }
    }
}

// ===========================================================================
// A.3 — UNIX_SOCKET_TABLE global table
// ===========================================================================

/// Maximum number of Unix domain sockets system-wide.
pub const MAX_UNIX_SOCKETS: usize = 32;

struct UnixSocketTable {
    entries: [Option<UnixSocket>; MAX_UNIX_SOCKETS],
}

impl UnixSocketTable {
    const fn new() -> Self {
        const NONE: Option<UnixSocket> = None;
        Self {
            entries: [NONE; MAX_UNIX_SOCKETS],
        }
    }
}

// Phase 57b G.2.a — IrqSafeMutex inherits Track F.1's preempt-discipline.
// AF_UNIX sockets are touched only from socket syscalls (task context);
// no ISR holds this lock.  Pure type change.
static UNIX_SOCKET_TABLE: IrqSafeMutex<UnixSocketTable> = IrqSafeMutex::new(UnixSocketTable::new());

/// Allocate a new Unix socket entry. Returns the handle (index) or None if full.
pub fn alloc_unix_socket(socket_type: UnixSocketType) -> Option<usize> {
    let mut table = UNIX_SOCKET_TABLE.lock();
    for (i, slot) in table.entries.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(UnixSocket::new(socket_type));
            return Some(i);
        }
    }
    None
}

/// Decrement refcount; free the entry only when it reaches zero.
pub fn free_unix_socket(handle: usize) {
    let mut cleanup_path: Option<String> = None;
    let mut peer_handle: Option<usize> = None;
    {
        let mut table = UNIX_SOCKET_TABLE.lock();
        let should_free =
            if let Some(entry) = table.entries.get_mut(handle).and_then(|s| s.as_mut()) {
                entry.refcount = entry.refcount.saturating_sub(1);
                entry.refcount == 0
            } else {
                return;
            };
        if should_free {
            if let Some(entry) = table.entries.get_mut(handle).and_then(|s| s.as_mut()) {
                cleanup_path = entry.path.clone();
                peer_handle = entry.peer;
                // Drain any pending backlog connections.
                entry.backlog.clear();
            }
            // Unbind path before freeing the slot to prevent stale resolution.
            if let Some(ref path) = cleanup_path {
                unbind_path(path);
            }
            // Clear the peer's reference to this handle to prevent stale pointers.
            if let Some(ph) = peer_handle
                && let Some(peer_entry) = table.entries.get_mut(ph).and_then(|s| s.as_mut())
                && peer_entry.peer == Some(handle)
            {
                peer_entry.peer = None;
            }
            if let Some(slot) = table.entries.get_mut(handle) {
                *slot = None;
            }
        }
    }
    // Wake peer so they see EOF/POLLHUP.
    if let Some(peer) = peer_handle {
        wake_unix_socket(peer);
    }
    // Wake any pollers on this socket.
    wake_unix_socket(handle);
}

/// Increment refcount (called when FD table is cloned on fork/dup).
pub fn add_unix_socket_ref(handle: usize) {
    let mut table = UNIX_SOCKET_TABLE.lock();
    if let Some(entry) = table.entries.get_mut(handle).and_then(|s| s.as_mut()) {
        entry.refcount += 1;
    }
}

/// Access a Unix socket entry immutably under the lock.
pub fn with_unix_socket<F, R>(handle: usize, f: F) -> Option<R>
where
    F: FnOnce(&UnixSocket) -> R,
{
    let table = UNIX_SOCKET_TABLE.lock();
    table.entries.get(handle)?.as_ref().map(f)
}

/// Access a Unix socket entry mutably under the lock.
pub fn with_unix_socket_mut<F, R>(handle: usize, f: F) -> Option<R>
where
    F: FnOnce(&mut UnixSocket) -> R,
{
    let mut table = UNIX_SOCKET_TABLE.lock();
    table.entries.get_mut(handle)?.as_mut().map(f)
}

/// Access two Unix socket entries mutably under the lock (for peer operations).
/// Returns None if either handle is invalid or they are the same.
#[allow(dead_code)]
pub fn with_unix_socket_pair<F, R>(h1: usize, h2: usize, f: F) -> Option<R>
where
    F: FnOnce(&mut UnixSocket, &mut UnixSocket) -> R,
{
    if h1 == h2 || h1 >= MAX_UNIX_SOCKETS || h2 >= MAX_UNIX_SOCKETS {
        return None;
    }
    let mut table = UNIX_SOCKET_TABLE.lock();
    // Split the entries slice to get mutable references to both.
    let (lo, hi) = if h1 < h2 { (h1, h2) } else { (h2, h1) };
    let (left, right) = table.entries.split_at_mut(hi);
    let lo_entry = left[lo].as_mut()?;
    let hi_entry = right[0].as_mut()?;
    if h1 < h2 {
        Some(f(lo_entry, hi_entry))
    } else {
        Some(f(hi_entry, lo_entry))
    }
}

// ===========================================================================
// A.4 — Unix socket WaitQueues
// ===========================================================================

/// Per-socket wait queues for blocking I/O and poll/epoll registration.
#[allow(clippy::declare_interior_mutable_const)]
pub static UNIX_SOCKET_WAITQUEUES: [WaitQueue; MAX_UNIX_SOCKETS] = {
    const WQ: WaitQueue = WaitQueue::new();
    [WQ; MAX_UNIX_SOCKETS]
};

/// Wake all tasks waiting on the given Unix socket.
pub fn wake_unix_socket(handle: usize) {
    if handle < MAX_UNIX_SOCKETS {
        UNIX_SOCKET_WAITQUEUES[handle].wake_all();
    }
}

// ===========================================================================
// D.4 — Path-to-handle map for named sockets
// ===========================================================================

// Phase 57b G.2.a — IrqSafeMutex inherits Track F.1's preempt-discipline.
// Path-map lookups happen only from task-context bind/connect syscalls.
static UNIX_PATH_MAP: IrqSafeMutex<BTreeMap<String, usize>> = IrqSafeMutex::new(BTreeMap::new());

/// Register a binding from a filesystem path to a Unix socket handle.
/// Returns `Err(())` if the path is already bound.
pub fn bind_path(path: &str, handle: usize) -> Result<(), ()> {
    let mut map = UNIX_PATH_MAP.lock();
    if map.contains_key(path) {
        return Err(());
    }
    map.insert(String::from(path), handle);
    Ok(())
}

/// Look up which Unix socket handle is bound to a given path.
pub fn lookup_path(path: &str) -> Option<usize> {
    let map = UNIX_PATH_MAP.lock();
    map.get(path).copied()
}

/// Remove the binding for a path (called on socket close or explicit unbind).
pub fn unbind_path(path: &str) {
    let mut map = UNIX_PATH_MAP.lock();
    map.remove(path);
}

// ===========================================================================
// E.3 — Stream read/write data path
// ===========================================================================

/// Write data to a connected stream socket's peer recv_buf.
/// Returns the number of bytes written, or a negative error.
pub fn unix_stream_write(handle: usize, data: &[u8]) -> Result<usize, i64> {
    let peer_handle = with_unix_socket(handle, |s| {
        if s.shut_wr {
            return Err(-32_i64); // EPIPE
        }
        match s.peer {
            Some(p) => Ok(p),
            None => Err(-107_i64), // ENOTCONN
        }
    })
    .ok_or(-9_i64)??; // EBADF

    // Check if peer is still alive and has space.
    let written = with_unix_socket_mut(peer_handle, |peer| {
        let space = UNIX_STREAM_BUF_SIZE.saturating_sub(peer.recv_buf.len());
        if space == 0 {
            return Err(-11_i64); // EAGAIN — buffer full
        }
        let n = data.len().min(space);
        peer.recv_buf.extend(&data[..n]);
        Ok(n)
    })
    .ok_or(-32_i64)??; // EPIPE — peer socket freed

    wake_unix_socket(peer_handle);
    Ok(written)
}

/// Read data from a stream socket's own recv_buf.
/// Returns the number of bytes read (0 = EOF).
pub fn unix_stream_read(handle: usize, buf: &mut [u8]) -> Result<usize, i64> {
    let (n, peer, state, shut_rd) = with_unix_socket_mut(handle, |s| {
        let n = buf.len().min(s.recv_buf.len());
        for (i, byte) in s.recv_buf.drain(..n).enumerate() {
            buf[i] = byte;
        }
        (n, s.peer, s.state, s.shut_rd)
    })
    .ok_or(-9_i64)?; // EBADF

    // Reject reads on unconnected sockets.
    if !matches!(state, UnixSocketState::Connected) && peer.is_none() && n == 0 {
        return Err(-107_i64); // ENOTCONN
    }

    // If we read data, wake the peer (space freed in recv_buf).
    if n > 0 {
        if let Some(p) = peer {
            wake_unix_socket(p);
        }
        return Ok(n);
    }

    // Buffer empty: check for EOF conditions.
    if shut_rd {
        return Ok(0); // shut_rd was set, return EOF
    }

    // Check if peer closed or shut_wr.
    let peer_alive = if let Some(p) = peer {
        with_unix_socket(p, |ps| !ps.shut_wr).unwrap_or(false)
    } else {
        false
    };
    if !peer_alive {
        return Ok(0); // EOF — peer gone or shut_wr
    }

    Err(-11_i64) // EAGAIN — no data yet, peer still alive
}

// ===========================================================================
// F.1/F.2 — Datagram send/receive
// ===========================================================================

/// Send a datagram to a target Unix socket.
/// `target_handle` is the destination socket's handle.
pub fn unix_dgram_send(
    sender_path: Option<String>,
    target_handle: usize,
    data: &[u8],
) -> Result<usize, i64> {
    let n = data.len();
    with_unix_socket_mut(target_handle, |target| {
        if target.dgram_queue.len() >= UNIX_DGRAM_QUEUE_MAX {
            return Err(-11_i64); // EAGAIN
        }
        target.dgram_queue.push_back(UnixDatagram {
            data: Vec::from(data),
            sender_path,
        });
        Ok(n)
    })
    .ok_or(-111_i64)? // ECONNREFUSED — target gone
}

/// Receive a datagram from a datagram socket's own queue.
/// Returns (bytes_copied, sender_path).
pub fn unix_dgram_recv(handle: usize, buf: &mut [u8]) -> Result<(usize, Option<String>), i64> {
    let result = with_unix_socket_mut(handle, |s| match s.dgram_queue.pop_front() {
        Some(dgram) => {
            let n = buf.len().min(dgram.data.len());
            buf[..n].copy_from_slice(&dgram.data[..n]);
            Ok((n, dgram.sender_path))
        }
        None => Err(-11_i64), // EAGAIN
    })
    .ok_or(-9_i64)?; // EBADF

    // Wake senders/pollers that may be waiting for queue space.
    if result.is_ok() {
        wake_unix_socket(handle);
    }
    result
}
